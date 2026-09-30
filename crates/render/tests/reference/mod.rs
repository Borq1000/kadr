//! An independent, deliberately naive implementation of the rendering contract
//! (`kadr-scene` crate docs). It exists only so the optimized `CpuRenderer`
//! can be compared against something written separately from it: f64
//! everywhere, single-threaded, every layer evaluated at every output pixel —
//! no bounding boxes, no fast paths, no LUTs. Each rule is written the way the
//! contract states it, not the way it would be made fast.
//!
//! Interpretation notes (where the contract text leaves room):
//! - Layer edge coverage uses the *raw* crop rectangle mapped to output space;
//!   only texel *sampling* intersects the crop with the decoded frame.
//! - An output pixel is a margin only if its centre is strictly outside the
//!   canvas rectangle (a centre exactly on the boundary counts as inside).
//! - A transition layer ignores its placement, crop and effects (the contract
//!   composites it "pixel for pixel, coverage 1"); opacity and blend apply.
//! - Every store that lands within 1e-3 LSB of a rounding tie is recorded per
//!   output pixel (`render_reference_with_ties`): an f32 and an f64 implementation may
//!   legitimately round such a value differently, and the flip then passes through
//!   the later `s + d·(1 − s.a)` blends, so parity allows one extra LSB per tie.
//! - Missing / mismatching inputs: `Media` without a `Cpu` input is drawn as
//!   `Rgba::MISSING`; a `Transition` without matching inputs gets no inputs.

#![allow(dead_code)]

use kadr_core::CpuFrame;
use kadr_core::color::AlphaMode;
use kadr_render::{LayerInput, RenderInputs};
use kadr_scene::{BlendMode, Effect, FrameScene, Layer, LayerContent, Rgba, TransitionOp};
use std::cell::RefCell;

/// Premultiplied RGBA, components in [0, 1].
type Px = [f64; 4];

/// Output geometry: output size, canvas size, uniform scale `k` and centring offset `o`.
struct Geo {
    ow: usize,
    oh: usize,
    cw: f64,
    ch: f64,
    k: f64,
    ox: f64,
    oy: f64,
    /// Per output pixel: how many stores there were within 1e-3 LSB of a rounding tie.
    ties: RefCell<Vec<u8>>,
}

impl Geo {
    fn new(scene: &FrameScene) -> Geo {
        let (ow, oh) = (scene.output.size.w as f64, scene.output.size.h as f64);
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let k = (ow / cw).min(oh / ch);
        Geo { ow: ow as usize, oh: oh as usize, cw, ch, k, ox: (ow - k * cw) / 2.0, oy: (oh - k * ch) / 2.0, ties: RefCell::new(vec![0; ow as usize * oh as usize]) }
    }

    fn in_canvas(&self, i: usize, j: usize) -> bool {
        let (x, y) = (i as f64 + 0.5, j as f64 + 0.5);
        !(x < self.ox || x > self.ox + self.k * self.cw || y < self.oy || y > self.oy + self.k * self.ch)
    }

    /// Rounds `p` into a buffer pixel `n`, noting near-ties.
    fn store4(&self, n: usize, p: Px) -> [u8; 4] {
        if p.iter().any(|v| {
            let x = v.clamp(0.0, 1.0) * 255.0 + 0.5;
            (x - x.round()).abs() < 1e-3
        }) {
            let mut ties = self.ties.borrow_mut();
            ties[n] = ties[n].saturating_add(1);
        }
        store4(p)
    }

    /// Canvas position of the centre of output pixel `(i, j)`.
    fn canvas_pos(&self, i: usize, j: usize) -> (f64, f64) {
        ((i as f64 + 0.5 - self.ox) / self.k, (j as f64 + 0.5 - self.oy) / self.k)
    }
}

/// Tightly packed premultiplied RGBA8 of `scene.output.size`.
pub fn render_reference(scene: &FrameScene, inputs: &RenderInputs) -> Vec<u8> {
    render_reference_with_ties(scene, inputs).0
}

/// [`render_reference`] plus, per output pixel, the number of stores that fell (almost)
/// exactly on a rounding tie.
pub fn render_reference_with_ties(scene: &FrameScene, inputs: &RenderInputs) -> (Vec<u8>, Vec<u8>) {
    let g = Geo::new(scene);
    let bg = premul(scene.background);
    let mut buf: Vec<[u8; 4]> = (0..g.ow * g.oh).map(|n| if g.in_canvas(n % g.ow, n / g.ow) { store4(bg) } else { [0, 0, 0, 255] }).collect();
    draw_layers(&g, &scene.layers, &inputs.layers, &mut buf);
    (buf.into_iter().flatten().collect(), g.ties.into_inner())
}

fn premul(c: Rgba) -> Px {
    let a = c.a as f64;
    [c.r as f64 * a, c.g as f64 * a, c.b as f64 * a, a]
}

fn store(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

fn store4(p: Px) -> [u8; 4] {
    p.map(store)
}

fn load4(b: [u8; 4]) -> Px {
    b.map(|v| v as f64 / 255.0)
}

fn lerp(a: Px, b: Px, t: f64) -> Px {
    [0, 1, 2, 3].map(|c| a[c] + (b[c] - a[c]) * t)
}

fn blend(mode: BlendMode, s: Px, d: Px) -> Px {
    [0, 1, 2, 3].map(|c| match mode {
        BlendMode::Normal => s[c] + d[c] * (1.0 - s[3]),
        BlendMode::Add => (s[c] + d[c]).min(1.0),
        BlendMode::Multiply => s[c] * d[c] + s[c] * (1.0 - d[3]) + d[c] * (1.0 - s[3]),
        BlendMode::Screen => s[c] + d[c] - s[c] * d[c],
    })
}

/// Composites `layers` bottom to top into `buf`, storing (rounding) after every layer.
fn draw_layers(g: &Geo, layers: &[Layer], inputs: &[LayerInput], buf: &mut [[u8; 4]]) {
    for (n, layer) in layers.iter().enumerate() {
        let none = LayerInput::None;
        let input = inputs.get(n).unwrap_or(&none);
        match &layer.content {
            LayerContent::Transition(t) => {
                let (from_in, to_in): (&[LayerInput], &[LayerInput]) = match input {
                    LayerInput::Transition { from, to } => (from, to),
                    _ => (&[], &[]),
                };
                let mut from = vec![[0u8; 4]; buf.len()];
                let mut to = vec![[0u8; 4]; buf.len()];
                draw_layers(g, &t.from, from_in, &mut from);
                draw_layers(g, &t.to, to_in, &mut to);
                for j in 0..g.oh {
                    for i in 0..g.ow {
                        if !g.in_canvas(i, j) {
                            continue;
                        }
                        let n = j * g.ow + i;
                        let mixed = mix(g, &t.op, t.progress as f64, load4(from[n]), load4(to[n]), g.canvas_pos(i, j));
                        let s = mixed.map(|v| v * layer.opacity as f64);
                        buf[n] = g.store4(n, blend(layer.blend, s, load4(buf[n])));
                    }
                }
            }
            _ => draw_plain(g, layer, input, buf),
        }
    }
}

fn mix(g: &Geo, op: &TransitionOp, p: f64, from: Px, to: Px, canvas_pos: (f64, f64)) -> Px {
    match op {
        TransitionOp::Dissolve => lerp(from, to, p),
        TransitionOp::DipToColor(c) => {
            let cp = premul(*c);
            let dip = |x: Px| cp.map(|v| v * x[3]);
            if p < 0.5 { lerp(from, dip(from), 2.0 * p) } else { lerp(dip(to), to, 2.0 * p - 1.0) }
        }
        TransitionOp::Wipe { angle, softness } => {
            let u = ((*angle as f64).cos(), (*angle as f64).sin());
            let dots = [(0.0, 0.0), (g.cw, 0.0), (0.0, g.ch), (g.cw, g.ch)].map(|(x, y)| x * u.0 + y * u.1);
            let lo = dots.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = dots.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let w = *softness as f64;
            let edge = (lo - w / 2.0) + ((hi + w / 2.0) - (lo - w / 2.0)) * p;
            let e = (edge - (canvas_pos.0 * u.0 + canvas_pos.1 * u.1)) * g.k;
            let m = (0.5 + e / (w * g.k).max(1.0)).clamp(0.0, 1.0);
            lerp(from, to, m)
        }
    }
}

enum Source<'a> {
    Solid(Px),
    Texels(&'a CpuFrame),
}

fn draw_plain(g: &Geo, layer: &Layer, input: &LayerInput, buf: &mut [[u8; 4]]) {
    let fwd = layer.placement.to_canvas();
    let Some(inv) = fwd.inverse() else { return };
    let source = match (&layer.content, input) {
        (LayerContent::Solid(c), _) => Source::Solid(premul(*c)),
        (LayerContent::Media { .. }, LayerInput::Cpu(f)) => Source::Texels(f),
        _ => Source::Solid(premul(Rgba::MISSING)),
    };

    // Crop rectangle in output space, for the edge coverage.
    let c = layer.crop;
    let corners = [(c.x0, c.y0), (c.x1, c.y0), (c.x1, c.y1), (c.x0, c.y1)].map(|(x, y)| {
        let (cx, cy) = fwd.apply(x as f64, y as f64);
        (g.k * cx + g.ox, g.k * cy + g.oy)
    });
    let centroid = (corners.iter().map(|p| p.0).sum::<f64>() / 4.0, corners.iter().map(|p| p.1).sum::<f64>() / 4.0);

    for j in 0..g.oh {
        for i in 0..g.ow {
            if !g.in_canvas(i, j) {
                continue;
            }
            let centre = (i as f64 + 0.5, j as f64 + 0.5);
            let coverage = (0.5 + signed_distance(&corners, centroid, centre)).clamp(0.0, 1.0);

            let (cx, cy) = g.canvas_pos(i, j);
            let (lx, ly) = inv.apply(cx, cy);
            let mut sample = match &source {
                Source::Solid(p) => *p,
                Source::Texels(f) => bilinear(f, layer, lx, ly),
            };
            sample = apply_effects(&layer.effects, sample);
            let s = sample.map(|v| v * layer.opacity as f64 * coverage);
            let n = j * g.ow + i;
            buf[n] = g.store4(n, blend(layer.blend, s, load4(buf[n])));
        }
    }
}

/// Minimum over the four edges of the signed distance from `pt` to the edge's
/// line, positive on the side of the rectangle's centre.
fn signed_distance(corners: &[(f64, f64); 4], centroid: (f64, f64), pt: (f64, f64)) -> f64 {
    let mut dist = f64::INFINITY;
    for e in 0..4 {
        let (p, q) = (corners[e], corners[(e + 1) % 4]);
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let len = dx.hypot(dy);
        if len == 0.0 {
            continue;
        }
        let n = (-dy / len, dx / len);
        let mut d = n.0 * (pt.0 - p.0) + n.1 * (pt.1 - p.1);
        if n.0 * (centroid.0 - p.0) + n.1 * (centroid.1 - p.1) < 0.0 {
            d = -d;
        }
        dist = dist.min(d);
    }
    dist
}

/// The premultiplied texel `(x, y)` of `f`, honouring the frame's alpha mode.
fn texel(f: &CpuFrame, x: usize, y: usize) -> Px {
    let at = y * f.stride + x * 4;
    let b = &f.data[at..at + 4];
    let (r, g, bl, a) = (b[0] as f64 / 255.0, b[1] as f64 / 255.0, b[2] as f64 / 255.0, b[3] as f64 / 255.0);
    match f.color.alpha {
        AlphaMode::Opaque => [r, g, bl, 1.0],
        AlphaMode::Straight => [r * a, g * a, bl * a, a],
        AlphaMode::Premultiplied => [r, g, bl, a],
    }
}

/// One bilinear sample at local position `(lx, ly)`, confined to the crop.
fn bilinear(f: &CpuFrame, layer: &Layer, lx: f64, ly: f64) -> Px {
    let (tw, th) = (f.width as f64, f.height as f64);
    let (sx, sy) = (tw / layer.placement.size.x as f64, th / layer.placement.size.y as f64);
    // Crop in texels, intersected with the decoded frame.
    let cx0 = (layer.crop.x0 as f64 * sx).max(0.0);
    let cy0 = (layer.crop.y0 as f64 * sy).max(0.0);
    let cx1 = (layer.crop.x1 as f64 * sx).min(tw);
    let cy1 = (layer.crop.y1 as f64 * sy).min(th);
    let kx0 = (cx0 + 0.5).floor().min(tw - 1.0);
    let ky0 = (cy0 + 0.5).floor().min(th - 1.0);
    let kx1 = (cx1 + 0.5).floor().max(kx0 + 1.0).min(tw);
    let ky1 = (cy1 + 0.5).floor().max(ky0 + 1.0).min(th);

    let u = (lx * sx).clamp(kx0 + 0.5, kx1 - 0.5);
    let v = (ly * sy).clamp(ky0 + 0.5, ky1 - 0.5);
    let (i, j) = ((u - 0.5).floor(), (v - 0.5).floor());
    let (fx, fy) = (u - 0.5 - i, v - 0.5 - j);
    let idx = |x: f64, lo: f64, hi: f64| x.clamp(lo, hi - 1.0) as usize;
    let (xa, xb) = (idx(i, kx0, kx1), idx(i + 1.0, kx0, kx1));
    let (ya, yb) = (idx(j, ky0, ky1), idx(j + 1.0, ky0, ky1));
    let (t00, t10, t01, t11) = (texel(f, xa, ya), texel(f, xb, ya), texel(f, xa, yb), texel(f, xb, yb));
    [0, 1, 2, 3].map(|c| t00[c] * (1.0 - fx) * (1.0 - fy) + t10[c] * fx * (1.0 - fy) + t01[c] * (1.0 - fx) * fy + t11[c] * fx * fy)
}

/// Effects on unpremultiplied colour: `rgb / a` → chain → clamp once → `× a`.
fn apply_effects(effects: &[Effect], s: Px) -> Px {
    if effects.is_empty() {
        return s;
    }
    let a = s[3];
    if a <= 0.0 {
        return [0.0; 4];
    }
    let mut c = [s[0] / a, s[1] / a, s[2] / a];
    for e in effects {
        let Effect::ColorAdjust(adj) = e else { continue };
        // Exposure: linear light with a pure 2.4 power (odd extension for negatives, only
        // reachable in a chain of several adjusts, before the single final clamp).
        let gain = 2f64.powf(adj.exposure as f64);
        for v in c.iter_mut() {
            let m = v.abs().powf(2.4) * gain;
            *v = if *v < 0.0 { -m.powf(1.0 / 2.4) } else { m.powf(1.0 / 2.4) };
        }
        // Contrast about 0.5.
        for v in c.iter_mut() {
            *v = (*v - 0.5) * adj.contrast as f64 + 0.5;
        }
        // Temperature and tint.
        let (t, u) = (adj.temperature as f64, adj.tint as f64);
        c[0] *= 1.0 + 0.1 * t;
        c[2] *= 1.0 - 0.1 * t;
        c[1] *= 1.0 - 0.05 * u;
        // Saturation about Rec.709 luma.
        let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        for v in c.iter_mut() {
            *v = y + (*v - y) * adj.saturation as f64;
        }
    }
    [c[0].clamp(0.0, 1.0) * a, c[1].clamp(0.0, 1.0) * a, c[2].clamp(0.0, 1.0) * a, a]
}
