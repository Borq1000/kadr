//! The colour picker's Rust side (titles spec §6.4): hex ↔ colour for the
//! `ColorUtil` global, and the recent colours (newest first, at most 8,
//! kept in the settings).

use crate::app::App;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

/// "#rrggbb" or "#rrggbbaa" (the # optional, surrounding spaces ignored).
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let t = s.trim();
    let h = t.strip_prefix('#').unwrap_or(t);
    if !h.is_ascii() || !(h.len() == 6 || h.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 255 }])
}

/// Lower-case hex; the alpha digits only when `alpha` and not opaque.
pub fn to_hex(c: [u8; 4], alpha: bool) -> String {
    if alpha && c[3] != 255 { format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]) } else { format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]) }
}

pub fn to_slint(c: [u8; 4]) -> slint::Color {
    slint::Color::from_argb_u8(c[3], c[0], c[1], c[2])
}

pub fn from_slint(c: slint::Color) -> [u8; 4] {
    [c.red(), c.green(), c.blue(), c.alpha()]
}

/// Puts `hex` first (once, case-insensitively), keeping at most 8.
pub fn push_recent(recent: &mut Vec<String>, hex: &str) {
    recent.retain(|h| !h.eq_ignore_ascii_case(hex));
    recent.insert(0, hex.to_ascii_lowercase());
    recent.truncate(8);
}

/// The `ColorUtil` callbacks (pure: no app state).
pub fn wire(ui: &crate::AppWindow) {
    let cu = ui.global::<crate::ColorUtil>();
    cu.on_parse(|s| parse_hex(&s).map(to_slint).unwrap_or(slint::Color::from_argb_u8(0, 0, 0, 0)));
    cu.on_valid(|s| parse_hex(&s).is_some());
    cu.on_hex(|c, alpha| to_hex(from_slint(c), alpha).into());
}

impl App {
    /// Shows the settings' recent colours in every picker.
    pub fn show_recent_colours(&self) {
        let colours: Vec<slint::Color> = self.settings.recent_colors.iter().filter_map(|h| parse_hex(h)).map(to_slint).collect();
        let ui = self.ui();
        let cu = ui.global::<crate::ColorUtil>();
        let old = cu.get_recent();
        if old.row_count() != colours.len() || old.iter().zip(&colours).any(|(a, b)| a != *b) {
            cu.set_recent(ModelRc::new(VecModel::from(colours)));
        }
    }

    /// A colour was committed in a picker: it becomes the newest recent one.
    pub fn colour_committed(&mut self, c: slint::Color) {
        push_recent(&mut self.settings.recent_colors, &to_hex(from_slint(c), true));
        self.settings.save(&self.dirs.settings_file());
        self.show_recent_colours();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_reads_six_or_eight_digits_and_writes_lower_case() {
        assert_eq!(parse_hex("#FF8000"), Some([255, 128, 0, 255]));
        assert_eq!(parse_hex(" #ff800080"), Some([255, 128, 0, 128]));
        assert_eq!(parse_hex("ff8000"), Some([255, 128, 0, 255]), "the # is optional when typing");
        for bad in ["#ff80", "#zz0000", "#ff800080ff", "", "#ффф000"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        assert_eq!(to_hex([255, 128, 0, 255], false), "#ff8000");
        assert_eq!(to_hex([255, 128, 0, 64], true), "#ff800040");
        assert_eq!(to_hex([255, 128, 0, 255], true), "#ff8000", "an opaque colour needs no alpha digits");
    }

    #[test]
    fn colours_round_trip_through_slint() {
        for c in [[0, 0, 0, 0], [255, 255, 255, 255], [12, 200, 7, 99]] {
            assert_eq!(from_slint(to_slint(c)), c);
        }
    }

    #[test]
    fn recent_colours_are_newest_first_unique_and_at_most_eight() {
        let mut r = vec![];
        for h in ["#111111", "#222222", "#111111"] {
            push_recent(&mut r, h);
        }
        assert_eq!(r, ["#111111", "#222222"], "a repeat moves to the front");
        for i in 0..10 {
            push_recent(&mut r, &format!("#0000{i:02}"));
        }
        assert_eq!(r.len(), 8);
        assert_eq!(r[0], "#000009");
        push_recent(&mut r, "#00000A");
        assert_eq!(r.iter().filter(|h| h.eq_ignore_ascii_case("#00000a")).count(), 1, "case-insensitive");
    }
}
