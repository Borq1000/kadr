//! Keyboard shortcuts. The dark top bar is Slint-drawn, so Ctrl shortcuts
//! are handled here too (Ctrl+O/Ctrl+Shift+S/Ctrl+I open native dialogs and
//! are intercepted in app.rs before the app is borrowed).

use crate::app::App;
use crate::ShortcutRow;
use kadr_core::{Time, TimeRange};
use kadr_i18n::t;
use kadr_timeline::InsertMode;
use slint::platform::Key;

fn is(text: &str, k: Key) -> bool {
    let s: slint::SharedString = k.into();
    text == s.as_str()
}

/// Shortcut reference shown in Settings → Keyboard (and on F1).
pub fn shortcut_rows() -> Vec<ShortcutRow> {
    let g = |k: &str| ShortcutRow { keys: "".into(), action: t(k).into(), group: true };
    let r = |keys: &str, k: &str| ShortcutRow { keys: keys.into(), action: t(k).into(), group: false };
    vec![
        g("keys.g.playback"),
        r("Space", "keys.play"),
        r("J / K / L", "keys.jkl"),
        r("← / →", "keys.frame"),
        r("Shift + ← / →", "keys.second"),
        r("↑ / ↓", "keys.edit_points"),
        r("Home / End", "keys.home_end"),
        g("keys.g.editing"),
        r("S · Ctrl+B", "keys.split"),
        r("Del", "keys.delete"),
        r("Shift+Del", "keys.ripple_delete"),
        r("B", "keys.blade"),
        r(",", "keys.insert"),
        r(".", "keys.overwrite"),
        r("M", "keys.marker"),
        r("1 … 9", "keys.angle"),
        r("I / O / X", "keys.in_out"),
        r("Ctrl+Z", "keys.undo"),
        r("Ctrl+Shift+Z · Ctrl+Y", "keys.redo"),
        r("Ctrl+A · Esc", "keys.select"),
        g("keys.g.timeline"),
        r("N", "keys.snapping"),
        r("R", "keys.ripple"),
        r("+ / −", "keys.zoom"),
        r("Shift+Z", "keys.zoom_fit"),
        r("Ctrl + wheel", "keys.wheel_zoom"),
        r("Shift + wheel", "keys.wheel_tracks"),
        g("keys.g.view"),
        r("\\", "keys.bypass"),
        r("'", "keys.safe"),
        r("F", "keys.fullscreen"),
        r("Ctrl+Shift+A", "keys.ai_panel"),
        r("F1", "keys.help"),
        g("keys.g.project"),
        r("Ctrl+N", "keys.new"),
        r("Ctrl+O", "keys.open"),
        r("Ctrl+S", "keys.save"),
        r("Ctrl+Shift+S", "keys.save_as"),
        r("Ctrl+I", "keys.import"),
        r("Ctrl+E", "keys.export"),
        r("Ctrl+,", "keys.settings"),
    ]
}

impl App {
    pub fn on_key(&mut self, text: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        if alt {
            return false;
        }
        if ctrl {
            let k = text.to_lowercase();
            match (k.as_str(), shift) {
                ("z", false) => self.undo(),
                ("z", true) | ("y", _) => self.redo(),
                ("n", false) => self.menu("new"),
                ("s", false) => self.menu("save"),
                ("e", false) => self.menu("export"),
                ("b", false) => self.split_at_playhead(),
                ("a", false) => self.select_all(),
                ("a", true) => self.menu("toggle-ai"),
                (",", _) => self.menu("settings"),
                _ => return false,
            }
            return true;
        }
        if is(text, Key::F1) {
            self.menu("shortcuts");
        } else if is(text, Key::LeftArrow) {
            if shift { self.step_seconds(-1.0) } else { self.step_frames(-1) }
        } else if is(text, Key::RightArrow) {
            if shift { self.step_seconds(1.0) } else { self.step_frames(1) }
        } else if is(text, Key::UpArrow) {
            self.stop_playback();
            self.jump_edit(false);
        } else if is(text, Key::DownArrow) {
            self.stop_playback();
            self.jump_edit(true);
        } else if is(text, Key::Home) {
            self.transport("start");
        } else if is(text, Key::End) {
            self.transport("end");
        } else if is(text, Key::Delete) || is(text, Key::Backspace) {
            self.delete_selection(shift || self.tl.ripple);
        } else if is(text, Key::Escape) {
            // Esc closes the topmost overlay first, then clears the selection.
            let ui = self.ui();
            if ui.get_confirm_open() {
                self.confirm_answer(false);
                return true;
            }
            if ui.get_prompt_open() {
                self.prompt_dismiss();
                return true;
            }
            if ui.get_mc_open() {
                self.mc_dismiss();
                return true;
            }
            if ui.get_settings_open() {
                ui.set_settings_open(false);
                return true;
            }
            if ui.get_export_open() && self.export.job.is_none() {
                ui.set_export_open(false);
                return true;
            }
            if ui.get_welcome_open() {
                ui.set_welcome_open(false);
                return true;
            }
            if self.fullscreen {
                self.transport("fullscreen");
            }
            if self.ui().get_jobs_open() {
                self.ui().set_jobs_open(false);
            }
            self.tl.blade = false;
            self.tl.selection.clear();
            self.refresh_timeline();
            self.refresh_inspector();
            self.refresh_status();
        } else if let Some(n) = text.chars().next().filter(|c| text.len() == 1 && ('1'..='9').contains(c)) {
            // 1-9: cut to that angle when a multicam clip is under the playhead.
            return self.cut_to_angle(n as u32 - '1' as u32);
        } else {
            match text.to_lowercase().as_str() {
                " " => self.toggle_playback(),
                "k" => self.stop_playback(),
                "l" => {
                    if !self.playing {
                        self.start_playback();
                    }
                }
                "j" => self.step_seconds(-2.0),
                "s" => self.split_at_playhead(),
                "b" => self.tl_tool("blade"),
                "n" => self.tl_tool("snap"),
                "r" => self.tl_tool("ripple"),
                "m" => self.add_marker(),
                "i" => self.set_in_out(true),
                "o" => self.set_in_out(false),
                "x" => self.clear_in_out(),
                "," => self.insert_selected_asset(InsertMode::Insert),
                "." => self.insert_selected_asset(InsertMode::Overwrite),
                "=" | "+" => self.tl_tool("zoom-in"),
                "-" => self.tl_tool("zoom-out"),
                "z" if shift => self.zoom_fit(),
                "\\" => self.transport("bypass"),
                "'" => self.transport("safe"),
                "f" => self.transport("fullscreen"),
                _ => return false,
            }
        }
        true
    }

    pub fn set_in_out(&mut self, is_in: bool) {
        let ph = self.playhead;
        let seq = self.project.sequence_mut();
        let cur = seq.in_out.unwrap_or(TimeRange::new(Time::ZERO, seq.duration().max(ph)));
        let (a, b) = if is_in { (ph, cur.end.max(ph)) } else { (cur.start.min(ph), ph) };
        seq.in_out = (a < b).then(|| TimeRange::new(a, b));
        self.meta_dirty = true;
        self.refresh_timeline();
        self.refresh_status();
        self.toast(if is_in { t("toast.in_set") } else { t("toast.out_set") });
    }

    pub fn clear_in_out(&mut self) {
        if self.project.sequence_mut().in_out.take().is_some() {
            self.meta_dirty = true;
            self.refresh_timeline();
        }
    }

    pub fn step_seconds(&mut self, s: f64) {
        self.stop_playback();
        let t = Time::from_secs_f64((self.playhead.as_secs_f64() + s).max(0.0));
        self.set_playhead(t);
    }
}
