//! CPU renderer: the rendering contract (kadr-scene crate docs) on
//! premultiplied RGBA8, rows in parallel with rayon.
//!
//! Within one layer everything is fused, as the contract allows: sample →
//! effects → opacity and coverage → composite in f32, with coordinates,
//! coverage and the wipe mask in f64; the destination is stored rounded after
//! every layer. Effect chains are therefore not timed on their own
//! (`RenderStats::effects` stays 0).

use crate::{LayerInput, PreparedFrame, RenderError, RenderStats, RenderTarget, Renderer};
use kadr_core::color::AlphaMode;
use kadr_core::{CpuFrame, FramePool};
use kadr_scene::{Affine2, BlendMode, ColorAdjust, Effect, FrameScene, Layer, LayerContent, Rgba, TransitionLayer, TransitionOp};
use rayon::prelude::*;
use std::time::Instant;

pub struct CpuRenderer {
    /// Transition buffers.
    pool: FramePool,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuRenderer {
    pub fn new() -> Self {
        CpuRenderer { pool: FramePool::new(256 << 20) }
    }

    /// Transition buffers allocated so far; constant once warmed up (a
    /// render allocates nothing frame-sized).
    pub fn pool_allocations(&self) -> u64 {
        self.pool.allocations()
    }
}

impl Renderer for CpuRenderer {
    fn name(&self) -> &str {
        "cpu"
    }

    fn render(&mut self, frame: &PreparedFrame, target: &mut RenderTarget) -> Result<RenderStats, RenderError> {
        let start = Instant::now();
        let scene = frame.scene;
        let RenderTarget::Cpu(t) = target;
        let (w, h) = (scene.output.size.w, scene.output.size.h);
        if t.width != w || t.height != h || !fits(w, h, t.stride, t.data.len()) {
            return Err(RenderError::TargetMismatch { expected: (w, h), got: (t.width, t.height) });
        }
        check_inputs(&scene.layers, &frame.inputs.layers, "")?;

        let mut stats = RenderStats::default();
        // No pixels: nothing to draw (and a zero stride would be a zero chunk size for the row split).
        if w == 0 || h == 0 {
            stats.composite = start.elapsed();
            return Ok(stats);
        }
        let grid = Grid::new(scene);
        fill_base(t.data, t.stride, &grid, premul(scene.background));
        for (layer, input) in scene.layers.iter().zip(&frame.inputs.layers) {
            match self.draw_layer(t.data, t.stride, &grid, layer, input) {
                Drawn::Nothing => {}
                Drawn::Sampled => stats.layers_drawn += 1,
                Drawn::FastPath => {
                    stats.layers_drawn += 1;
                    stats.fast_paths += 1;
                }
            }
        }
        stats.composite = start.elapsed();
        Ok(stats)
    }
}

fn check_inputs(layers: &[Layer], inputs: &[LayerInput], within: &'static str) -> Result<(), RenderError> {
    if layers.len() != inputs.len() {
        return Err(RenderError::InputMismatch(format!("{} layers{within} but {} inputs", layers.len(), inputs.len())));
    }
    for (n, (layer, input)) in layers.iter().zip(inputs).enumerate() {
        match (&layer.content, input) {
            (LayerContent::Media { .. }, LayerInput::Cpu(f)) => check_frame(f).map_err(|m| RenderError::InputMismatch(format!("layer {n}{within}: {m}")))?,
            (LayerContent::Media { .. }, LayerInput::Missing(_)) | (LayerContent::Solid(_), LayerInput::None) => {}
            (LayerContent::Transition(t), LayerInput::Transition { from, to }) => {
                check_inputs(&t.from, from, " in a transition's `from`")?;
                check_inputs(&t.to, to, " in a transition's `to`")?;
            }
            (content, input) => {
                let kind = match content {
                    LayerContent::Media { .. } => "media",
                    LayerContent::Solid(_) => "solid",
                    LayerContent::Transition(_) => "transition",
                };
                let input = match input {
                    LayerInput::Cpu(_) => "Cpu",
                    LayerInput::Missing(_) => "Missing",
                    LayerInput::None => "None",
                    LayerInput::Transition { .. } => "Transition",
                };
                return Err(RenderError::InputMismatch(format!("layer {n}{within} is a {kind} layer with a {input} input")));
            }
        }
    }
    Ok(())
}

/// Whether `len` bytes hold `h` rows of `w` RGBA8 pixels, `stride` bytes apart.
fn fits(w: u32, h: u32, stride: usize, len: usize) -> bool {
    let row = w as usize * 4;
    let needed = match (h as usize).checked_sub(1) {
        None => Some(0),
        Some(rows) => rows.checked_mul(stride).and_then(|n| n.checked_add(row)),
    };
    stride >= row && needed.is_some_and(|n| n <= len)
}

fn check_frame(f: &CpuFrame) -> Result<(), String> {
    if f.width == 0 || f.height == 0 || !fits(f.width, f.height, f.stride, f.data.len()) {
        return Err(format!("a {}x{} frame with stride {} has {} bytes", f.width, f.height, f.stride, f.data.len()));
    }
    Ok(())
}

/// The output pixel grid and where the canvas lies on it.
#[derive(Clone, Copy)]
struct Grid {
    w: usize,
    h: usize,
    /// Canvas → output: `output = k · canvas + o`.
    k: f64,
    ox: f64,
    oy: f64,
    cw: f64,
    ch: f64,
    /// Output pixels whose centre lies in the canvas rectangle: columns `x0..x1`, rows `y0..y1`.
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Grid {
    fn new(scene: &FrameScene) -> Grid {
        let (w, h) = (scene.output.size.w as usize, scene.output.size.h as usize);
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let mut g = Grid { w, h, k: 0.0, ox: 0.0, oy: 0.0, cw, ch, x0: 0, x1: 0, y0: 0, y1: 0 };
        if cw == 0.0 || ch == 0.0 {
            return g;
        }
        g.k = (w as f64 / cw).min(h as f64 / ch);
        g.ox = (w as f64 - g.k * cw) / 2.0;
        g.oy = (h as f64 - g.k * ch) / 2.0;
        (g.x0, g.x1) = centres_within(w, g.ox, g.ox + g.k * cw);
        (g.y0, g.y1) = centres_within(h, g.oy, g.oy + g.k * ch);
        g
    }

    fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    fn canvas_to_output(&self) -> Affine2 {
        Affine2 { a: self.k, b: 0.0, c: 0.0, d: self.k, tx: self.ox, ty: self.oy }
    }
}

/// The pixels `i` of `0..n` whose centre `i + 0.5` lies in `[lo, hi]` (the
/// same test as per pixel, so boundary centres agree exactly).
fn centres_within(n: usize, lo: f64, hi: f64) -> (usize, usize) {
    let inside = |i: usize| {
        let c = i as f64 + 0.5;
        c >= lo && c <= hi
    };
    match (0..n).position(inside) {
        Some(a) => (a, a + (a..n).take_while(|&i| inside(i)).count()),
        None => (0, 0),
    }
}

const INV255: f32 = 1.0 / 255.0;

#[inline(always)]
fn store(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[inline(always)]
fn load(px: &[u8; 4]) -> [f32; 4] {
    px.map(|c| c as f32 * INV255)
}

/// The 4 bytes of pixel `i` in a row.
#[inline(always)]
fn pixel(row: &mut [u8], i: usize) -> &mut [u8; 4] {
    (&mut row[i * 4..i * 4 + 4]).try_into().unwrap()
}

/// A straight scene colour, premultiplied.
fn premul(c: Rgba) -> [f32; 4] {
    [c.r * c.a, c.g * c.a, c.b * c.a, c.a]
}

/// Composites premultiplied `s` onto the stored pixel `px` and stores the result.
#[inline(always)]
fn composite(blend: BlendMode, s: [f32; 4], px: &mut [u8; 4]) {
    // Exactly `s + d·0`.
    if blend == BlendMode::Normal && s[3] == 1.0 {
        *px = s.map(store);
        return;
    }
    let d = load(px);
    let (sa, da) = (s[3], d[3]);
    let o: [f32; 4] = match blend {
        BlendMode::Normal => std::array::from_fn(|c| s[c] + d[c] * (1.0 - sa)),
        BlendMode::Add => std::array::from_fn(|c| (s[c] + d[c]).min(1.0)),
        BlendMode::Multiply => std::array::from_fn(|c| s[c] * d[c] + s[c] * (1.0 - da) + d[c] * (1.0 - sa)),
        BlendMode::Screen => std::array::from_fn(|c| s[c] + d[c] - s[c] * d[c]),
    };
    *px = o.map(store);
}

/// Rows `rows.0..rows.1` of `data` in parallel, each as its own slice.
fn par_rows(data: &mut [u8], stride: usize, rows: (usize, usize), f: impl Fn(usize, &mut [u8]) + Sync + Send) {
    if rows.0 >= rows.1 {
        return;
    }
    let end = (rows.1 * stride).min(data.len());
    data[rows.0 * stride..end].par_chunks_mut(stride).enumerate().for_each(|(n, row)| f(rows.0 + n, row));
}

/// Margins opaque black, the canvas the premultiplied background.
fn fill_base(data: &mut [u8], stride: usize, g: &Grid, bg: [f32; 4]) {
    let bg = bg.map(store);
    const MARGIN: [u8; 4] = [0, 0, 0, 255];
    par_rows(data, stride, (0, g.h), |j, row| {
        let inside = j >= g.y0 && j < g.y1;
        for (i, px) in row[..g.w * 4].as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *px = if inside && i >= g.x0 && i < g.x1 { bg } else { MARGIN };
        }
    });
}

/// A layer's effect chain as one affine map on unpremultiplied RGB. Every
/// `ColorAdjust` step is affine — exposure `(c^2.4 · 2^ev)^(1/2.4)` is exactly
/// `c · 2^(ev/2.4)` for `c ≥ 0` — and nothing is clamped before the end of
/// the chain, so the whole chain composes into one matrix (in f64).
#[derive(Clone, Copy)]
struct ColorFx {
    m: [[f32; 3]; 3],
    t: [f32; 3],
}

type Affine3 = ([[f64; 3]; 3], [f64; 3]);

fn adjust_affine(a: &ColorAdjust) -> Affine3 {
    const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];
    let e = (a.exposure as f64 / 2.4).exp2();
    let k = a.contrast as f64;
    let (t, u, s) = (a.temperature as f64, a.tint as f64, a.saturation as f64);
    let wb = [1.0 + 0.1 * t, 1.0 - 0.05 * u, 1.0 - 0.1 * t];
    // c → wb · (k·e·c + 0.5·(1 − k)), then saturation S = s·I + (1 − s)·1·lumaᵀ.
    let sat = |r: usize, c: usize| if r == c { s } else { 0.0 } + (1.0 - s) * LUMA[c];
    let m = std::array::from_fn(|r| std::array::from_fn(|c| sat(r, c) * wb[c] * k * e));
    let t = std::array::from_fn(|r| (0..3).map(|c| sat(r, c) * wb[c] * 0.5 * (1.0 - k)).sum());
    (m, t)
}

impl ColorFx {
    /// `None` when the chain does nothing.
    fn of(effects: &[Effect]) -> Option<ColorFx> {
        let mut acc: Option<Affine3> = None;
        for e in effects {
            match e {
                Effect::ColorAdjust(a) if a.is_neutral() => {}
                Effect::ColorAdjust(a) => {
                    let (m2, t2) = adjust_affine(a);
                    acc = Some(match acc {
                        None => (m2, t2),
                        // Later after earlier: m2·(m1·c + t1) + t2.
                        Some((m1, t1)) => (
                            std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|n| m2[r][n] * m1[n][c]).sum())),
                            std::array::from_fn(|r| (0..3).map(|n| m2[r][n] * t1[n]).sum::<f64>() + t2[r]),
                        ),
                    });
                }
                // A primitive this renderer does not implement yet is skipped.
                _ => {}
            }
        }
        acc.map(|(m, t)| ColorFx { m: m.map(|r| r.map(|v| v as f32)), t: t.map(|v| v as f32) })
    }

    /// On a premultiplied sample: unpremultiply, map, clamp once, premultiply.
    #[inline(always)]
    fn apply(&self, s: [f32; 4]) -> [f32; 4] {
        let a = s[3];
        if a == 0.0 {
            return s;
        }
        let c = [s[0] / a, s[1] / a, s[2] / a];
        let o = |r: usize| (self.m[r][0] * c[0] + self.m[r][1] * c[1] + self.m[r][2] * c[2] + self.t[r]).clamp(0.0, 1.0) * a;
        [o(0), o(1), o(2), a]
    }
}

/// What drawing a layer amounted to, for `RenderStats`.
enum Drawn {
    Nothing,
    Sampled,
    FastPath,
}

/// Per-layer constants of the general path.
struct Plan {
    /// Output pixel → local pixel.
    inv: Affine2,
    /// Output pixel → texel (frames only).
    tex: Affine2,
    /// Signed distance to each crop edge's line in output pixels, positive
    /// inside: `a·x + b·y + c` at the output point `(x, y)`.
    edges: [[f64; 3]; 4],
    opacity: f32,
    blend: BlendMode,
    fx: Option<ColorFx>,
    /// Output rows and columns that can have coverage, within the canvas.
    rows: (usize, usize),
    cols: (usize, usize),
}

/// `v` as an index clamped into `lo..=hi` (NaN → `lo`).
fn index_in(v: f64, lo: usize, hi: usize) -> usize {
    if v.is_nan() { lo } else { v.clamp(lo as f64, hi as f64) as usize }
}

impl Plan {
    fn new(g: &Grid, layer: &Layer) -> Option<Plan> {
        let crop = layer.crop;
        // Opacity 0 and an empty crop both leave every pixel as it is.
        if g.is_empty() || layer.opacity == 0.0 || crop.is_empty() {
            return None;
        }
        let m = g.canvas_to_output().after(&layer.placement.to_canvas());
        let inv = m.inverse()?;
        let (x0, y0, x1, y1) = (crop.x0 as f64, crop.y0 as f64, crop.x1 as f64, crop.y1 as f64);
        // local.x = inv.a·x + inv.c·y + inv.tx, so its level lines are |∇| = hypot(inv.a, inv.c) apart per output pixel.
        let (gx, gy) = (inv.a.hypot(inv.c), inv.b.hypot(inv.d));
        let edges = [
            [inv.a / gx, inv.c / gx, (inv.tx - x0) / gx],
            [-inv.a / gx, -inv.c / gx, (x1 - inv.tx) / gx],
            [inv.b / gy, inv.d / gy, (inv.ty - y0) / gy],
            [-inv.b / gy, -inv.d / gy, (y1 - inv.ty) / gy],
        ];
        let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| m.apply(x, y));
        let (mut bx0, mut by0, mut bx1, mut by1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in corners {
            (bx0, by0, bx1, by1) = (bx0.min(x), by0.min(y), bx1.max(x), by1.max(y));
        }
        // Coverage reaches 0.5 px beyond each edge (≤ 0.71 px beyond a corner along an axis).
        let rows = (index_in((by0 - 1.5).floor(), g.y0, g.y1), index_in((by1 + 1.5).ceil(), g.y0, g.y1));
        let cols = (index_in((bx0 - 1.5).floor(), g.x0, g.x1), index_in((bx1 + 1.5).ceil(), g.x0, g.x1));
        if rows.0 >= rows.1 || cols.0 >= cols.1 {
            return None;
        }
        Some(Plan { inv, tex: inv, edges, opacity: layer.opacity, blend: layer.blend, fx: ColorFx::of(&layer.effects), rows, cols })
    }
}

/// The pixel centres `x` of a row where every edge function `a·x + b` (row
/// part folded into `b`) exceeds `level`: the open interval `(lo, hi)`.
#[inline(always)]
fn centres_above(e: &[(f64, f64); 4], level: f64, cols: (usize, usize)) -> (f64, f64) {
    let (mut lo, mut hi) = (cols.0 as f64, cols.1 as f64);
    for &(a, b) in e {
        let x = (level - b) / a;
        if a > 0.0 {
            lo = lo.max(x);
        } else if a < 0.0 {
            hi = hi.min(x);
        } else if b <= level {
            return (0.0, -1.0);
        }
    }
    (lo, hi)
}

/// For one row: the columns `outer` that may have coverage > 0 (one extra
/// pixel each side) and within them `inner`, whose pixels certainly have
/// coverage 1 (one pixel less each side), so the distances are evaluated
/// only near the edges.
#[inline(always)]
fn row_spans(e: &[(f64, f64); 4], cols: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    let (lo, hi) = centres_above(e, -0.5, cols);
    if lo >= hi || lo.is_nan() || hi.is_nan() {
        return ((0, 0), (0, 0));
    }
    let outer = (index_in((lo - 0.5).floor(), cols.0, cols.1), index_in((hi - 0.5).ceil() + 1.0, cols.0, cols.1));
    let (lo, hi) = centres_above(e, 0.5, cols);
    let a = index_in((lo - 0.5).floor() + 2.0, outer.0, outer.1);
    let inner = if lo < hi { (a, index_in((hi - 0.5).ceil() - 1.0, a, outer.1)) } else { (a, a) };
    (outer, inner)
}

trait Sampler: Sync {
    /// The premultiplied sample at texel point `(u, v)`.
    fn sample(&self, u: f64, v: f64) -> [f32; 4];
}

/// A solid colour (premultiplied, effects already applied).
struct Flat([f32; 4]);

impl Sampler for Flat {
    #[inline(always)]
    fn sample(&self, _: f64, _: f64) -> [f32; 4] {
        self.0
    }
}

const OPAQUE: u8 = 0;
const STRAIGHT: u8 = 1;
const PREMULTIPLIED: u8 = 2;

/// Bilinear sampling of a frame within its crop in whole texels.
struct Texels<'a, const ALPHA: u8> {
    data: &'a [u8],
    stride: usize,
    /// Crop in whole texels, inclusive: `kx0..=kx1`, `ky0..=ky1`.
    kx0: isize,
    kx1: isize,
    ky0: isize,
    ky1: isize,
}

impl<const ALPHA: u8> Texels<'_, ALPHA> {
    /// A texel premultiplied, in units of `SCALE` (so the 8-bit values need
    /// no per-texel division).
    #[inline(always)]
    fn texel(&self, o: usize) -> [f32; 4] {
        let p = <&[u8; 4]>::try_from(&self.data[o..o + 4]).unwrap().map(|c| c as f32);
        match ALPHA {
            OPAQUE => [p[0], p[1], p[2], 255.0],
            STRAIGHT => [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3] * 255.0],
            _ => p,
        }
    }

    const SCALE: f32 = if ALPHA == STRAIGHT { 1.0 / 65025.0 } else { INV255 };
}

impl<const ALPHA: u8> Sampler for Texels<'_, ALPHA> {
    #[inline(always)]
    fn sample(&self, u: f64, v: f64) -> [f32; 4] {
        let u = u.max(self.kx0 as f64 + 0.5).min(self.kx1 as f64 + 0.5) - 0.5;
        let v = v.max(self.ky0 as f64 + 0.5).min(self.ky1 as f64 + 0.5) - 0.5;
        // Clamped, u and v are ≥ 0, so truncation is `floor` (which is a libm call without SSE4.1).
        let (i, j) = (u as isize, v as isize);
        let (f, g) = ((u - i as f64) as f32, (v - j as f64) as f32);
        let x = |i: isize| i.max(self.kx0).min(self.kx1) as usize * 4;
        let y = |j: isize| j.max(self.ky0).min(self.ky1) as usize * self.stride;
        let (i0, i1, r0, r1) = (x(i), x(i + 1), y(j), y(j + 1));
        let (t00, t10, t01, t11) = (self.texel(r0 + i0), self.texel(r0 + i1), self.texel(r1 + i0), self.texel(r1 + i1));
        let (w00, w10, w01, w11) = ((1.0 - f) * (1.0 - g), f * (1.0 - g), (1.0 - f) * g, f * g);
        let px: [f32; 4] = std::array::from_fn(|c| (t00[c] * w00 + t10[c] * w10 + t01[c] * w01 + t11[c] * w11) * Self::SCALE);
        if ALPHA == OPAQUE { [px[0], px[1], px[2], 1.0] } else { px }
    }
}

/// Effects, opacity and coverage on a sample, composited into `px`.
#[inline(always)]
fn shade(plan: &Plan, mut s: [f32; 4], cov: f32, px: &mut [u8; 4]) {
    if let Some(fx) = &plan.fx {
        s = fx.apply(s);
    }
    let k = plan.opacity * cov;
    composite(plan.blend, s.map(|c| c * k), px);
}

/// The general path: every output pixel of the plan, evaluated at its centre.
fn draw_sampled<S: Sampler>(data: &mut [u8], stride: usize, plan: &Plan, s: &S) {
    par_rows(data, stride, plan.rows, |j, row| {
        let y = j as f64 + 0.5;
        let e = plan.edges.map(|[a, b, c]| (a, b * y + c));
        let (outer, inner) = row_spans(&e, plan.cols);
        let t = &plan.tex;
        let (ub, vb) = (t.c * y + t.tx, t.d * y + t.ty);
        for i in (outer.0..inner.0).chain(inner.1..outer.1) {
            let x = i as f64 + 0.5;
            let dist = e[0].0 * x + e[0].1;
            let dist = dist.min(e[1].0 * x + e[1].1).min(e[2].0 * x + e[2].1).min(e[3].0 * x + e[3].1);
            let cov = (0.5 + dist).clamp(0.0, 1.0) as f32;
            // Coverage 0 composites s = 0, which leaves the stored pixel as it is.
            if cov > 0.0 {
                shade(plan, s.sample(t.a * x + ub, t.b * x + vb), cov, pixel(row, i));
            }
        }
        for i in inner.0..inner.1 {
            let x = i as f64 + 0.5;
            shade(plan, s.sample(t.a * x + ub, t.b * x + vb), 1.0, pixel(row, i));
        }
    });
}

fn draw_flat(data: &mut [u8], stride: usize, g: &Grid, layer: &Layer, colour: Rgba) -> Drawn {
    let Some(mut plan) = Plan::new(g, layer) else { return Drawn::Nothing };
    let mut px = premul(colour);
    if let Some(fx) = plan.fx.take() {
        px = fx.apply(px);
    }
    draw_sampled(data, stride, &plan, &Flat(px));
    Drawn::Sampled
}

/// Integers within this of a whole number count as whole for the fast path;
/// over a frame the drift stays far below what could change a stored value.
const SNAP: f64 = 1e-6;

fn whole(v: f64) -> Option<isize> {
    let r = v.round();
    ((v - r).abs() <= SNAP).then_some(r as isize)
}

fn draw_frame(data: &mut [u8], stride: usize, g: &Grid, layer: &Layer, frame: &CpuFrame) -> Drawn {
    let size = layer.placement.size;
    if !(size.x > 0.0 && size.y > 0.0) {
        return Drawn::Nothing;
    }
    let Some(mut plan) = Plan::new(g, layer) else { return Drawn::Nothing };
    let (tw, th) = (frame.width as f64, frame.height as f64);
    let (sx, sy) = (size.x as f64, size.y as f64);
    plan.tex = Affine2::scale(tw / sx, th / sy).after(&plan.inv);
    let crop = layer.crop;
    let (cx0, cx1) = ((crop.x0 as f64 * tw / sx).max(0.0), (crop.x1 as f64 * tw / sx).min(tw));
    let (cy0, cy1) = ((crop.y0 as f64 * th / sy).max(0.0), (crop.y1 as f64 * th / sy).min(th));
    let kx0 = (cx0 + 0.5).floor().min(tw - 1.0);
    let kx1 = (cx1 + 0.5).floor().max(kx0 + 1.0).min(tw);
    let ky0 = (cy0 + 0.5).floor().min(th - 1.0);
    let ky1 = (cy1 + 0.5).floor().max(ky0 + 1.0).min(th);

    if frame.color.alpha == AlphaMode::Opaque
        && plan.fx.is_none()
        && layer.opacity == 1.0
        && layer.blend == BlendMode::Normal
        && let Some(d) = grid_aligned(&plan.tex, crop, tw / sx, th / sy, tw, th)
    {
        copy_texels(data, stride, g, frame, d);
        return Drawn::FastPath;
    }

    let (kx0, kx1, ky0, ky1) = (kx0 as isize, kx1 as isize - 1, ky0 as isize, ky1 as isize - 1);
    let fd = &frame.data[..];
    let st = frame.stride;
    match frame.color.alpha {
        AlphaMode::Opaque => draw_sampled(data, stride, &plan, &Texels::<OPAQUE> { data: fd, stride: st, kx0, kx1, ky0, ky1 }),
        AlphaMode::Straight => draw_sampled(data, stride, &plan, &Texels::<STRAIGHT> { data: fd, stride: st, kx0, kx1, ky0, ky1 }),
        AlphaMode::Premultiplied => draw_sampled(data, stride, &plan, &Texels::<PREMULTIPLIED> { data: fd, stride: st, kx0, kx1, ky0, ky1 }),
    }
    Drawn::Sampled
}

/// Texel rectangle `x0..x1 × y0..y1` shown one-to-one at output pixel `texel − (dx, dy)`.
struct Aligned {
    dx: isize,
    dy: isize,
    x0: isize,
    x1: isize,
    y0: isize,
    y1: isize,
}

/// When the output → texel map is a whole-pixel translation and the crop lies
/// on texel boundaries inside the frame, each output pixel centre hits a
/// texel centre and coverage is exactly 0 or 1: the general path would
/// reproduce the texels, so they can be copied.
fn grid_aligned(tex: &Affine2, crop: kadr_scene::RectF, sx: f64, sy: f64, tw: f64, th: f64) -> Option<Aligned> {
    let unit = (tex.a - 1.0).abs() < 1e-9 && (tex.d - 1.0).abs() < 1e-9 && tex.b.abs() < 1e-9 && tex.c.abs() < 1e-9;
    if !unit {
        return None;
    }
    let (dx, dy) = (whole(tex.tx)?, whole(tex.ty)?);
    let (x0, x1) = (whole(crop.x0 as f64 * sx)?, whole(crop.x1 as f64 * sx)?);
    let (y0, y1) = (whole(crop.y0 as f64 * sy)?, whole(crop.y1 as f64 * sy)?);
    (0 <= x0 && x0 < x1 && x1 as f64 <= tw && 0 <= y0 && y0 < y1 && y1 as f64 <= th).then_some(Aligned { dx, dy, x0, x1, y0, y1 })
}

/// Opaque texels straight into the output (alpha 255), within the canvas.
fn copy_texels(data: &mut [u8], stride: usize, g: &Grid, frame: &CpuFrame, a: Aligned) {
    let span = |lo: isize, hi: isize, d: isize, c0: usize, c1: usize| ((lo - d).clamp(c0 as isize, c1 as isize) as usize, (hi - d).clamp(c0 as isize, c1 as isize) as usize);
    let (i0, i1) = span(a.x0, a.x1, a.dx, g.x0, g.x1);
    let rows = span(a.y0, a.y1, a.dy, g.y0, g.y1);
    if i0 >= i1 {
        return;
    }
    par_rows(data, stride, rows, |j, row| {
        let src = frame.row((j as isize + a.dy) as u32);
        let src = &src[(i0 as isize + a.dx) as usize * 4..(i1 as isize + a.dx) as usize * 4];
        for (d, s) in row[i0 * 4..i1 * 4].as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
            *d = [s[0], s[1], s[2], 255];
        }
    });
}

/// How two stored transition buffers mix into one premultiplied value.
enum Mix {
    Dissolve(f32),
    Dip { colour: [f32; 4], p: f32 },
    /// Mask `m = clamp(0.5 + e / band, 0, 1)` with `e = (E − dot(canvas point, u)) · k`.
    Wipe { edge: f64, ux: f64, uy: f64, band: f64 },
}

impl Mix {
    fn new(t: &TransitionLayer, g: &Grid) -> Mix {
        let p = t.progress;
        match t.op {
            TransitionOp::Dissolve => Mix::Dissolve(p),
            TransitionOp::DipToColor(c) => Mix::Dip { colour: premul(c), p },
            TransitionOp::Wipe { angle, softness } => {
                let (uy, ux) = (angle as f64).sin_cos();
                let dots = [0.0, g.cw * ux, g.ch * uy, g.cw * ux + g.ch * uy];
                let (lo, hi) = (dots.iter().copied().fold(f64::INFINITY, f64::min), dots.iter().copied().fold(f64::NEG_INFINITY, f64::max));
                let w = softness as f64;
                let (a, b) = (lo - w / 2.0, hi + w / 2.0);
                Mix::Wipe { edge: a + (b - a) * p as f64, ux, uy, band: (w * g.k).max(1.0) }
            }
        }
    }

    /// `x`, `y`: the output pixel centre.
    #[inline(always)]
    fn at(&self, g: &Grid, x: f64, y: f64, from: [f32; 4], to: [f32; 4]) -> [f32; 4] {
        let lerp = |a: [f32; 4], b: [f32; 4], t: f32| -> [f32; 4] { std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t) };
        match *self {
            Mix::Dissolve(p) => lerp(from, to, p),
            Mix::Dip { colour, p } => {
                let dip = |x: [f32; 4]| colour.map(|c| c * x[3]);
                if p < 0.5 { lerp(from, dip(from), 2.0 * p) } else { lerp(dip(to), to, 2.0 * p - 1.0) }
            }
            Mix::Wipe { edge, ux, uy, band } => {
                let (cx, cy) = ((x - g.ox) / g.k, (y - g.oy) / g.k);
                let e = (edge - (cx * ux + cy * uy)) * g.k;
                lerp(from, to, (0.5 + e / band).clamp(0.0, 1.0) as f32)
            }
        }
    }
}

impl CpuRenderer {
    fn draw_layer(&self, data: &mut [u8], stride: usize, g: &Grid, layer: &Layer, input: &LayerInput) -> Drawn {
        match (&layer.content, input) {
            (LayerContent::Transition(t), LayerInput::Transition { from, to }) => self.draw_transition(data, stride, g, layer, t, from, to),
            (LayerContent::Solid(c), _) => draw_flat(data, stride, g, layer, *c),
            (LayerContent::Media { .. }, LayerInput::Cpu(f)) => draw_frame(data, stride, g, layer, f),
            (LayerContent::Media { .. }, _) => draw_flat(data, stride, g, layer, Rgba::MISSING),
            // Inputs are checked before drawing.
            (LayerContent::Transition(_), _) => Drawn::Nothing,
        }
    }

    /// `from` and `to` into their own transparent buffers (canvas pixels
    /// only: margins are neither drawn nor read), mixed and composited.
    #[allow(clippy::too_many_arguments)]
    fn draw_transition(&self, data: &mut [u8], stride: usize, g: &Grid, layer: &Layer, t: &TransitionLayer, from: &[LayerInput], to: &[LayerInput]) -> Drawn {
        if g.is_empty() || layer.opacity == 0.0 {
            return Drawn::Nothing;
        }
        let bs = g.w * 4;
        let mut bufs = [self.pool.take(bs * g.h), self.pool.take(bs * g.h)];
        for (buf, (layers, inputs)) in bufs.iter_mut().zip([(&t.from, from), (&t.to, to)]) {
            par_rows(buf, bs, (g.y0, g.y1), |_, row| row[g.x0 * 4..g.x1 * 4].fill(0));
            for (l, i) in layers.iter().zip(inputs) {
                self.draw_layer(buf, bs, g, l, i);
            }
        }
        let (a, b) = (&bufs[0][..], &bufs[1][..]);
        let mix = Mix::new(t, g);
        let (opacity, blend) = (layer.opacity, layer.blend);
        par_rows(data, stride, (g.y0, g.y1), |j, row| {
            let (fr, tr) = (&a[j * bs..(j + 1) * bs], &b[j * bs..(j + 1) * bs]);
            let y = j as f64 + 0.5;
            for i in g.x0..g.x1 {
                let px = i * 4..i * 4 + 4;
                let m = mix.at(g, i as f64 + 0.5, y, load(fr[px.clone()].try_into().unwrap()), load(tr[px].try_into().unwrap()));
                composite(blend, m.map(|c| c * opacity), pixel(row, i));
            }
        });
        Drawn::Sampled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CpuTarget, MissingReason, RenderInputs};
    use kadr_core::{AssetId, ColorInfo, Time};
    use kadr_scene::*;
    use std::sync::Arc;

    const RED: Rgba = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    const BLUE: Rgba = Rgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
    const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

    fn layer(content: LayerContent, placement: Placement) -> Layer {
        Layer { id: LayerId(1), content, crop: placement.full_crop(), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
    }

    fn solid(c: Rgba, placement: Placement) -> Layer {
        layer(LayerContent::Solid(c), placement)
    }

    fn media(placement: Placement) -> Layer {
        let media = MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Video, display_size: SizeU::new(placement.size.x as u32, placement.size.y as u32), color: ColorInfo::WORKING_SDR };
        layer(LayerContent::Media { media, source_time: Time::ZERO }, placement)
    }

    fn transition(op: TransitionOp, progress: f32, canvas: SizeU, from: Vec<Layer>, to: Vec<Layer>) -> Layer {
        layer(LayerContent::Transition(Box::new(TransitionLayer { op, progress, from, to })), Placement::fill(canvas))
    }

    fn scene(canvas: (u32, u32), out: (u32, u32), background: Rgba, layers: Vec<Layer>) -> FrameScene {
        FrameScene { time: Time::ZERO, canvas: SizeU::new(canvas.0, canvas.1), output: OutputSpec::new(SizeU::new(out.0, out.1), RenderQuality::Export), background, layers }
    }

    fn frame(w: u32, h: u32, alpha: AlphaMode, px: impl Fn(u32, u32) -> [u8; 4]) -> Arc<CpuFrame> {
        let bytes = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| px(x, y)).collect();
        Arc::new(CpuFrame::from_rgba8(w, h, ColorInfo { alpha, ..ColorInfo::WORKING_SDR }, bytes))
    }

    fn render_with(r: &mut CpuRenderer, s: &FrameScene, inputs: Vec<LayerInput>) -> (Vec<u8>, RenderStats) {
        let (w, h) = (s.output.size.w, s.output.size.h);
        let mut out = vec![7u8; (w * h * 4) as usize];
        let inputs = RenderInputs { layers: inputs };
        let stats = r.render(&PreparedFrame { scene: s, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(w, h, &mut out))).unwrap();
        (out, stats)
    }

    fn render(s: &FrameScene, inputs: Vec<LayerInput>) -> Vec<u8> {
        render_with(&mut CpuRenderer::new(), s, inputs).0
    }

    fn px(out: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [out[o], out[o + 1], out[o + 2], out[o + 3]]
    }

    fn near(a: [u8; 4], b: [u8; 4]) -> bool {
        a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 1)
    }

    #[test]
    fn letterbox_margins_are_black_and_the_canvas_gets_the_premultiplied_background() {
        let bg = Rgba { r: 1.0, g: 0.5, b: 0.0, a: 0.5 };
        let out = render(&scene((16, 9), (16, 12), bg, vec![]), vec![]);
        // k = 1, o = (0, 1.5): rows whose centre is in [1.5, 10.5] are canvas, i.e. 1..=10.
        for y in 0..12 {
            let want = if (1..=10).contains(&y) { [128, 64, 0, 128] } else { [0, 0, 0, 255] };
            assert_eq!(px(&out, 16, 5, y), want, "row {y}");
        }
        let out = render(&scene((9, 16), (16, 16), Rgba::BLACK, vec![]), vec![]);
        assert!((0..16).all(|x| px(&out, 16, x, 8) == [0, 0, 0, 255]));
        // Pillarbox: k = 1, o.x = 3.5 → columns whose centre is in [3.5, 12.5], i.e. 3..=12, are canvas.
        let clear = render(&scene((9, 16), (16, 16), Rgba::TRANSPARENT, vec![]), vec![]);
        let alphas: Vec<_> = (0..16).map(|x| px(&clear, 16, x, 8)[3]).collect();
        assert_eq!(alphas, [255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255]);
    }

    #[test]
    fn half_opaque_red_over_opaque_blue_is_exact() {
        let c = SizeU::new(8, 8);
        let mut red = solid(RED, Placement::fill(c));
        red.opacity = 0.5;
        let out = render(&scene((8, 8), (8, 8), BLUE, vec![red]), vec![LayerInput::None]);
        assert_eq!(px(&out, 8, 3, 3), [128, 0, 128, 255]);
    }

    #[test]
    fn zero_saturation_gives_rec709_grey() {
        let c = SizeU::new(4, 4);
        let colour = Rgba { r: 0.8, g: 0.3, b: 0.6, a: 1.0 };
        let mut l = solid(colour, Placement::fill(c));
        l.effects.push(Effect::ColorAdjust(ColorAdjust { saturation: 0.0, ..ColorAdjust::NEUTRAL }));
        let out = render(&scene((4, 4), (4, 4), Rgba::BLACK, vec![l]), vec![LayerInput::None]);
        let y = (255.0 * (0.2126 * 0.8 + 0.7152 * 0.3 + 0.0722 * 0.6) + 0.5f64).floor() as u8;
        assert!(near(px(&out, 4, 1, 1), [y, y, y, 255]), "{:?} vs {y}", px(&out, 4, 1, 1));
    }

    #[test]
    fn colour_adjust_follows_the_formulas_in_order() {
        let c = SizeU::new(2, 2);
        let colour = Rgba { r: 0.6, g: 0.4, b: 0.2, a: 0.5 };
        let adj = ColorAdjust { exposure: 0.7, contrast: 1.3, saturation: 0.6, temperature: 0.4, tint: -0.5 };
        let mut l = solid(colour, Placement::fill(c));
        l.effects = vec![Effect::ColorAdjust(adj), Effect::ColorAdjust(ColorAdjust::NEUTRAL)];
        let out = render(&scene((2, 2), (2, 2), Rgba::TRANSPARENT, vec![l]), vec![LayerInput::None]);
        let mut rgb = [0.6f64, 0.4, 0.2].map(|c| (c.powf(2.4) * 2f64.powf(0.7)).powf(1.0 / 2.4));
        rgb = rgb.map(|c| (c - 0.5) * 1.3 + 0.5);
        rgb = [rgb[0] * (1.0 + 0.04), rgb[1] * (1.0 + 0.025), rgb[2] * (1.0 - 0.04)];
        let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        let want = rgb.map(|c| ((y + (c - y) * 0.6).clamp(0.0, 1.0) * 0.5 * 255.0 + 0.5).floor() as u8);
        assert!(near(px(&out, 2, 0, 0), [want[0], want[1], want[2], 128]), "{:?} vs {want:?}", px(&out, 2, 0, 0));
    }

    fn gradient(w: u32, h: u32) -> Arc<CpuFrame> {
        frame(w, h, AlphaMode::Opaque, |x, y| [(x * 37 % 256) as u8, (y * 53 % 256) as u8, ((x + y) * 11 % 256) as u8, 255])
    }

    #[test]
    fn an_aligned_opaque_frame_is_copied_and_matches_the_general_path() {
        let c = SizeU::new(24, 16);
        let f = gradient(24, 16);
        let s = scene((24, 16), (24, 16), Rgba::BLACK, vec![media(Placement::fill(c))]);
        let (fast, stats) = render_with(&mut CpuRenderer::new(), &s, vec![LayerInput::Cpu(f.clone())]);
        assert_eq!((stats.fast_paths, stats.layers_drawn), (1, 1));
        for y in 0..16 {
            assert_eq!(&fast[(y * 24 * 4) as usize..((y + 1) * 24 * 4) as usize], f.row(y), "row {y}");
        }
        // Add over opaque black is the same picture through the general path.
        let mut add = s.clone();
        add.layers[0].blend = BlendMode::Add;
        let (general, stats) = render_with(&mut CpuRenderer::new(), &add, vec![LayerInput::Cpu(f.clone())]);
        assert_eq!(stats.fast_paths, 0);
        assert_eq!(fast, general);
        // A whole-pixel offset PiP at scale 1 with a whole-texel crop is also a copy, identical to sampling it.
        let mut pip = media(Placement { size: Vec2::new(24.0, 16.0), anchor: Vec2::new(0.0, 0.0), position: Vec2::new(5.0, -3.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 });
        pip.crop = RectF::new(2.0, 4.0, 15.0, 16.0);
        let s = scene((24, 16), (24, 16), BLUE, vec![pip]);
        let (fast, stats) = render_with(&mut CpuRenderer::new(), &s, vec![LayerInput::Cpu(f.clone())]);
        assert_eq!(stats.fast_paths, 1);
        let mut sampled = s.clone();
        sampled.layers[0].opacity = 0.999_999_9; // just below 1: the general path
        let (general, stats) = render_with(&mut CpuRenderer::new(), &sampled, vec![LayerInput::Cpu(f)]);
        assert_eq!(stats.fast_paths, 0);
        assert_eq!(fast, general);
        assert_eq!(px(&fast, 24, 6, 1), [0, 0, 255, 255], "left of the crop: background");
        assert_eq!(px(&fast, 24, 7, 1), px(&general, 24, 7, 1));
    }

    #[test]
    fn straight_alpha_is_premultiplied_before_filtering() {
        // Opaque white next to fully transparent black: halfway between them is white at half alpha, not grey.
        let f = frame(2, 1, AlphaMode::Straight, |x, _| if x == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 0] });
        let c = SizeU::new(2, 1);
        let s = scene((2, 1), (4, 2), Rgba::TRANSPARENT, vec![media(Placement::fill(c))]);
        let out = render(&s, vec![LayerInput::Cpu(f)]);
        // k = 2: output centre 1.5 → u = 0.75 (f = 0.25), centre 2.5 → u = 1.25 (f = 0.75).
        assert_eq!(px(&out, 4, 1, 0), [191, 191, 191, 191]);
        assert_eq!(px(&out, 4, 2, 0), [64, 64, 64, 64], "unpremultiplied: white, not grey");
    }

    #[test]
    fn crop_keeps_the_remaining_part_in_place() {
        let c = SizeU::new(8, 4);
        let f = frame(8, 4, AlphaMode::Opaque, |x, _| [x as u8 * 30, 0, 0, 255]);
        let mut l = media(Placement::fill(c));
        l.crop = RectF::new(4.0, 0.0, 8.0, 4.0);
        let out = render(&scene((8, 4), (8, 4), BLUE, vec![l]), vec![LayerInput::Cpu(f)]);
        for x in 0..8 {
            let want = if x < 4 { [0, 0, 255, 255] } else { [x as u8 * 30, 0, 0, 255] };
            assert_eq!(px(&out, 8, x, 2), want, "column {x}");
        }
    }

    #[test]
    fn crop_never_reads_outside_itself() {
        // Scaled up 2×, the texel just inside the crop is repeated rather than blended with the cropped-away one.
        let f = frame(4, 1, AlphaMode::Opaque, |x, _| if x < 2 { [0, 0, 0, 255] } else { [200, 200, 200, 255] });
        let mut l = media(Placement { size: Vec2::new(4.0, 1.0), anchor: Vec2::new(0.0, 0.0), position: Vec2::new(0.0, 0.0), scale: Vec2::new(2.0, 8.0), rotation: 0.0 });
        l.crop = RectF::new(2.0, 0.0, 4.0, 1.0);
        let out = render(&scene((8, 8), (8, 8), Rgba::BLACK, vec![l]), vec![LayerInput::Cpu(f)]);
        assert_eq!(px(&out, 8, 4, 4), [200, 200, 200, 255]);
        assert_eq!(px(&out, 8, 3, 4), [0, 0, 0, 255]);
    }

    #[test]
    fn rotation_by_90_degrees_turns_a_wide_layer_upright() {
        // 6×2 layer, left half red and right half blue, rotated 90° clockwise about its centre on a 10×10 canvas.
        let f = frame(6, 2, AlphaMode::Opaque, |x, _| if x < 3 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        let p = Placement { size: Vec2::new(6.0, 2.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(5.0, 5.0), scale: Vec2::new(1.0, 1.0), rotation: std::f32::consts::FRAC_PI_2 };
        let out = render(&scene((10, 10), (10, 10), Rgba::BLACK, vec![media(p)]), vec![LayerInput::Cpu(f)]);
        // Canvas footprint: x ∈ [4, 6], y ∈ [2, 8]; the left end (red) goes up.
        assert_eq!(px(&out, 10, 4, 2), [255, 0, 0, 255]);
        assert_eq!(px(&out, 10, 5, 7), [0, 0, 255, 255]);
        assert_eq!(px(&out, 10, 3, 5), [0, 0, 0, 255]);
        assert_eq!(px(&out, 10, 6, 5), [0, 0, 0, 255]);
        assert_eq!(px(&out, 10, 5, 1), [0, 0, 0, 255]);
        assert_eq!(px(&out, 10, 5, 8), [0, 0, 0, 255]);
    }

    #[test]
    fn edges_are_antialiased_by_coverage() {
        // A white square whose left edge sits on a pixel centre: that column is half covered.
        let p = Placement { size: Vec2::new(4.0, 4.0), anchor: Vec2::new(0.0, 0.0), position: Vec2::new(2.5, 2.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 };
        let out = render(&scene((10, 10), (10, 10), Rgba::BLACK, vec![solid(WHITE, p)]), vec![LayerInput::None]);
        assert_eq!(px(&out, 10, 2, 3), [128, 128, 128, 255]);
        assert_eq!(px(&out, 10, 3, 3), [255, 255, 255, 255]);
        assert_eq!(px(&out, 10, 6, 3), [128, 128, 128, 255]);
        assert_eq!(px(&out, 10, 7, 3), [0, 0, 0, 255]);
    }

    #[test]
    fn blend_modes_follow_their_formulas() {
        let c = SizeU::new(1, 1);
        let d = Rgba { r: 0.6, g: 0.2, b: 0.4, a: 1.0 };
        let s = Rgba { r: 0.5, g: 0.8, b: 0.1, a: 0.5 };
        let sp = [s.r * s.a, s.g * s.a, s.b * s.a, s.a];
        let dp = [d.r, d.g, d.b, d.a].map(|v| store(v) as f32 / 255.0);
        for (mode, f) in [
            (BlendMode::Normal, (|s: f32, d: f32, sa: f32, _: f32| s + d * (1.0 - sa)) as fn(f32, f32, f32, f32) -> f32),
            (BlendMode::Add, |s, d, _, _| (s + d).min(1.0)),
            (BlendMode::Multiply, |s, d, sa, da| s * d + s * (1.0 - da) + d * (1.0 - sa)),
            (BlendMode::Screen, |s, d, _, _| s + d - s * d),
        ] {
            let mut l = solid(s, Placement::fill(c));
            l.blend = mode;
            let out = render(&scene((1, 1), (1, 1), d, vec![l]), vec![LayerInput::None]);
            let want: [u8; 4] = std::array::from_fn(|k| ((f(sp[k], dp[k], sp[3], dp[3]).clamp(0.0, 1.0) as f64) * 255.0 + 0.5).floor() as u8);
            assert!(near(px(&out, 1, 0, 0), want), "{mode:?}: {:?} vs {want:?}", px(&out, 1, 0, 0));
        }
    }

    #[test]
    fn missing_media_is_drawn_in_the_missing_colour() {
        let c = SizeU::new(4, 4);
        let out = render(&scene((4, 4), (4, 4), Rgba::BLACK, vec![media(Placement::fill(c))]), vec![LayerInput::Missing(MissingReason::Offline)]);
        let m = Rgba::MISSING;
        assert_eq!(px(&out, 4, 2, 2), [store(m.r), store(m.g), store(m.b), 255]);
    }

    fn transition_scene(op: TransitionOp, p: f32) -> (FrameScene, Vec<LayerInput>) {
        let c = SizeU::new(10, 2);
        let t = transition(op, p, c, vec![solid(RED, Placement::fill(c))], vec![solid(BLUE, Placement::fill(c))]);
        (scene((10, 2), (10, 2), Rgba::BLACK, vec![t]), vec![LayerInput::Transition { from: vec![LayerInput::None], to: vec![LayerInput::None] }])
    }

    #[test]
    fn dissolve_mixes_the_stored_buffers() {
        for (p, want) in [(0.0, [255, 0, 0, 255]), (1.0, [0, 0, 255, 255]), (0.25, [191, 0, 64, 255])] {
            let (s, i) = transition_scene(TransitionOp::Dissolve, p);
            assert_eq!(px(&render(&s, i), 10, 4, 1), want, "p = {p}");
        }
    }

    #[test]
    fn dip_to_colour_passes_through_the_colour_at_half_way() {
        let white = TransitionOp::DipToColor(WHITE);
        for (p, want) in [(0.0, [255, 0, 0, 255]), (0.25, [255, 128, 128, 255]), (0.5, [255, 255, 255, 255]), (0.75, [128, 128, 255, 255]), (1.0, [0, 0, 255, 255])] {
            let (s, i) = transition_scene(white, p);
            assert_eq!(px(&render(&s, i), 10, 4, 1), want, "p = {p}");
        }
    }

    #[test]
    fn dip_colour_only_appears_where_the_clip_has_coverage() {
        // A PiP clip dipping to black over a white track: outside the PiP the track shows untouched.
        let c = SizeU::new(10, 10);
        let pip = Placement { size: Vec2::new(4.0, 4.0), anchor: Vec2::new(0.0, 0.0), position: Vec2::new(0.0, 0.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 };
        let t = transition(TransitionOp::DipToColor(Rgba::BLACK), 0.5, c, vec![solid(RED, pip)], vec![solid(BLUE, pip)]);
        let s = scene((10, 10), (10, 10), Rgba::BLACK, vec![solid(WHITE, Placement::fill(c)), t]);
        let out = render(&s, vec![LayerInput::None, LayerInput::Transition { from: vec![LayerInput::None], to: vec![LayerInput::None] }]);
        assert_eq!(px(&out, 10, 1, 1), [0, 0, 0, 255]);
        assert_eq!(px(&out, 10, 7, 7), [255, 255, 255, 255]);
    }

    #[test]
    fn wipe_sweeps_from_the_side_its_angle_names() {
        // Angle π: the edge moves right → left, so `to` (blue) appears on the right.
        let pi = std::f32::consts::PI;
        let (s, i) = transition_scene(TransitionOp::Wipe { angle: pi, softness: 0.0 }, 0.4);
        let out = render(&s, i);
        // lo = −10, hi = 0, E = −6: pixels with −x < −6, i.e. centre x > 6, are blue; centre 6.5 is past by 0.5.
        let row: Vec<_> = (0..10).map(|x| px(&out, 10, x, 0)).collect();
        assert_eq!(row[5], [255, 0, 0, 255]);
        assert_eq!(row[6], [0, 0, 255, 255], "centre 6.5: e = 0.5 → fully `to`");
        assert_eq!(row[9], [0, 0, 255, 255]);
        for (p, x, want) in [(0.0, 9, [255, 0, 0, 255]), (1.0, 0, [0, 0, 255, 255])] {
            let (s, i) = transition_scene(TransitionOp::Wipe { angle: 0.0, softness: 4.0 }, p);
            assert_eq!(px(&render(&s, i), 10, x, 0), want, "p = {p}");
        }
        // Angle 0 with a 4 px band at p = 0.5: E = lerp(−2, 12, 0.5) = 5; centre 4.5 → e = 0.5, m = 0.5 + 0.125.
        let (s, i) = transition_scene(TransitionOp::Wipe { angle: 0.0, softness: 4.0 }, 0.5);
        assert_eq!(px(&render(&s, i), 10, 4, 0), [96, 0, 159, 255]);
    }

    #[test]
    fn transition_buffers_start_transparent_and_nest() {
        // `from` empty, `to` a nested dissolve at 1 → the nested `to`: half-way to blue over the black background.
        let c = SizeU::new(4, 4);
        let inner = transition(TransitionOp::Dissolve, 1.0, c, vec![solid(RED, Placement::fill(c))], vec![solid(BLUE, Placement::fill(c))]);
        let t = transition(TransitionOp::Dissolve, 0.5, c, vec![], vec![inner]);
        let s = scene((4, 4), (4, 4), Rgba::BLACK, vec![t]);
        let inner_in = LayerInput::Transition { from: vec![LayerInput::None], to: vec![LayerInput::None] };
        let out = render(&s, vec![LayerInput::Transition { from: vec![], to: vec![inner_in] }]);
        assert_eq!(px(&out, 4, 2, 2), [0, 0, 128, 255]);
    }

    #[test]
    fn mismatched_inputs_and_targets_are_errors() {
        let c = SizeU::new(4, 4);
        let s = scene((4, 4), (4, 4), Rgba::BLACK, vec![media(Placement::fill(c))]);
        let mut r = CpuRenderer::new();
        let mut buf = [0u8; 64];
        let mut go = |s: &FrameScene, inputs: Vec<LayerInput>, w: u32, len: usize| {
            let inputs = RenderInputs { layers: inputs };
            r.render(&PreparedFrame { scene: s, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(w, 4, &mut buf[..len])))
        };
        assert!(matches!(go(&s, vec![], 4, 64), Err(RenderError::InputMismatch(_))));
        assert!(matches!(go(&s, vec![LayerInput::None], 4, 64), Err(RenderError::InputMismatch(_))));
        assert_eq!(go(&s, vec![LayerInput::Missing(MissingReason::NotReady)], 3, 48), Err(RenderError::TargetMismatch { expected: (4, 4), got: (3, 4) }));
        assert!(matches!(go(&s, vec![LayerInput::Missing(MissingReason::NotReady)], 4, 60), Err(RenderError::TargetMismatch { .. })));
        let t = scene((4, 4), (4, 4), Rgba::BLACK, vec![transition(TransitionOp::Dissolve, 0.5, c, vec![solid(RED, Placement::fill(c))], vec![])]);
        let nested_wrong = LayerInput::Transition { from: vec![LayerInput::Missing(MissingReason::Offline)], to: vec![] };
        assert!(matches!(go(&t, vec![nested_wrong], 4, 64), Err(RenderError::InputMismatch(_))));
        assert!(matches!(go(&t, vec![LayerInput::None], 4, 64), Err(RenderError::InputMismatch(_))));
        assert!(go(&t, vec![LayerInput::Transition { from: vec![LayerInput::None], to: vec![] }], 4, 64).is_ok());
    }

    #[test]
    fn empty_outputs_and_absurd_strides_are_handled_without_panics() {
        let c = SizeU::new(4, 4);
        let t = transition(TransitionOp::Dissolve, 0.5, c, vec![solid(RED, Placement::fill(c))], vec![]);
        let inputs = RenderInputs { layers: vec![LayerInput::Transition { from: vec![LayerInput::None], to: vec![] }] };
        let mut r = CpuRenderer::new();
        for (w, h) in [(0, 5), (5, 0), (0, 0)] {
            let s = scene((4, 4), (w, h), Rgba::BLACK, vec![t.clone()]);
            let target = CpuTarget { width: w, height: h, stride: w as usize * 4, data: &mut [] };
            let stats = r.render(&PreparedFrame { scene: &s, inputs: &inputs }, &mut RenderTarget::Cpu(target)).unwrap();
            assert_eq!(stats.layers_drawn, 0, "{w}x{h}");
        }
        let s = scene((4, 4), (4, 4), Rgba::BLACK, vec![]);
        let mut buf = [0u8; 64];
        let target = CpuTarget { width: 4, height: 4, stride: usize::MAX / 2, data: &mut buf };
        let empty = RenderInputs::default();
        assert!(matches!(r.render(&PreparedFrame { scene: &s, inputs: &empty }, &mut RenderTarget::Cpu(target)), Err(RenderError::TargetMismatch { .. })));
        let mut f = CpuFrame::from_rgba8(1, 2, ColorInfo::WORKING_SDR, vec![0; 8]);
        f.stride = usize::MAX / 2;
        let s = scene((4, 4), (4, 4), Rgba::BLACK, vec![media(Placement::fill(c))]);
        let inputs = RenderInputs { layers: vec![LayerInput::Cpu(Arc::new(f))] };
        assert!(matches!(r.render(&PreparedFrame { scene: &s, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(4, 4, &mut buf))), Err(RenderError::InputMismatch(_))));
    }

    #[test]
    fn a_warm_renderer_allocates_no_frame_buffers() {
        let c = SizeU::new(32, 18);
        let t = transition(TransitionOp::Dissolve, 0.3, c, vec![media(Placement::fill(c))], vec![solid(BLUE, Placement::fill(c))]);
        let s = scene((32, 18), (16, 9), Rgba::BLACK, vec![t]);
        let inputs = || vec![LayerInput::Transition { from: vec![LayerInput::Cpu(gradient(32, 18))], to: vec![LayerInput::None] }];
        let mut r = CpuRenderer::new();
        let (first, _) = render_with(&mut r, &s, inputs());
        let warm = r.pool_allocations();
        assert_eq!(warm, 2);
        for _ in 0..3 {
            let (again, stats) = render_with(&mut r, &s, inputs());
            assert_eq!(again, first);
            assert_eq!(stats.layers_drawn, 1);
        }
        assert_eq!(r.pool_allocations(), warm);
    }

    #[test]
    fn opacity_scales_colour_and_alpha() {
        let c = SizeU::new(2, 2);
        let mut l = solid(WHITE, Placement::fill(c));
        l.opacity = 0.25;
        let out = render(&scene((2, 2), (2, 2), Rgba::TRANSPARENT, vec![l]), vec![LayerInput::None]);
        assert_eq!(px(&out, 2, 0, 0), [64, 64, 64, 64]);
    }
}
