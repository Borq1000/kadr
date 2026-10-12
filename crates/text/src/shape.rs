//! The line text runs along (titles spec §1): straight, an arc, a circle.

use crate::frame::Bounds;
use crate::layout::Laid;
use kadr_project::{TextDoc, TextShape};

/// A glyph's pen origin in layout px and its rotation (radians, clockwise).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
}

/// The largest circle a text runs round, canvas px.
const MAX_RADIUS: f32 = 20_000.0;

/// An arc's bend as an angle (radians): the bend (−1..1, not a number:
/// 0) times π; under 1e-3 the arc is straight.
fn arc_angle(bend: f32) -> f32 {
    let bend = if bend.is_finite() { bend.clamp(-1.0, 1.0) } else { 0.0 };
    bend * std::f32::consts::PI
}

/// Whether [`place`] bends the text (an arc that bends, a circle).
pub(crate) fn is_curved(doc: &TextDoc) -> bool {
    match doc.shape {
        TextShape::Straight => false,
        TextShape::Arc { bend } => arc_angle(bend).abs() >= 1e-3,
        TextShape::Circle { .. } => true,
    }
}

/// The text block, canvas px about the layout box centre (TX-R15): the
/// layout box for a straight text; for a curved one the box of its glyphs
/// as placed — each visible glyph's advance (a line's last without the
/// letter spacing after it) by its line's height about the baseline,
/// turned with it — and the centre point, the anchor. It is not made
/// symmetric about the centre: a rainbow's block reaches down to its
/// ends, not as far up.
pub(crate) fn block(doc: &TextDoc, laid: &Laid, placed: &[Placed]) -> Bounds {
    let (cx, cy) = (laid.width / 2.0, laid.height / 2.0);
    if !is_curved(doc) {
        return Bounds { x0: -cx, y0: -cy, x1: cx, y1: cy };
    }
    let mut b = Bounds { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0 };
    for (g, p) in laid.glyphs.iter().zip(placed).filter(|(g, _)| g.visible) {
        let line = laid.lines.get(g.line);
        let advance = line.map_or(g.advance, |l| g.advance.min(l.right - g.x).max(0.0));
        let (top, bottom) = line.map_or((-g.size, 0.35 * g.size), |l| (l.top - l.baseline, l.bottom - l.baseline));
        let (s, c) = p.rotation.sin_cos();
        for (x, y) in [(0.0, top), (advance, top), (advance, bottom), (0.0, bottom)] {
            b.add_point(p.x + x * c - y * s - cx, p.y + x * s + y * c - cy);
        }
    }
    b
}

/// Each glyph's pen origin and rotation along the document's shape. A
/// glyph keeps its arc length from the line's start (a circle) or the
/// block's middle (an arc) on the circle through its own baseline, its
/// baseline the circle's tangent: the pen origin is the baseline's middle
/// moved back half an advance along `(cos rot, sin rot)`. A combining
/// mark (no advance of its own, shaped over its letter) is placed where
/// it stands, not with its letter: on a tight curve it can drift off it.
pub fn place(doc: &TextDoc, laid: &Laid) -> Vec<Placed> {
    let straight = || laid.glyphs.iter().map(|g| Placed { x: g.x, y: g.y, rotation: 0.0 }).collect();
    let (w, h) = (laid.width, laid.height);
    let on_tangent = |mx: f32, my: f32, rot: f32, advance: f32| Placed { x: mx - advance / 2.0 * rot.cos(), y: my - advance / 2.0 * rot.sin(), rotation: rot };
    match doc.shape {
        TextShape::Straight => straight(),
        TextShape::Arc { bend } => {
            let angle = arc_angle(bend);
            if angle.abs() < 1e-3 {
                return straight();
            }
            // The block's width spans `angle` on a circle of radius R; its
            // centre lies R below the block centre (a rainbow, s = 1) or
            // above it (a smile, s = −1).
            let s = angle.signum();
            let r = w / angle.abs();
            let (cx, cy) = (w / 2.0, h / 2.0 + s * r);
            laid.glyphs
                .iter()
                .map(|g| {
                    let rr = (s * (cy - g.y)).max(1.0);
                    let alpha = (g.x + g.advance / 2.0 - cx) / rr;
                    on_tangent(cx + rr * alpha.sin(), cy - s * rr * alpha.cos(), s * alpha, g.advance)
                })
                .collect()
        }
        TextShape::Circle { radius, start_deg } => {
            // Clockwise round the outside of a circle about the block
            // centre, from `start_deg` (0 = top); each further line one
            // line inwards.
            let radius = if radius.is_finite() { radius.clamp(10.0, MAX_RADIUS) } else { 200.0 };
            let start = if start_deg.is_finite() { start_deg.to_radians() } else { 0.0 };
            let (cx, cy) = (w / 2.0, h / 2.0);
            let first_baseline = laid.lines.first().map_or(0.0, |l| l.baseline);
            laid.glyphs
                .iter()
                .map(|g| {
                    let left = laid.lines.get(g.line).map_or(0.0, |l| l.left);
                    let rr = (radius - (g.y - first_baseline)).max(1.0);
                    let phi = start + (g.x + g.advance / 2.0 - left) / rr;
                    on_tangent(cx + rr * phi.sin(), cy - rr * phi.cos(), phi, g.advance)
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{TextDoc, TextShape};

    fn doc(shape: TextShape) -> TextDoc {
        let mut d = TextDoc::new("радуга текста");
        d.style.size = 60.0;
        d.shape = shape;
        d
    }

    fn visible(d: &TextDoc) -> Vec<(Placed, f32)> {
        let l = crate::layout(d);
        place(d, &l).into_iter().zip(l.glyphs.iter()).filter(|(_, g)| g.visible).map(|(p, g)| (p, g.advance)).collect()
    }

    #[test]
    fn a_rainbow_turns_its_ends_down_and_a_smile_up() {
        let flat = visible(&doc(TextShape::Straight));
        assert!(flat.iter().all(|(p, _)| p.rotation == 0.0));
        let up = visible(&doc(TextShape::Arc { bend: 0.5 }));
        let (first, mid, last) = (up[0].0, up[up.len() / 2].0, up[up.len() - 1].0);
        assert!(first.rotation < -0.3 && last.rotation > 0.3, "the ends lean out: {} {}", first.rotation, last.rotation);
        assert!(mid.rotation.abs() < 0.15);
        assert!(first.y > mid.y + 10.0 && last.y > mid.y + 10.0, "a rainbow's ends drop");
        let down = visible(&doc(TextShape::Arc { bend: -0.5 }));
        assert!(down[0].0.y < down[down.len() / 2].0.y - 10.0 && down[0].0.rotation > 0.3, "a smile's ends rise and lean in");
        let almost = visible(&doc(TextShape::Arc { bend: 0.0 }));
        assert!(almost.iter().zip(&flat).all(|(a, b)| (a.0.x - b.0.x).abs() < 1e-3 && (a.0.y - b.0.y).abs() < 1e-3), "bend 0 is straight");
    }

    #[test]
    fn a_circle_puts_the_glyphs_on_its_radius_clockwise_from_its_start() {
        let d = doc(TextShape::Circle { radius: 200.0, start_deg: 0.0 });
        let l = crate::layout(&d);
        let (cx, cy) = (l.width / 2.0, l.height / 2.0);
        let g = visible(&d);
        for (p, adv) in &g {
            // The baseline's middle lies on the circle.
            let (mx, my) = (p.x + adv / 2.0 * p.rotation.cos(), p.y + adv / 2.0 * p.rotation.sin());
            assert!(((mx - cx).hypot(my - cy) - 200.0).abs() < 1.0, "{mx} {my}");
        }
        assert!(g[0].0.rotation >= 0.0 && g[0].0.rotation < 0.2, "starts at the top");
        assert!(g.windows(2).all(|w| w[1].0.rotation > w[0].0.rotation), "runs clockwise");
    }
}
