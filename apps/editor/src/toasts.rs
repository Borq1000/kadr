//! Floating notifications (bottom-right). Errors stay longer and are logged.

use crate::app::App;
use crate::ToastView;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info = 0,
    Success = 1,
    Warning = 2,
    Error = 3,
}

pub struct Toast {
    pub id: i32,
    pub kind: Kind,
    pub text: String,
    pub until: Instant,
}

/// How many past notifications `since` can still report. Independent of
/// `MAX_VISIBLE` (the on-screen toast list), which dedupes and evicts —
/// MCP callers need every notification that actually happened, not just
/// what's currently shown.
const LOG_CAP: usize = 32;

#[derive(Default)]
pub struct Toasts {
    pub items: Vec<Toast>,
    next: i32,
    seq: u64,
    log: VecDeque<(u64, String)>,
}

impl Toasts {
    /// Current toast texts, oldest first — used by MCP's `get_state`.
    pub fn texts(&self) -> Vec<String> {
        self.items.iter().map(|t| t.text.clone()).collect()
    }

    /// The sequence number of the most recent notification (0 if none yet).
    /// MCP tools snapshot this before an action and pass it to `since`.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Every notification recorded after `seq`, oldest first, up to the last
    /// `LOG_CAP` notifications total. Unlike `texts()`, this reports
    /// identical repeated messages as separate events and isn't affected by
    /// `MAX_VISIBLE` eviction from the visible toast list — so a second
    /// identical failure (e.g. "export failed" twice in a row) still shows
    /// up here even though it dedupes on screen.
    pub fn since(&self, seq: u64) -> Vec<String> {
        self.log.iter().filter(|(s, _)| *s > seq).map(|(_, t)| t.clone()).collect()
    }

    /// Records that a notification with `text` happened: bumps the sequence
    /// counter and appends to the log ring unconditionally, even for
    /// notifications that will dedupe against an already-visible toast.
    /// Pure bookkeeping, independent of the visible-toast list — unit
    /// tested directly below without any `App`/UI setup.
    fn record(&mut self, text: &str) -> u64 {
        self.seq += 1;
        self.log.push_back((self.seq, text.to_string()));
        if self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
        self.seq
    }
}

const MAX_VISIBLE: usize = 4;

impl App {
    pub fn notify(&mut self, kind: Kind, text: impl Into<String>) {
        let text = text.into();
        if kind == Kind::Error {
            tracing::warn!(%text, "user-visible error");
        }
        self.toasts.record(&text);
        // Don't stack identical messages (e.g. repeated key presses).
        if let Some(t) = self.toasts.items.iter_mut().find(|t| t.text == text) {
            t.until = Instant::now() + life(kind);
            return;
        }
        self.toasts.next += 1;
        self.toasts.items.push(Toast { id: self.toasts.next, kind, text, until: Instant::now() + life(kind) });
        if self.toasts.items.len() > MAX_VISIBLE {
            self.toasts.items.remove(0);
        }
        self.refresh_toasts();
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.notify(Kind::Info, msg);
    }
    pub fn toast_ok(&mut self, msg: impl Into<String>) {
        self.notify(Kind::Success, msg);
    }
    pub fn toast_warn(&mut self, msg: impl Into<String>) {
        self.notify(Kind::Warning, msg);
    }
    pub fn toast_error(&mut self, msg: impl Into<String>) {
        self.notify(Kind::Error, msg);
    }

    pub fn dismiss_toast(&mut self, id: i32) {
        self.toasts.items.retain(|t| t.id != id);
        self.refresh_toasts();
    }

    pub fn expire_toasts(&mut self) {
        let n = self.toasts.items.len();
        let now = Instant::now();
        self.toasts.items.retain(|t| t.until > now);
        if self.toasts.items.len() != n {
            self.refresh_toasts();
        }
    }

    fn refresh_toasts(&mut self) {
        let v: Vec<ToastView> =
            self.toasts.items.iter().map(|t| ToastView { id: t.id, kind: t.kind as i32, text: t.text.clone().into() }).collect();
        let ui = self.ui();
        crate::util::sync_rows_by_key(ui.get_toasts(), v, |t| t.id, |m| ui.set_toasts(m));
    }
}

fn life(k: Kind) -> Duration {
    match k {
        Kind::Error => Duration::from_secs(10),
        Kind::Warning => Duration::from_secs(7),
        _ => Duration::from_secs(4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_reports_every_notification_even_repeated_identical_text() {
        let mut t = Toasts::default();
        let before = t.seq();
        t.record("export failed");
        t.record("export failed");
        assert_eq!(t.since(before), vec!["export failed".to_string(), "export failed".to_string()]);
    }

    #[test]
    fn since_only_reports_what_happened_after_the_snapshot() {
        let mut t = Toasts::default();
        t.record("earlier");
        let before = t.seq();
        t.record("later");
        assert_eq!(t.since(before), vec!["later".to_string()]);
    }

    #[test]
    fn since_is_capped_at_the_log_ring_size_but_keeps_the_most_recent() {
        let mut t = Toasts::default();
        let before = t.seq();
        for i in 0..(LOG_CAP + 8) {
            t.record(&format!("m{i}"));
        }
        let got = t.since(before);
        assert_eq!(got.len(), LOG_CAP);
        assert_eq!(got.first().unwrap(), "m8");
        assert_eq!(got.last().unwrap(), &format!("m{}", LOG_CAP + 7));
    }
}
