//! The line text runs along (titles spec §1): straight, an arc, a circle.
//! This task places glyphs straight; Task 9 adds arcs and circles.

use crate::layout::Laid;
use kadr_project::TextDoc;

/// A glyph's pen origin in layout px and its rotation (radians, clockwise).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
}

pub fn place(_doc: &TextDoc, laid: &Laid) -> Vec<Placed> {
    laid.glyphs.iter().map(|g| Placed { x: g.x, y: g.y, rotation: 0.0 }).collect()
}
