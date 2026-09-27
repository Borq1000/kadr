//! Startup environment for MCP control: flags, ports, in-window menus.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flags {
    /// Started by `kadr-mcp`: off-screen window, no restore prompt.
    pub headless: bool,
}

/// The port Slint's embedded MCP server listens on (0 = disabled).
pub static UI_PORT: OnceLock<u16> = OnceLock::new();

pub fn parse_flags(args: &[OsString]) -> (Flags, Vec<OsString>) {
    let mut flags = Flags::default();
    let rest = args
        .iter()
        .filter(|a| {
            let hit = a.as_os_str() == "--headless";
            flags.headless |= hit;
            !hit
        })
        .cloned()
        .collect();
    (flags, rest)
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
    fn free_port_can_be_bound_on_loopback() {
        let p = free_port().unwrap();
        assert!(p > 0);
        std::net::TcpListener::bind(("127.0.0.1", p)).expect("port is free right after the probe");
    }
}
