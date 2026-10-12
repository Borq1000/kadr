//! Text animation (titles spec §4): per-glyph state at an instant. This
//! task has the types and the resting state; Task 10 adds the presets.

use crate::layout::Laid;
use kadr_core::Time;
use kadr_project::{Rgba8, TextDoc};
use serde::Serialize;

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
    /// of the rest: equal keys draw equal pixels.
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

/// The animated state of every glyph at `clock` (Task 10: the presets).
pub fn animate(_doc: &TextDoc, laid: &Laid, _clock: Clock) -> FrameAnim {
    FrameAnim::rest(laid.glyphs.len())
}
