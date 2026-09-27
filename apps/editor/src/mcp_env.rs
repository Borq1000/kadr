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

pub fn parse_flags(args: &[OsString]) -> (Flags, Vec<OsString>) {
    parse_flags_with_env(args, std::env::var_os(kadr_mcp_bridge::PARENT_PID_ENV))
}

/// `kadr-mcp` passes its pid in `KADR_PARENT_PID`; `--parent-pid <pid>`
/// (what the first MCP release sent) is still understood and wins.
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let (f, _) = parse_flags_with_env(&["--parent-pid".into(), "7".into()], Some("4242".into()));
        assert_eq!(f.parent_pid, Some(7), "the explicit argument wins");
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
