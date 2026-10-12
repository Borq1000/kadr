//! Raster effects of text (titles spec §5.1): a Gaussian-like blur (three
//! box blurs) for shadows, glows and blur-in; the fill paint (solid or a
//! linear gradient across the text block); the perspective tilt (a
//! homography of the finished flat picture). Sizes from a document are
//! clamped here (TX-R5): absurd values never allocate huge rasters, never
//! hang and never put a tilted picture's bounds out of reach.

use crate::frame::Bounds;
use crate::raster::{premul, rgba8, Canvas, Mask};
use kadr_core::CpuFrame;
use kadr_project::{Fill, Perspective, Rgba8, TextDoc};

/// The widest shadow blur and glow radius, canvas px.
pub(crate) const MAX_BLUR: f32 = 500.0;
/// The farthest shadow offset, canvas px.
pub(crate) const MAX_OFFSET: f32 = 10_000.0;
/// The steepest tilt, degrees.
pub(crate) const MAX_TILT: f32 = 60.0;

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// The shadow as drawn: its colour, offset (±[`MAX_OFFSET`]) and blur
/// (0..=[`MAX_BLUR`]), canvas px; a value that is not a number is 0.
/// `None` without a shadow or when it is transparent.
pub(crate) fn shadow(doc: &TextDoc) -> Option<(Rgba8, f32, f32, f32)> {
    let s = doc.style.shadow.filter(|s| s.colour.0[3] > 0)?;
    let off = |v: f32| finite(v).clamp(-MAX_OFFSET, MAX_OFFSET);
    Some((s.colour, off(s.offset_x), off(s.offset_y), finite(s.blur).clamp(0.0, MAX_BLUR)))
}

/// The glow as drawn: its colour, radius (0..=[`MAX_BLUR`] canvas px, not
/// a number: 0) and intensity (0..=2, not a number: 1). `None` without a
/// glow or when it would draw nothing.
pub(crate) fn glow(doc: &TextDoc) -> Option<(Rgba8, f32, f32)> {
    let g = doc.style.glow.filter(|g| g.colour.0[3] > 0)?;
    let radius = finite(g.radius).clamp(0.0, MAX_BLUR);
    let k = if g.intensity.is_finite() { g.intensity.clamp(0.0, 2.0) } else { 1.0 };
    (radius > 0.0 && k > 0.0).then_some((g.colour, radius, k))
}

/// The document's tilt, each axis within ±[`MAX_TILT`] (not a number: 0);
/// `None` when it has none.
pub(crate) fn tilt(doc: &TextDoc) -> Option<Perspective> {
    let deg = |v: f32| finite(v).clamp(-MAX_TILT, MAX_TILT);
    doc.perspective.map(|p| Perspective { tilt_x: deg(p.tilt_x), tilt_y: deg(p.tilt_y) }).filter(|p| p.tilt_x != 0.0 || p.tilt_y != 0.0)
}

/// Three box blurs of radius `b` ≈ a Gaussian of σ = `r` raster px
/// (`(2b + 1)² = 4r² + 1`); edges read as transparent; under half a pixel
/// nothing happens. Its cost does not depend on `r`.
pub(crate) fn blur(m: &mut Mask, r: f32) {
    if !r.is_finite() || r <= 0.5 {
        return;
    }
    let (w, h) = (m.w, m.h);
    // A box wider than the mask averages it whole: no wider box is needed.
    let b = (((4.0 * r * r + 1.0).sqrt() - 1.0) / 2.0).round().clamp(1.0, w.max(h) as f32) as usize;
    let mut tmp = vec![0.0f32; w.max(h)];
    let mut col = vec![0.0f32; h];
    for _ in 0..3 {
        for row in m.a.chunks_exact_mut(w) {
            box_line(row, b, &mut tmp);
        }
        for x in 0..w {
            for (y, v) in col.iter_mut().enumerate() {
                *v = m.a[y * w + x];
            }
            box_line(&mut col, b, &mut tmp);
            for (y, v) in col.iter().enumerate() {
                m.a[y * w + x] = *v;
            }
        }
    }
}

/// One box blur of width `2b + 1` along a line (zero outside it).
fn box_line(line: &mut [f32], b: usize, tmp: &mut [f32]) {
    let n = line.len();
    let k = 1.0 / (2 * b + 1) as f32;
    let mut sum: f32 = line.iter().take(b + 1).sum();
    for i in 0..n {
        tmp[i] = sum * k;
        if i + b + 1 < n {
            sum += line[i + b + 1];
        }
        if i >= b {
            sum -= line[i - b];
        }
    }
    line.copy_from_slice(&tmp[..n]);
}

/// The paint of a fill at a raster pixel: its colour, or its gradient
/// across the text block `b` (raster px `[x0, y0, x1, y1]`) at its angle
/// (0° left → right, 90° top → bottom), stops mixed premultiplied.
pub(crate) enum Paint {
    Solid([f32; 4]),
    Gradient { stops: Vec<(f32, [f32; 4])>, cx: f32, cy: f32, ux: f32, uy: f32, len: f32 },
}

impl Paint {
    pub fn of(fill: &Fill, b: [f32; 4]) -> Paint {
        match &fill.gradient {
            Some(g) if g.stops.iter().any(|s| s.pos.is_finite()) => {
                let a = finite(g.angle).to_radians();
                let (ux, uy) = (a.cos(), a.sin());
                let len = ((b[2] - b[0]) * ux).abs() + ((b[3] - b[1]) * uy).abs();
                let mut stops: Vec<(f32, [f32; 4])> = g.stops.iter().filter(|s| s.pos.is_finite()).map(|s| (s.pos.clamp(0.0, 1.0), premul(s.colour, 1.0))).collect();
                stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                Paint::Gradient { stops, cx: (b[0] + b[2]) / 2.0, cy: (b[1] + b[3]) / 2.0, ux, uy, len: len.max(1e-3) }
            }
            _ => Paint::Solid(premul(fill.colour, 1.0)),
        }
    }

    pub fn at(&self, x: usize, y: usize) -> [f32; 4] {
        match self {
            Paint::Solid(c) => *c,
            Paint::Gradient { stops, cx, cy, ux, uy, len } => {
                let t = (0.5 + ((x as f32 + 0.5 - cx) * ux + (y as f32 + 0.5 - cy) * uy) / len).clamp(0.0, 1.0);
                let first = stops[0];
                let last = stops[stops.len() - 1];
                if t <= first.0 {
                    return first.1;
                }
                if t >= last.0 {
                    return last.1;
                }
                let i = stops.iter().rposition(|s| s.0 <= t).unwrap_or(0);
                let (a, b) = (stops[i], stops[(i + 1).min(stops.len() - 1)]);
                let f = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 1.0 };
                [0, 1, 2, 3].map(|c| a.1[c] + (b.1[c] - a.1[c]) * f)
            }
        }
    }
}

/// The camera distance of a tilt: 2.5 × the text block's larger side
/// (the block's, not the bounds': a shadow or a plaque never changes how
/// strongly the text recedes).
pub(crate) fn camera_distance(laid: &crate::layout::Laid) -> f32 {
    2.5 * laid.width.max(laid.height).max(1.0)
}

/// The tilt as a homography of centred canvas px: rotate the flat text
/// about its vertical axis by `tilt_y` (the right side recedes) and its
/// horizontal axis by `tilt_x` (the top recedes), each clamped to
/// ±[`MAX_TILT`], then project from distance `d`: `[X·w, Y·w, w] = M ·
/// [x, y, 1]`. The centre stays where it is.
pub(crate) fn homography(p: Perspective, d: f32) -> [[f32; 3]; 3] {
    let c = |deg: f32| finite(deg).clamp(-MAX_TILT, MAX_TILT).to_radians();
    let (tx, ty) = (c(p.tilt_x), c(p.tilt_y));
    let d = if d.is_finite() && d > 0.0 { d } else { 1.0 };
    [[ty.cos(), 0.0, 0.0], [0.0, tx.cos(), 0.0], [ty.sin() / d, -tx.sin() / d, 1.0]]
}

/// The nearest a point of a tilted picture may come to the camera within
/// its bounds, as its `w` (1 on the picture's plane, 0 at the camera):
/// the bounds are cut there, so they stay within `1 / W_NEAR` = 4× the
/// flat ones however steep the tilt and however far a shadow or a plaque
/// reaches (TX-R5); what is nearer falls outside the raster.
const W_NEAR: f32 = 0.25;

/// `M · [x, y, 1]`.
fn apply(m: &[[f32; 3]; 3], x: f32, y: f32) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r][0] * x + m[r][1] * y + m[r][2])
}

/// A point's image through the homography `m`; `None` behind the camera
/// (`w ≤ 0`). The inverse of a tilt is a homography too: through it an
/// image maps back to its flat point (`w` is then 1 / the flat point's).
pub(crate) fn project(m: &[[f32; 3]; 3], x: f32, y: f32) -> Option<(f32, f32)> {
    let [xw, yw, w] = apply(m, x, y);
    (w > 1e-6).then(|| (xw / w, yw / w))
}

fn invert(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let k = 1.0 / det;
    Some([
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ])
}

/// The tilted picture's bounds: the flat bounds as a quadrilateral, cut
/// where it comes nearer than [`W_NEAR`], projected; grown by a pixel.
/// They hold the centre and stay within `1 / W_NEAR` of the flat bounds'
/// reach from it.
pub(crate) fn project_bounds(flat: Bounds, p: Perspective, d: f32) -> Bounds {
    let m = homography(p, d);
    let corners = [(flat.x0, flat.y0), (flat.x1, flat.y0), (flat.x1, flat.y1), (flat.x0, flat.y1)];
    let reach = corners.iter().fold(0.0f32, |r, &(x, y)| r.max(x.abs()).max(y.abs())) / W_NEAR;
    let mut out: Option<Bounds> = None;
    let mut add = |x: f32, y: f32| {
        let [xw, yw, w] = apply(&m, x, y);
        let w = w.max(W_NEAR);
        let (px, py) = ((xw / w).clamp(-reach, reach), (yw / w).clamp(-reach, reach));
        if px.is_finite() && py.is_finite() {
            let pt = Bounds { x0: px, y0: py, x1: px, y1: py };
            out = Some(out.map_or(pt, |b| b.union(pt)));
        }
    };
    // The quadrilateral clipped to `w ≥ W_NEAR` (one plane of Sutherland–Hodgman).
    let near = |(x, y): (f32, f32)| apply(&m, x, y)[2] - W_NEAR;
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        let (fa, fb) = (near(a), near(b));
        if fa >= 0.0 {
            add(a.0, a.1);
        }
        if (fa >= 0.0) != (fb >= 0.0) {
            let t = fa / (fa - fb);
            add(a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
        }
    }
    out.unwrap_or(flat).grow(1.0)
}

/// The flat raster `src` (it fills `flat`, TX-R3) seen tilted, written to
/// every pixel of `dst` (it fills `out`): each output pixel centre maps
/// back through the inverse homography and is sampled bilinearly
/// (premultiplied); a point behind the camera has nothing.
pub(crate) fn warp(src: &Canvas, flat: Bounds, out: Bounds, p: Perspective, d: f32, dst: &mut CpuFrame) {
    let per_axis = |n: usize, extent: f32| if extent > 0.0 { n as f32 / extent } else { 1.0 };
    let (sfx, sfy) = (per_axis(src.w, flat.width()), per_axis(src.h, flat.height()));
    let (w, h) = (dst.width as usize, dst.height as usize);
    let (sox, soy) = (per_axis(w, out.width()), per_axis(h, out.height()));
    let inv = invert(&homography(p, d));
    let texel = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || y < 0 || x >= src.w as i64 || y >= src.h as i64 { [0.0; 4] } else { src.px[y as usize * src.w + x as usize] }
    };
    for y in 0..h {
        let row = dst.row_mut(y as u32);
        for x in 0..w {
            let (cx, cy) = (out.x0 + (x as f32 + 0.5) / sox, out.y0 + (y as f32 + 0.5) / soy);
            let px = match inv.as_ref().and_then(|inv| project(inv, cx, cy)) {
                Some((fx, fy)) => {
                    let (u, v) = ((fx - flat.x0) * sfx - 0.5, (fy - flat.y0) * sfy - 0.5);
                    if u.is_finite() && v.is_finite() && u > -2.0 && v > -2.0 && u < src.w as f32 + 1.0 && v < src.h as f32 + 1.0 {
                        let (i, j) = (u.floor() as i64, v.floor() as i64);
                        let (a, b) = (u - i as f32, v - j as f32);
                        let (p00, p10, p01, p11) = (texel(i, j), texel(i + 1, j), texel(i, j + 1), texel(i + 1, j + 1));
                        [0, 1, 2, 3].map(|c| p00[c] * (1.0 - a) * (1.0 - b) + p10[c] * a * (1.0 - b) + p01[c] * (1.0 - a) * b + p11[c] * a * b)
                    } else {
                        [0.0; 4]
                    }
                }
                _ => [0.0; 4],
            };
            row[x * 4..x * 4 + 4].copy_from_slice(&rgba8(px));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Bounds;
    use crate::raster::Mask;
    use kadr_project::{Fill, GradientFill, GradientStop, Perspective, Rgba8};

    #[test]
    fn blur_spreads_and_keeps_the_mass() {
        let mut m = Mask::new(41, 41);
        m.a[20 * 41 + 20] = 1.0;
        blur(&mut m, 4.0);
        let total: f32 = m.a.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "{total}");
        assert!(m.a[20 * 41 + 20] < 0.1 && m.a[20 * 41 + 26] > 0.0, "spread over the radius");
        let mut none = Mask::new(5, 5);
        none.a[12] = 1.0;
        blur(&mut none, 0.2);
        assert_eq!(none.a[12], 1.0, "under half a pixel: untouched");
    }

    #[test]
    fn a_gradient_runs_across_the_box_at_its_angle() {
        let red = Rgba8::rgba(255, 0, 0, 255);
        let blue = Rgba8::rgba(0, 0, 255, 255);
        let fill = Fill { colour: red, gradient: Some(GradientFill::new(vec![GradientStop { pos: 0.0, colour: red }, GradientStop { pos: 1.0, colour: blue }], 0.0)), extra: Default::default() };
        let p = Paint::of(&fill, [0.0, 0.0, 100.0, 10.0]);
        assert!((p.at(0, 5)[0] - 0.995).abs() < 1e-4, "{:?}", p.at(0, 5));
        assert!((p.at(49, 5)[2] - 0.5).abs() < 0.02 && (p.at(99, 5)[2] - 0.995).abs() < 1e-3);
        let down = Fill { gradient: Some(GradientFill { angle: 90.0, ..fill.gradient.clone().unwrap() }), ..fill.clone() };
        assert!(Paint::of(&down, [0.0, 0.0, 100.0, 10.0]).at(50, 9)[2] > 0.9, "90°: top to bottom");
        assert_eq!(Paint::of(&Fill::solid(red), [0.0; 4]).at(3, 3), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_tilt_recedes_the_named_side() {
        let none = homography(Perspective { tilt_x: 0.0, tilt_y: 0.0 }, 1000.0);
        assert_eq!(project(&none, 100.0, -50.0), Some((100.0, -50.0)));
        let top_back = homography(Perspective { tilt_x: 30.0, tilt_y: 0.0 }, 1000.0);
        let (top, bottom) = (project(&top_back, 100.0, -100.0).unwrap(), project(&top_back, 100.0, 100.0).unwrap());
        assert!(top.0 < bottom.0, "the top edge is narrower: {top:?} {bottom:?}");
        let right_back = homography(Perspective { tilt_x: 0.0, tilt_y: 30.0 }, 1000.0);
        assert!(project(&right_back, 100.0, -100.0).unwrap().1 > -100.0, "the right side shrinks towards the middle");
        let b = project_bounds(Bounds { x0: -100.0, y0: -50.0, x1: 100.0, y1: 50.0 }, Perspective { tilt_x: 30.0, tilt_y: 0.0 }, 1000.0);
        assert!(b.width() > 200.0 && b.height() < 100.0, "the near edge widens, the height shrinks: {b:?}");
    }
}
