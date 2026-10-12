//! Layout (titles spec §5.1): a document's runs shaped and broken into
//! lines by parley — fallback fonts, kerning, wrapping, alignment, line
//! and letter spacing, ALL CAPS — as glyphs in canvas px (layout box
//! coordinates: origin top left, y down, `y` on the baseline) with their
//! face, document run and animation units. Cached by what shapes it.

use crate::fonts::{fonts, DEFAULT_FAMILY};
use kadr_project::{Align, TextDoc};
use parking_lot::Mutex;
use parley::fontique::Blob;
use parley::{Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, LineHeight, PositionedLayoutItem, StyleProperty};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

/// The brush of laid-out text: the index of the document run it comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunIx(pub u16);

/// A face as a run uses it: the font file, its index, the variation
/// coordinates (synthesis variations included) and the synthesis left to
/// the rasteriser (fake bold, fake italic skew in degrees).
#[derive(Clone, Debug)]
pub struct Face {
    pub data: Blob<u8>,
    pub index: u32,
    pub coords: Vec<i16>,
    pub embolden: bool,
    pub skew: Option<f32>,
}

impl Face {
    fn same(&self, o: &Face) -> bool {
        self.data.id() == o.data.id() && self.index == o.index && self.coords == o.coords && self.embolden == o.embolden && self.skew == o.skew
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub face: usize,
    pub id: u16,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub advance: f32,
    /// The document run (its fill).
    pub run: usize,
    pub letter: usize,
    pub word: usize,
    pub line: usize,
    /// False for whitespace: never drawn, not a letter.
    pub visible: bool,
}

/// A line's box: `top`..`bottom` its line box, `left`..`right` its visible glyphs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineBox {
    pub top: f32,
    pub bottom: f32,
    pub baseline: f32,
    pub left: f32,
    pub right: f32,
}

#[derive(Debug)]
pub struct Laid {
    pub faces: Vec<Face>,
    pub glyphs: Vec<Glyph>,
    pub lines: Vec<LineBox>,
    /// The layout box: the wrap width (or the widest line) × the lines' height.
    pub width: f32,
    pub height: f32,
    pub letters: usize,
    pub words: usize,
    /// Families the document names that this machine lacks (drawn in DEFAULT_FAMILY).
    pub missing: Vec<String>,
    /// The largest font size in use.
    pub max_size: f32,
}

static CACHE: LazyLock<Mutex<HashMap<u64, Arc<Laid>>>> = LazyLock::new(Default::default);

fn clamp_size(s: f32) -> f32 {
    if s.is_finite() { s.clamp(1.0, 2000.0) } else { 96.0 }
}

fn weight(w: u16) -> FontWeight {
    FontWeight::new(w.clamp(100, 900) as f32)
}

fn style(italic: bool) -> FontStyle {
    if italic { FontStyle::Italic } else { FontStyle::Normal }
}

/// The document's layout, from the cache when the runs, the base font and
/// size, the paragraph and the font set are unchanged.
pub fn layout(doc: &TextDoc) -> Arc<Laid> {
    let key = crate::hash_of(&(&doc.runs, &doc.style.font, doc.style.size.to_bits(), &doc.paragraph, fonts().generation()));
    if let Some(l) = CACHE.lock().get(&key) {
        return l.clone();
    }
    let laid = Arc::new(build(doc));
    let mut c = CACHE.lock();
    if c.len() >= 256 {
        c.clear();
    }
    c.insert(key, laid.clone());
    laid
}

fn build(doc: &TextDoc) -> Laid {
    let f = fonts();
    let mut missing: Vec<String> = vec![];
    let mut family = |name: &str| -> String {
        if f.has(name) {
            name.to_string()
        } else {
            if !missing.iter().any(|m| m == name) {
                missing.push(name.to_string());
            }
            DEFAULT_FAMILY.to_string()
        }
    };
    let base_family = family(&doc.style.font.family);
    let run_families: Vec<Option<String>> = doc.runs.iter().map(|r| r.style.family.as_deref().map(&mut family)).collect();
    // The laid-out text (ALL CAPS applied) and each run's bytes in it.
    let mut text = String::new();
    let ranges: Vec<Range<usize>> = doc
        .runs
        .iter()
        .map(|r| {
            let start = text.len();
            text.push_str(&if doc.paragraph.caps { r.text.to_uppercase() } else { r.text.clone() });
            start..text.len()
        })
        .collect();
    let size = clamp_size(doc.style.size);
    let line_height = if doc.paragraph.line_height.is_finite() { doc.paragraph.line_height.clamp(0.5, 4.0) } else { 1.2 };
    let spacing = |v: f32| if v.is_finite() { v.clamp(-200.0, 2000.0) } else { 0.0 };
    let wrap = doc.paragraph.wrap.filter(|w| w.is_finite() && *w > 0.0);
    let layout = f.with_cx(|cx| {
        let mut b = cx.layout.ranged_builder(&mut cx.fonts, &text, 1.0, false);
        b.push_default(StyleProperty::FontFamily(FontFamily::named(&base_family)));
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::FontWeight(weight(doc.style.font.weight)));
        b.push_default(StyleProperty::FontStyle(style(doc.style.font.italic)));
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(line_height)));
        b.push_default(StyleProperty::LetterSpacing(spacing(doc.paragraph.letter_spacing)));
        b.push_default(StyleProperty::Brush(RunIx(0)));
        for (i, (r, range)) in doc.runs.iter().zip(&ranges).enumerate() {
            if range.is_empty() {
                continue;
            }
            b.push(StyleProperty::Brush(RunIx(i as u16)), range.clone());
            if let Some(name) = &run_families[i] {
                b.push(StyleProperty::FontFamily(FontFamily::named(name)), range.clone());
            }
            if let Some(s) = r.style.size {
                b.push(StyleProperty::FontSize(clamp_size(s)), range.clone());
            }
            if let Some(w) = r.style.weight {
                b.push(StyleProperty::FontWeight(weight(w)), range.clone());
            }
            if let Some(it) = r.style.italic {
                b.push(StyleProperty::FontStyle(style(it)), range.clone());
            }
            if let Some(ls) = r.style.letter_spacing {
                b.push(StyleProperty::LetterSpacing(spacing(ls)), range.clone());
            }
        }
        let mut layout = b.build(&text);
        layout.break_all_lines(wrap);
        let align = match doc.paragraph.align {
            Align::Left => Alignment::Left,
            Align::Center => Alignment::Center,
            Align::Right => Alignment::Right,
        };
        layout.align(align, AlignmentOptions { align_when_overflowing: false });
        layout
    });

    // Per byte of `text`: the letter and the word it belongs to.
    let (mut letter_at, mut word_at) = (vec![0usize; text.len() + 1], vec![0usize; text.len() + 1]);
    let (mut letters, mut words, mut in_word) = (0usize, 0usize, false);
    for (i, ch) in text.char_indices() {
        let ws = ch.is_whitespace();
        if !ws && !in_word {
            words += 1;
        }
        if !ws {
            letters += 1;
        }
        in_word = !ws;
        for b in i..i + ch.len_utf8() {
            (letter_at[b], word_at[b]) = (letters.saturating_sub(1), words.saturating_sub(1));
        }
    }

    let mut faces: Vec<Face> = vec![];
    let mut glyphs = vec![];
    let mut lines = vec![];
    for (li, line) in layout.lines().enumerate() {
        let m = line.metrics();
        let (mut left, mut right) = (f32::MAX, f32::MIN);
        let (mut last_run, mut cursor) = (usize::MAX, 0usize);
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else { continue };
            let run = gr.run();
            if run.index() != last_run {
                (last_run, cursor) = (run.index(), 0);
            }
            let n = gr.glyphs().count();
            // Glyph → its cluster's bytes, in the order of `positioned_glyphs`.
            let spans: Vec<Range<usize>> = run
                .visual_clusters()
                .flat_map(|c| {
                    let r = c.text_range();
                    c.glyphs().map(move |_| r.clone())
                })
                .skip(cursor)
                .take(n)
                .collect();
            cursor += n;
            let font = run.font();
            let syn = run.synthesis();
            let face = Face { data: font.data.clone(), index: font.index, coords: run.normalized_coords().to_vec(), embolden: syn.embolden(), skew: syn.skew() };
            let fi = match faces.iter().position(|f| f.same(&face)) {
                Some(i) => i,
                None => {
                    faces.push(face);
                    faces.len() - 1
                }
            };
            let doc_run = gr.style().brush.0 as usize;
            let size = run.font_size();
            for (g, span) in gr.positioned_glyphs().zip(spans) {
                let visible = !text[span.clone()].chars().all(char::is_whitespace);
                if visible {
                    left = left.min(g.x);
                    right = right.max(g.x + g.advance);
                }
                glyphs.push(Glyph { face: fi, id: g.id as u16, x: g.x, y: g.y, size, advance: g.advance, run: doc_run, letter: letter_at[span.start], word: word_at[span.start], line: li, visible });
            }
        }
        if left > right {
            (left, right) = (m.offset, m.offset);
        }
        lines.push(LineBox { top: m.block_min_coord, bottom: m.block_max_coord, baseline: m.baseline, left, right });
    }
    let width = wrap.unwrap_or(layout.width()).max(size * 0.3);
    let height = layout.height().max(size * line_height);
    let max_size = glyphs.iter().map(|g| g.size).fold(size, f32::max);
    Laid { faces, glyphs, lines, width, height, letters, words, missing, max_size }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{Fill, Rgba8, RunStyle, TextDoc};

    fn doc(text: &str) -> TextDoc {
        let mut d = TextDoc::new(text);
        d.style.size = 50.0;
        d
    }

    #[test]
    fn glyphs_map_back_to_runs_letters_words_and_lines() {
        let mut d = doc("ab cd\nэф");
        d.style_range(3..5, &RunStyle { fill: Some(Fill::solid(Rgba8::BLACK)), ..Default::default() });
        let l = layout(&d);
        let vis: Vec<(usize, usize, usize, usize)> = l.glyphs.iter().filter(|g| g.visible).map(|g| (g.run, g.letter, g.word, g.line)).collect();
        assert_eq!(vis, [(0, 0, 0, 0), (0, 1, 0, 0), (1, 2, 1, 0), (1, 3, 1, 0), (2, 4, 2, 1), (2, 5, 2, 1)]);
        assert_eq!((l.letters, l.words, l.lines.len()), (6, 3, 2));
        // Line boxes may overlap by a pixel (parley rounds each one outwards).
        assert!(l.lines[1].top >= l.lines[0].bottom - 1.5 && l.lines[1].baseline > l.lines[0].baseline);
        assert!(l.glyphs.iter().all(|g| g.size == 50.0));
        assert!(l.glyphs.iter().filter(|g| g.visible).all(|g| g.id != 0));
    }

    #[test]
    fn wrapping_and_alignment_shape_the_box() {
        let mut d = doc("один два три четыре пять шесть");
        let one_line = layout(&d);
        assert_eq!(one_line.lines.len(), 1);
        d.paragraph.wrap = Some(300.0);
        let wrapped = layout(&d);
        assert!(wrapped.lines.len() >= 3, "{}", wrapped.lines.len());
        assert_eq!(wrapped.width, 300.0, "a wrapped block is as wide as its wrap");
        // Tolerances of 0.3 em: a line's trailing space may count in its alignment.
        for line in &wrapped.lines {
            assert!(line.right <= 300.5 && line.left >= -0.5);
            assert!(((line.left + line.right) / 2.0 - 150.0).abs() < 15.0, "centred: {line:?}");
        }
        d.paragraph.align = kadr_project::Align::Left;
        assert!(layout(&d).lines.iter().all(|l| l.left.abs() < 1.0));
        d.paragraph.align = kadr_project::Align::Right;
        assert!(layout(&d).lines.iter().all(|l| (l.right - 300.0).abs() < 15.0));
        let empty = layout(&doc(""));
        assert!(empty.width > 0.0 && empty.height > 0.0, "an empty text still has a box to grab");
    }

    #[test]
    fn caps_upper_case_at_layout_only() {
        let mut d = doc("мяч");
        let lower: Vec<u16> = layout(&d).glyphs.iter().map(|g| g.id).collect();
        d.paragraph.caps = true;
        let upper: Vec<u16> = layout(&d).glyphs.iter().map(|g| g.id).collect();
        assert_ne!(lower, upper);
        assert_eq!(upper, layout(&doc("МЯЧ")).glyphs.iter().map(|g| g.id).collect::<Vec<_>>());
        assert_eq!(d.plain(), "мяч");
    }

    #[test]
    fn cyrillic_emoji_and_symbols_never_draw_notdef() {
        for family in crate::BUNDLED_FAMILIES {
            // ✓ after the Han text, as the probe verified: a Common-script symbol takes its neighbour's script.
            let mut d = doc("Привет, мир! 🎉⚽ 日本 ✓");
            d.style.font.family = family.to_string();
            let l = layout(&d);
            assert!(l.glyphs.iter().filter(|g| g.visible).all(|g| g.id != 0), "{family}: a .notdef glyph");
            assert!(l.missing.is_empty());
            let faces: std::collections::BTreeSet<usize> = l.glyphs.iter().filter(|g| g.visible).map(|g| g.face).collect();
            assert!(faces.len() >= 2, "{family}: the emoji and the CJK come from fallback fonts");
        }
    }

    #[test]
    fn a_missing_family_falls_back_and_says_so() {
        let mut d = doc("Матч");
        d.style.font.family = "Кадр Несуществующий".into();
        d.runs[0].style.family = Some("Also Missing".into());
        let l = layout(&d);
        assert_eq!(l.missing, ["Кадр Несуществующий", "Also Missing"]);
        assert!(l.glyphs.iter().all(|g| g.id != 0));
        let mut m = doc("Матч");
        m.style.font.family = crate::DEFAULT_FAMILY.into();
        let ids = |l: &Laid| l.glyphs.iter().map(|g| (g.id, g.x)).collect::<Vec<_>>();
        assert_eq!(ids(&l), ids(&layout(&m)), "drawn exactly as Montserrat");
    }

    #[test]
    fn layouts_are_cached_by_what_shapes_them() {
        let _serial = crate::TEST_FONTS.lock();
        let d = doc("кэш");
        let a = layout(&d);
        assert!(Arc::ptr_eq(&a, &layout(&d.clone())));
        let mut recoloured = d.clone();
        recoloured.style.fill = Fill::solid(Rgba8::BLACK);
        recoloured.anim.looping = Some(kadr_project::LoopAnim { preset: "pulse".into(), unit: kadr_project::Unit::Block, speed: 1.0, amount: 0.5, extra: Default::default() });
        assert!(Arc::ptr_eq(&a, &layout(&recoloured)), "paint and animation do not change the layout");
        let mut bigger = d.clone();
        bigger.style.size = 60.0;
        assert!(!Arc::ptr_eq(&a, &layout(&bigger)));
    }
}
