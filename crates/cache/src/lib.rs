//! Derived-data cache (thumbnails, audio proxies, peaks, analysis).
//!
//! Layout: `<root>/<asset-key>/<artifact>` plus `meta.json` recording the
//! algorithm version of each artifact. The asset key hashes the canonical
//! path, size and mtime, so a modified source file gets a fresh key
//! automatically; bumping an artifact's version invalidates old results.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Per-user application directories.
pub struct AppDirs {
    pub data: PathBuf,
}

impl AppDirs {
    pub fn new() -> Self {
        let base = std::env::var_os("KADR_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Kadr")))
            .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".kadr")))
            .unwrap_or_else(|| PathBuf::from(".kadr"));
        AppDirs { data: base }
    }
    pub fn cache(&self) -> PathBuf {
        self.data.join("cache")
    }
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.data.join("settings.json")
    }
}

impl Default for AppDirs {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Cache {
    root: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct Meta {
    source: String,
    artifacts: BTreeMap<String, u32>,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Cache { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Cache entry for a source file (created lazily).
    pub fn for_source(&self, source: &Path) -> io::Result<AssetCache> {
        let key = source_key(source)?;
        let dir = self.root.join(&key);
        fs::create_dir_all(&dir)?;
        Ok(AssetCache { dir, source: source.to_string_lossy().into_owned() })
    }

    /// Total size in bytes (for the settings UI).
    pub fn size_bytes(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            fs::read_dir(p)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| match e.metadata() {
                            Ok(m) if m.is_dir() => walk(&e.path()),
                            Ok(m) => m.len(),
                            Err(_) => 0,
                        })
                        .sum()
                })
                .unwrap_or(0)
        }
        walk(&self.root)
    }
}

pub fn source_key(source: &Path) -> io::Result<String> {
    let canon = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let meta = fs::metadata(source)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = blake3::Hasher::new();
    h.update(canon.to_string_lossy().to_lowercase().as_bytes());
    h.update(&meta.len().to_le_bytes());
    h.update(&mtime.to_le_bytes());
    Ok(h.finalize().to_hex()[..24].to_string())
}

pub struct AssetCache {
    dir: PathBuf,
    source: String,
}

impl AssetCache {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, artifact: &str) -> PathBuf {
        self.dir.join(artifact)
    }

    fn meta(&self) -> Meta {
        fs::read(self.dir.join("meta.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// True if `artifact` exists and was produced by algorithm `version`.
    pub fn is_valid(&self, artifact: &str, version: u32) -> bool {
        self.meta().artifacts.get(artifact) == Some(&version) && self.path(artifact).exists()
    }

    /// Records that `artifact` (already written at `path(artifact)`) is complete.
    pub fn commit(&self, artifact: &str, version: u32) -> io::Result<()> {
        let mut m = self.meta();
        m.source = self.source.clone();
        m.artifacts.insert(artifact.to_string(), version);
        let tmp = self.dir.join("meta.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&m).expect("meta"))?;
        fs::rename(tmp, self.dir.join("meta.json"))
    }

    pub fn read(&self, artifact: &str, version: u32) -> Option<Vec<u8>> {
        if !self.is_valid(artifact, version) {
            return None;
        }
        match fs::read(self.path(artifact)) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(artifact, error = %e, "cache read failed");
                None
            }
        }
    }

    /// Atomic write + commit.
    pub fn write(&self, artifact: &str, version: u32, bytes: &[u8]) -> io::Result<()> {
        let tmp = self.path(&format!("{artifact}.tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, self.path(artifact))?;
        self.commit(artifact, version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioning_and_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.mp4");
        fs::write(&src, b"hello").unwrap();
        let cache = Cache::new(dir.path().join("cache"));
        let c = cache.for_source(&src).unwrap();
        assert!(c.read("peaks", 1).is_none());
        c.write("peaks", 1, b"data").unwrap();
        assert_eq!(c.read("peaks", 1).unwrap(), b"data");
        assert!(c.read("peaks", 2).is_none(), "new algorithm version invalidates");

        // Changing the source changes the key → fresh, empty entry.
        let k1 = source_key(&src).unwrap();
        fs::write(&src, b"hello, modified").unwrap();
        assert_ne!(k1, source_key(&src).unwrap());
        assert!(cache.for_source(&src).unwrap().read("peaks", 1).is_none());
    }
}
