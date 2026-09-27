//! Finding or launching the Kadr instance to drive.

use kadr_mcp_bridge::{call, discovery, pid_alive, Discovery, EXIT_MCP_DISABLED};
use serde_json::json;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

pub fn pick(list: Vec<Discovery>, alive: impl Fn(&Discovery) -> bool) -> Option<Discovery> {
    list.into_iter().filter(|d| alive(d)).max_by_key(|d| (!d.headless, d.started_ms))
}

/// Deletes discovery files of processes that no longer exist. A live
/// process's file is never deleted, however slowly it answers.
pub fn clean_stale(dir: &Path) {
    for d in discovery::list(dir) {
        if !pid_alive(d.pid) {
            discovery::remove(dir, d.pid);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Liveness {
    /// Process alive and its bridge answers `ping` (answered on the bridge
    /// thread, so a busy UI thread doesn't matter).
    Ready,
    /// Process alive but the bridge didn't answer in time: keep it; calls
    /// report `busy_timeout`.
    Busy,
    /// Process exited, or nothing listens on its bridge port any more.
    Gone,
}

pub fn liveness(d: &Discovery) -> Liveness {
    if !pid_alive(d.pid) {
        return Liveness::Gone;
    }
    match call(d.port, &d.token, "ping", json!({}), Duration::from_millis(800)) {
        Ok(_) => Liveness::Ready,
        Err(e) if e.code == "refused" => Liveness::Gone,
        Err(_) => Liveness::Busy,
    }
}

fn ready(d: &Discovery) -> bool {
    liveness(d) == Liveness::Ready
}

/// Why a launched `kadr --headless` exited before becoming ready.
pub fn launch_exit_message(pid: u32, code: Option<i32>) -> String {
    match code {
        Some(EXIT_MCP_DISABLED) => "MCP control is disabled in Kadr settings (Settings → General)".into(),
        Some(c) => format!("Kadr (pid {pid}) exited during startup with code {c}"),
        None => format!("Kadr (pid {pid}) exited during startup"),
    }
}

pub struct Instance {
    pub d: Discovery,
    /// Set when we launched it: terminated when the session ends.
    pub child: Option<Child>,
    /// Kill-on-close job holding `child`: it dies with us even if we are
    /// terminated without running destructors.
    pub job: Option<crate::job::Job>,
}

impl Instance {
    /// An instance we didn't launch (the user's window, or `use_instance`).
    pub fn attached(d: Discovery) -> Instance {
        Instance { d, child: None, job: None }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// The user's window if one answers, else a headless Kadr we start.
pub fn connect(data: &Path) -> Result<Instance, String> {
    clean_stale(data);
    if let Some(d) = pick(discovery::list(data), ready) {
        return Ok(Instance::attached(d));
    }
    let exe = std::env::var_os("KADR_EXE").map(Into::into).unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "kadr.exe" } else { "kadr" }))).unwrap_or_default()
    });
    launch(data, &exe)
}

/// Starts `exe --headless` and waits for its discovery file. On every
/// failure path the child is killed and reaped: no hidden orphans.
pub fn launch(data: &Path, exe: &Path) -> Result<Instance, String> {
    let mut child = Command::new(exe)
        .arg("--headless")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let job = crate::job::Job::kill_on_close(&child);
    let pid = child.id();
    let fail = |child: &mut Child, msg: String| {
        let _ = child.kill();
        let _ = child.wait();
        Err(msg)
    };
    let t0 = Instant::now();
    while t0.elapsed() < LAUNCH_TIMEOUT {
        match child.try_wait() {
            Ok(Some(status)) => return Err(launch_exit_message(pid, status.code())),
            Ok(None) => {}
            Err(e) => return fail(&mut child, format!("cannot watch Kadr (pid {pid}): {e}")),
        }
        if let Some(d) = discovery::list(data).into_iter().find(|d| d.pid == pid && ready(d)) {
            return Ok(Instance { d, child: Some(child), job });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    fail(&mut child, format!("Kadr (pid {pid}) did not become ready within {} s", LAUNCH_TIMEOUT.as_secs()))
}
