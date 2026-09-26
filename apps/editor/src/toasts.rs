//! Floating notifications (bottom-right). Errors stay longer and are logged.

use crate::app::App;
use crate::ToastView;
use slint::{ModelRc, VecModel};
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

#[derive(Default)]
pub struct Toasts {
    pub items: Vec<Toast>,
    next: i32,
}

const MAX_VISIBLE: usize = 4;

impl App {
    pub fn notify(&mut self, kind: Kind, text: impl Into<String>) {
        let text = text.into();
        if kind == Kind::Error {
            tracing::warn!(%text, "user-visible error");
        }
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
        self.ui().set_toasts(ModelRc::new(VecModel::from(v)));
    }
}

fn life(k: Kind) -> Duration {
    match k {
        Kind::Error => Duration::from_secs(10),
        Kind::Warning => Duration::from_secs(7),
        _ => Duration::from_secs(4),
    }
}
