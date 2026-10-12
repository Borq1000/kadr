//! Text clips (titles spec §2): a clip whose picture is a [`TextDoc`] —
//! runs of styled text, a base style, a paragraph, a shape, a
//! perspective and an animation. Plain data: layout and pixels live in
//! `kadr-text`. Fields from a newer Kadr are kept verbatim (`extra`); a
//! closed choice this build does not know reads as its default.

use kadr_core::{AssetId, Time};
use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// The asset of every text clip: no media (titles spec §2.1). Never in
/// `Project.assets`, so every asset lookup of a text clip finds nothing.
pub const TEXT_ASSET: AssetId = AssetId(uuid::Uuid::nil());

type Extra = BTreeMap<String, Value>;

/// A straight-alpha colour in the working space, 8 bits per channel.
/// JSON: `"#rrggbbaa"` (reads `"#rrggbb"` too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba8(pub [u8; 4]);

impl Rgba8 {
    pub const WHITE: Rgba8 = Rgba8([255, 255, 255, 255]);
    pub const BLACK: Rgba8 = Rgba8([0, 0, 0, 255]);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba8 {
        Rgba8([r, g, b, a])
    }

    pub fn hex(&self) -> String {
        let [r, g, b, a] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }

    pub fn parse(s: &str) -> Option<Rgba8> {
        let h = s.trim().strip_prefix('#')?;
        // Hex digits only: `from_str_radix` alone would take a sign ("+f").
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) || !(h.len() == 6 || h.len() == 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgba8([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 255 }]))
    }

    /// Components in [0, 1], straight alpha.
    pub fn to_f32(self) -> [f32; 4] {
        self.0.map(|v| v as f32 / 255.0)
    }
}

impl Serialize for Rgba8 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgba8 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba8::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("not a colour: {s:?} (\"#rrggbb\" or \"#rrggbbaa\")")))
    }
}

/// A closed choice read leniently: a value this build does not know (a
/// newer Kadr's) reads as the default, so the project still opens.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).unwrap_or_default())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

/// What an animation moves one at a time (titles spec §4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Letter,
    #[default]
    Word,
    Line,
    Block,
}

/// The line the text runs along (titles spec §1): straight; an arc whose
/// `bend` −1..1 is its angle in half turns (positive bows upwards like a
/// rainbow); or a circle of `radius` canvas px, starting at `start_deg`
/// (0 = top, clockwise).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextShape {
    #[default]
    Straight,
    Arc { bend: f32 },
    Circle { radius: f32, start_deg: f32 },
}

fn is_straight(s: &TextShape) -> bool {
    *s == TextShape::Straight
}

/// A tilt of the finished text in degrees (−60..60): `tilt_x` about the
/// horizontal axis (positive: the top recedes), `tilt_y` about the
/// vertical one (positive: the right side recedes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Perspective {
    #[serde(default)]
    pub tilt_x: f32,
    #[serde(default)]
    pub tilt_y: f32,
}

fn default_family() -> String {
    "Montserrat".into()
}
fn w700() -> u16 {
    700
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontRef {
    #[serde(default = "default_family")]
    pub family: String,
    /// 100..900.
    #[serde(default = "w700")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

impl FontRef {
    pub fn new(family: impl Into<String>, weight: u16, italic: bool) -> FontRef {
        FontRef { family: family.into(), weight, italic, extra: Extra::new() }
    }
}

impl Default for FontRef {
    fn default() -> Self {
        FontRef::new(default_family(), 700, false)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// 0..1 along the gradient.
    pub pos: f32,
    pub colour: Rgba8,
}

/// A linear gradient across the text block: `angle` in degrees, 0 = left
/// to right, 90 = top to bottom.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GradientFill {
    pub stops: Vec<GradientStop>,
    #[serde(default)]
    pub angle: f32,
    #[serde(flatten)]
    pub extra: Extra,
}

impl GradientFill {
    pub fn new(stops: Vec<GradientStop>, angle: f32) -> GradientFill {
        GradientFill { stops, angle, extra: Extra::new() }
    }
}

/// The paint of letters: `colour`, or the `gradient` when there is one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub colour: Rgba8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<GradientFill>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Fill {
    pub fn solid(colour: Rgba8) -> Fill {
        Fill { colour, gradient: None, extra: Extra::new() }
    }
}

/// A band of `width` canvas px around the letters (outside them).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub width: f32,
    pub colour: Rgba8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    pub colour: Rgba8,
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub blur: f32,
}

fn one_f32() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Glow {
    pub colour: Rgba8,
    pub radius: f32,
    /// 0..2: the glow's alpha multiplier.
    #[serde(default = "one_f32")]
    pub intensity: f32,
}

/// A rounded rectangle under the block or under each line.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plaque {
    #[serde(default)]
    pub per_line: bool,
    pub colour: Rgba8,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub padding_x: f32,
    #[serde(default)]
    pub padding_y: f32,
}

fn size96() -> f32 {
    96.0
}
fn white_fill() -> Fill {
    Fill::solid(Rgba8::WHITE)
}

/// The whole text's style (titles spec §2.2): sizes in canvas pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default)]
    pub font: FontRef,
    /// Font size, canvas px.
    #[serde(default = "size96")]
    pub size: f32,
    #[serde(default = "white_fill")]
    pub fill: Fill,
    /// At most two; the first is the inner one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<Outline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<Glow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plaque: Option<Plaque>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { font: FontRef::default(), size: 96.0, fill: white_fill(), outlines: vec![], shadow: None, glow: None, plaque: None, extra: Extra::new() }
    }
}

/// A run's own style: each set field overrides the base style.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Fill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f32>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl RunStyle {
    pub fn is_empty(&self) -> bool {
        *self == RunStyle::default()
    }

    /// `patch`'s set fields over this style's.
    pub fn merged(&self, patch: &RunStyle) -> RunStyle {
        RunStyle {
            family: patch.family.clone().or_else(|| self.family.clone()),
            weight: patch.weight.or(self.weight),
            italic: patch.italic.or(self.italic),
            size: patch.size.or(self.size),
            fill: patch.fill.clone().or_else(|| self.fill.clone()),
            letter_spacing: patch.letter_spacing.or(self.letter_spacing),
            extra: self.extra.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "RunStyle::is_empty")]
    pub style: RunStyle,
    #[serde(flatten)]
    pub extra: Extra,
}

fn lh12() -> f32 {
    1.2
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphStyle {
    #[serde(default, deserialize_with = "lenient")]
    pub align: Align,
    /// Line distance as a multiple of the font size.
    #[serde(default = "lh12")]
    pub line_height: f32,
    /// Extra advance after every letter, canvas px.
    #[serde(default)]
    pub letter_spacing: f32,
    /// ALL CAPS at layout (the stored text keeps its case).
    #[serde(default)]
    pub caps: bool,
    /// Wrap lines at this block width, canvas px; `None`: only at line breaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<f32>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        ParagraphStyle { align: Align::Center, line_height: 1.2, letter_spacing: 0.0, caps: false, wrap: None, extra: Extra::new() }
    }
}

fn smooth() -> String {
    "smooth".into()
}

/// An appearance or disappearance (titles spec §4): `preset` from
/// `kadr-text`'s list (an unknown one is kept and not run); unit `i` of
/// `N` runs over `[i·stagger, i·stagger + duration]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reveal {
    pub preset: String,
    #[serde(default, deserialize_with = "lenient")]
    pub unit: Unit,
    pub duration: Time,
    #[serde(default)]
    pub stagger: Time,
    /// A curve key: smooth, linear, ease_in, ease_out, cinematic, snap, gentle.
    #[serde(default = "smooth")]
    pub ease: String,
    #[serde(flatten)]
    pub extra: Extra,
}

fn one_f64() -> f64 {
    1.0
}
fn amount_default() -> f64 {
    0.6
}

/// A motion repeating while the text is shown, in source time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopAnim {
    pub preset: String,
    #[serde(default, deserialize_with = "lenient")]
    pub unit: Unit,
    /// Cycles per second.
    #[serde(default = "one_f64")]
    pub speed: f64,
    /// 0..1.
    #[serde(default = "amount_default")]
    pub amount: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

fn full_span() -> (f64, f64) {
    (0.0, 1.0)
}
fn is_full_span(s: &(f64, f64)) -> bool {
    *s == full_span()
}
fn highlight_colour() -> Rgba8 {
    Rgba8::rgba(255, 212, 0, 255)
}
fn highlight_scale() -> f32 {
    1.08
}

/// The current word in another colour and size: words spread evenly over
/// the progress `span` (titles spec §3), which a split cuts like an
/// effect's progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    #[serde(default = "highlight_colour")]
    pub colour: Rgba8,
    #[serde(default = "highlight_scale")]
    pub scale: f32,
    #[serde(default = "full_span", skip_serializing_if = "is_full_span")]
    pub span: (f64, f64),
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextAnim {
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub reveal_in: Option<Reveal>,
    #[serde(rename = "out", default, skip_serializing_if = "Option::is_none")]
    pub reveal_out: Option<Reveal>,
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub looping: Option<LoopAnim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<Highlight>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl TextAnim {
    pub fn is_empty(&self) -> bool {
        *self == TextAnim::default()
    }
}

fn one_empty_run() -> Vec<TextRun> {
    vec![TextRun::default()]
}

/// `"runs": []` (a hand-edited or foreign file) reads as one empty run:
/// a document always has a run to type into.
fn runs_or_one<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<TextRun>, D::Error> {
    let runs = Vec::<TextRun>::deserialize(d)?;
    Ok(if runs.is_empty() { one_empty_run() } else { runs })
}

/// The style of no run at all (`run_style_at` on a doc without runs).
static NO_STYLE: RunStyle = RunStyle { family: None, weight: None, italic: None, size: None, fill: None, letter_spacing: None, extra: BTreeMap::new() };

/// A text clip's picture (titles spec §2.2). The text is the runs'
/// concatenation; there is always at least one run (maybe empty).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextDoc {
    #[serde(default = "one_empty_run", deserialize_with = "runs_or_one")]
    pub runs: Vec<TextRun>,
    #[serde(default)]
    pub style: TextStyle,
    #[serde(default)]
    pub paragraph: ParagraphStyle,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_straight")]
    pub shape: TextShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perspective: Option<Perspective>,
    #[serde(default, skip_serializing_if = "TextAnim::is_empty")]
    pub anim: TextAnim,
    /// Seed of the random-looking animations; a split keeps it.
    #[serde(default)]
    pub seed: u64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The largest grapheme boundary of `s` at or below `i` (clamped): a
/// style never starts or ends inside an emoji sequence or a letter with
/// its combining marks.
fn floor_grapheme(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    s.grapheme_indices(true).map(|(at, _)| at).chain([s.len()]).take_while(|&at| at <= i).last().unwrap_or(0)
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite())
}

impl Fill {
    fn is_finite(&self) -> bool {
        self.gradient.as_ref().is_none_or(|g| g.angle.is_finite() && g.stops.iter().all(|s| s.pos.is_finite()))
    }
}

impl TextDoc {
    /// `text` in one run of the default style, a fresh seed.
    pub fn new(text: &str) -> TextDoc {
        TextDoc {
            runs: vec![TextRun { text: text.to_string(), ..Default::default() }],
            style: TextStyle::default(),
            paragraph: ParagraphStyle::default(),
            shape: TextShape::Straight,
            perspective: None,
            anim: TextAnim::default(),
            seed: Self::fresh_seed(),
            extra: Extra::new(),
        }
    }

    pub fn fresh_seed() -> u64 {
        uuid::Uuid::new_v4().as_u64_pair().0
    }

    /// A new title (titles spec §2.3): Montserrat 700 at 9 % of the frame
    /// height, white, a soft shadow, centred.
    pub fn starter(text: &str, canvas_height: u32) -> TextDoc {
        let mut d = TextDoc::new(text);
        let size = (canvas_height as f32 * 0.09).round().max(8.0);
        d.style.size = size;
        d.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 153), offset_x: 0.0, offset_y: (size * 0.03).round(), blur: (size * 0.06).round() });
        d
    }

    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// The first non-empty line, trimmed, at most 60 characters (then "…"):
    /// the clip's name on the timeline.
    pub fn label(&self) -> String {
        let text = self.plain();
        let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        // Cut between graphemes: never inside an emoji sequence or a letter
        // with its combining marks.
        match line.grapheme_indices(true).nth(60) {
            Some((at, _)) => format!("{}…", &line[..at]),
            None => line.to_string(),
        }
    }

    pub fn is_animated(&self) -> bool {
        let a = &self.anim;
        a.reveal_in.is_some() || a.reveal_out.is_some() || a.looping.is_some() || a.highlight.is_some()
    }

    /// Every number of the document is finite: a NaN or an infinity from
    /// a caller (JSON cannot carry them) never reaches layout. The edit
    /// commands refuse a document that is not.
    pub fn is_finite(&self) -> bool {
        let (st, p, a) = (&self.style, &self.paragraph, &self.anim);
        let shape = match self.shape {
            TextShape::Straight => true,
            TextShape::Arc { bend } => bend.is_finite(),
            TextShape::Circle { radius, start_deg } => finite(&[radius, start_deg]),
        };
        finite(&[st.size, p.line_height, p.letter_spacing, p.wrap.unwrap_or(0.0)])
            && st.fill.is_finite()
            && st.outlines.iter().all(|o| o.width.is_finite())
            && st.shadow.is_none_or(|s| finite(&[s.offset_x, s.offset_y, s.blur]))
            && st.glow.is_none_or(|g| finite(&[g.radius, g.intensity]))
            && st.plaque.is_none_or(|q| finite(&[q.radius, q.padding_x, q.padding_y]))
            && shape
            && self.perspective.is_none_or(|v| finite(&[v.tilt_x, v.tilt_y]))
            && self.runs.iter().all(|r| {
                let rs = &r.style;
                finite(&[rs.size.unwrap_or(0.0), rs.letter_spacing.unwrap_or(0.0)]) && rs.fill.as_ref().is_none_or(Fill::is_finite)
            })
            && a.looping.as_ref().is_none_or(|l| l.speed.is_finite() && l.amount.is_finite())
            && a.highlight.as_ref().is_none_or(|h| h.scale.is_finite() && h.span.0.is_finite() && h.span.1.is_finite())
    }

    /// The style of the run holding byte `at` (the last run at the end).
    pub fn run_style_at(&self, at: usize) -> &RunStyle {
        let mut start = 0;
        for r in &self.runs {
            if at < start + r.text.len() {
                return &r.style;
            }
            start += r.text.len();
        }
        self.runs.last().map_or(&NO_STYLE, |r| &r.style)
    }

    /// Makes `at` (a char boundary) a run boundary; returns the index of
    /// the run starting there (`runs.len()` at the end).
    fn boundary(&mut self, at: usize) -> usize {
        let mut start = 0;
        for i in 0..self.runs.len() {
            let len = self.runs[i].text.len();
            if at == start {
                return i;
            }
            if at < start + len {
                let tail = self.runs[i].text.split_off(at - start);
                let r = TextRun { text: tail, style: self.runs[i].style.clone(), extra: self.runs[i].extra.clone() };
                self.runs.insert(i + 1, r);
                return i + 1;
            }
            start += len;
        }
        self.runs.len()
    }

    /// Replaces the text, keeping the styles of what did not change (spec
    /// §6.3): the changed middle (after the common prefix and suffix) is
    /// replaced by the new middle in the style of the character before it.
    /// `false` when the text is the same.
    pub fn set_plain(&mut self, new: &str) -> bool {
        let old = self.plain();
        if old == new {
            return false;
        }
        let mut pre = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
            pre -= 1;
        }
        let room = (old.len() - pre).min(new.len() - pre);
        let mut suf = old.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(old.len() - suf) || !new.is_char_boundary(new.len() - suf) {
            suf -= 1;
        }
        let style = self.run_style_at(pre.saturating_sub(1)).clone();
        let first_style = self.runs.first().map(|r| r.style.clone()).unwrap_or_default();
        let i = self.boundary(pre);
        let j = self.boundary(old.len() - suf);
        let inserted = &new[pre..new.len() - suf];
        self.runs.splice(i..j, (!inserted.is_empty()).then(|| TextRun { text: inserted.to_string(), style, extra: Extra::new() }));
        if self.runs.iter().all(|r| r.text.is_empty()) {
            self.runs = vec![TextRun { text: String::new(), style: first_style, extra: Extra::new() }];
        }
        self.normalise();
        true
    }

    /// Merges `patch` into the style of the text in `range` (bytes, floored
    /// to grapheme boundaries — so to char boundaries too — and clamped to
    /// the text). `false` when nothing changed.
    pub fn style_range(&mut self, range: Range<usize>, patch: &RunStyle) -> bool {
        self.restyle(range, |s| s.merged(patch))
    }

    /// Gives the text in `range` the base style again.
    pub fn reset_range(&mut self, range: Range<usize>) -> bool {
        self.restyle(range, |_| RunStyle::default())
    }

    fn restyle(&mut self, range: Range<usize>, f: impl Fn(&RunStyle) -> RunStyle) -> bool {
        let text = self.plain();
        let (a, b) = (floor_grapheme(&text, range.start), floor_grapheme(&text, range.end));
        if a >= b {
            return false;
        }
        let before = self.runs.clone();
        let i = self.boundary(a);
        let j = self.boundary(b);
        for r in &mut self.runs[i..j] {
            r.style = f(&r.style);
        }
        self.normalise();
        self.runs != before
    }

    /// Neighbours of one style merge, empty runs go; one run always stays
    /// (an empty doc keeps its first run's style).
    pub fn normalise(&mut self) {
        let first = self.runs.first().map(|r| r.style.clone()).unwrap_or_default();
        let mut out: Vec<TextRun> = Vec::with_capacity(self.runs.len());
        for r in self.runs.drain(..) {
            if r.text.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(l) if l.style == r.style && l.extra == r.extra => l.text.push_str(&r.text),
                _ => out.push(r),
            }
        }
        if out.is_empty() {
            out.push(TextRun { text: String::new(), style: first, extra: Extra::new() });
        }
        self.runs = out;
    }

    /// The two documents of a split at `frac` of the visible length
    /// (titles spec §3): the appearance stays left, the disappearance
    /// right, the highlight span is cut at `from + (to − from)·frac` (a
    /// non-finite `frac` counts as 0); the loop and the seed go to both.
    pub fn split_at(&self, frac: f64) -> (TextDoc, TextDoc) {
        let (mut l, mut r) = (self.clone(), self.clone());
        l.anim.reveal_out = None;
        r.anim.reveal_in = None;
        if let Some(h) = &self.anim.highlight {
            let f = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
            let c = h.span.0 + (h.span.1 - h.span.0) * f;
            let c = if c.is_finite() { c } else { h.span.0 };
            if let Some(x) = l.anim.highlight.as_mut() {
                x.span = (h.span.0, c);
            }
            if let Some(x) = r.anim.highlight.as_mut() {
                x.span = (c, h.span.1);
            }
        }
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn red() -> RunStyle {
        RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 0, 0, 255))), ..Default::default() }
    }

    fn doc(runs: &[(&str, RunStyle)]) -> TextDoc {
        let mut d = TextDoc::new("");
        d.runs = runs.iter().map(|(t, s)| TextRun { text: t.to_string(), style: s.clone(), extra: Default::default() }).collect();
        d
    }

    fn texts(d: &TextDoc) -> Vec<(&str, bool)> {
        d.runs.iter().map(|r| (r.text.as_str(), r.style.fill.is_some())).collect()
    }

    #[test]
    fn colours_are_hex_strings() {
        assert_eq!(Rgba8::parse("#ff8000"), Some(Rgba8([255, 128, 0, 255])));
        assert_eq!(Rgba8::parse(" #FF800080 "), Some(Rgba8([255, 128, 0, 128])));
        for bad in ["ff8000", "#ff80", "#gg8000", "#ff8000801", "#ффф000", "", "#+f8000", "#ff+f00", "#ff8000-1"] {
            assert_eq!(Rgba8::parse(bad), None, "{bad:?}");
        }
        assert_eq!(serde_json::to_value(Rgba8([1, 2, 3, 4])).unwrap(), json!("#01020304"));
        assert!(serde_json::from_value::<Rgba8>(json!("red")).is_err());
        assert_eq!(Rgba8([255, 0, 51, 255]).to_f32(), [1.0, 0.0, 0.2, 1.0]);
    }

    #[test]
    fn a_text_doc_round_trips_and_keeps_unknown_fields() {
        let mut d = TextDoc::starter("Привет, мир", 1080);
        d.runs[0].style = red();
        d.style.outlines = vec![Outline { width: 4.0, colour: Rgba8::BLACK }];
        d.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::rgba(0, 0, 0, 160), radius: 8.0, padding_x: 16.0, padding_y: 6.0 });
        d.shape = TextShape::Arc { bend: 0.4 };
        d.perspective = Some(Perspective { tilt_x: 20.0, tilt_y: 0.0 });
        d.anim.reveal_in = Some(Reveal { preset: "rise".into(), unit: Unit::Word, duration: Time::from_millis(500), stagger: Time::from_millis(80), ease: "smooth".into(), extra: Default::default() });
        d.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.08, span: (0.25, 1.0), extra: Default::default() });
        d.anim.looping = Some(LoopAnim { preset: "wave".into(), unit: Unit::Letter, speed: 2.0, amount: 0.3, extra: Default::default() });
        d.style.fill.gradient = Some(GradientFill::new(vec![GradientStop { pos: 0.0, colour: Rgba8::WHITE }, GradientStop { pos: 1.0, colour: Rgba8::BLACK }], 90.0));
        let mut v = serde_json::to_value(&d).unwrap();
        assert_eq!(serde_json::from_value::<TextDoc>(v.clone()).unwrap(), d);
        // A newer Kadr's fields at every level that may grow survive load → save.
        v["mask"] = json!({"shape": "star"});
        v["runs"][0]["ruby"] = json!("пример");
        v["runs"][0]["style"]["underline"] = json!(true);
        v["style"]["bevel"] = json!(2);
        v["style"]["fill"]["texture"] = json!("paper");
        v["paragraph"]["indent"] = json!(12);
        v["anim"]["path"] = json!("zigzag");
        v["anim"]["in"]["bounce"] = json!(3);
        v["anim"]["highlight"]["word_times"] = json!([[0, 1]]);
        v["anim"]["loop"]["wobble"] = json!(0.5);
        v["style"]["font"]["stretch"] = json!("condensed");
        v["style"]["fill"]["gradient"]["kind"] = json!("radial");
        let back = serde_json::to_value(serde_json::from_value::<TextDoc>(v.clone()).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn unknown_closed_choices_read_as_their_default() {
        let mut v = serde_json::to_value(TextDoc::new("x")).unwrap();
        v["paragraph"]["align"] = json!("justify_all");
        v["shape"] = json!({"spiral": {"turns": 3}});
        v["anim"] = json!({"in": {"preset": "rise", "unit": "syllable", "duration": 1}});
        let d: TextDoc = serde_json::from_value(v).unwrap();
        assert_eq!((d.paragraph.align, d.shape, d.anim.reveal_in.unwrap().unit), (Align::Center, TextShape::Straight, Unit::Word));
    }

    #[test]
    fn set_plain_keeps_the_styles_of_untouched_text() {
        let mut d = doc(&[("Привет, ", RunStyle::default()), ("мир", red()), ("!", RunStyle::default())]);
        assert!(d.set_plain("Привет, огромный мир!"));
        assert_eq!(texts(&d), [("Привет, огромный ", false), ("мир", true), ("!", false)], "an insertion takes the style before it");
        assert!(d.set_plain("Привет, огромный миры!"));
        assert_eq!(texts(&d), [("Привет, огромный ", false), ("миры", true), ("!", false)]);
        assert!(d.set_plain("Привет, !"));
        assert_eq!(texts(&d), [("Привет, !", false)], "a deleted run goes, its neighbours merge");
        assert!(!d.set_plain("Привет, !"), "the same text is no change");
        assert!(d.set_plain(""));
        assert_eq!((d.runs.len(), d.plain()), (1, String::new()));
    }

    #[test]
    fn deleting_everything_keeps_the_first_runs_style() {
        let mut d = doc(&[("мир", red()), ("!", RunStyle::default())]);
        assert!(d.set_plain(""));
        assert_eq!(texts(&d), [("", true)], "typing again continues in that style");
        assert!(d.set_plain("ок"));
        assert_eq!(texts(&d), [("ок", true)]);
    }

    #[test]
    fn style_range_splits_and_merges_runs_on_character_boundaries() {
        let mut d = TextDoc::new("один два три");
        // "один " is 9 bytes (4 × 2 + 1), "два" 6.
        assert!(d.style_range(9..15, &red()));
        assert_eq!(texts(&d), [("один ", false), ("два", true), (" три", false)]);
        assert!(!d.style_range(9..15, &red()), "the same style again is no change");
        assert!(d.style_range(10..13, &RunStyle { size: Some(40.0), ..Default::default() }), "10 is inside «д»: floored to 9");
        assert_eq!(d.runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["один ", "дв", "а", " три"]);
        assert_eq!((d.runs[1].style.size, d.runs[2].style.size), (Some(40.0), None));
        assert_eq!(d.runs[1].style.fill, red().fill, "a patch keeps the fields it does not set");
        assert!(d.reset_range(0..d.plain().len()));
        assert_eq!(texts(&d), [("один два три", false)]);
        assert!(!d.style_range(5..5, &red()), "an empty range changes nothing");
        assert!(!d.style_range(400..900, &red()), "a range past the end is clamped to nothing");
        assert_eq!(d.run_style_at(0), &RunStyle::default());
    }

    #[test]
    fn split_at_hands_in_left_out_right_and_cuts_the_highlight() {
        let mut d = TextDoc::new("раз два");
        let rev = |p: &str| Reveal { preset: p.into(), unit: Unit::Word, duration: Time::from_millis(400), stagger: Time::ZERO, ease: "smooth".into(), extra: Default::default() };
        d.anim.reveal_in = Some(rev("fade"));
        d.anim.reveal_out = Some(rev("drop"));
        d.anim.looping = Some(LoopAnim { preset: "pulse".into(), unit: Unit::Block, speed: 1.0, amount: 0.5, extra: Default::default() });
        d.anim.highlight = Some(Highlight { colour: Rgba8::WHITE, scale: 1.1, span: (0.2, 0.6), extra: Default::default() });
        let (l, r) = d.split_at(0.5);
        assert_eq!((l.anim.reveal_in.is_some(), l.anim.reveal_out.is_some()), (true, false));
        assert_eq!((r.anim.reveal_in.is_some(), r.anim.reveal_out.is_some()), (false, true));
        assert_eq!((l.anim.looping.is_some(), r.anim.looping.is_some()), (true, true));
        let span = |d: &TextDoc| d.anim.highlight.as_ref().unwrap().span;
        assert!((span(&l).1 - 0.4).abs() < 1e-12 && span(&l).0 == 0.2);
        assert!((span(&r).0 - 0.4).abs() < 1e-12 && span(&r).1 == 0.6);
        assert_eq!((l.seed, r.seed), (d.seed, d.seed), "one seed: scatter and shake look the same on both sides");
        assert_eq!(span(&d.split_at(f64::NAN).0), (0.2, 0.2), "a non-finite cut counts as 0");
    }

    #[test]
    fn empty_runs_load_as_one_empty_run_and_take_typing() {
        let mut v = serde_json::to_value(TextDoc::new("x")).unwrap();
        v["runs"] = json!([]);
        let mut d: TextDoc = serde_json::from_value(v).unwrap();
        assert_eq!((d.runs.len(), d.plain()), (1, String::new()), "a doc always has a run to type into");
        assert!(d.set_plain("ок"));
        assert!(d.style_range(0..2, &red()));
        assert_eq!(texts(&d), [("о", true), ("к", false)]);
        // Built without runs in code: nothing panics, typing still works.
        let mut d = doc(&[]);
        assert_eq!((d.run_style_at(0), d.run_style_at(9), d.plain()), (&RunStyle::default(), &RunStyle::default(), String::new()));
        assert!(!d.style_range(0..3, &red()) && !d.reset_range(0..3));
        assert!(d.set_plain("да"));
        assert_eq!(texts(&d), [("да", false)]);
        let mut d = doc(&[]);
        d.normalise();
        assert_eq!(d.runs.len(), 1);
    }

    /// Emoji (4 bytes), a ZWJ sequence and a combining mark: no panic, no
    /// style boundary inside a grapheme, and the text is never cut apart.
    #[test]
    fn emoji_zwj_sequences_and_combining_marks_survive_editing() {
        let family = "👨\u{200D}👩\u{200D}👧"; // bytes 6..24 below
        let text = format!("a👍 {family} е\u{301}ж"); // «е» 25..27, the acute 27..29
        let mut d = TextDoc::new(&text);
        assert!(!d.style_range(2..4, &red()), "inside 👍: floored to 1..1");
        assert!(!d.style_range(10..20, &red()), "inside the family: both ends floor to its start");
        assert!(!d.style_range(25..28, &red()), "е without its accent: the accent belongs to it");
        assert!(d.style_range(1..5, &red()));
        assert!(d.style_range(8..29, &RunStyle { size: Some(50.0), ..Default::default() }), "8 floors to the family's start");
        let sized: Vec<_> = d.runs.iter().map(|r| (r.text.as_str(), r.style.size.is_some())).collect();
        let styled = format!("{family} е\u{301}");
        assert_eq!(sized, [("a", false), ("👍", false), (" ", false), (styled.as_str(), true), ("ж", false)]);
        assert_eq!(d.runs[1].style.fill, red().fill);
        assert_eq!(d.plain(), text);

        let boy = "👨\u{200D}👩\u{200D}👦";
        let mut d = doc(&[(family, red()), ("!", RunStyle::default())]);
        assert!(d.set_plain(&format!("{boy}!")), "👧 → 👦 shares 3 bytes: the edit floors to a char boundary");
        assert_eq!(texts(&d), [(boy, true), ("!", false)], "the new emoji takes the style before it");
        let mut d = doc(&[("е", red()), ("ж", RunStyle::default())]);
        assert!(d.set_plain("е\u{301}ж"));
        assert_eq!(texts(&d), [("е\u{301}", true), ("ж", false)], "a combining mark joins its letter's style");
        let mut d = TextDoc::new("a👍b");
        assert!(d.set_plain("a👎b") && d.plain() == "a👎b");
        assert!(d.set_plain("ab") && d.plain() == "ab");

        let d = TextDoc::new(&text);
        let (l, r) = d.split_at(0.5);
        assert_eq!((l.plain(), r.plain()), (text.clone(), text.clone()), "a split keeps the whole text on both sides");
    }

    #[test]
    fn a_doc_with_a_non_finite_number_is_not_finite() {
        let ok = TextDoc::starter("x", 1080);
        assert!(ok.is_finite());
        let bad: [fn(&mut TextDoc); 6] = [
            |d| d.style.size = f32::NAN,
            |d| d.paragraph.wrap = Some(f32::INFINITY),
            |d| d.runs[0].style.letter_spacing = Some(f32::NAN),
            |d| d.shape = TextShape::Circle { radius: f32::NAN, start_deg: 0.0 },
            |d| d.style.fill.gradient = Some(GradientFill::new(vec![GradientStop { pos: f32::NAN, colour: Rgba8::WHITE }], 0.0)),
            |d| d.anim.highlight = Some(Highlight { colour: Rgba8::WHITE, scale: 1.0, span: (0.0, f64::NAN), extra: Default::default() }),
        ];
        for (i, f) in bad.iter().enumerate() {
            let mut d = ok.clone();
            f(&mut d);
            assert!(!d.is_finite(), "case {i}");
        }
    }

    #[test]
    fn starter_scales_with_the_frame_and_labels_cut_long_lines() {
        let hd = TextDoc::starter("x", 1080);
        assert_eq!(hd.style.size, 97.0);
        assert_eq!(TextDoc::starter("x", 2160).style.size, 194.0);
        assert_eq!(hd.style.font, FontRef::new("Montserrat", 700, false));
        assert!(hd.style.shadow.is_some() && hd.style.outlines.is_empty() && hd.anim.is_empty());
        assert_ne!(TextDoc::new("a").seed, TextDoc::new("a").seed);
        let long = TextDoc::new(&format!("\n  {}\nвторая", "ж".repeat(80)));
        assert_eq!(long.label(), format!("{}…", "ж".repeat(60)));
        assert_eq!(TextDoc::new("").label(), "");
        let accented = "е\u{301}";
        assert_eq!(TextDoc::new(&accented.repeat(70)).label(), format!("{}…", accented.repeat(60)), "60 letters, accents kept");
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(TextDoc::new(&family.repeat(61)).label(), format!("{}…", family.repeat(60)), "never inside an emoji sequence");
    }

    #[test]
    fn a_text_clip_round_trips_and_a_media_clip_writes_no_text() {
        let c = crate::Clip::new_text(TextDoc::new("Гол!"), Time::from_secs(2), Time::from_secs(5));
        assert!(c.is_text());
        assert_eq!((c.asset, c.source_in, c.source_out, c.timeline_out), (TEXT_ASSET, Time::ZERO, Time::from_secs(5), Time::from_secs(7)));
        assert_eq!(c.name, "Гол!");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["asset"], json!("00000000-0000-0000-0000-000000000000"));
        assert_eq!(serde_json::from_value::<crate::Clip>(v).unwrap(), c);
        let m = crate::Clip::new(kadr_core::AssetId::new(), "m", kadr_core::TimeRange::new(Time::ZERO, Time::from_secs(1)), Time::ZERO);
        assert!(serde_json::to_value(&m).unwrap().get("text").is_none(), "a media clip's JSON is unchanged");
        assert!(!m.is_text());
    }
}
