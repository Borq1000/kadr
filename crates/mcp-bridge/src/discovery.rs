use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Discovery {
    pub port: u16,
    pub token: String,
    pub ui_port: u16,
    pub pid: u32,
    pub started_ms: i64,
    pub headless: bool,
}

fn file(dir: &Path, pid: u32) -> PathBuf {
    dir.join("mcp").join(format!("{pid}.json"))
}

pub fn write(dir: &Path, d: &Discovery) -> std::io::Result<PathBuf> {
    let p = file(dir, d.pid);
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, serde_json::to_vec_pretty(d).unwrap())?;
    Ok(p)
}

/// Removes only this instance's file.
pub fn remove(dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(file(dir, pid));
}

pub fn list(dir: &Path) -> Vec<Discovery> {
    std::fs::read_dir(dir.join("mcp"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect()
}
