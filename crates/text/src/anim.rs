//! Text animation (titles spec §3–4): every glyph's state at an instant.
//! The reveals (appearance, disappearance) run per unit — letter, word,
//! line or the whole block — with a stagger and a curve, and shrink
//! together when they do not fit the clip; the disappearance is the
//! appearance played back from the clip's end. The loops run in the
//! clip's source time (a split or a trim never moves their phase). The
//! highlight walks the words over its progress. The same document, clock
//! and seed give the same state (preview = export = after a split).
//!
//! Absurd numbers never abort (TX-R5): durations and staggers below 0
//! count as 0 and any length works (a reveal longer than the clip
//! shrinks); a loop's speed is 0..20 Hz (not a number: 1), its amount
//! 0..1 (not a number: the preset's), a highlight's scale 0.2..5 (not a
//! number: 1); every offset, scale and blur a preset makes is a bounded
//! multiple of the glyph's size.

use crate::layout::{Glyph, Laid};
use kadr_core::ease::Curve;
use kadr_core::hash::{fmix64, pcg32_hash};
use kadr_core::Time;
use kadr_project::{LoopAnim, Reveal, Rgba8, TextDoc, Unit};
use serde::Serialize;
use std::f64::consts::TAU;

/// Where a text clip's frame is in time (titles spec §3): since its
/// visible start, until its visible end (both clip-bound), its source time
/// (loops) and the highlight progress (0..1 over the words).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    pub since_in: Time,
    pub until_out: Time,
    pub source: Time,
    pub highlight: f64,
}

impl Clock {
    /// Long after the appearance and before the disappearance, loops at 0.
    pub const REST: Clock = Clock { since_in: Time(i64::MAX / 4), until_out: Time(i64::MAX / 4), source: Time::ZERO, highlight: 0.0 };
}

/// One glyph's (or the whole block's) animated state: an offset (canvas
/// px), a scale and a rotation (radians, clockwise) about its centre, an
/// opacity, a blur (canvas px) and a colour that replaces its fill.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GlyphAnim {
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub rotation: f32,
    pub opacity: f32,
    pub blur: f32,
    pub colour: Option<Rgba8>,
}

impl GlyphAnim {
    pub const REST: GlyphAnim = GlyphAnim { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0, opacity: 1.0, blur: 0.0, colour: None };
    /// A glyph not drawn: where it would be does not matter.
    pub const HIDDEN: GlyphAnim = GlyphAnim { opacity: 0.0, ..GlyphAnim::REST };
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrameAnim {
    /// One per glyph of the layout.
    pub glyphs: Vec<GlyphAnim>,
    /// The whole block, about the layout box centre (pulse, float).
    pub block: GlyphAnim,
    pub plaque_opacity: f32,
    /// The typewriter cursor: after this glyph (`None`: no cursor).
    pub cursor: Option<usize>,
}

impl FrameAnim {
    pub fn rest(n: usize) -> FrameAnim {
        FrameAnim { glyphs: vec![GlyphAnim::REST; n], block: GlyphAnim::REST, plaque_opacity: 1.0, cursor: None }
    }

    pub fn is_rest(&self) -> bool {
        self.block == GlyphAnim::REST && self.plaque_opacity == 1.0 && self.cursor.is_none() && self.glyphs.iter().all(|g| *g == GlyphAnim::REST)
    }

    /// 0 at rest, else a hash of the state quantised to 1/64 px and 1/1024
    /// of the rest: equal keys draw equal pixels. [`animate`] leaves out
    /// what draws nothing (a hidden glyph's place, a space's state, the
    /// opacity of a plaque the text does not have), so a still frame of
    /// an animated title has the key of the frame before it.
    pub fn key(&self) -> u64 {
        if self.is_rest() {
            return 0;
        }
        let q = |g: &GlyphAnim| {
            let r = |v: f32, k: f32| (v * k).round() as i64;
            (r(g.dx, 64.0), r(g.dy, 64.0), r(g.scale, 1024.0), r(g.rotation, 1024.0), r(g.opacity, 1024.0), r(g.blur, 64.0), g.colour)
        };
        let glyphs: Vec<_> = self.glyphs.iter().map(q).collect();
        crate::hash_of(&(glyphs, q(&self.block), (self.plaque_opacity * 1024.0).round() as i64, self.cursor)).max(1)
    }
}

/// The reveal curves' keys (spec §4) and their `kadr_core::ease` curves.
pub const EASES: &[&str] = &["smooth", "linear", "ease_in", "ease_out", "cinematic", "snap", "gentle"];

pub fn curve(key: &str) -> Option<Curve> {
    Some(match key {
        "smooth" => Curve::EaseInOut,
        "linear" => Curve::Linear,
        "ease_in" => Curve::EaseIn,
        "ease_out" => Curve::EaseOut,
        "cinematic" => Curve::Cinematic,
        "snap" => Curve::Snap,
        "gentle" => Curve::Gentle,
        _ => return None,
    })
}

/// A unit's look at reveal progress `u` (0 hidden … 1 at rest) for a glyph
/// of `size` px; `rnd`: three seeded numbers in −1..1 for this unit.
type RevealLook = fn(f32, f32, [f32; 3]) -> GlyphAnim;

/// A reveal preset (titles spec §4): its id (the i18n key `text.anim.<id>`),
/// its defaults and its look.
pub struct RevealDef {
    pub id: &'static str,
    pub unit: Unit,
    pub duration_ms: i64,
    pub stagger_ms: i64,
    pub ease: &'static str,
    look: RevealLook,
}

impl RevealDef {
    /// A unit's state at progress `u` (clamped to 0..1; at 1 exactly
    /// [`GlyphAnim::REST`]).
    pub fn look(&self, u: f32, size: f32, rnd: [f32; 3]) -> GlyphAnim {
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        (self.look)(u, size, rnd)
    }

    /// This preset with its defaults, for a document.
    pub fn fresh(&self) -> Reveal {
        Reveal { preset: self.id.into(), unit: self.unit, duration: Time::from_millis(self.duration_ms), stagger: Time::from_millis(self.stagger_ms), ease: self.ease.into(), extra: Default::default() }
    }
}

/// Back-out (overshoot 1.70158): 0 → 1 passing about 1.1; exactly 1 at 1.
fn back_out(u: f32) -> f32 {
    let (c1, v) = (1.70158f32, u - 1.0);
    1.0 + (c1 + 1.0) * v * v * v + c1 * v * v
}

const R: GlyphAnim = GlyphAnim::REST;

pub const REVEALS: &[RevealDef] = &[
    RevealDef { id: "typewriter", unit: Unit::Letter, duration_ms: 1, stagger_ms: 50, ease: "linear", look: |u, _, _| GlyphAnim { opacity: if u > 0.0 { 1.0 } else { 0.0 }, ..R } },
    RevealDef { id: "fade", unit: Unit::Word, duration_ms: 400, stagger_ms: 80, ease: "smooth", look: |u, _, _| GlyphAnim { opacity: u, ..R } },
    RevealDef { id: "rise", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "snap", look: |u, s, _| GlyphAnim { dy: (1.0 - u) * 0.5 * s, opacity: u, ..R } },
    RevealDef { id: "drop", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "snap", look: |u, s, _| GlyphAnim { dy: -(1.0 - u) * 0.5 * s, opacity: u, ..R } },
    RevealDef { id: "slide_left", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "cinematic", look: |u, s, _| GlyphAnim { dx: (1.0 - u) * s, opacity: u, ..R } },
    RevealDef { id: "slide_right", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "cinematic", look: |u, s, _| GlyphAnim { dx: -(1.0 - u) * s, opacity: u, ..R } },
    RevealDef { id: "pop", unit: Unit::Letter, duration_ms: 350, stagger_ms: 30, ease: "linear", look: |u, _, _| GlyphAnim { scale: back_out(u).max(0.0), opacity: (3.0 * u).min(1.0), ..R } },
    RevealDef { id: "wave_in", unit: Unit::Letter, duration_ms: 450, stagger_ms: 40, ease: "linear", look: |u, s, _| GlyphAnim { dy: (1.0 - back_out(u)) * 0.6 * s, opacity: (2.0 * u).min(1.0), ..R } },
    RevealDef { id: "scatter", unit: Unit::Letter, duration_ms: 600, stagger_ms: 20, ease: "smooth", look: |u, s, r| GlyphAnim { dx: (1.0 - u) * r[0] * 2.0 * s, dy: (1.0 - u) * r[1] * 2.0 * s, rotation: (1.0 - u) * r[2] * 0.7, opacity: u, ..R } },
    RevealDef { id: "zoom", unit: Unit::Word, duration_ms: 450, stagger_ms: 80, ease: "cinematic", look: |u, _, _| GlyphAnim { scale: 2.0 - u, opacity: u, ..R } },
    RevealDef { id: "blur_in", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "smooth", look: |u, s, _| GlyphAnim { blur: (1.0 - u) * 0.3 * s, opacity: u, ..R } },
];

/// A loop's look at `phase` (cycles of source time × speed) for unit `i`,
/// `amount` 0..1, glyph `size`, the document's `seed`.
type LoopLook = fn(f64, usize, f32, f32, u64) -> GlyphAnim;

/// A loop preset (titles spec §4): its id (the i18n key `text.anim.<id>`),
/// its defaults and its look.
pub struct LoopDef {
    pub id: &'static str,
    pub unit: Unit,
    pub speed: f64,
    pub amount: f64,
    /// Moves the whole block (`FrameAnim::block`) rather than each unit.
    pub block: bool,
    look: LoopLook,
}

impl LoopDef {
    /// A unit's state at `phase` (an amount of 0 is exactly
    /// [`GlyphAnim::REST`]; not a number: the preset's).
    pub fn look(&self, phase: f64, i: usize, amount: f32, size: f32, seed: u64) -> GlyphAnim {
        let amount = if amount.is_finite() { amount.clamp(0.0, 1.0) } else { self.amount as f32 };
        let phase = if phase.is_finite() { phase } else { 0.0 };
        (self.look)(phase, i, amount, size, seed)
    }

    pub fn fresh(&self) -> LoopAnim {
        LoopAnim { preset: self.id.into(), unit: self.unit, speed: self.speed, amount: self.amount, extra: Default::default() }
    }
}

/// Smooth value noise in −1..1 over `t`, one stream per (seed, unit).
fn noise(seed: u64, i: usize, t: f64) -> f32 {
    let s = seed ^ fmix64(i as u64 + 1);
    let h = |k: f64| pcg32_hash(s, k as i64 as u64) as f64 / u32::MAX as f64 * 2.0 - 1.0;
    let (k, f) = (t.floor(), t - t.floor());
    let w = f * f * (3.0 - 2.0 * f);
    (h(k) + (h(k + 1.0) - h(k)) * w) as f32
}

fn sin(x: f64) -> f32 {
    x.sin() as f32
}

pub const LOOPS: &[LoopDef] = &[
    LoopDef { id: "pulse", unit: Unit::Block, speed: 1.0, amount: 0.6, block: true, look: |p, _, a, _, _| GlyphAnim { scale: 1.0 + 0.06 * a * sin(TAU * p), ..R } },
    LoopDef { id: "sway", unit: Unit::Word, speed: 0.5, amount: 0.6, block: false, look: |p, i, a, _, _| GlyphAnim { rotation: 4f32.to_radians() * a * sin(TAU * (p - 0.15 * i as f64)), ..R } },
    LoopDef { id: "shake", unit: Unit::Letter, speed: 1.0, amount: 0.5, block: false, look: |p, i, a, s, seed| GlyphAnim { dx: 0.04 * a * s * noise(seed, 2 * i, p * 12.0), dy: 0.04 * a * s * noise(seed, 2 * i + 1, p * 12.0), ..R } },
    LoopDef { id: "flicker", unit: Unit::Letter, speed: 1.0, amount: 0.6, block: false, look: |p, i, a, _, seed| GlyphAnim { opacity: 1.0 - 0.5 * a * noise(seed, i, p * 8.0).max(0.0), ..R } },
    LoopDef { id: "wave", unit: Unit::Letter, speed: 1.0, amount: 0.6, block: false, look: |p, i, a, s, _| GlyphAnim { dy: -0.15 * a * s * sin(TAU * (p - 0.1 * i as f64)), ..R } },
    LoopDef { id: "float", unit: Unit::Block, speed: 0.5, amount: 0.6, block: true, look: |p, _, a, s, _| GlyphAnim { dy: 0.05 * a * s * sin(TAU * p), ..R } },
];

pub fn reveal(id: &str) -> Option<&'static RevealDef> {
    REVEALS.iter().find(|r| r.id == id)
}

pub fn looping(id: &str) -> Option<&'static LoopDef> {
    LOOPS.iter().find(|l| l.id == id)
}

/// Both states at once: offsets add, scales multiply, turns add,
/// opacities multiply, the larger blur, the later colour.
fn combine(a: GlyphAnim, b: GlyphAnim) -> GlyphAnim {
    GlyphAnim { dx: a.dx + b.dx, dy: a.dy + b.dy, scale: a.scale * b.scale, rotation: a.rotation + b.rotation, opacity: a.opacity * b.opacity, blur: a.blur.max(b.blur), colour: b.colour.or(a.colour) }
}

fn unit_index(g: &Glyph, u: Unit) -> usize {
    match u {
        Unit::Letter => g.letter,
        Unit::Word => g.word,
        Unit::Line => g.line,
        Unit::Block => 0,
    }
}

fn units(laid: &Laid, u: Unit) -> usize {
    match u {
        Unit::Letter => laid.letters,
        Unit::Word => laid.words,
        Unit::Line => laid.lines.len(),
        Unit::Block => 1,
    }
    .max(1)
}

/// Progress of `t` seconds into a span of `d` seconds (a zero span is a step at 0).
fn progress(t: f64, d: f64) -> f32 {
    if d <= 1e-9 {
        if t >= 0.0 { 1.0 } else { 0.0 }
    } else {
        (t / d).clamp(0.0, 1.0) as f32
    }
}

/// Three seeded numbers in −1..1 for unit `i`.
fn rnds(seed: u64, i: usize) -> [f32; 3] {
    [0u64, 1, 2].map(|k| pcg32_hash(seed ^ fmix64(k + 17), i as u64) as f32 / u32::MAX as f32 * 2.0 - 1.0)
}

/// A reveal this build runs: its settings, its preset and curve, its
/// duration and stagger in seconds (≥ 0) and its number of units.
struct Run<'a> {
    r: &'a Reveal,
    def: &'static RevealDef,
    curve: Curve,
    d: f64,
    st: f64,
    n: usize,
}

impl Run<'_> {
    fn of<'a>(r: &'a Option<Reveal>, laid: &Laid) -> Option<Run<'a>> {
        let r = r.as_ref()?;
        let def = reveal(&r.preset)?;
        let secs = |t: Time| t.as_secs_f64().max(0.0);
        Some(Run { r, def, curve: curve(&r.ease).or_else(|| curve(def.ease)).unwrap_or(Curve::Linear), d: secs(r.duration), st: secs(r.stagger), n: units(laid, r.unit) })
    }

    /// From the first unit's start to the last one's end, seconds.
    fn total(&self) -> f64 {
        self.d + self.st * (self.n - 1) as f64
    }

    /// Unit `g`'s look `t` seconds into the reveal, everything `k` times as fast.
    fn look(&self, g: &Glyph, t: f64, k: f64, seed: u64) -> GlyphAnim {
        let i = unit_index(g, self.r.unit);
        let u = progress(t - i as f64 * self.st * k, self.d * k);
        self.def.look(self.curve.apply(u as f64) as f32, g.size, rnds(seed, i))
    }
}

/// The animated state of every glyph at `clock` (titles spec §3–4).
pub fn animate(doc: &TextDoc, laid: &Laid, clock: Clock) -> FrameAnim {
    let mut out = FrameAnim::rest(laid.glyphs.len());
    let secs = |t: Time| t.as_secs_f64().max(0.0);
    let (since, until) = (secs(clock.since_in), secs(clock.until_out));
    let len = since + until;
    let (pin, pout) = (Run::of(&doc.anim.reveal_in, laid), Run::of(&doc.anim.reveal_out, laid));
    let (tin, tout) = (pin.as_ref().map_or(0.0, Run::total), pout.as_ref().map_or(0.0, Run::total));
    // Reveals longer than the clip shrink together (titles spec §3).
    let k = if tin + tout > len && len > 0.0 { len / (tin + tout) } else { 1.0 };
    let lp = doc.anim.looping.as_ref().and_then(|l| Some((l, looping(&l.preset)?)));
    let speed = |l: &LoopAnim| if l.speed.is_finite() { l.speed.clamp(0.0, 20.0) } else { 1.0 };
    let phase = lp.map_or(0.0, |(l, _)| clock.source.as_secs_f64() * speed(l));
    let words = laid.words.max(1);
    let p = if clock.highlight.is_finite() { clock.highlight.clamp(0.0, 1.0) } else { 0.0 };
    let lit = doc.anim.highlight.as_ref().map(|h| (h, ((p * words as f64).floor() as usize).min(words - 1)));
    // The glyphs the appearance has shown (the typewriter's cursor follows them).
    let mut typed = vec![false; laid.glyphs.len()];
    for (gi, g) in laid.glyphs.iter().enumerate() {
        if !g.visible {
            continue; // a space: never drawn
        }
        let mut a = GlyphAnim::REST;
        if let Some(run) = &pin {
            let look = run.look(g, since, k, doc.seed);
            typed[gi] = look.opacity > 0.0;
            a = combine(a, look);
        }
        if let Some(run) = &pout {
            // The out mirrors the in from the clip's end: unit 0 leaves first.
            let i = unit_index(g, run.r.unit);
            let w = progress(tout * k - until - i as f64 * run.st * k, run.d * k);
            a = combine(a, run.def.look(run.curve.apply(1.0 - w as f64) as f32, g.size, rnds(doc.seed ^ 0x5eed, i)));
        }
        if let Some((l, def)) = lp.filter(|(_, def)| !def.block) {
            a = combine(a, def.look(phase, unit_index(g, l.unit), l.amount as f32, g.size, doc.seed));
        }
        if let Some((h, word)) = lit
            && g.word == word
        {
            let scale = if h.scale.is_finite() { h.scale.clamp(0.2, 5.0) } else { 1.0 };
            a = combine(a, GlyphAnim { scale, colour: Some(h.colour), ..GlyphAnim::REST });
        }
        // A glyph not drawn is one state whatever its place: it never changes the key.
        out.glyphs[gi] = if a.opacity > 0.0 { a } else { GlyphAnim::HIDDEN };
    }
    if let Some((l, def)) = lp.filter(|(_, def)| def.block) {
        out.block = def.look(phase, 0, l.amount as f32, doc.style.size, doc.seed);
    }
    // The plaque fades with the reveals — when the text has one to draw.
    let plaque = doc.style.plaque.is_some_and(|p| p.colour.0[3] > 0) && !crate::raster::plaque_rects(doc, laid).is_empty();
    if plaque {
        let fade_in = if tin > 0.0 { progress(since, tin * k) } else { 1.0 };
        let fade_out = if tout > 0.0 { 1.0 - progress(tout * k - until, tout * k) } else { 1.0 };
        out.plaque_opacity = fade_in * fade_out;
    }
    // The typewriter's cursor: after the last letter typed, while typing.
    if pin.as_ref().is_some_and(|r| r.def.id == "typewriter") {
        let visible = laid.glyphs.iter().filter(|g| g.visible).count();
        let shown = typed.iter().filter(|t| **t).count();
        if shown > 0 && shown < visible {
            out.cursor = typed.iter().rposition(|t| *t);
        }
    }
    out
}
