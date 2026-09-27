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
    let mcp_dir = dir.join("mcp");

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&mcp_dir)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
    }

    #[cfg(not(unix))]
    {
        // On Windows, rely on per-user %LOCALAPPDATA% ACL
        std::fs::create_dir_all(&mcp_dir)?;
    }

    let p = file(dir, d.pid);
    std::fs::write(&p, serde_json::to_vec_pretty(d).unwrap())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&p)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&p, perms)?;
    }

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
