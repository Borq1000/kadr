//! The built-in text styles (titles spec §1, §6.5): data in
//! `crates/text/styles.json`, lengths for a 1080p frame (refinement 17).
//! Applying a style replaces a document's look — base style, paragraph,
//! shape, perspective, animation — and keeps its text and its runs' own
//! styles. A user's saved style is the same data (category "mine").
//!
//! Sizes follow the frame's SHORTER side (TX-R10): a style's lengths are
//! for a 1080-px short side and scale by `min(w, h) / 1080`, so a title
//! fits a vertical (9:16) or a square frame as it fits a 16:9 one. A wrap
//! width is a share of the frame's WIDTH (1080p: of 1920 px) and never
//! more than 90 % of it. A new clip of a style sits inside the 90 %
//! title-safe area on 16:9, 9:16 and 1:1 frames ([`StyleDef::position`]).

use kadr_project::{ParagraphStyle, Perspective, TextAnim, TextDoc, TextShape, TextStyle};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub const CATEGORIES: &[&str] = &["titles", "lower_thirds", "plaques", "neon", "captions", "other"];

/// The title-safe area: 90 % of the frame, so 45 % of each side from its centre.
pub const SAFE: f64 = 0.45;

/// The frame a style's lengths are for (1080p): its short side, its width.
const REF_SHORT: f32 = 1080.0;
const REF_W: f32 = 1920.0;
const REF_H: f32 = 1080.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StyleDef {
    pub id: String,
    pub category: String,
    /// The i18n key of the gallery's sample text.
    #[serde(default = "default_sample")]
    pub sample: String,
    #[serde(default)]
    pub style: TextStyle,
    #[serde(default)]
    pub paragraph: ParagraphStyle,
    #[serde(default)]
    pub shape: TextShape,
    #[serde(default)]
    pub perspective: Option<Perspective>,
    #[serde(default)]
    pub anim: TextAnim,
    /// A new clip's block centre on a 16:9 frame: fractions of the frame
    /// from its centre (x of its width, y of its height; see [`StyleDef::position`]).
    #[serde(default)]
    pub at: (f32, f32),
}

fn default_sample() -> String {
    "text.sample.other".into()
}

static BUILTIN: LazyLock<Vec<StyleDef>> = LazyLock::new(|| serde_json::from_str(include_str!("../styles.json")).expect("crates/text/styles.json"));

pub fn builtin() -> &'static [StyleDef] {
    &BUILTIN
}

pub fn by_id(id: &str) -> Option<&'static StyleDef> {
    BUILTIN.iter().find(|s| s.id == id)
}

/// The shorter side of a `w × h` frame (at least 1 px).
pub fn short_side(w: u32, h: u32) -> u32 {
    w.min(h).max(1)
}

/// Every length of a style times `k`.
fn scale_style(s: &TextStyle, k: f32) -> TextStyle {
    let mut s = s.clone();
    s.size *= k;
    for o in &mut s.outlines {
        o.width *= k;
    }
    if let Some(x) = s.shadow.as_mut() {
        (x.offset_x, x.offset_y, x.blur) = (x.offset_x * k, x.offset_y * k, x.blur * k);
    }
    if let Some(x) = s.glow.as_mut() {
        x.radius *= k;
    }
    if let Some(x) = s.plaque.as_mut() {
        (x.radius, x.padding_x, x.padding_y) = (x.radius * k, x.padding_x * k, x.padding_y * k);
    }
    s
}

/// The paragraph's letter spacing times `k`, its wrap times `kw`, at most `max_wrap`.
fn scale_paragraph(p: &ParagraphStyle, k: f32, kw: f32, max_wrap: f32) -> ParagraphStyle {
    let mut p = p.clone();
    p.letter_spacing *= k;
    p.wrap = p.wrap.map(|w| (w * kw).min(max_wrap));
    p
}

fn scale_shape(s: TextShape, k: f32) -> TextShape {
    match s {
        TextShape::Circle { radius, start_deg } => TextShape::Circle { radius: radius * k, start_deg },
        other => other,
    }
}

/// One axis of [`StyleDef::position`]: the centre `a` of the frame's
/// `len` px from its middle, or nearer the middle so that the room the
/// block had on its near side on a 1080p 16:9 frame (`reference` px long),
/// grown with the text by `k`, stays between it and the title-safe edge.
fn place_axis(a: f32, len: u32, reference: f32, k: f64) -> f64 {
    let (a, len) = (if a.is_finite() { a as f64 } else { 0.0 }, len as f64);
    let fraction = a.abs() * len;
    let room = (SAFE - a.abs()).max(0.0) * reference as f64 * k;
    let edge = (SAFE * len - room).max(0.0);
    a.signum() * if edge < fraction - 1e-6 { edge } else { fraction }
}

impl StyleDef {
    pub fn name_key(&self) -> String {
        format!("text.style.{}", self.id)
    }

    /// This style's look on `doc`, sized for a `w × h` frame: lengths by
    /// its shorter side (`min(w, h) / 1080`, TX-R10), the wrap by its
    /// width (`w / 1920`, at most 90 % of `w`).
    pub fn apply(&self, doc: &mut TextDoc, w: u32, h: u32) {
        let k = short_side(w, h) as f32 / REF_SHORT;
        let w = w.max(1) as f32;
        doc.style = scale_style(&self.style, k);
        doc.paragraph = scale_paragraph(&self.paragraph, k, w / REF_W, 2.0 * SAFE as f32 * w);
        doc.shape = scale_shape(self.shape, k);
        doc.perspective = self.perspective;
        doc.anim = self.anim.clone();
    }

    /// A new document of `text` in this style for a `w × h` frame.
    pub fn new_doc(&self, text: &str, w: u32, h: u32) -> TextDoc {
        let mut d = TextDoc::new(text);
        self.apply(&mut d, w, h);
        d
    }

    /// Where a new clip of this style goes on a `w × h` frame:
    /// `Transform.x / y` in canvas px. On 16:9 the `at` fractions of the
    /// frame; on a narrower (or taller) frame no farther out than keeps
    /// the room between the block and the 90 % title-safe edge that it has
    /// on 16:9 — the text is the same size there (it follows the short
    /// side), so a block that fits 16:9 fits 9:16 and 1:1 too.
    pub fn position(&self, w: u32, h: u32) -> (f64, f64) {
        let k = short_side(w, h) as f64 / REF_SHORT as f64;
        (place_axis(self.at.0, w, REF_W, k), place_axis(self.at.1, h, REF_H, k))
    }

    /// A user's style from a document of a `w × h` frame, stored for 1080p
    /// (the inverse of [`StyleDef::apply`]).
    pub fn from_doc(id: &str, doc: &TextDoc, w: u32, h: u32) -> StyleDef {
        let k = REF_SHORT / short_side(w, h) as f32;
        StyleDef {
            id: id.into(),
            category: "mine".into(),
            sample: "text.sample.other".into(),
            style: scale_style(&doc.style, k),
            paragraph: scale_paragraph(&doc.paragraph, k, REF_W / w.max(1) as f32, f32::MAX),
            shape: scale_shape(doc.shape, k),
            perspective: doc.perspective,
            anim: doc.anim.clone(),
            at: (0.0, 0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_style_loads_with_a_known_category_font_and_presets() {
        let all = builtin();
        assert_eq!(all.len(), 22);
        let mut ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 22, "unique ids");
        for s in all {
            assert!(CATEGORIES.contains(&s.category.as_str()), "{}: {}", s.id, s.category);
            assert!(crate::BUNDLED_FAMILIES.contains(&s.style.font.family.as_str()), "{}: a bundled font", s.id);
            for r in [&s.anim.reveal_in, &s.anim.reveal_out].into_iter().flatten() {
                assert!(crate::anim::reveal(&r.preset).is_some(), "{}: {}", s.id, r.preset);
                assert!(crate::anim::curve(&r.ease).is_some(), "{}: {}", s.id, r.ease);
            }
            if let Some(l) = &s.anim.looping {
                assert!(crate::anim::looping(&l.preset).is_some(), "{}: {}", s.id, l.preset);
            }
            assert!(s.sample.starts_with("text.sample."));
        }
        assert!(by_id("caption").is_some_and(|c| c.category == "captions" && c.at.1 > 0.3), "add_caption's style sits low");
    }

    #[test]
    fn applying_a_style_keeps_the_text_and_scales_to_the_frame() {
        let mut d = TextDoc::new("Победа");
        d.style_range(0..4, &kadr_project::RunStyle { size: Some(30.0), ..Default::default() });
        let neon = by_id("neon_pink").unwrap();
        neon.apply(&mut d, 3840, 2160);
        assert_eq!((d.plain(), d.runs[0].style.size), ("Победа".to_string(), Some(30.0)), "the text and its own styles stay");
        assert_eq!(d.style.size, neon.style.size * 2.0, "1080p sizes doubled for 2160 lines");
        assert_eq!(d.style.glow.unwrap().radius, neon.style.glow.unwrap().radius * 2.0);
        assert_eq!(d.anim, neon.anim);
        let back = StyleDef::from_doc("mine-1", &d, 3840, 2160);
        assert_eq!((back.style.size, back.category.as_str()), (neon.style.size, "mine"), "a saved style is stored at 1080p");
        assert_eq!(neon.position(1920, 1080), (neon.at.0 as f64 * 1920.0, neon.at.1 as f64 * 1080.0));
    }

    /// TX-R10: lengths follow the frame's shorter side; a wrap its width,
    /// capped at 90 % of it; a saved style is the same at 1080p again.
    #[test]
    fn sizes_follow_the_short_side_and_wraps_the_width() {
        let caption = by_id("caption").unwrap();
        let wrap = caption.paragraph.wrap.unwrap();
        for (w, h, k, kw) in [(1920, 1080, 1.0, 1.0), (1080, 1920, 1.0, 0.5625), (1080, 1080, 1.0, 0.5625), (3840, 2160, 2.0, 2.0), (720, 1280, 720.0 / 1080.0, 0.375)] {
            let d = caption.new_doc("Подпись", w, h);
            assert_eq!(d.style.size, caption.style.size * k, "{w}×{h}");
            assert_eq!(d.style.plaque.unwrap().padding_x, caption.style.plaque.unwrap().padding_x * k, "{w}×{h}");
            assert_eq!(d.paragraph.wrap, Some(wrap * kw), "{w}×{h}");
            let back = StyleDef::from_doc("u", &d, w, h);
            assert!((back.style.size - caption.style.size).abs() < 1e-3 && (back.paragraph.wrap.unwrap() - wrap).abs() < 1e-2, "{w}×{h}: {back:?}");
        }
        let mut wide = caption.clone();
        wide.paragraph.wrap = Some(5000.0);
        assert_eq!(wide.new_doc("x", 1080, 1920).paragraph.wrap, Some(0.9 * 1080.0), "never wider than 90 % of the frame");
        assert_eq!(wide.new_doc("x", 1920, 1080).paragraph.wrap, Some(0.9 * 1920.0));
    }

    /// A catalog's strings (`crates/i18n/locales/<lang>.json`).
    fn catalog(lang: &str) -> serde_json::Map<String, serde_json::Value> {
        let text = match lang {
            "ru" => include_str!("../../i18n/locales/ru.json"),
            _ => include_str!("../../i18n/locales/en.json"),
        };
        serde_json::from_str(text).expect("a catalog")
    }

    /// The texts a style is first seen with: a new clip's («Ваш текст»)
    /// and its gallery sample, in both languages.
    fn first_texts(s: &StyleDef) -> Vec<String> {
        let mut out = vec![];
        for lang in ["ru", "en"] {
            let c = catalog(lang);
            for key in ["text.default", s.sample.as_str()] {
                out.push(c.get(key).and_then(|v| v.as_str()).unwrap_or_else(|| panic!("{key} in {lang}.json")).to_string());
            }
        }
        out
    }

    /// TX-R10: every built-in style's new clip — its block at rest and its
    /// plaque — lies inside the 90 % title-safe area on 16:9, 9:16 and
    /// 1:1 frames, with a new clip's text and with its sample text.
    #[test]
    fn every_style_starts_inside_the_title_safe_area() {
        for s in builtin() {
            for (w, h) in [(1920u32, 1080u32), (1080, 1920), (1080, 1080), (3840, 2160)] {
                let (px, py) = s.position(w, h);
                let (sx, sy) = (SAFE as f32 * w as f32, SAFE as f32 * h as f32);
                for text in first_texts(s) {
                    let d = s.new_doc(&text, w, h);
                    let mut b = crate::frame_box(&d);
                    let laid = crate::layout(&d);
                    for (x0, y0, x1, y1) in crate::raster::plaque_rects(&d, &laid) {
                        let (cx, cy) = (laid.width / 2.0, laid.height / 2.0);
                        b = b.union(crate::Bounds { x0: x0 - cx, y0: y0 - cy, x1: x1 - cx, y1: y1 - cy });
                    }
                    let (x0, x1, y0, y1) = (b.x0 + px as f32, b.x1 + px as f32, b.y0 + py as f32, b.y1 + py as f32);
                    assert!(x0 >= -sx && x1 <= sx && y0 >= -sy && y1 <= sy, "{} on {w}×{h} with {text:?}: x {x0:.0}..{x1:.0} of ±{sx:.0}, y {y0:.0}..{y1:.0} of ±{sy:.0}", s.id);
                }
            }
        }
    }

    #[test]
    fn a_style_goes_no_farther_out_than_its_place_on_16_9() {
        let lower = by_id("lower_third_bar").unwrap();
        let (x, y) = lower.position(1920, 1080);
        assert!((x - lower.at.0 as f64 * 1920.0).abs() < 1e-6 && (y - lower.at.1 as f64 * 1080.0).abs() < 1e-6, "16:9: the fractions");
        let (x4, y4) = lower.position(3840, 2160);
        assert!((x4 - 2.0 * x).abs() < 1e-6 && (y4 - 2.0 * y).abs() < 1e-6, "4K: twice 1080p");
        // 9:16: the same room to the left safe edge as on 16:9, so nearer the middle.
        let (xv, yv) = lower.position(1080, 1920);
        assert!(((SAFE * 1080.0 + xv) - (SAFE * 1920.0 + x)).abs() < 1e-6, "{xv} vs {x}");
        assert!(yv > y && yv <= lower.at.1 as f64 * 1920.0 + 1e-6, "lower on the taller frame, never past its fraction: {yv}");
        assert_eq!(by_id("title").unwrap().position(1080, 1920), (0.0, 0.0));
    }
}
