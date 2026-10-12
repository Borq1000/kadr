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

/// A Gaussian blur of σ = `r` raster px that spreads by `r` from half a
/// pixel up (Task 9 review, Minor 4), so a title blurs alike at every
/// output scale: under [`TAPS_BELOW`] a sampled Gaussian whose taps spread
/// by exactly `r`; from there three extended box blurs (Gwosdek et al.
/// 2011: a box plus a fraction of the next pixel on each side) whose
/// spreads add up to exactly `r`, at a box blur's cost whatever `r`.
/// Edges read as transparent; under half a pixel nothing happens. Rows
/// are filtered one by one, columns [`BAND`] at a time row by row
/// (Minor 5: row-major, no strided walk down each column); the passes of
/// an axis run back to back (the two axes' filters commute).
pub(crate) fn blur(m: &mut Mask, r: f32) {
    if !r.is_finite() || r < 0.5 {
        return;
    }
    let (w, h) = (m.w, m.h);
    // A box wider than the mask averages it whole: no wider box is needed.
    let r = r.min(w.max(h) as f32);
    let passes: Vec<Pass> = if r < TAPS_BELOW { vec![Pass::Taps(gaussian_taps(r))] } else { vec![extended_box(r / 3f32.sqrt()); 3] };
    // Rows: each a line of one lane.
    let (mut a, mut b) = (vec![0.0f32; w], vec![0.0f32; w]);
    for row in m.a.chunks_exact_mut(w) {
        a.copy_from_slice(row);
        for p in &passes {
            p.run(&a, &mut b, 1);
            std::mem::swap(&mut a, &mut b);
        }
        row.copy_from_slice(&a);
    }
    // Columns: a band of up to BAND of them at a time, gathered row by row.
    let (mut a, mut b) = (vec![0.0f32; h * BAND.min(w)], vec![0.0f32; h * BAND.min(w)]);
    for x0 in (0..w).step_by(BAND) {
        let lanes = BAND.min(w - x0);
        let (a, b) = (&mut a[..h * lanes], &mut b[..h * lanes]);
        for (y, dst) in a.chunks_exact_mut(lanes).enumerate() {
            dst.copy_from_slice(&m.a[y * w + x0..y * w + x0 + lanes]);
        }
        let mut swapped = false;
        for p in &passes {
            if swapped { p.run(b, a, lanes) } else { p.run(a, b, lanes) }
            swapped = !swapped;
        }
        let out = if swapped { &*b } else { &*a };
        for (y, src) in out.chunks_exact(lanes).enumerate() {
            m.a[y * w + x0..y * w + x0 + lanes].copy_from_slice(src);
        }
    }
}

/// Under this σ (raster px) the blur is a sampled Gaussian, from it three
/// extended boxes. Where a scale-1 and a scale-2 raster of one title
/// straddle it (σ 0.75–1.5 at scale 1) the kernels differ in shape; there
/// a pixel grid is too coarse for either to match the finer raster anyway.
const TAPS_BELOW: f32 = 1.5;
/// Columns blurred together (row-major within the band).
const BAND: usize = 32;

/// One pass of a blur along a line: an extended box — the `2l + 1` steps
/// about a step and `alpha` (0..1) of the next one on each side, over
/// their weight `2l + 1 + 2·alpha` — or symmetric taps (the centre's
/// first).
#[derive(Clone)]
enum Pass {
    Box { l: usize, alpha: f32 },
    Taps(Vec<f32>),
}

impl Pass {
    /// The pass along a line of `src.len() / lanes` steps, each step
    /// `lanes` values side by side (lane `c` of step `i` at `i·lanes + c`),
    /// into `dst`; zero outside the line.
    fn run(&self, src: &[f32], dst: &mut [f32], lanes: usize) {
        let n = src.len() / lanes;
        let step = |i: usize| &src[i * lanes..(i + 1) * lanes];
        match self {
            Pass::Box { l, alpha } => {
                let (l, alpha) = (*l, *alpha);
                let k = 1.0 / (2.0 * l as f32 + 1.0 + 2.0 * alpha);
                // The running sum of each lane over the steps within `l` of `i`.
                let mut sum = vec![0.0f32; lanes];
                for i in 0..=l.min(n - 1) {
                    sum.iter_mut().zip(step(i)).for_each(|(s, v)| *s += v);
                }
                for i in 0..n {
                    let out = &mut dst[i * lanes..(i + 1) * lanes];
                    out.iter_mut().zip(&sum).for_each(|(d, s)| *d = s * k);
                    if alpha > 0.0 {
                        let ka = k * alpha;
                        for s in [i.checked_sub(l + 1), Some(i + l + 1).filter(|&s| s < n)].into_iter().flatten() {
                            out.iter_mut().zip(step(s)).for_each(|(d, v)| *d += v * ka);
                        }
                    }
                    if i + l + 1 < n {
                        sum.iter_mut().zip(step(i + l + 1)).for_each(|(s, v)| *s += v);
                    }
                    if i >= l {
                        sum.iter_mut().zip(step(i - l)).for_each(|(s, v)| *s -= v);
                    }
                }
            }
            Pass::Taps(taps) => {
                for i in 0..n {
                    let out = &mut dst[i * lanes..(i + 1) * lanes];
                    out.iter_mut().zip(step(i)).for_each(|(d, v)| *d = v * taps[0]);
                    for (j, &t) in taps.iter().enumerate().skip(1) {
                        for s in [i.checked_sub(j), Some(i + j).filter(|&s| s < n)].into_iter().flatten() {
                            out.iter_mut().zip(step(s)).for_each(|(d, v)| *d += v * t);
                        }
                    }
                }
            }
        }
    }
}

/// The taps (centre first, then ±1, ±2, …) of a sampled Gaussian whose
/// spread — the taps' standard deviation — is `r` (0.5 ≤ `r` <
/// [`TAPS_BELOW`]): the sampled curve is narrower than its σ below a pixel
/// or so, so σ is tuned until the taps spread by `r`.
fn gaussian_taps(r: f32) -> Vec<f32> {
    let sampled = |s: f32| {
        let n = (3.0 * s).ceil() as usize + 1;
        let mut t: Vec<f32> = (0..=n).map(|i| (-((i * i) as f32) / (2.0 * s * s)).exp()).collect();
        let total = t[0] + 2.0 * t[1..].iter().sum::<f32>();
        t.iter_mut().for_each(|v| *v /= total);
        t
    };
    let spread = |t: &[f32]| (2.0 * t.iter().enumerate().map(|(i, v)| (i * i) as f32 * v).sum::<f32>()).sqrt();
    let mut s = r;
    for _ in 0..6 {
        s *= r / spread(&sampled(s)).max(1e-6);
    }
    sampled(s)
}

/// The extended box that spreads by exactly `s` (Gwosdek, Grewenig, Bruhn
/// and Weickert 2011): the widest whole box `2l + 1` that spreads by at
/// most `s` (a box of width `w` spreads by `√((w² − 1) / 12)`), and the
/// fraction `alpha` of the next step on each side that makes up the rest.
fn extended_box(s: f32) -> Pass {
    let v = s * s;
    let l = (0.5 * (12.0 * v + 1.0).sqrt() - 0.5).floor().max(0.0);
    let alpha = (2.0 * l + 1.0) * (v - l * (l + 1.0) / 3.0) / (2.0 * ((l + 1.0) * (l + 1.0) - v));
    Pass::Box { l: l as usize, alpha: alpha.clamp(0.0, 1.0) }
}

/// The paint of a fill at a raster pixel: its colour, or its gradient
/// across the text block `b` (raster px `[x0, y0, x1, y1]`) at its angle
/// (0° left → right, 90° top → bottom), stops mixed premultiplied. Not
/// dithered: a gentle gradient over a tall block may band in 8 bits.
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
/// [x, y, 1]`. The centre stays where it is. Both axes at once are the
/// two turns side by side (each foreshortens its own axis, both recede),
/// not one true 3D rotation — no shear between the axes; close enough at
/// the tilts titles use.
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
/// (premultiplied); a point behind the camera has nothing. One sample per
/// pixel: where a steep tilt shrinks the far side more than 2× it can
/// alias (Task 9 review, Minor 7: supersampling there would cost 4× the
/// warp — measured and decided in Task 14).
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

    /// The spread of a blurred impulse along x and y, raster px.
    fn spread(m: &Mask) -> (f32, f32) {
        let (cx, cy) = ((m.w / 2) as f32, (m.h / 2) as f32);
        let total: f32 = m.a.iter().sum();
        let (mut vx, mut vy) = (0.0, 0.0);
        for (i, a) in m.a.iter().enumerate() {
            let (x, y) = ((i % m.w) as f32, (i / m.w) as f32);
            (vx, vy) = (vx + (x - cx).powi(2) * a, vy + (y - cy).powi(2) * a);
        }
        ((vx / total).sqrt(), (vy / total).sqrt())
    }

    /// Minor 4: the blur spreads by the asked σ within ±10 % from half a
    /// raster px up — small radii (a shadow at a small preview) included.
    #[test]
    fn the_blur_spreads_by_the_asked_sigma() {
        for r in [0.5, 0.6, 0.75, 1.0, 1.3, 1.5, 2.0, 2.5, 2.99, 3.0, 3.5, 4.0, 5.0, 6.3, 8.0, 10.0, 17.0, 25.0, 60.0] {
            let n = (8.0 * r) as usize * 2 + 21;
            let mut m = Mask::new(n, n);
            m.a[(n / 2) * n + n / 2] = 1.0;
            blur(&mut m, r);
            let (sx, sy) = spread(&m);
            assert!((sx / r - 1.0).abs() <= 0.1 && (sy / r - 1.0).abs() <= 0.1, "σ {r}: spread {sx} × {sy}");
            assert!((m.a.iter().sum::<f32>() - 1.0).abs() < 1e-3, "σ {r}: the mass is kept");
        }
    }

    /// Minor 5: columns (blurred a band at a time) and rows are the same
    /// filter — a mask's blur is its transpose's blur transposed, across
    /// band edges and with a ragged last band.
    #[test]
    fn columns_blur_like_rows() {
        let (w, h) = (75, 53);
        let mut m = Mask::new(w, h);
        for (i, a) in m.a.iter_mut().enumerate() {
            *a = ((i * 7919 + 13) % 101) as f32 / 100.0 * (((i % w) / 9 + (i / w) / 5) % 2) as f32;
        }
        let mut t = Mask::new(h, w);
        for y in 0..h {
            for x in 0..w {
                t.a[x * h + y] = m.a[y * w + x];
            }
        }
        for r in [1.2, 4.5] {
            let (mut a, mut b) = (m.clone(), t.clone());
            blur(&mut a, r);
            blur(&mut b, r);
            for y in 0..h {
                for x in 0..w {
                    assert!((a.a[y * w + x] - b.a[x * h + y]).abs() < 1e-5, "σ {r} at ({x}, {y})");
                }
            }
        }
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
