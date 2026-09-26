//! `.kadr` persistence with crash safety.
//!
//! Save = serialize → `<file>.tmp` → fsync → keep previous as `<file>.bak` →
//! rename tmp over target. A rename within one NTFS volume is atomic, so a
//! crash at any point leaves either the old or the new project intact.

use crate::model::{now_ms, Project, FORMAT_VERSION};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const EXTENSION: &str = "kadr";

#[derive(Debug, Error)]
pub enum ProjectIoError {
    #[error("i/o error on {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("project file {path} is corrupt: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("project was saved by a newer Kadr (format {found}, supported {supported})")]
    TooNew { found: u32, supported: u32 },
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ProjectIoError + '_ {
    move |source| ProjectIoError::Io { path: path.to_path_buf(), source }
}

pub fn to_json(project: &Project) -> Vec<u8> {
    serde_json::to_vec_pretty(project).expect("project serialization is infallible")
}

/// Writes bytes atomically to `path` (tmp + fsync + rename).
pub fn write_atomic(path: &Path, bytes: &[u8], keep_backup: bool) -> Result<(), ProjectIoError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let tmp = sidecar(path, "tmp");
    {
        let mut f = File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(bytes).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
    }
    if keep_backup && path.exists() {
        let bak = sidecar(path, "bak");
        // Best effort: a failed backup must not block saving.
        if let Err(e) = fs::copy(path, &bak) {
            tracing::warn!(error = %e, "could not write project backup");
        }
    }
    fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(())
}

/// Saves the project, updating relative media paths and the modified stamp.
pub fn save(project: &mut Project, path: &Path) -> Result<(), ProjectIoError> {
    project.modified_ms = now_ms();
    project.format_version = FORMAT_VERSION;
    let base = path.parent().unwrap_or(Path::new("."));
    for a in &mut project.assets {
        a.relative_path = relative_to(&a.path, base);
    }
    write_atomic(path, &to_json(project), true)?;
    // A successful explicit save supersedes any autosave.
    let _ = fs::remove_file(autosave_path(path));
    tracing::info!(path = %path.display(), "project saved");
    Ok(())
}

pub fn load(path: &Path) -> Result<Project, ProjectIoError> {
    let bytes = fs::read(path).map_err(io_err(path))?;
    let mut project = parse(&bytes, path)?;
    relink(&mut project, path.parent().unwrap_or(Path::new(".")));
    Ok(project)
}

pub fn parse(bytes: &[u8], path: &Path) -> Result<Project, ProjectIoError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|source| ProjectIoError::Parse { path: path.to_path_buf(), source })?;
    let found = value.get("format_version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if found > FORMAT_VERSION {
        return Err(ProjectIoError::TooNew { found, supported: FORMAT_VERSION });
    }
    let value = migrate(value, found);
    serde_json::from_value(value).map_err(|source| ProjectIoError::Parse { path: path.to_path_buf(), source })
}

/// Upgrades older JSON layouts in place. Format 1 is the first release.
fn migrate(value: serde_json::Value, _from: u32) -> serde_json::Value {
    value
}

/// If an absolute media path no longer exists but the path relative to the
/// project does, switch to it (project folder was moved/copied).
fn relink(project: &mut Project, base: &Path) {
    for a in &mut project.assets {
        if a.path.exists() {
            continue;
        }
        if let Some(rel) = &a.relative_path {
            let candidate = base.join(rel);
            if candidate.exists() {
                tracing::info!(from = %a.path.display(), to = %candidate.display(), "relinked media");
                a.path = candidate;
            }
        }
    }
}

fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    // Only handles media inside the project folder tree; anything else keeps
    // its absolute path.
    path.strip_prefix(base).ok().map(Path::to_path_buf)
}

fn sidecar(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

pub fn autosave_path(project_path: &Path) -> PathBuf {
    sidecar(project_path, "autosave")
}

/// Autosave location for a project that was never saved.
pub fn untitled_autosave_path(data_dir: &Path, project: &Project) -> PathBuf {
    data_dir.join("recovery").join(format!("{}.{EXTENSION}.autosave", project.id))
}

pub fn write_autosave(project: &Project, autosave: &Path) -> Result<(), ProjectIoError> {
    write_atomic(autosave, &to_json(project), false)
}

/// Returns the autosave file if it is newer than the project file — i.e. the
/// previous session ended without saving (crash, power loss).
pub fn recovery_candidate(project_path: &Path) -> Option<PathBuf> {
    let auto = autosave_path(project_path);
    let auto_m = fs::metadata(&auto).and_then(|m| m.modified()).ok()?;
    match fs::metadata(project_path).and_then(|m| m.modified()) {
        Ok(proj_m) if proj_m >= auto_m => None,
        _ => Some(auto),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.kadr");
        write_atomic(&p, b"one", true).unwrap();
        write_atomic(&p, b"two", true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read(sidecar(&p, "bak")).unwrap(), b"one");
        assert!(!sidecar(&p, "tmp").exists());
    }

    #[test]
    fn rejects_newer_format() {
        let json = format!(r#"{{"format_version": {}}}"#, FORMAT_VERSION + 1);
        let err = parse(json.as_bytes(), Path::new("x.kadr")).unwrap_err();
        assert!(matches!(err, ProjectIoError::TooNew { .. }));
    }
}
