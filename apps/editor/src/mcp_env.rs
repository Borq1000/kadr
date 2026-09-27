//! Startup environment for MCP control: flags, ports, in-window menus.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flags {
    /// Started by `kadr-mcp`: off-screen window, no restore prompt.
    pub headless: bool,
    /// `--parent-pid <pid>`: the `kadr-mcp` that launched us.
    pub parent_pid: Option<u32>,
}

/// A headless Kadr exists only to be driven over MCP: with MCP disabled it
/// would sit hidden forever, so it exits at once with a code `kadr-mcp`
/// turns into "MCP control is disabled in Kadr settings".
pub fn headless_exit_code(flags: Flags, allow_mcp: bool) -> Option<i32> {
    (flags.headless && !allow_mcp).then_some(kadr_mcp_bridge::EXIT_MCP_DISABLED)
}

/// The port Slint's embedded MCP server listens on (0 = disabled).
pub static UI_PORT: OnceLock<u16> = OnceLock::new();

/// Reads the command line and takes `KADR_PARENT_PID` out of the
/// environment, so programs Kadr starts (Explorer, a player, another Kadr)
/// don't inherit it. Call before any other thread exists.
pub fn parse_flags(args: &[OsString]) -> (Flags, Vec<OsString>) {
    let env_parent = std::env::var_os(kadr_mcp_bridge::PARENT_PID_ENV);
    // SAFETY: called first thing in `main`, before any thread starts.
    unsafe { std::env::remove_var(kadr_mcp_bridge::PARENT_PID_ENV) };
    parse_flags_with_env(args, env_parent)
}

/// `kadr-mcp` passes its pid in `KADR_PARENT_PID`; `--parent-pid <pid>`
/// (what the first MCP release sent) is still understood and wins. Only a
/// headless instance follows its parent: a user's window never quits
/// because some other process exited.
fn parse_flags_with_env(args: &[OsString], env_parent: Option<OsString>) -> (Flags, Vec<OsString>) {
    let mut flags = Flags { parent_pid: env_parent.and_then(|v| v.to_str()?.parse().ok()), ..Flags::default() };
    let mut rest = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--headless" {
            flags.headless = true;
        } else if a == "--parent-pid" {
            flags.parent_pid = it.next().and_then(|v| v.to_str()?.parse().ok());
        } else {
            rest.push(a.clone());
        }
    }
    if !flags.headless {
        flags.parent_pid = None;
    }
    (flags, rest)
}

/// The port to export as `SLINT_MCP_PORT`, or `None` to remove the variable
/// (MCP off, or no free port found): an inherited value must never start
/// Slint's UI server on a port we didn't choose.
pub fn slint_ui_port(allow_mcp: bool, free: std::io::Result<u16>) -> Option<u16> {
    if allow_mcp { free.ok() } else { None }
}

/// An OS-chosen free loopback port (bind to :0, read it, release it).
pub fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Untitled autosaves: a headless instance keeps its own, so it never
/// offers (or deletes) the user's.
pub fn recovery_dir(data: &Path, headless: bool) -> PathBuf {
    data.join(if headless { "recovery-headless" } else { "recovery" })
}

/// Headless autosaves are never offered for restore (nobody sees a prompt),
/// so they are only a safety net kept this long, then pruned.
pub const HEADLESS_AUTOSAVE_KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// Deletes files in `dir` last modified more than `max_age` before `now`;
/// returns how many. Subdirectories and unreadable entries are left alone.
pub fn prune_old_files(dir: &Path, max_age: std::time::Duration, now: std::time::SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| {
            let Ok(m) = e.metadata() else { return false };
            let old = m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > max_age);
            m.is_file() && old && std::fs::remove_file(e.path()).is_ok()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_headless_autosaves_are_pruned_and_recent_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        let file = |name: &str, age_days: u64| {
            let p = dir.path().join(name);
            let f = std::fs::File::create(&p).unwrap();
            f.set_modified(now - std::time::Duration::from_secs(age_days * 86_400)).unwrap();
            p
        };
        let old = file("a.kadr.autosave", 8);
        let fresh = file("b.kadr.autosave", 1);
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(prune_old_files(dir.path(), HEADLESS_AUTOSAVE_KEEP, now), 1);
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(dir.path().join("sub").exists(), "only files are pruned");
        assert_eq!(prune_old_files(&dir.path().join("missing"), HEADLESS_AUTOSAVE_KEEP, now), 0);
    }

    #[test]
    fn headless_recovery_dir_is_separate() {
        let d = std::path::Path::new("C:/data");
        assert_eq!(recovery_dir(d, false), d.join("recovery"));
        assert_eq!(recovery_dir(d, true), d.join("recovery-headless"));
    }

    #[test]
    fn headless_flag_is_consumed_and_other_args_kept() {
        let args: Vec<OsString> = ["a.mp4", "--headless", "b.kadr"].iter().map(OsString::from).collect();
        let (f, rest) = parse_flags(&args);
        assert!(f.headless);
        assert_eq!(rest, vec![OsString::from("a.mp4"), OsString::from("b.kadr")]);
        let (f, rest) = parse_flags(&[OsString::from("x.mp4")]);
        assert!(!f.headless);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn parent_pid_is_parsed_and_consumed() {
        let args: Vec<OsString> = ["--headless", "--parent-pid", "4242", "a.mp4"].iter().map(OsString::from).collect();
        let (f, rest) = parse_flags(&args);
        assert!(f.headless);
        assert_eq!(f.parent_pid, Some(4242));
        assert_eq!(rest, vec![OsString::from("a.mp4")]);
        // A missing or bad value is consumed and ignored, never taken as a file.
        let (f, rest) = parse_flags(&["--parent-pid".into(), "x".into()]);
        assert_eq!(f.parent_pid, None);
        assert!(rest.is_empty());
        let (f, rest) = parse_flags(&["--parent-pid".into()]);
        assert_eq!(f.parent_pid, None);
        assert!(rest.is_empty());
    }

    #[test]
    fn headless_without_mcp_exits_with_the_disabled_code() {
        let headless = Flags { headless: true, ..Flags::default() };
        assert_eq!(headless_exit_code(headless, false), Some(kadr_mcp_bridge::EXIT_MCP_DISABLED));
        assert_eq!(headless_exit_code(headless, true), None);
        assert_eq!(headless_exit_code(Flags::default(), false), None, "a normal window starts regardless");
    }

    #[test]
    fn parent_pid_comes_from_the_environment_too() {
        let (f, rest) = parse_flags_with_env(&["--headless".into()], Some("4242".into()));
        assert_eq!(f.parent_pid, Some(4242));
        assert!(rest.is_empty());
        let (f, _) = parse_flags_with_env(&[], Some("junk".into()));
        assert_eq!(f.parent_pid, None, "a bad value is ignored");
        let (f, _) = parse_flags_with_env(&["--headless".into(), "--parent-pid".into(), "7".into()], Some("4242".into()));
        assert_eq!(f.parent_pid, Some(7), "the explicit argument wins");
    }

    #[test]
    fn a_users_window_ignores_a_parent_pid() {
        // Inherited by a window started from a headless Kadr's process tree,
        // it would make that window quit when kadr-mcp goes away.
        let (f, _) = parse_flags_with_env(&[], Some("4242".into()));
        assert_eq!(f.parent_pid, None);
        let (f, rest) = parse_flags_with_env(&["--parent-pid".into(), "7".into()], None);
        assert_eq!(f.parent_pid, None);
        assert!(rest.is_empty(), "still consumed, never a media path");
    }

    #[test]
    fn slint_ui_port_only_when_mcp_is_allowed_and_a_port_was_found() {
        let none = || Err(std::io::Error::other("no port"));
        assert_eq!(slint_ui_port(true, Ok(5000)), Some(5000));
        assert_eq!(slint_ui_port(true, none()), None, "no port: the inherited variable must be dropped too");
        assert_eq!(slint_ui_port(false, Ok(5000)), None);
    }

    #[test]
    fn free_port_can_be_bound_on_loopback() {
        let p = free_port().unwrap();
        assert!(p > 0);
        std::net::TcpListener::bind(("127.0.0.1", p)).expect("port is free right after the probe");
    }
}
