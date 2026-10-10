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

/// Upgrades older JSON layouts in place. Format 1 is the first release;
/// format 2 replaced the never-evaluated string-keyed `keyframes` with
/// source-bound `animation` (keyframes spec section 2.2).
fn migrate(mut value: serde_json::Value, from: u32) -> serde_json::Value {
    if from < 2 {
        if let Some(seqs) = value.get_mut("sequences").and_then(|s| s.as_array_mut()) {
            for clip in seqs
                .iter_mut()
                .flat_map(|s| s["tracks"].as_array_mut().into_iter().flatten())
                .flat_map(|t| t["clips"].as_array_mut().into_iter().flatten())
            {
                migrate_clip_keyframes(clip);
            }
        }
        // The upgraded document is a format-2 document in memory too.
        if let Some(obj) = value.as_object_mut() {
            obj.insert("format_version".into(), 2.into());
        }
    }
    value
}

fn migrate_clip_keyframes(clip: &mut serde_json::Value) {
    use serde_json::{json, Map, Value};
    let Some(Value::Array(old)) = clip.as_object_mut().and_then(|o| o.remove("keyframes")) else { return };
    let num = |k: &str| clip.get(k).and_then(Value::as_i64).unwrap_or(0);
    let (sin, sout, tin, tout) = (num("source_in"), num("source_out"), num("timeline_in"), num("timeline_out"));
    let mut anim: Map<String, Value> = Map::new();
    for k in old {
        let prop = k.get("property").and_then(Value::as_str).unwrap_or_default().to_string();
        let param = match prop.as_str() {
            "transform.x" => Some("x"),
            "transform.y" => Some("y"),
            "transform.scale" => Some("scale"),
            "transform.rotation_deg" => Some("rotation"),
            "transform.opacity" => Some("opacity"),
            "transform.crop_left" => Some("crop_left"),
            "transform.crop_right" => Some("crop_right"),
            "transform.crop_top" => Some("crop_top"),
            "transform.crop_bottom" => Some("crop_bottom"),
            "color.exposure" => Some("exposure"),
            "color.contrast" => Some("contrast"),
            "color.saturation" => Some("saturation"),
            "color.temperature" => Some("temperature"),
            _ => None,
        };
        let entry = match param {
            Some(p) => {
                let rel = kadr_core::Time(k.get("time").and_then(Value::as_i64).unwrap_or(0));
                let at = kadr_core::Time(sin) + rel.mul_ratio(sout - sin, (tout - tin).max(1));
                let ease = k.get("interpolation").cloned().unwrap_or(json!("ease_in_out"));
                (p.to_string(), json!({"at": at.flicks(), "value": k.get("value").cloned().unwrap_or(Value::Null), "ease": ease}))
            }
            None => (prop, k),
        };
        anim.entry(entry.0).or_insert_with(|| json!([])).as_array_mut().unwrap().push(entry.1);
    }
    if !anim.is_empty() {
        clip["animation"] = Value::Object(anim);
    }
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

    /// Format 1 clip JSON with the old string-keyed keyframes, 2x speed:
    /// source 10..30 s over timeline 0..10 s.
    fn v1_project() -> serde_json::Value {
        let p = crate::Project::new("old");
        let mut v = serde_json::to_value(&p).unwrap();
        v["format_version"] = serde_json::json!(1);
        let f = |s: f64| (s * kadr_core::FLICKS_PER_SECOND as f64) as i64;
        let clip = serde_json::json!({
            "id": kadr_core::ClipId::new(), "asset": kadr_core::AssetId::new(), "name": "c",
            "source_in": f(10.0), "source_out": f(30.0), "timeline_in": 0, "timeline_out": f(10.0),
            "keyframes": [
                {"property": "transform.scale", "time": f(1.0), "value": 1.2, "interpolation": "linear"},
                {"property": "color.exposure", "time": 0, "value": 0.5, "interpolation": "hold"},
                {"property": "audio.gain_db", "time": 0, "value": -6.0, "interpolation": "ease_in_out"}
            ]
        });
        v["sequences"][0]["tracks"][0]["clips"] = serde_json::json!([clip]);
        v
    }

    #[test]
    fn format_1_keyframes_migrate_to_source_bound_animation() {
        let bytes = serde_json::to_vec(&v1_project()).unwrap();
        let p = parse(&bytes, Path::new("old.kadr")).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        let scale = c.animation.track(crate::AnimParam::Scale);
        assert_eq!(scale.len(), 1);
        assert_eq!(scale[0].at, crate::SourceTime(kadr_core::Time::from_secs(12)), "timeline 1 s at 2x = source 12 s");
        assert_eq!((scale[0].value, &scale[0].ease), (1.2, &crate::Ease::Linear));
        assert_eq!(c.animation.track(crate::AnimParam::Exposure)[0].ease, crate::Ease::Hold);
        assert_eq!(c.animation.unknown_len(), 1, "audio.gain_db is not a V1 animation param: kept, not run");
        let saved = serde_json::to_value(&p).unwrap();
        assert!(saved["sequences"][0]["tracks"][0]["clips"][0].get("keyframes").is_none(), "old field never written");
        assert_eq!(saved["format_version"], serde_json::json!(FORMAT_VERSION));
    }

    #[test]
    fn a_format_2_file_is_too_new_for_a_format_1_reader() {
        assert_eq!(FORMAT_VERSION, 2);
        let json = format!(r#"{{"format_version": {}}}"#, FORMAT_VERSION + 1);
        assert!(matches!(parse(json.as_bytes(), Path::new("x.kadr")).unwrap_err(), ProjectIoError::TooNew { .. }));
    }
}
