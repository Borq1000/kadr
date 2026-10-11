//! The colour primitives of effects E2 and the generators of E3 (effects spec §4; formulas: the
//! effects system draft §3 «Примитивы цвета», exact rules in the crate docs,
//! "Point operations"): plain data, plus pure helpers that are the
//! normative formulas — the test reference calls them and the CPU renderer
//! matches them within ±1 LSB (so will a GPU one: the tone table becomes its
//! 1D texture, the grain hash its integer shader code).

/// Rec.709 luma weights of non-linear R'G'B'.
pub const LUMA709: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// `Y(c)`: the Rec.709 luma of non-linear R'G'B'.
pub fn luma709(c: [f64; 3]) -> f64 {
    LUMA709[0] * c[0] + LUMA709[1] * c[1] + LUMA709[2] * c[2]
}

/// `t²·(3 − 2t)` with `t = clamp((x − e0) / (e1 − e0), 0, 1)`; `e1 > e0`.
pub fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One tone curve for R', G' and B' (E2 has no per-channel curves, D11).
#[derive(Clone, Debug, PartialEq)]
pub struct ToneCurve {
    /// Control points `(x, y)`: a recipe gives 2–5 in [0, 1], x strictly
    /// increasing, y non-decreasing. Any list is accepted as given (more
    /// than 5 points included); a renderer uses [`ToneCurve::usable`], which
    /// cleans up the rest.
    pub points: Vec<(f32, f32)>,
}

impl ToneCurve {
    pub fn identity() -> ToneCurve {
        ToneCurve { points: vec![(0.0, 0.0), (1.0, 1.0)] }
    }

    /// The points the curve uses (refinement 8): non-finite ones dropped,
    /// the rest clamped into [0, 1] (−0 counts as 0) and sorted by x; of
    /// points sharing an x the first given; each y at least the one before.
    /// Fewer than two left: the identity's two.
    pub fn usable(&self) -> Vec<(f64, f64)> {
        // `+ 0.0` turns −0 into +0, so the stable sort keeps ±0 in the given order.
        let unit = |v: f32| (v as f64).clamp(0.0, 1.0) + 0.0;
        let mut p: Vec<(f64, f64)> = self
            .points
            .iter()
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|&(x, y)| (unit(x), unit(y)))
            .collect();
        p.sort_by(|a, b| a.0.total_cmp(&b.0));
        p.dedup_by(|later, earlier| later.0 == earlier.0);
        for i in 1..p.len() {
            p[i].1 = p[i].1.max(p[i - 1].1);
        }
        if p.len() < 2 { vec![(0.0, 0.0), (1.0, 1.0)] } else { p }
    }

    /// Whether the curve is the identity on its domain [0, 1]: its usable
    /// points run from (0, 0) to (1, 1) on the diagonal (the curve is
    /// constant beyond its first and last points, so one that starts later is
    /// not). A neutral curve is skipped wherever it stands in a chain, so it
    /// does not clamp its input; a chain of only neutral primitives is
    /// skipped entirely, its final clamp included (crate docs, "Point
    /// operations").
    pub fn is_neutral(&self) -> bool {
        let p = self.usable();
        p.first() == Some(&(0.0, 0.0)) && p.last() == Some(&(1.0, 1.0)) && p.iter().all(|(x, y)| x == y)
    }

    /// The normative 256-entry table: the monotone cubic Hermite spline of
    /// Fritsch and Carlson through [`ToneCurve::usable`], evaluated in f64 at
    /// `x = i/255` and stored as f32. Secants `d_k`; tangents `m_0 = d_0`,
    /// `m_{n−1} = d_{n−2}`, inside `m_k = 0` where `d_{k−1}·d_k ≤ 0`, else
    /// `(d_{k−1} + d_k)/2`; then for `k = 0 … n−2` in order: `d_k = 0` sets
    /// `m_k = m_{k+1} = 0`, else with `α = m_k/d_k`, `β = m_{k+1}/d_k`,
    /// `α² + β² > 9` scales both by `3/√(α² + β²)`. Below the first point the
    /// curve is its y, above the last point the last y.
    pub fn table(&self) -> [f32; 256] {
        let p = self.usable();
        let n = p.len();
        let d: Vec<f64> = (0..n - 1).map(|k| (p[k + 1].1 - p[k].1) / (p[k + 1].0 - p[k].0)).collect();
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { (d[k - 1] + d[k]) / 2.0 };
        }
        for k in 0..n - 1 {
            if d[k] == 0.0 {
                m[k] = 0.0;
                m[k + 1] = 0.0;
            } else {
                let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
                let s = a * a + b * b;
                if s > 9.0 {
                    let tau = 3.0 / s.sqrt();
                    m[k] = tau * a * d[k];
                    m[k + 1] = tau * b * d[k];
                }
            }
        }
        std::array::from_fn(|i| {
            let x = i as f64 / 255.0;
            let v = if x <= p[0].0 {
                p[0].1
            } else if x >= p[n - 1].0 {
                p[n - 1].1
            } else {
                let k = (0..n - 1).find(|&k| x < p[k + 1].0).unwrap_or(n - 2);
                let h = p[k + 1].0 - p[k].0;
                let t = (x - p[k].0) / h;
                let (t2, t3) = (t * t, t * t * t);
                (2.0 * t3 - 3.0 * t2 + 1.0) * p[k].1 + (t3 - 2.0 * t2 + t) * h * m[k] + (-2.0 * t3 + 3.0 * t2) * p[k + 1].1 + (t3 - t2) * h * m[k + 1]
            };
            v.clamp(0.0, 1.0) as f32
        })
    }
}

/// One channel through a tone table: `v` clamped into [0, 1] — the
/// curve's domain, the one clamp inside a chain, done by a non-neutral curve
/// only (a neutral one is skipped) — `× 255`, then linear between the two
/// neighbouring entries. NaN reads as 0.
#[inline]
pub fn tone_lookup(table: &[f32; 256], v: f32) -> f32 {
    let x = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) } * 255.0;
    let i = (x as usize).min(254);
    table[i] + (table[i + 1] - table[i]) * (x - i as f32)
}

/// Tints the shadows and the highlights by two hues without changing luma.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitTone {
    /// Degrees, any value (taken mod 360).
    pub shadow_hue_deg: f32,
    /// 0..1; 0 = no tint.
    pub shadow_amount: f32,
    pub highlight_hue_deg: f32,
    pub highlight_amount: f32,
    /// −1..1: positive gives more of the picture to the highlights' tint.
    pub balance: f32,
}

impl SplitTone {
    pub const NEUTRAL: SplitTone = SplitTone { shadow_hue_deg: 0.0, shadow_amount: 0.0, highlight_hue_deg: 0.0, highlight_amount: 0.0, balance: 0.0 };

    pub fn is_neutral(&self) -> bool {
        self.shadow_amount == 0.0 && self.highlight_amount == 0.0
    }
}

/// `d(h)` of the split tone: the hue `h` (degrees, taken mod 360) at full
/// saturation and value, `hsv(h, 1, 1)`, minus its own Rec.709 luma — a
/// pure chroma shift.
pub fn hue_offset(hue_deg: f64) -> [f64; 3] {
    let h = hue_deg.rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let rgb = match h as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    };
    let y = luma709(rgb);
    rgb.map(|v| v - y)
}

/// Darkens towards the canvas corners: a circle in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vignette {
    /// 0..1: the darkening at full distance.
    pub amount: f32,
    /// Where the darkening starts, as a [`Vignette::distance`].
    pub midpoint: f32,
    /// Width of the soft edge in distance units (at least 0.01).
    pub feather: f32,
}

impl Vignette {
    pub fn is_neutral(&self) -> bool {
        self.amount == 0.0
    }

    /// `d` of canvas point `(x, y)`: its distance from the canvas centre ÷
    /// half the canvas diagonal — 0 in the centre, 1 in the corners.
    pub fn distance(canvas_w: f64, canvas_h: f64, x: f64, y: f64) -> f64 {
        let (dx, dy) = (x - canvas_w / 2.0, y - canvas_h / 2.0);
        (dx * dx + dy * dy).sqrt() / ((canvas_w * canvas_w + canvas_h * canvas_h).sqrt() / 2.0)
    }

    /// `v = 1 − amount · smoothstep(midpoint, midpoint + max(feather, 0.01), d)`.
    pub fn gain(&self, d: f64) -> f64 {
        let e0 = self.midpoint as f64;
        1.0 - self.amount as f64 * smoothstep(e0, e0 + (self.feather as f64).max(0.01), d)
    }
}

/// Monochrome film grain on canvas-pixel cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grain {
    /// Peak-to-peak strength at mid-grey (0.05 is visible film grain).
    pub amount: f32,
    /// Side of a grain cell in canvas pixels.
    pub size: f32,
    /// The recipe varies it per frame.
    pub seed: u64,
}

impl Grain {
    pub fn is_neutral(&self) -> bool {
        self.amount == 0.0
    }

    /// The cell side in canvas pixels: `size`, but at least 1 (a size below
    /// 1 or not finite counts as 1).
    pub fn cell_size(&self) -> f64 {
        let s = self.size as f64;
        if s.is_finite() && s >= 1.0 { s } else { 1.0 }
    }

    /// The hash index of the cell of side `size` holding canvas point
    /// `(x, y)`: `floor(y/size) << 32 | floor(x/size)`, each taken as 32 bits.
    #[inline]
    pub fn cell_index(size: f64, x: f64, y: f64) -> u64 {
        // Truncation is floor for q ≥ 0 — the whole canvas — and needs no libm call.
        let cell = |v: f64| {
            let q = v / size;
            (if q >= 0.0 { q as i64 } else { q.floor() as i64 }) as u32 as u64
        };
        (cell(y) << 32) | cell(x)
    }

    /// `h` of canvas point `(x, y)`: `pcg32_hash(seed, cell index) / (2³² − 1)`, in [0, 1].
    pub fn noise(&self, x: f64, y: f64) -> f64 {
        kadr_core::hash::pcg32_hash(self.seed, Self::cell_index(self.cell_size(), x, y)) as f64 / u32::MAX as f64
    }
}

/// The grain's weight `4·Y·(1 − Y)` with `Y` clamped into [0, 1]: no grain
/// in pure black or white.
pub fn grain_weight(y: f64) -> f64 {
    let y = y.clamp(0.0, 1.0);
    4.0 * y * (1.0 - y)
}

// ------------------------------------------------------------ generators (E3)

/// One colour of a [`Gradient`]: where it sits along the gradient (0..1)
/// and its straight colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub pos: f32,
    pub colour: crate::Rgba,
}

/// How a gradient runs in its layer's local pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientShape {
    /// Straight bands across the unit vector `u = (cos angle, sin angle)`
    /// (y down): `t = (dot(p, u) − offset) / length`.
    Linear { angle: f32, offset: f32, length: f32 },
    /// Circles about `centre`: `t = |p − centre| / radius`.
    Radial { centre: crate::Vec2, radius: f32 },
}

/// A smooth colour generator (effects spec §4 «Gradient»): a recipe gives
/// 2–3 stops along a linear or radial shape. Exact rules: crate docs,
/// "Generators".
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub shape: GradientShape,
    /// Any list is accepted as given; a renderer uses [`Gradient::usable_stops`].
    pub stops: Vec<GradientStop>,
}

/// A stop as a gradient uses it: its position in [0, 1] and its colour premultiplied.
pub type UsableStop = (f64, [f64; 4]);

impl Gradient {
    /// `t` of local point `(x, y)`, in [0, 1]: the shape's distance over its
    /// length or radius (below 1e-3, or not finite, it counts as 1e-3); a
    /// NaN `t` (a non-finite geometry) is 0.
    pub fn t(&self, x: f64, y: f64) -> f64 {
        let (d, len) = match self.shape {
            GradientShape::Linear { angle, offset, length } => {
                let (s, c) = (angle as f64).sin_cos();
                (x * c + y * s - offset as f64, length as f64)
            }
            GradientShape::Radial { centre, radius } => ((x - centre.x as f64).hypot(y - centre.y as f64), radius as f64),
        };
        let t = d / len.max(1e-3);
        if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
    }

    /// The stops in use: non-finite positions dropped, the rest clamped
    /// into [0, 1] (−0 counts as 0) and stably sorted by position — stops
    /// sharing one are a hard edge, in the given order; each colour's
    /// straight components clamped into [0, 1] (a non-finite one 0) and
    /// premultiplied.
    pub fn usable_stops(&self) -> Vec<UsableStop> {
        let unit = |v: f32| if v.is_finite() { (v as f64).clamp(0.0, 1.0) } else { 0.0 };
        let mut s: Vec<UsableStop> = self
            .stops
            .iter()
            .filter(|s| s.pos.is_finite())
            .map(|s| {
                let a = unit(s.colour.a);
                (unit(s.pos) + 0.0, [unit(s.colour.r) * a, unit(s.colour.g) * a, unit(s.colour.b) * a, a])
            })
            .collect();
        s.sort_by(|a, b| a.0.total_cmp(&b.0));
        s
    }

    /// The premultiplied colour at local point `(x, y)`.
    pub fn at(&self, x: f64, y: f64) -> [f64; 4] {
        stop_colour(&self.usable_stops(), self.t(x, y))
    }
}

/// The colour of usable `stops` at `t`: transparent without stops; at or
/// before the first the first's, at or after the last the last's; else
/// linear, in premultiplied values, between the last stop at or before `t`
/// and the next one (of stops sharing a position the later counts).
pub fn stop_colour(stops: &[UsableStop], t: f64) -> [f64; 4] {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    if t >= last.0 {
        return last.1;
    }
    // first.0 < t < last.0: 1 ≤ k ≤ len − 1, and stops[k].0 > t ≥ stops[k − 1].0.
    let k = stops.partition_point(|s| s.0 <= t);
    let (a, b) = (stops[k - 1], stops[k]);
    let f = (t - a.0) / (b.0 - a.0);
    std::array::from_fn(|c| a.1[c] + (b.1[c] - a.1[c]) * f)
}

/// Soft value noise (effects spec §4 «Noise»: the light leak's texture): a
/// lattice every `cell` local pixels, smoothstep-bilinear between its
/// points, two time slices blended. Exact rules: crate docs, "Generators".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Noise {
    /// Lattice spacing in local pixels (at least 1).
    pub cell: f32,
    pub seed: u64,
    /// Slices `⌊time⌋` and the next one, blended by the fraction.
    pub time: f32,
    /// Straight colour at full value.
    pub colour: crate::Rgba,
    /// 0..1: the coverage at full value.
    pub amount: f32,
    /// Around ½: 1 keeps the values, more makes harder patches.
    pub contrast: f32,
}

impl Noise {
    /// The lattice spacing: `cell`, but at least 1 (below 1 or not finite: 1).
    pub fn cell_size(&self) -> f64 {
        let s = self.cell as f64;
        if s.is_finite() && s >= 1.0 { s } else { 1.0 }
    }

    /// The two slices' seeds and the second's weight: with `n = ⌊time⌋`
    /// (a non-finite time counts as 0), `seed ^ fmix64(n)`,
    /// `seed ^ fmix64(n + 1)` and `smoothstep(0, 1, time − n)`.
    pub fn slices(&self) -> (u64, u64, f64) {
        let time = if self.time.is_finite() { self.time as f64 } else { 0.0 };
        let n = time.floor();
        let slice = |k: f64| self.seed ^ kadr_core::hash::fmix64(k as i64 as u64);
        (slice(n), slice(n + 1.0), smoothstep(0.0, 1.0, time - n))
    }

    /// The hash index of lattice point `(i, j)`: `j << 32 | i`, each taken
    /// as 32 bits (as [`Grain::cell_index`]).
    #[inline]
    pub fn lattice_index(i: i64, j: i64) -> u64 {
        ((j as u32 as u64) << 32) | i as u32 as u64
    }

    /// The value of lattice point `(i, j)`, in [0, 1]: `lerp(h₀, h₁, w)`
    /// with `h = pcg32_hash(slice seed, lattice_index) / (2³² − 1)`.
    pub fn lattice(&self, i: i64, j: i64) -> f64 {
        let (s0, s1, w) = self.slices();
        let idx = Self::lattice_index(i, j);
        let h = |s: u64| kadr_core::hash::pcg32_hash(s, idx) as f64 / u32::MAX as f64;
        let (a, b) = (h(s0), h(s1));
        a + (b - a) * w
    }

    /// The noise at local point `(x, y)`, in [0, 1]: the cell `(i, j) =
    /// (⌊x/s⌋, ⌊y/s⌋)`, its four corners blended by [`smooth_bilerp`] at
    /// `(x/s − i, y/s − j)`. A non-finite coordinate counts as 0.
    pub fn value(&self, x: f64, y: f64) -> f64 {
        let s = self.cell_size();
        let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
        let (gx, gy) = (finite(x / s), finite(y / s));
        let (fi, fj) = (gx.floor(), gy.floor());
        let (i, j) = (fi as i64, fj as i64);
        let (i1, j1) = (i.wrapping_add(1), j.wrapping_add(1));
        smooth_bilerp([self.lattice(i, j), self.lattice(i1, j), self.lattice(i, j1), self.lattice(i1, j1)], gx - fi, gy - fj)
    }

    /// Value `v` as a premultiplied colour: `m = clamp((v − ½)·contrast + ½,
    /// 0, 1) · clamp(amount, 0, 1)` (a non-finite contrast counts as 1, a
    /// non-finite amount as 0) times the straight colour clamped into [0, 1]:
    /// `(r·a·m, g·a·m, b·a·m, a·m)`.
    pub fn shade(&self, v: f64) -> [f64; 4] {
        let unit = |c: f32| if c.is_finite() { (c as f64).clamp(0.0, 1.0) } else { 0.0 };
        let contrast = if self.contrast.is_finite() { self.contrast as f64 } else { 1.0 };
        let shaped = ((v - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
        let m = if shaped.is_nan() { 0.0 } else { shaped } * unit(self.amount);
        let a = unit(self.colour.a) * m;
        [unit(self.colour.r) * a, unit(self.colour.g) * a, unit(self.colour.b) * a, a]
    }

    /// The premultiplied colour at local point `(x, y)`.
    pub fn at(&self, x: f64, y: f64) -> [f64; 4] {
        self.shade(self.value(x, y))
    }
}

/// `lerp(lerp(l00, l10, wx), lerp(l01, l11, wx), wy)` with
/// `wx = smoothstep(0, 1, fx)`, `wy = smoothstep(0, 1, fy)`; `l = [l00, l10, l01, l11]`.
#[inline]
pub fn smooth_bilerp(l: [f64; 4], fx: f64, fy: f64) -> f64 {
    let (wx, wy) = (smoothstep(0.0, 1.0, fx), smoothstep(0.0, 1.0, fy));
    let top = l[0] + (l[1] - l[0]) * wx;
    let bottom = l[2] + (l[3] - l[2]) * wx;
    top + (bottom - top) * wy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorAdjust, Effect, LayerId, Rgba, Vec2};

    fn curve(points: &[(f32, f32)]) -> ToneCurve {
        ToneCurve { points: points.to_vec() }
    }

    #[test]
    fn an_identity_curve_is_neutral_and_its_table_is_the_diagonal() {
        for c in [ToneCurve::identity(), curve(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]), curve(&[(0.4, 0.9)]), curve(&[])] {
            assert!(c.is_neutral(), "{c:?}");
            let t = c.table();
            assert!(t.iter().enumerate().all(|(i, v)| (v - i as f32 / 255.0).abs() < 1e-6), "{c:?}");
        }
        assert!(!curve(&[(0.2, 0.2), (0.8, 0.8)]).is_neutral(), "constant beyond its points: not the identity");
        assert!(!curve(&[(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)]).is_neutral());
    }

    /// The normative table: a GPU's 1D texture of this curve holds these numbers.
    #[test]
    fn the_tone_table_is_pinned() {
        let t = curve(&[(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)]).table();
        for (i, want) in [(0, 0.0), (32, 0.062_745_3), (64, 0.150_982_7), (128, 0.503_137_2), (191, 0.849_017_3), (255, 1.0)] {
            assert!((t[i] - want).abs() < 1e-6, "T[{i}] = {} (want {want})", t[i]);
        }
    }

    /// Fritsch–Carlson: monotone and without overshoot, even through a near-step.
    #[test]
    fn the_spline_never_overshoots() {
        let step = curve(&[(0.0, 0.0), (0.45, 0.05), (0.55, 0.95), (1.0, 1.0)]).table();
        assert!(step.windows(2).all(|w| w[0] <= w[1]), "monotone");
        assert!(step[..115].iter().all(|&v| v <= 0.05 + 1e-6), "below the step: within its two points");
        assert!(step[141..].iter().all(|&v| v >= 0.95 - 1e-6), "above the step: within its two points");
        let flat = curve(&[(0.0, 0.2), (1.0, 0.2)]).table();
        assert!(flat.iter().all(|&v| v == 0.2), "a flat curve stays flat");
    }

    #[test]
    fn unusable_points_are_cleaned_up() {
        let messy = curve(&[(0.9, 0.7), (f32::NAN, 0.5), (0.1, 0.4), (0.1, 0.0), (1.3, 1.2), (0.5, 0.2)]);
        let f = |v: f32| v as f64;
        assert_eq!(messy.usable(), vec![(f(0.1), f(0.4)), (f(0.5), f(0.4)), (f(0.9), f(0.7)), (1.0, 1.0)], "finite, clamped, sorted, first of equal x, rising");
        assert_eq!(curve(&[(0.5, 0.5)]).usable(), vec![(0.0, 0.0), (1.0, 1.0)], "one point: the identity");
        // −0 is 0: of (0, …) and (−0, …) the first given wins, whichever sign comes first.
        for (first, second) in [(0.0f32, -0.0f32), (-0.0, 0.0)] {
            let p = curve(&[(first, 0.3), (second, 0.1), (1.0, 1.0)]).usable();
            assert_eq!(p, vec![(0.0, f(0.3)), (1.0, 1.0)], "{first} then {second}");
            assert!(p[0].0.is_sign_positive(), "{first} then {second}");
        }
        let seven: Vec<(f32, f32)> = (0..7).map(|i| (i as f32 / 6.0, i as f32 / 6.0)).collect();
        assert_eq!(curve(&seven).usable().len(), 7, "more than 5 points are used as given");
        assert!(curve(&seven).is_neutral());
    }

    #[test]
    fn lookup_clamps_its_input_and_interpolates() {
        let t = curve(&[(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)]).table();
        assert_eq!(tone_lookup(&t, -3.0), t[0]);
        assert_eq!(tone_lookup(&t, 7.0), t[255]);
        assert_eq!(tone_lookup(&t, f32::NAN), t[0]);
        assert!((tone_lookup(&t, 64.5 / 255.0) - (t[64] + t[65]) / 2.0).abs() < 1e-6, "half way between two entries");
    }

    #[test]
    fn split_tone_offsets_are_pure_chroma() {
        for h in [0.0, 30.0, 35.0, 120.0, 200.0, 300.0, -60.0, 725.0] {
            assert!(luma709(hue_offset(h)).abs() < 1e-12, "{h}: luma stays");
        }
        let close = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9);
        assert!(close(hue_offset(0.0), [0.7874, -0.2126, -0.2126]));
        assert!(close(hue_offset(200.0), [-0.549, 0.117_666_666_666_666_7, 0.451]));
        let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        assert!(near(hue_offset(90.0), [-0.3215, 0.1785, -0.8215]), "sector 1");
        assert!(near(hue_offset(150.0), [-0.7513, 0.2487, -0.2513]), "sector 2");
        assert!(near(hue_offset(270.0), [0.3215, -0.1785, 0.8215]), "sector 4");
        assert!(near(hue_offset(330.0), [0.7513, -0.2487, 0.2513]), "sector 5");
        assert!(close(hue_offset(-60.0), hue_offset(300.0)) && close(hue_offset(725.0), hue_offset(5.0)), "taken mod 360");
        assert!(SplitTone::NEUTRAL.is_neutral());
        assert!(!SplitTone { highlight_amount: 0.1, ..SplitTone::NEUTRAL }.is_neutral());
    }

    #[test]
    fn the_vignette_is_a_circle_in_canvas_pixels() {
        assert_eq!(Vignette::distance(1920.0, 1080.0, 960.0, 540.0), 0.0);
        assert!((Vignette::distance(1920.0, 1080.0, 0.0, 0.0) - 1.0).abs() < 1e-12, "the corners are 1");
        assert!((Vignette::distance(1920.0, 1080.0, 0.0, 540.0) - 1920.0 / 1920f64.hypot(1080.0)).abs() < 1e-12, "a side's middle is nearer than a corner");
        let v = Vignette { amount: 0.5, midpoint: 0.2, feather: 0.0 };
        assert_eq!(v.gain(0.1), 1.0);
        assert!((v.gain(0.205) - 0.75).abs() < 1e-6, "a zero feather is 0.01 wide");
        assert_eq!(v.gain(0.5), 0.5);
        assert_eq!(smoothstep(0.0, 1.0, 0.25), 0.15625);
        assert!(Vignette { amount: 0.0, midpoint: 0.3, feather: 0.4 }.is_neutral());
    }

    #[test]
    fn grain_is_pinned_per_canvas_cell() {
        let g = Grain { amount: 1.0, size: 1.0, seed: 7 };
        assert_eq!(g.noise(0.5, 0.5), 1_630_685_630.0 / u32::MAX as f64, "cell (0, 0): index 0");
        assert_eq!(g.noise(1.5, 0.5), 1_910_406_292.0 / u32::MAX as f64, "cell (1, 0): index 1");
        assert_eq!(g.noise(0.5, 1.5), 4_255_874_242.0 / u32::MAX as f64, "cell (0, 1): index 2³²");
        let big = Grain { size: 2.0, ..g };
        assert_eq!(big.noise(0.5, 0.5), big.noise(1.9, 1.9), "one 2 × 2 cell");
        assert_ne!(big.noise(0.5, 0.5), big.noise(2.1, 0.5));
        for bad in [0.3, 0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(Grain { size: bad, ..g }.cell_size(), 1.0, "{bad}");
        }
        assert_eq!(Grain::cell_index(1.0, 3.5, 2.5), (2 << 32) | 3);
        assert_eq!(Grain::cell_index(1.0, -0.5, 0.0), 0xFFFF_FFFF, "left of the canvas: cell −1, taken as 32 bits");
        assert_eq!([grain_weight(0.5), grain_weight(0.0), grain_weight(1.0), grain_weight(1.7), grain_weight(-1.0)], [1.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(grain_weight(0.25), 0.75);
        assert!(!g.is_neutral() && Grain { amount: 0.0, ..g }.is_neutral());
    }

    #[test]
    fn effects_know_whether_they_change_anything_or_read_a_position() {
        let none = [
            Effect::ColorAdjust(ColorAdjust::NEUTRAL),
            Effect::ToneCurve(ToneCurve::identity()),
            Effect::SplitTone(SplitTone::NEUTRAL),
            Effect::Vignette(Vignette { amount: 0.0, midpoint: 0.5, feather: 0.5 }),
            Effect::Grain(Grain { amount: 0.0, size: 2.0, seed: 1 }),
        ];
        assert!(none.iter().all(Effect::is_neutral));
        assert_eq!(none.iter().map(Effect::is_spatial).collect::<Vec<_>>(), [false, false, false, true, true]);
        assert!(!Effect::Vignette(Vignette { amount: 0.3, midpoint: 0.5, feather: 0.5 }).is_neutral());
    }

    fn stop(pos: f32, r: f32, g: f32, b: f32, a: f32) -> GradientStop {
        GradientStop { pos, colour: Rgba { r, g, b, a } }
    }

    #[test]
    fn a_gradient_maps_its_shape_to_t_and_mixes_premultiplied_stops() {
        let lin = Gradient { shape: GradientShape::Linear { angle: 0.0, offset: 10.0, length: 20.0 }, stops: vec![stop(0.0, 1.0, 0.0, 0.0, 1.0), stop(1.0, 0.0, 0.0, 1.0, 0.5)] };
        assert_eq!(lin.t(5.0, 99.0), 0.0, "before its start: the first stop");
        assert_eq!(lin.t(20.0, -3.0), 0.5, "only the axis counts");
        assert_eq!(lin.t(40.0, 0.0), 1.0);
        let mid = lin.at(20.0, 0.0);
        // lerp((1, 0, 0, 1), (0, 0, 0.5, 0.5), ½): premultiplied, so the blue stop's alpha halves its blue.
        assert!(mid.iter().zip([0.5, 0.0, 0.25, 0.75]).all(|(a, b)| (a - b).abs() < 1e-12), "{mid:?}");
        let down = Gradient { shape: GradientShape::Linear { angle: std::f32::consts::FRAC_PI_2, offset: 0.0, length: 10.0 }, ..lin.clone() };
        assert!((down.t(123.0, 5.0) - 0.5).abs() < 1e-6, "π/2 runs down the y axis (y points down)");
        let radial = Gradient { shape: GradientShape::Radial { centre: Vec2::new(10.0, 10.0), radius: 5.0 }, stops: lin.stops.clone() };
        assert_eq!((radial.t(10.0, 10.0), radial.t(13.0, 14.0), radial.t(30.0, 10.0)), (0.0, 1.0, 1.0));
        let flat = Gradient { shape: GradientShape::Linear { angle: 0.0, offset: 0.0, length: 0.0 }, stops: vec![] };
        assert_eq!(flat.t(0.0005, 0.0), 0.5, "a length under 1e-3 counts as 1e-3");
        assert_eq!(Gradient { shape: GradientShape::Radial { centre: Vec2::new(f32::NAN, 0.0), radius: 1.0 }, stops: vec![] }.t(1.0, 1.0), 0.0, "a NaN t is 0");
    }

    #[test]
    fn gradient_stops_are_cleaned_up_and_a_shared_position_is_a_hard_edge() {
        let g = Gradient {
            shape: GradientShape::Radial { centre: Vec2::new(0.0, 0.0), radius: 1.0 },
            stops: vec![stop(0.75, 0.0, 1.0, 0.0, 1.0), stop(f32::NAN, 1.0, 1.0, 1.0, 1.0), stop(-2.0, 2.0, -1.0, 0.0, 1.5), stop(0.75, 0.0, 0.0, 1.0, 1.0)],
        };
        let s = g.usable_stops();
        assert_eq!(s, vec![(0.0, [1.0, 0.0, 0.0, 1.0]), (0.75, [0.0, 1.0, 0.0, 1.0]), (0.75, [0.0, 0.0, 1.0, 1.0])], "NaN dropped, clamped, sorted, equal positions kept in order");
        assert_eq!(stop_colour(&s, 0.75), [0.0, 0.0, 1.0, 1.0], "at a shared position: the later stop");
        assert_eq!(stop_colour(&s, 0.9), [0.0, 0.0, 1.0, 1.0], "past the last: the last");
        let half = stop_colour(&s, 0.375);
        assert!(half.iter().zip([0.5, 0.5, 0.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-12), "{half:?}");
        assert_eq!(stop_colour(&[], 0.5), [0.0; 4], "no stops: nothing");
    }

    #[test]
    fn noise_is_its_lattice_at_lattice_points_and_smooth_between() {
        let n = Noise { cell: 8.0, seed: 5, time: 0.0, colour: Rgba { r: 1.0, g: 0.5, b: 0.0, a: 1.0 }, amount: 1.0, contrast: 1.0 };
        let h = |seed: u64, i: i64, j: i64| kadr_core::hash::pcg32_hash(seed, Noise::lattice_index(i, j)) as f64 / u32::MAX as f64;
        assert_eq!(n.lattice(3, -2), h(5 ^ kadr_core::hash::fmix64(0), 3, -2), "time 0: slice 0 alone");
        assert_eq!(Noise::lattice_index(-1, 2), (2 << 32) | 0xFFFF_FFFF, "each coordinate taken as 32 bits");
        assert_eq!(n.value(24.0, 16.0), n.lattice(3, 2), "a lattice point is its value");
        let (a, b) = (n.lattice(3, 2), n.lattice(4, 2));
        assert!((n.value(28.0, 16.0) - (a + b) / 2.0).abs() < 1e-12, "half way between two lattice points");
        assert!((n.value(31.999, 20.0) - n.value(32.0, 20.0)).abs() < 1e-3, "continuous across a cell border");
        for x in 0..64 {
            let d = (n.value(x as f64 + 1.0, 9.5) - n.value(x as f64, 9.5)).abs();
            assert!(d <= 1.5 / 8.0, "x {x}: one pixel moves the value at most the smoothstep's slope per cell, {d}");
        }
        assert!((0..200).all(|x| (0.0..=1.0).contains(&n.value(x as f64 * 1.7, x as f64 * 0.3))));
    }

    #[test]
    fn noise_time_blends_two_slices_and_shade_maps_value_to_colour() {
        let n = Noise { cell: 4.0, seed: 9, time: 1.25, colour: Rgba { r: 1.0, g: 0.5, b: 0.0, a: 0.8 }, amount: 0.5, contrast: 2.0 };
        let (s0, s1, w) = n.slices();
        assert_eq!((s0, s1), (9 ^ kadr_core::hash::fmix64(1), 9 ^ kadr_core::hash::fmix64(2)));
        assert_eq!(w, smoothstep(0.0, 1.0, 0.25));
        let idx = Noise::lattice_index(-1, 4);
        let h = |s: u64| kadr_core::hash::pcg32_hash(s, idx) as f64 / u32::MAX as f64;
        assert!((n.lattice(-1, 4) - (h(s0) + (h(s1) - h(s0)) * w)).abs() < 1e-15);
        assert_eq!(Noise { time: 2.0, ..n }.lattice(-1, 4), h(s1), "a whole time is one slice");
        // m = clamp((0.6 − ½)·2 + ½, 0, 1) · ½ = 0.35 on the straight colour (1, 0.5, 0, 0.8).
        let c = n.shade(0.6);
        // The colour's alpha is the f32 0.8, not the f64 one.
        let a = 0.8f32 as f64 * 0.35;
        assert!(c.iter().zip([a, 0.5 * a, 0.0, a]).all(|(x, y)| (x - y).abs() < 1e-12), "{c:?}");
        assert_eq!(n.shade(0.0)[3], 0.0, "below the contrast's floor: nothing");
        assert_eq!(Noise { cell: f32::NAN, ..n }.cell_size(), 1.0);
        assert_eq!(Noise { cell: 0.2, ..n }.cell_size(), 1.0);
        assert_eq!(Noise { time: f32::NAN, ..n }.slices().2, 0.0, "a non-finite time is 0");
        assert_eq!(Noise { amount: f32::NAN, ..n }.shade(0.9)[3], 0.0, "a non-finite amount is 0");
    }

    #[test]
    fn generators_dispatch_and_derived_layer_ids_differ() {
        let n = Noise { cell: 3.0, seed: 1, time: 0.5, colour: Rgba::BLACK, amount: 1.0, contrast: 1.0 };
        assert_eq!(crate::Generator::Noise(n).at(4.0, 5.0), n.at(4.0, 5.0));
        let base = 0x1234_5678_9abc_def0_0fed_cba9_8765_4321u128;
        let ids: Vec<LayerId> = (0..3).map(|k| LayerId::derived(base, k)).collect();
        assert!(ids.iter().all(|l| l.0 != base), "never the base's own id");
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
        assert_eq!(LayerId::derived(base, 1), LayerId::derived(base, 1));
    }
}
