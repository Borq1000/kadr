//! Finding or launching the Kadr instance to drive.

use kadr_mcp_bridge::{call, discovery, Discovery};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

pub fn pick(list: Vec<Discovery>, alive: impl Fn(&Discovery) -> bool) -> Option<Discovery> {
    list.into_iter().filter(|d| alive(d)).max_by_key(|d| (!d.headless, d.started_ms))
}

/// Deletes discovery files of instances that no longer answer.
pub fn clean_stale(dir: &Path, alive: impl Fn(&Discovery) -> bool) {
    for d in discovery::list(dir) {
        if !alive(&d) {
            discovery::remove(dir, d.pid);
        }
    }
}

pub fn answers(d: &Discovery) -> bool {
    call(d.port, &d.token, "ping", json!({}), Duration::from_millis(800)).is_ok()
}

pub struct Instance {
    pub d: Discovery,
    /// Set when we launched it: terminated when the session ends.
    pub child: Option<std::process::Child>,
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
        }
    }
}

/// The user's window if one answers, else a headless Kadr we start.
pub fn connect(data: &Path) -> Result<Instance, String> {
    clean_stale(data, answers);
    if let Some(d) = pick(discovery::list(data), answers) {
        return Ok(Instance { d, child: None });
    }
    let exe = std::env::var_os("KADR_EXE").map(Into::into).unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "kadr.exe" } else { "kadr" }))).unwrap_or_default()
    });
    let child = std::process::Command::new(&exe)
        .arg("--headless")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let pid = child.id();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(15) {
        if let Some(d) = discovery::list(data).into_iter().find(|d| d.pid == pid && answers(d)) {
            return Ok(Instance { d, child: Some(child) });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!("Kadr (pid {pid}) did not become ready within 15 s"))
}
