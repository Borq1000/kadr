//! The inspector's «ТЕКСТ» section (titles spec §6.3): the text field (a
//! typing session — live, one undo entry when it ends), and the style
//! controls (slider drags live through `LiveEdit`, one entry on release;
//! switches and choices one entry each). Run properties (font, size,
//! weight, italic, colour, letter spacing) go to the selection when there
//! is one (`App.text_scope`, Task 11), else to the whole text; block
//! properties (outlines, plaque, paragraph) always to the whole text.

use crate::app::App;
use crate::color_ui::{from_slint, to_slint};
use crate::{FontRow, TextInspectorData};
use kadr_core::ClipId;
use kadr_i18n::{t, tf};
use kadr_project::{Align, Fill, Outline, Plaque, Project, Rgba8, RunStyle, TextDoc};
use kadr_timeline::{ClipProperty, EditCommand};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub enum TextValue {
    Num(f64),
    Flag(bool),
    Name(String),
    Colour(Rgba8),
}

impl TextValue {
    fn num(&self) -> Option<f64> {
        match self {
            TextValue::Num(v) if v.is_finite() => Some(*v),
            _ => None,
        }
    }
    fn flag(&self) -> Option<bool> {
        match self {
            TextValue::Flag(b) => Some(*b),
            _ => None,
        }
    }
    fn colour(&self) -> Option<Rgba8> {
        match self {
            TextValue::Colour(c) => Some(*c),
            _ => None,
        }
    }
}

/// Sets `*field` to `v`; `true` when it changed.
fn put<T: PartialEq>(field: &mut T, v: T) -> bool {
    if *field == v {
        false
    } else {
        *field = v;
        true
    }
}

/// One run property over the whole text: the base gets it, the runs' own
/// values of it go (so the whole text shows it).
fn whole<T: PartialEq>(doc: &mut TextDoc, base: impl Fn(&mut TextDoc) -> &mut T, v: T, clear: impl Fn(&mut RunStyle) -> bool) -> bool {
    let mut changed = put(base(doc), v);
    for r in &mut doc.runs {
        changed |= clear(&mut r.style);
    }
    if changed {
        doc.normalise();
    }
    changed
}

fn take<T>(o: &mut Option<T>) -> bool {
    o.take().is_some()
}

/// Applies one inspector edit; `false` when nothing changes (no undo entry).
/// Numbers are clamped to the property's range here, so a command built
/// from the result never carries a value the edit commands refuse.
pub fn apply_text_prop(doc: &mut TextDoc, scope: Option<Range<usize>>, prop: &str, v: &TextValue) -> bool {
    let scoped = scope.filter(|r| r.start < r.end);
    let run = |patch: RunStyle, doc: &mut TextDoc| scoped.as_ref().map(|r| doc.style_range(r.clone(), &patch));
    match prop {
        "family" => {
            let TextValue::Name(n) = v else { return false };
            if n.trim().is_empty() {
                return false;
            }
            run(RunStyle { family: Some(n.clone()), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.font.family, n.clone(), |s| take(&mut s.family)))
        }
        "size" => {
            let Some(x) = v.num() else { return false };
            let x = x.clamp(1.0, 2000.0).round() as f32;
            run(RunStyle { size: Some(x), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.size, x, |s| take(&mut s.size)))
        }
        "weight" => {
            let Some(x) = v.num() else { return false };
            let w = ((x / 100.0).round() * 100.0).clamp(100.0, 900.0) as u16;
            run(RunStyle { weight: Some(w), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.font.weight, w, |s| take(&mut s.weight)))
        }
        "italic" => {
            let Some(b) = v.flag() else { return false };
            run(RunStyle { italic: Some(b), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.font.italic, b, |s| take(&mut s.italic)))
        }
        "fill" => {
            let Some(c) = v.colour() else { return false };
            let fill = Fill::solid(c);
            run(RunStyle { fill: Some(fill.clone()), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.fill, fill, |s| take(&mut s.fill)))
        }
        "letter-spacing" => {
            let Some(x) = v.num() else { return false };
            let x = x.clamp(-200.0, 2000.0) as f32;
            run(RunStyle { letter_spacing: Some(x), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.paragraph.letter_spacing, x, |s| take(&mut s.letter_spacing)))
        }
        "outline1" | "outline2" => {
            let Some(on) = v.flag() else { return false };
            let n = if prop == "outline1" { 1 } else { 2 };
            let o = &mut doc.style.outlines;
            match (on, o.len() >= n) {
                (true, true) | (false, false) => false,
                (true, false) => {
                    while o.len() < n {
                        o.push(Outline { width: 4.0, colour: if o.is_empty() { Rgba8::BLACK } else { Rgba8::WHITE } });
                    }
                    true
                }
                (false, true) => {
                    o.truncate(n - 1);
                    true
                }
            }
        }
        "outline1-width" | "outline2-width" | "outline1-color" | "outline2-color" => {
            let i = if prop.starts_with("outline1") { 0 } else { 1 };
            let Some(o) = doc.style.outlines.get_mut(i) else { return false };
            match v {
                TextValue::Num(x) if x.is_finite() => put(&mut o.width, x.clamp(0.0, 200.0) as f32),
                TextValue::Colour(c) => put(&mut o.colour, *c),
                _ => false,
            }
        }
        "plaque" => {
            let Some(on) = v.flag() else { return false };
            let size = doc.style.size;
            match (on, doc.style.plaque.is_some()) {
                (true, false) => {
                    doc.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::rgba(0, 0, 0, 180), radius: (size * 0.12).round(), padding_x: (size * 0.3).round(), padding_y: (size * 0.12).round() });
                    true
                }
                (false, true) => take(&mut doc.style.plaque),
                _ => false,
            }
        }
        "plaque-per-line" | "plaque-color" | "plaque-radius" | "plaque-pad-x" | "plaque-pad-y" => {
            let Some(p) = doc.style.plaque.as_mut() else { return false };
            match (prop, v) {
                ("plaque-per-line", TextValue::Flag(b)) => put(&mut p.per_line, *b),
                ("plaque-color", TextValue::Colour(c)) => put(&mut p.colour, *c),
                ("plaque-radius", TextValue::Num(x)) if x.is_finite() => put(&mut p.radius, x.clamp(0.0, 500.0) as f32),
                ("plaque-pad-x", TextValue::Num(x)) if x.is_finite() => put(&mut p.padding_x, x.clamp(0.0, 500.0) as f32),
                ("plaque-pad-y", TextValue::Num(x)) if x.is_finite() => put(&mut p.padding_y, x.clamp(0.0, 500.0) as f32),
                _ => false,
            }
        }
        "align" => {
            let Some(x) = v.num() else { return false };
            let a = match x.round() as i64 {
                0 => Align::Left,
                2 => Align::Right,
                _ => Align::Center,
            };
            put(&mut doc.paragraph.align, a)
        }
        "line-height" => v.num().is_some_and(|x| put(&mut doc.paragraph.line_height, x.clamp(0.5, 4.0) as f32)),
        "caps" => v.flag().is_some_and(|b| put(&mut doc.paragraph.caps, b)),
        "wrap" => {
            let Some(on) = v.flag() else { return false };
            match (on, doc.paragraph.wrap.is_some()) {
                (true, false) => {
                    doc.paragraph.wrap = Some(800.0);
                    true
                }
                (false, true) => take(&mut doc.paragraph.wrap),
                _ => false,
            }
        }
        "wrap-width" => match (v.num(), doc.paragraph.wrap.as_mut()) {
            (Some(x), Some(w)) => put(w, x.clamp(20.0, 20000.0).round() as f32),
            _ => false,
        },
        _ => false,
    }
}

/// The i18n key naming `prop` in history labels («Текст: размер»).
pub fn prop_label_key(prop: &str) -> &'static str {
    match prop {
        "family" => "text.font",
        "size" => "text.size",
        "weight" => "text.weight",
        "italic" => "text.italic",
        "fill" => "text.fill",
        "letter-spacing" => "text.letter_spacing",
        p if p.starts_with("outline") => "text.outline",
        p if p.starts_with("plaque") => "text.plaque",
        "align" => "text.align",
        "line-height" => "text.line_height",
        "caps" => "text.caps",
        "wrap" | "wrap-width" => "text.wrap",
        _ => "inspector.text",
    }
}

/// The catalog key («text.font_err.<kind>», with `{file}`) of why a font
/// could not be added.
pub fn font_error_key(e: &kadr_text::FontError) -> String {
    format!("text.font_err.{}", e.kind())
}

/// What the section shows: the style of the scope's first character
/// (over the base), or the base; `locked` makes the field read-only.
pub fn text_data(doc: &TextDoc, scope: Option<&Range<usize>>, locked: bool) -> TextInspectorData {
    let base = &doc.style;
    let rs = scope.map(|r| doc.run_style_at(r.start).clone()).unwrap_or_default();
    let family = rs.family.clone().unwrap_or_else(|| base.font.family.clone());
    let fill = rs.fill.as_ref().unwrap_or(&base.fill).colour;
    let o = |i: usize| base.outlines.get(i);
    let p = base.plaque;
    TextInspectorData {
        has: true,
        locked,
        family_missing: !kadr_text::fonts().has(&family),
        family: family.into(),
        size: rs.size.unwrap_or(base.size),
        weight: rs.weight.unwrap_or(base.font.weight) as f32,
        italic: rs.italic.unwrap_or(base.font.italic),
        fill: to_slint(fill.0),
        letter_spacing: rs.letter_spacing.unwrap_or(doc.paragraph.letter_spacing),
        outline1: o(0).is_some(),
        outline1_width: o(0).map_or(4.0, |x| x.width),
        outline1_color: to_slint(o(0).map_or(Rgba8::BLACK, |x| x.colour).0),
        outline2: o(1).is_some(),
        outline2_width: o(1).map_or(4.0, |x| x.width),
        outline2_color: to_slint(o(1).map_or(Rgba8::WHITE, |x| x.colour).0),
        plaque: p.is_some(),
        plaque_per_line: p.is_some_and(|p| p.per_line),
        plaque_color: to_slint(p.map_or(Rgba8::rgba(0, 0, 0, 180), |p| p.colour).0),
        plaque_radius: p.map_or(0.0, |p| p.radius),
        plaque_pad_x: p.map_or(0.0, |p| p.padding_x),
        plaque_pad_y: p.map_or(0.0, |p| p.padding_y),
        align: match doc.paragraph.align {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        },
        line_height: doc.paragraph.line_height,
        caps: doc.paragraph.caps,
        wrap: doc.paragraph.wrap.is_some(),
        wrap_width: doc.paragraph.wrap.unwrap_or(800.0),
        ..Default::default()
    }
}

/// The font picker's rows: «Встроенные», «Мои», «Системные», each with its
/// header, filtered by `filter` (case-insensitive); `current` marked.
pub fn font_rows(filter: &str, current: &str) -> Vec<FontRow> {
    let f = filter.trim().to_lowercase();
    let families = kadr_text::fonts().families();
    let mut out = vec![];
    for (source, key) in [(kadr_text::FontSource::Bundled, "text.fonts.bundled"), (kadr_text::FontSource::User, "text.fonts.mine"), (kadr_text::FontSource::System, "text.fonts.system")] {
        let names: Vec<&str> = families.iter().filter(|x| x.source == source && x.name.to_lowercase().contains(&f)).map(|x| x.name.as_str()).collect();
        if names.is_empty() {
            continue;
        }
        out.push(FontRow { name: t(key).into(), header: true, ..Default::default() });
        out.extend(names.into_iter().map(|n| FontRow { current: n.eq_ignore_ascii_case(current), name: n.into(), ..Default::default() }));
    }
    out
}

/// Typing into a text clip (titles spec §6.3): changes go into the project
/// live; `finish` puts the recorded state back and returns the one command
/// that records the whole session (`None`: nothing changed). An auto-named
/// title's name follows its text live, by `ClipProperty::Text`'s rule (the
/// name changes while it equals the old label; TX-R9), so the timeline
/// shows what is typed; a name the user gave stays.
#[derive(Default)]
pub struct TextSession(Option<Recorded>);

/// The clip a session types into and its recorded document and name.
struct Recorded {
    clip: ClipId,
    doc: TextDoc,
    name: String,
}

impl TextSession {
    pub fn clip(&self) -> Option<ClipId> {
        self.0.as_ref().map(|r| r.clip)
    }

    /// `text` into clip `id`, live; the session starts (remembering the
    /// document and the name) at its first change. `true` when the text changed.
    pub fn type_text(&mut self, project: &mut Project, id: ClipId, text: &str) -> bool {
        let Some((ti, ci)) = project.sequence().locate_clip(id) else { return false };
        let c = &mut project.sequence_mut().tracks[ti].clips[ci];
        let Some(doc) = c.text.as_deref_mut() else { return false };
        if doc.plain() == text {
            return false;
        }
        if self.clip() != Some(id) {
            self.0 = Some(Recorded { clip: id, doc: doc.clone(), name: c.name.clone() });
        }
        let old_label = doc.label();
        let changed = doc.set_plain(text);
        if c.name == old_label {
            c.name = doc.label();
        }
        changed
    }

    pub fn finish(&mut self, project: &mut Project) -> Option<EditCommand> {
        let Recorded { clip: id, doc: before, name } = self.0.take()?;
        let (ti, ci) = project.sequence().locate_clip(id)?;
        let c = &mut project.sequence_mut().tracks[ti].clips[ci];
        let now = c.text.as_deref()?.clone();
        c.text = Some(Box::new(before.clone()));
        c.name = name;
        (now != before).then(|| EditCommand::Batch { label: t("cmd.text_edit"), commands: vec![EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Text(Box::new(now)) }] })
    }

    /// Drops the session without touching the project (another one was loaded).
    pub fn forget(&mut self) {
        self.0 = None;
    }
}

impl App {
    /// Ends the typing session: records it as one entry (titles spec §6.3).
    /// Called before every other command, undo, redo, save and selection change.
    pub fn commit_text_session(&mut self) {
        if let Some(cmd) = self.text_session.finish(&mut self.project) {
            self.execute(cmd);
        }
    }

    pub fn text_typed(&mut self, text: &str) {
        let Some(id) = self.inspected() else { return };
        let locked = self.project.sequence().locate_clip(id).is_some_and(|(ti, _)| self.project.sequence().tracks[ti].locked);
        if locked {
            return;
        }
        if self.text_session.clip().is_some_and(|c| c != id) {
            self.commit_text_session();
        }
        if self.text_session.type_text(&mut self.project, id, text) {
            self.show_typed_name(id);
            self.preview.invalidate();
            self.refresh_timeline();
            self.request_frame();
        }
    }

    /// The inspector's header follows an auto-named title's name while
    /// typing (the rest of the inspector waits for the session's end).
    fn show_typed_name(&mut self, id: ClipId) {
        let Some(name) = self.project.sequence().clip(id).map(|c| c.name.clone()) else { return };
        let ui = self.ui();
        let mut d = ui.get_inspector();
        if d.name.as_str() != name {
            d.name = name.into();
            ui.set_inspector(d);
        }
    }

    pub fn text_field_focus(&mut self, focused: bool) {
        self.text_field_focused = focused;
        if !focused {
            self.commit_text_session();
        }
    }

    fn text_edit_live(&mut self, prop: &str, v: TextValue) {
        let Some(id) = self.inspected() else { return };
        let Some((ti, ci)) = self.project.sequence().locate_clip(id) else { return };
        if self.project.sequence().tracks[ti].locked {
            return;
        }
        // A slider's first live change ends a typing session first (one
        // entry each); the commit moves no clip, so (ti, ci) still hold.
        self.commit_text_session();
        self.live_edit.arm(self.project.sequence_mut(), id);
        let scope = self.text_scope.clone();
        let Some(doc) = self.project.sequence_mut().tracks[ti].clips[ci].text.as_deref_mut() else { return };
        if apply_text_prop(doc, scope, prop, &v) {
            self.preview.invalidate();
            self.refresh_text_inspector();
            self.request_frame();
        }
    }

    fn text_edit_commit(&mut self, prop: &str, v: TextValue) {
        let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
        self.commit_text_session();
        let Some(id) = self.inspected() else {
            if rolled_back {
                self.show_rolled_back();
            }
            return;
        };
        let Some(mut doc) = self.project.sequence().clip(id).and_then(|c| c.text.as_deref()).cloned() else { return };
        if !apply_text_prop(&mut doc, self.text_scope.clone(), prop, &v) {
            return self.show_rolled_back(); // nothing changed: no undo entry
        }
        let label = tf("cmd.text_named", &[("name", &t(prop_label_key(prop)))]);
        let set = EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Text(Box::new(doc)) };
        if !self.execute(EditCommand::Batch { label, commands: vec![set] }) {
            self.show_rolled_back();
        }
    }

    pub fn text_live(&mut self, prop: &str, v: f32) {
        self.text_edit_live(prop, TextValue::Num(v as f64));
    }

    pub fn text_commit(&mut self, prop: &str, v: f32) {
        self.text_edit_commit(prop, TextValue::Num(v as f64));
    }

    /// A switch, a choice or a font: "1" / "0", a number, or a name.
    pub fn text_set(&mut self, prop: &str, value: &str) {
        let v = match prop {
            "family" => TextValue::Name(value.to_string()),
            "italic" | "outline1" | "outline2" | "plaque" | "plaque-per-line" | "caps" | "wrap" => TextValue::Flag(value == "1"),
            _ => match value.parse::<f64>() {
                Ok(x) => TextValue::Num(x),
                Err(_) => return,
            },
        };
        self.text_edit_commit(prop, v);
    }

    pub fn text_color_live(&mut self, prop: &str, c: slint::Color) {
        self.text_edit_live(prop, TextValue::Colour(Rgba8(from_slint(c))));
    }

    pub fn text_color_commit(&mut self, prop: &str, c: slint::Color) {
        self.colour_committed(c);
        self.text_edit_commit(prop, TextValue::Colour(Rgba8(from_slint(c))));
    }

    /// The «ТЕКСТ» section for the inspected clip (called by `refresh_inspector`).
    pub fn refresh_text_inspector(&mut self) {
        let ui = self.ui();
        let Some(id) = self.inspected() else { return };
        let seq = self.project.sequence();
        let Some((ti, ci)) = seq.locate_clip(id) else { return };
        let Some(doc) = seq.tracks[ti].clips[ci].text.as_deref() else { return };
        ui.set_text_insp(text_data(doc, self.text_scope.as_ref(), seq.tracks[ti].locked));
        // The field shows the document unless it is the one being typed in.
        let plain = doc.plain();
        if self.text_session.clip() != Some(id) && ui.get_text_field().as_str() != plain {
            ui.set_text_field(plain.into());
        }
    }

    pub fn font_rows_refresh(&mut self) {
        let ui = self.ui();
        let current = ui.get_text_insp().family.to_string();
        let rows = font_rows(ui.get_font_search().as_str(), &current);
        crate::util::sync_rows_by_key(ui.get_font_rows(), rows, |r| (r.name.clone(), r.header), |m| ui.set_font_rows(m));
    }
}

/// «Добавить шрифт…»: a native dialog (outside the app borrow); each font
/// is installed into the user's font folder; the first family added is
/// applied to the inspected text; each file that fails says why.
pub fn add_font_dialog() {
    let Some(files) = rfd::FileDialog::new().set_title(t("text.add_font")).add_filter(t("dlg.filter.fonts"), &["ttf", "otf", "ttc"]).pick_files() else { return };
    let mut added: Vec<String> = vec![];
    let mut failed: Vec<(String, kadr_text::FontError)> = vec![];
    for f in files {
        match kadr_text::fonts().install_user_font(&f) {
            Ok(names) => added.extend(names),
            Err(e) => {
                tracing::warn!(file = %f.display(), error = %e, "font not added");
                failed.push((f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), e));
            }
        }
    }
    crate::app::with_app(|app| {
        if let Some(first) = added.first().cloned() {
            app.toast_ok(tf("text.font_added", &[("name", &added.join(", "))]));
            app.text_set("family", &first);
        }
        for (file, e) in failed {
            app.toast_warn(tf(&font_error_key(&e), &[("file", &file)]));
        }
        app.font_rows_refresh();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;
    use kadr_project::{Clip, Project, RunStyle};
    use kadr_timeline::EditEngine;

    fn red() -> Rgba8 {
        Rgba8::rgba(255, 0, 0, 255)
    }

    #[test]
    fn whole_text_edits_set_the_base_and_clear_the_runs_overrides() {
        let mut d = TextDoc::new("раз два");
        d.style_range(7..13, &RunStyle { size: Some(40.0), fill: Some(Fill::solid(red())), ..Default::default() });
        assert!(apply_text_prop(&mut d, None, "size", &TextValue::Num(120.0)));
        assert_eq!((d.style.size, d.runs.iter().any(|r| r.style.size.is_some())), (120.0, false));
        assert!(d.runs.iter().any(|r| r.style.fill.is_some()), "only the edited field's overrides go");
        assert!(!apply_text_prop(&mut d, None, "size", &TextValue::Num(120.0)), "the same value: no change");
        assert!(apply_text_prop(&mut d, None, "weight", &TextValue::Num(640.0)));
        assert_eq!(d.style.font.weight, 600, "weights land on hundreds");
        assert!(apply_text_prop(&mut d, None, "fill", &TextValue::Colour(Rgba8::BLACK)));
        assert_eq!((d.style.fill.colour, d.runs.len()), (Rgba8::BLACK, 1), "no overrides left: one run");
        assert!(apply_text_prop(&mut d, None, "family", &TextValue::Name("Oswald".into())));
        assert_eq!(d.style.font.family, "Oswald");
        assert!(apply_text_prop(&mut d, None, "letter-spacing", &TextValue::Num(6.0)));
        assert_eq!(d.paragraph.letter_spacing, 6.0);
    }

    #[test]
    fn scoped_edits_style_only_the_selection() {
        let mut d = TextDoc::new("раз два");
        assert!(apply_text_prop(&mut d, Some(7..13), "fill", &TextValue::Colour(red())));
        assert_eq!(d.runs.len(), 2);
        assert_eq!((d.runs[1].text.as_str(), d.runs[1].style.fill.as_ref().map(|f| f.colour)), ("два", Some(red())));
        assert!(apply_text_prop(&mut d, Some(7..13), "letter-spacing", &TextValue::Num(4.0)));
        assert_eq!(d.runs[1].style.letter_spacing, Some(4.0));
        assert!(!apply_text_prop(&mut d, Some(7..13), "outline1", &TextValue::Flag(false)), "block props ignore the scope: already off");
    }

    #[test]
    fn outlines_and_the_plaque_switch_on_with_defaults() {
        let mut d = TextDoc::starter("x", 1080);
        assert!(apply_text_prop(&mut d, None, "outline1", &TextValue::Flag(true)));
        assert_eq!(d.style.outlines, [Outline { width: 4.0, colour: Rgba8::BLACK }]);
        assert!(apply_text_prop(&mut d, None, "outline2", &TextValue::Flag(true)));
        assert_eq!(d.style.outlines.len(), 2);
        assert!(apply_text_prop(&mut d, None, "outline2-width", &TextValue::Num(9.0)));
        assert!(apply_text_prop(&mut d, None, "outline1-color", &TextValue::Colour(red())));
        assert_eq!((d.style.outlines[1].width, d.style.outlines[0].colour), (9.0, red()));
        assert!(apply_text_prop(&mut d, None, "outline1", &TextValue::Flag(false)));
        assert!(d.style.outlines.is_empty(), "the inner outline off takes the outer one too");
        assert!(!apply_text_prop(&mut d, None, "outline2-width", &TextValue::Num(3.0)), "no outline to size");
        assert!(apply_text_prop(&mut d, None, "plaque", &TextValue::Flag(true)));
        let p = d.style.plaque.unwrap();
        assert!(!p.per_line && p.colour.0[3] < 255 && p.radius > 0.0);
        assert!(apply_text_prop(&mut d, None, "plaque-per-line", &TextValue::Flag(true)));
        assert!(apply_text_prop(&mut d, None, "plaque-pad-x", &TextValue::Num(30.0)));
        assert_eq!((d.style.plaque.unwrap().per_line, d.style.plaque.unwrap().padding_x), (true, 30.0));
    }

    #[test]
    fn paragraph_edits() {
        let mut d = TextDoc::new("x");
        assert!(apply_text_prop(&mut d, None, "align", &TextValue::Num(0.0)));
        assert_eq!(d.paragraph.align, Align::Left);
        assert!(apply_text_prop(&mut d, None, "caps", &TextValue::Flag(true)));
        assert!(apply_text_prop(&mut d, None, "wrap", &TextValue::Flag(true)));
        assert_eq!(d.paragraph.wrap, Some(800.0));
        assert!(apply_text_prop(&mut d, None, "wrap-width", &TextValue::Num(500.0)));
        assert!(apply_text_prop(&mut d, None, "wrap", &TextValue::Flag(false)));
        assert_eq!(d.paragraph.wrap, None);
        assert!(apply_text_prop(&mut d, None, "line-height", &TextValue::Num(1.5)));
        assert!(!apply_text_prop(&mut d, None, "no-such-prop", &TextValue::Num(1.0)));
    }

    #[test]
    fn a_release_where_the_drag_began_records_nothing() {
        let d = TextDoc::starter("x", 1080);
        let mut same = d.clone();
        assert!(!apply_text_prop(&mut same, None, "size", &TextValue::Num(d.style.size as f64)));
        assert!(!apply_text_prop(&mut same, None, "fill", &TextValue::Colour(d.style.fill.colour)), "a colour picked again: nothing");
    }

    #[test]
    fn the_data_shows_the_doc() {
        let mut d = TextDoc::starter("x", 1080);
        d.style.font.weight = 650;
        d.style.outlines = vec![Outline { width: 3.0, colour: red() }];
        let v = text_data(&d, None, false);
        assert!(v.has && v.outline1 && !v.outline2 && !v.locked);
        assert_eq!((v.size, v.weight, v.outline1_width), (97.0, 650.0, 3.0));
        assert_eq!(crate::color_ui::from_slint(v.outline1_color), [255, 0, 0, 255]);
        assert_eq!(v.family.as_str(), "Montserrat");
        let mut m = d.clone();
        m.style.font.family = "Nope Sans".into();
        assert!(text_data(&m, None, true).family_missing);
    }

    #[test]
    fn font_rows_group_bundled_mine_system_and_filter() {
        let rows = font_rows("", "Inter");
        assert!(rows[0].header);
        assert!(rows.iter().any(|r| !r.header && r.name == "Montserrat"));
        assert!(rows.iter().filter(|r| r.current).all(|r| r.name == "Inter"));
        let some = font_rows("MONT", "x");
        assert!(some.iter().filter(|r| !r.header).all(|r| r.name.to_lowercase().contains("mont")));
        assert!(font_rows("zzzz-nothing", "x").is_empty(), "no match: no headers either");
    }

    fn project_with_text() -> (Project, kadr_core::ClipId) {
        let mut p = Project::new("t");
        let c = Clip::new_text(TextDoc::new("Гол"), Time::ZERO, Time::from_secs(5));
        let id = c.id;
        p.sequence_mut().tracks[0].clips.push(c);
        (p, id)
    }

    fn text_of(p: &Project, id: kadr_core::ClipId) -> String {
        p.sequence().clip(id).unwrap().text.as_deref().unwrap().plain()
    }

    #[test]
    fn a_typing_session_is_one_entry_and_survives_another_command() {
        let (mut p, id) = project_with_text();
        let mut e = EditEngine::new();
        let mut s = TextSession::default();
        for t in ["Гол!", "Гол!!", "Гол!!!"] {
            assert!(s.type_text(&mut p, id, t));
        }
        assert_eq!((s.clip(), text_of(&p, id)), (Some(id), "Гол!!!".to_string()), "live while typing");
        assert!(!e.can_undo(), "nothing recorded yet");
        // Another command arrives (App::execute_as finishes the session first).
        let cmd = s.finish(&mut p).expect("the session changed the text");
        assert_eq!(text_of(&p, id), "Гол", "finish puts the recorded state back…");
        e.execute(&mut p, cmd).unwrap();
        assert_eq!(text_of(&p, id), "Гол!!!", "…and the one command brings the typing back");
        e.execute(&mut p, kadr_timeline::EditCommand::MoveClips { clips: vec![id], delta: Time::from_secs(1), track_delta: 0 }).unwrap();
        assert_eq!(text_of(&p, id), "Гол!!!", "the other command keeps the typing");
        e.undo(&mut p);
        e.undo(&mut p);
        assert_eq!(text_of(&p, id), "Гол", "two entries: the move, the whole typing");
        assert!(s.clip().is_none() && s.finish(&mut p).is_none());
    }

    #[test]
    fn a_session_without_changes_records_nothing_and_a_finished_one_is_saved() {
        let (mut p, id) = project_with_text();
        let mut s = TextSession::default();
        assert!(!s.type_text(&mut p, id, "Гол"), "the same text starts nothing");
        assert!(s.finish(&mut p).is_none());
        s.type_text(&mut p, id, "Гол!");
        s.type_text(&mut p, id, "Гол");
        assert!(s.finish(&mut p).is_none(), "typed and typed back: no entry");
        s.type_text(&mut p, id, "Счёт");
        let mut e = EditEngine::new();
        let cmd = s.finish(&mut p).unwrap();
        e.execute(&mut p, cmd).unwrap();
        let json = String::from_utf8(kadr_project::io::to_json(&p)).unwrap();
        assert!(json.contains("Счёт"), "what is saved after finishing is the typed text");
    }

    fn name_of(p: &Project, id: kadr_core::ClipId) -> String {
        p.sequence().clip(id).unwrap().name.clone()
    }

    /// TX-R9: an auto-named title's name follows its text live while
    /// typing (the timeline shows `c.name`), and the one recorded command
    /// leaves it there and undoes it; a title the user renamed keeps its name.
    #[test]
    fn a_session_renames_an_auto_named_title_live_and_keeps_a_renamed_one() {
        let (mut p, id) = project_with_text();
        let mut e = EditEngine::new();
        let mut s = TextSession::default();
        s.type_text(&mut p, id, "Гол!\nвторая строка");
        assert_eq!(name_of(&p, id), "Гол!", "the auto name follows the first line live");
        let cmd = s.finish(&mut p).unwrap();
        assert_eq!(name_of(&p, id), "Гол", "finish puts the recorded name back too");
        e.execute(&mut p, cmd).unwrap();
        assert_eq!(name_of(&p, id), "Гол!");
        e.undo(&mut p);
        assert_eq!(name_of(&p, id), "Гол");

        let (ti, ci) = p.sequence().locate_clip(id).unwrap();
        p.sequence_mut().tracks[ti].clips[ci].name = "Счёт матча".into();
        s.type_text(&mut p, id, "2:1");
        assert_eq!(name_of(&p, id), "Счёт матча", "a renamed title keeps its name while typing");
        let cmd = s.finish(&mut p).unwrap();
        e.execute(&mut p, cmd).unwrap();
        assert_eq!((name_of(&p, id), text_of(&p, id)), ("Счёт матча".to_string(), "2:1".to_string()));
    }

    /// History labels and font errors build their keys at run time (the
    /// catalog scan cannot see them): each one is in both catalogs.
    #[test]
    fn every_runtime_text_key_exists_in_both_catalogs() {
        let props = [
            "family", "size", "weight", "italic", "fill", "letter-spacing", "outline1", "outline2-width", "plaque", "plaque-pad-y", "align", "line-height", "caps", "wrap",
            "wrap-width", "unknown",
        ];
        let mut keys: Vec<String> = props.iter().map(|p| prop_label_key(p).to_string()).collect();
        let errors = [kadr_text::FontError::Unreadable(String::new()), kadr_text::FontError::NotAFont, kadr_text::FontError::CopyFailed(String::new())];
        keys.extend(errors.iter().map(font_error_key));
        for lang in kadr_i18n::Lang::ALL {
            let have = kadr_i18n::all_keys(lang);
            for k in &keys {
                assert!(have.contains(&k.as_str()), "{lang:?} lacks {k}");
            }
        }
    }
}
