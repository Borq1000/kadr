//! Parity of the optimized `CpuRenderer` with an independent, naive
//! implementation of the rendering contract (`tests/reference`), on
//! deterministic pseudo-random scenes, plus hand-checked sanity tests of the
//! reference itself (so it is right, not merely equal).

mod reference;

use kadr_core::color::AlphaMode;
use kadr_core::{AssetId, ColorInfo, CpuFrame, Time};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::*;
use reference::render_reference;
use std::f32::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

// ───────────────────────────── helpers for hand-written scenes ─────────────────────────────

fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

fn output(w: u32, h: u32) -> OutputSpec {
    OutputSpec::new(SizeU::new(w, h), RenderQuality::Export)
}

fn scene(canvas: (u32, u32), out: (u32, u32), background: Rgba, layers: Vec<Layer>) -> FrameScene {
    FrameScene { time: Time::ZERO, canvas: SizeU::new(canvas.0, canvas.1), output: output(out.0, out.1), background, layers }
}

/// A full-canvas layer of `content` (opacity 1, Normal, no effects).
fn full_layer(canvas: (u32, u32), content: LayerContent) -> Layer {
    let c = SizeU::new(canvas.0, canvas.1);
    let placement = Placement::fill(c);
    Layer { id: LayerId(1), content, crop: placement.full_crop(), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
}

fn solid(canvas: (u32, u32), c: Rgba) -> Layer {
    full_layer(canvas, LayerContent::Solid(c))
}

fn transition(canvas: (u32, u32), op: TransitionOp, progress: f32, from: Vec<Layer>, to: Vec<Layer>) -> Layer {
    full_layer(canvas, LayerContent::Transition(Box::new(TransitionLayer { op, progress, from, to })))
}

/// Inputs for a scene made of Solid and Transition layers only.
fn solid_inputs(layers: &[Layer]) -> Vec<LayerInput> {
    layers
        .iter()
        .map(|l| match &l.content {
            LayerContent::Transition(t) => LayerInput::Transition { from: solid_inputs(&t.from), to: solid_inputs(&t.to) },
            _ => LayerInput::None,
        })
        .collect()
}

fn reference_of(s: &FrameScene) -> Vec<u8> {
    render_reference(s, &RenderInputs { layers: solid_inputs(&s.layers) })
}

fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let at = (y as usize * w as usize + x as usize) * 4;
    buf[at..at + 4].try_into().unwrap()
}

fn media_ref(size: SizeU) -> MediaRef {
    MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Image, display_size: size, color: ColorInfo::IMAGE_SRGB }
}

// ───────────────────────────────── reference sanity tests ─────────────────────────────────

#[test]
fn sanity_half_transparent_red_over_blue_is_128_0_128_255() {
    let mut red = solid((4, 4), rgba(1.0, 0.0, 0.0, 1.0));
    red.opacity = 0.5;
    let s = scene((4, 4), (4, 4), rgba(0.0, 0.0, 1.0, 1.0), vec![red]);
    let out = reference_of(&s);
    for (x, y) in [(0, 0), (3, 3), (1, 2)] {
        assert_eq!(px(&out, 4, x, y), [128, 0, 128, 255]);
    }
}

#[test]
fn sanity_letterbox_and_pillarbox_margins_are_opaque_black() {
    // 16×16 canvas into 32×16: k = 1, 8 px margins left and right.
    let s = scene((16, 16), (32, 16), rgba(1.0, 1.0, 1.0, 0.5), vec![solid((16, 16), rgba(1.0, 1.0, 1.0, 1.0))]);
    let out = reference_of(&s);
    for x in [0, 3, 7, 24, 28, 31] {
        assert_eq!(px(&out, 32, x, 5), [0, 0, 0, 255], "x = {x}");
    }
    for x in [8, 15, 23] {
        assert_eq!(px(&out, 32, x, 5), [255, 255, 255, 255], "x = {x}");
    }
    // 16×16 canvas into 16×32: margins above and below; the translucent background stays inside.
    let s = scene((16, 16), (16, 32), rgba(1.0, 0.0, 0.0, 0.5), vec![]);
    let out = reference_of(&s);
    assert_eq!(px(&out, 16, 4, 7), [0, 0, 0, 255]);
    assert_eq!(px(&out, 16, 4, 24), [0, 0, 0, 255]);
    assert_eq!(px(&out, 16, 4, 8), [128, 0, 0, 128]);
    assert_eq!(px(&out, 16, 4, 23), [128, 0, 0, 128]);
}

#[test]
fn sanity_saturation_zero_gives_rec709_grey() {
    let (r, g, b) = (0.8f32, 0.2f32, 0.4f32);
    let y = 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
    let expect = (y * 255.0 + 0.5).floor() as u8;
    assert_eq!(expect, 87, "hand-computed Y = 0.342");
    let mut l = solid((4, 4), rgba(r, g, b, 1.0));
    l.effects = vec![Effect::ColorAdjust(ColorAdjust { saturation: 0.0, ..ColorAdjust::NEUTRAL })];
    let out = reference_of(&scene((4, 4), (4, 4), Rgba::BLACK, vec![l.clone()]));
    assert_eq!(px(&out, 4, 2, 2), [87, 87, 87, 255]);
    // Translucent: grey is computed on the unpremultiplied colour, then × alpha (0.342 · 0.5 → 44).
    l.content = LayerContent::Solid(rgba(r, g, b, 0.5));
    let out = reference_of(&scene((4, 4), (4, 4), Rgba::BLACK, vec![l]));
    assert_eq!(px(&out, 4, 2, 2), [44, 44, 44, 255]);
}

#[test]
fn sanity_exposure_contrast_temperature_formulas() {
    // Exposure +1 EV on 0.5: (0.5^2.4 · 2)^(1/2.4) = 0.5 · 2^(1/2.4) = 0.6674 → 170.
    let mut l = solid((2, 2), rgba(0.5, 0.5, 0.5, 1.0));
    l.effects = vec![Effect::ColorAdjust(ColorAdjust { exposure: 1.0, ..ColorAdjust::NEUTRAL })];
    assert_eq!(px(&reference_of(&scene((2, 2), (2, 2), Rgba::BLACK, vec![l])), 2, 0, 0), [170, 170, 170, 255]);
    // Contrast 2 about 0.5: 0.25 → 0; 0.6 → 0.7 → 178.5 → 179.
    let mut l = solid((2, 2), rgba(0.25, 0.6, 0.5, 1.0));
    l.effects = vec![Effect::ColorAdjust(ColorAdjust { contrast: 2.0, ..ColorAdjust::NEUTRAL })];
    assert_eq!(px(&reference_of(&scene((2, 2), (2, 2), Rgba::BLACK, vec![l])), 2, 0, 0), [0, 179, 128, 255]);
    // Temperature +1: R × 1.1, B × 0.9; tint +1: G × 0.95. On (0.5, 0.5, 0.5): 0.55, 0.475, 0.45.
    let mut l = solid((2, 2), rgba(0.5, 0.5, 0.5, 1.0));
    l.effects = vec![Effect::ColorAdjust(ColorAdjust { temperature: 1.0, tint: 1.0, ..ColorAdjust::NEUTRAL })];
    assert_eq!(px(&reference_of(&scene((2, 2), (2, 2), Rgba::BLACK, vec![l])), 2, 0, 0), [140, 121, 115, 255]);
}

#[test]
fn sanity_dissolve_at_zero_is_from_and_at_one_is_to() {
    let cv = (8, 8);
    let (red, blue) = (rgba(1.0, 0.0, 0.0, 1.0), rgba(0.0, 0.0, 1.0, 1.0));
    let with = |p: f32| scene(cv, cv, rgba(0.0, 1.0, 0.0, 1.0), vec![transition(cv, TransitionOp::Dissolve, p, vec![solid(cv, red)], vec![solid(cv, blue)])]);
    let only = |c: Rgba| scene(cv, cv, rgba(0.0, 1.0, 0.0, 1.0), vec![solid(cv, c)]);
    assert_eq!(reference_of(&with(0.0)), reference_of(&only(red)));
    assert_eq!(reference_of(&with(1.0)), reference_of(&only(blue)));
    assert_eq!(px(&reference_of(&with(0.0)), 8, 3, 3), [255, 0, 0, 255]);
    assert_eq!(px(&reference_of(&with(1.0)), 8, 3, 3), [0, 0, 255, 255]);
    assert_eq!(px(&reference_of(&with(0.5)), 8, 3, 3), [128, 0, 128, 255]);
    // Buffers start transparent, so a one-sided dissolve fades the layer in over the background.
    let half = scene(cv, cv, rgba(0.0, 0.0, 1.0, 1.0), vec![transition(cv, TransitionOp::Dissolve, 0.5, vec![], vec![solid(cv, red)])]);
    assert_eq!(px(&reference_of(&half), 8, 0, 0), [128, 0, 128, 255]);
}

#[test]
fn sanity_dip_to_colour_and_wipe() {
    let cv = (16, 16);
    let (red, blue, white) = (rgba(1.0, 0.0, 0.0, 1.0), rgba(0.0, 0.0, 1.0, 1.0), rgba(1.0, 1.0, 1.0, 1.0));
    // p = 0.25: half way from red to white.
    let dip = |p: f32| scene(cv, cv, Rgba::BLACK, vec![transition(cv, TransitionOp::DipToColor(white), p, vec![solid(cv, red)], vec![solid(cv, blue)])]);
    assert_eq!(px(&reference_of(&dip(0.25)), 16, 5, 5), [255, 128, 128, 255]);
    assert_eq!(px(&reference_of(&dip(0.5)), 16, 5, 5), [255, 255, 255, 255]);
    assert_eq!(px(&reference_of(&dip(0.75)), 16, 5, 5), [128, 128, 255, 255]);
    // The dip colour appears only where the clip has coverage: an empty `from` stays the background.
    let pip = scene(cv, cv, rgba(0.0, 1.0, 0.0, 1.0), vec![transition(cv, TransitionOp::DipToColor(white), 0.25, vec![], vec![solid(cv, blue)])]);
    assert_eq!(px(&reference_of(&pip), 16, 5, 5), [0, 255, 0, 255]);
    // Hard wipe left → right at p = 0.5: pixels left of x = 8 show `to`, the rest `from`.
    let wipe = |p: f32| scene(cv, cv, Rgba::BLACK, vec![transition(cv, TransitionOp::Wipe { angle: 0.0, softness: 0.0 }, p, vec![solid(cv, red)], vec![solid(cv, blue)])]);
    let out = reference_of(&wipe(0.5));
    assert_eq!(px(&out, 16, 7, 3), [0, 0, 255, 255]);
    assert_eq!(px(&out, 16, 8, 3), [255, 0, 0, 255]);
    assert_eq!(px(&reference_of(&wipe(0.0)), 16, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&reference_of(&wipe(1.0)), 16, 15, 15), [0, 0, 255, 255]);
    // A soft wipe (w = 8 canvas pixels) at p = 0.5 is half and half on the centre line.
    let soft = scene(cv, cv, Rgba::BLACK, vec![transition(cv, TransitionOp::Wipe { angle: 0.0, softness: 8.0 }, 0.5, vec![solid(cv, red)], vec![solid(cv, blue)])]);
    let out = reference_of(&soft);
    assert_eq!(px(&out, 16, 7, 0), [(255.0f64 * (0.5 - 0.5 / 8.0) + 0.5) as u8, 0, (255.0f64 * (0.5 + 0.5 / 8.0) + 0.5) as u8, 255]);
}

#[test]
fn sanity_bilinear_is_confined_to_the_frame_and_flips_mirror() {
    // A 2×1 frame, black and white, on a 4×1 canvas.
    let data = vec![0, 0, 0, 255, 255, 255, 255, 255];
    let frame = Arc::new(CpuFrame::from_rgba8(2, 1, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, data));
    let cv = (4, 1);
    let media = |scale_x: f32| {
        let mut l = full_layer(cv, LayerContent::Media { media: media_ref(SizeU::new(2, 1)), source_time: Time::ZERO });
        l.placement.scale.x = scale_x;
        l
    };
    let render = |l: Layer| {
        let s = scene(cv, cv, Rgba::BLACK, vec![l]);
        render_reference(&s, &RenderInputs { layers: vec![LayerInput::Cpu(frame.clone())] })
    };
    let out = render(media(1.0));
    let row: Vec<u8> = (0..4).map(|x| px(&out, 4, x, 0)[0]).collect();
    assert_eq!(row, [0, 64, 191, 255]);
    let out = render(media(-1.0));
    let row: Vec<u8> = (0..4).map(|x| px(&out, 4, x, 0)[0]).collect();
    assert_eq!(row, [255, 191, 64, 0]);
}

#[test]
fn sanity_straight_texels_are_premultiplied_before_filtering() {
    // Left texel: opaque red. Right texel: fully transparent (with garbage green). Filtering
    // straight colour and alpha separately would leak green; premultiplying first gives pure
    // red fading with alpha: weights 0.75 / 0.25 at the two middle pixels.
    let data = vec![255, 0, 0, 255, 0, 255, 0, 0];
    let frame = Arc::new(CpuFrame::from_rgba8(2, 1, ColorInfo::IMAGE_SRGB, data));
    let cv = (4, 1);
    let l = full_layer(cv, LayerContent::Media { media: media_ref(SizeU::new(2, 1)), source_time: Time::ZERO });
    let s = scene(cv, cv, rgba(0.0, 0.0, 0.0, 0.0), vec![l]);
    let out = render_reference(&s, &RenderInputs { layers: vec![LayerInput::Cpu(frame)] });
    assert_eq!(px(&out, 4, 1, 0), [191, 0, 0, 191]);
    assert_eq!(px(&out, 4, 2, 0), [64, 0, 0, 64]);
    assert_eq!(px(&out, 4, 3, 0), [0, 0, 0, 0]);
}

#[test]
fn sanity_missing_media_is_drawn_as_missing_colour_and_singular_placement_draws_nothing() {
    let cv = (4, 4);
    let mut l = full_layer(cv, LayerContent::Media { media: media_ref(SizeU::new(4, 4)), source_time: Time::ZERO });
    let s = scene(cv, cv, Rgba::BLACK, vec![l.clone()]);
    let out = render_reference(&s, &RenderInputs { layers: vec![LayerInput::Missing(MissingReason::Offline)] });
    let m = Rgba::MISSING;
    let e = |v: f32| (v as f64 * 255.0 + 0.5).floor() as u8;
    assert_eq!(px(&out, 4, 1, 1), [e(m.r), e(m.g), e(m.b), 255]);
    l.placement.scale.x = 0.0;
    let s = scene(cv, cv, Rgba::BLACK, vec![l]);
    let out = render_reference(&s, &RenderInputs { layers: vec![LayerInput::Missing(MissingReason::Offline)] });
    assert_eq!(px(&out, 4, 1, 1), [0, 0, 0, 255]);
}

#[test]
fn sanity_layer_edge_coverage_is_antialiased_over_one_pixel() {
    // A 8×8 white square whose right edge lies half way through pixel 5: coverage 0.5 there.
    let cv = (8, 8);
    let mut l = solid(cv, rgba(1.0, 1.0, 1.0, 1.0));
    l.crop = RectF::new(0.0, 0.0, 5.5, 8.0);
    let out = reference_of(&scene(cv, cv, Rgba::BLACK, vec![l]));
    assert_eq!(px(&out, 8, 4, 2), [255, 255, 255, 255]);
    assert_eq!(px(&out, 8, 5, 2), [128, 128, 128, 255]);
    assert_eq!(px(&out, 8, 6, 2), [0, 0, 0, 255]);
}

// ─────────────────────────────────── random scene generator ───────────────────────────────────

/// xorshift64* — deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        // splitmix64 scramble so nearby seeds diverge and the state is never 0.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f()
    }
    /// Inclusive integer range.
    fn int(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % (hi - lo + 1) as u64) as u32
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f() < p
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
}

struct Gen {
    rng: Rng,
    canvas: SizeU,
    out: SizeU,
    next_id: u128,
}

/// True if some output pixel centre lies exactly on the canvas rectangle's boundary, where
/// «inside or margin» is a knife edge that f32 and f64 implementations may legitimately split.
fn has_boundary_tie(canvas: SizeU, out: SizeU) -> bool {
    let k = (out.w as f64 / canvas.w as f64).min(out.h as f64 / canvas.h as f64);
    let axes = [(out.w as f64, canvas.w as f64), (out.h as f64, canvas.h as f64)];
    axes.iter().any(|&(o, c)| {
        let off = (o - k * c) / 2.0;
        [off, off + k * c].iter().any(|b| {
            let t = b - 0.5;
            (t - t.round()).abs() < 1e-6 && b.fract() != 0.0
        })
    })
}

impl Gen {
    fn new(seed: u64) -> Gen {
        let mut rng = Rng::new(seed);
        let canvas = SizeU::new(rng.int(16, 96), rng.int(16, 96));
        let out = loop {
            let r = rng.f();
            let cand = if r < 0.15 {
                canvas
            } else if r < 0.25 {
                SizeU::new(canvas.w * 2, canvas.h * 2)
            } else if r < 0.65 {
                let s = rng.range(0.5, 1.5);
                SizeU::new(((canvas.w as f64 * s).round() as u32).max(1), ((canvas.h as f64 * s).round() as u32).max(1))
            } else {
                SizeU::new(rng.int(16, 120), rng.int(16, 120))
            };
            if !has_boundary_tie(canvas, cand) {
                break cand;
            }
        };
        Gen { rng, canvas, out, next_id: 1 }
    }

    fn colour(&mut self, opaque: bool) -> Rgba {
        let pick = |r: &mut Rng| match r.int(0, 5) {
            0 => 0.0,
            1 => 1.0,
            _ => r.f() as f32,
        };
        let (r, g, b) = (pick(&mut self.rng), pick(&mut self.rng), pick(&mut self.rng));
        let a = if opaque || self.rng.chance(0.5) { 1.0 } else if self.rng.chance(0.1) { 0.0 } else { self.rng.f() as f32 };
        Rgba { r, g, b, a }
    }

    fn frame(&mut self, w: u32, h: u32) -> CpuFrame {
        let mode = [AlphaMode::Opaque, AlphaMode::Straight, AlphaMode::Premultiplied][self.rng.int(0, 2) as usize];
        let flat = self.rng.chance(0.15).then(|| [self.rng.byte(), self.rng.byte(), self.rng.byte(), self.rng.byte()]);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            let (r, g, b) = match flat {
                Some(f) => (f[0], f[1], f[2]),
                None => (self.rng.byte(), self.rng.byte(), self.rng.byte()),
            };
            let a = match flat {
                Some(f) => f[3],
                None => match self.rng.int(0, 9) {
                    0..=3 => 255,
                    4 => 0,
                    _ => self.rng.byte(),
                },
            };
            match mode {
                // Premultiplied texels must have rgb ≤ a.
                AlphaMode::Premultiplied => data.extend([(r as u32 * a as u32 / 255) as u8, (g as u32 * a as u32 / 255) as u8, (b as u32 * a as u32 / 255) as u8, a]),
                _ => data.extend([r, g, b, a]),
            }
        }
        CpuFrame::from_rgba8(w, h, ColorInfo { alpha: mode, ..ColorInfo::WORKING_SDR }, data)
    }

    fn blend(&mut self, normal_bias: f64) -> BlendMode {
        if self.rng.chance(normal_bias) {
            BlendMode::Normal
        } else {
            [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen][self.rng.int(0, 3) as usize]
        }
    }

    fn opacity(&mut self) -> f32 {
        match self.rng.int(0, 9) {
            0..=3 => 1.0,
            4 => 0.0,
            _ => self.rng.f() as f32,
        }
    }

    fn effects(&mut self) -> Vec<Effect> {
        if !self.rng.chance(0.3) {
            return vec![];
        }
        let mut param = |neutral: f64, lo: f64, hi: f64| if self.rng.chance(0.4) { neutral as f32 } else { self.rng.range(lo, hi) as f32 };
        vec![Effect::ColorAdjust(ColorAdjust {
            exposure: param(0.0, -3.0, 3.0),
            contrast: param(1.0, 0.0, 2.0),
            saturation: param(1.0, 0.0, 2.0),
            temperature: param(0.0, -1.0, 1.0),
            tint: param(0.0, -1.0, 1.0),
        })]
    }

    fn placement(&mut self) -> Placement {
        let (cw, ch) = (self.canvas.w as f64, self.canvas.h as f64);
        let r = &mut self.rng;
        if r.chance(0.15) {
            return Placement::fill(self.canvas);
        }
        let size = Vec2::new((r.range(0.2, 1.3) * cw) as f32, (r.range(0.2, 1.3) * ch) as f32);
        let anchor = if r.chance(0.3) { Vec2::new(0.5, 0.5) } else { Vec2::new(r.f() as f32, r.f() as f32) };
        let position = if r.chance(0.2) { Vec2::new((cw / 2.0) as f32, (ch / 2.0) as f32) } else { Vec2::new((r.range(-0.3, 1.3) * cw) as f32, (r.range(-0.3, 1.3) * ch) as f32) };
        let axis = |r: &mut Rng| {
            if r.chance(0.015) {
                return 0.0f32; // singular: the layer draws nothing
            }
            let mag = if r.chance(0.2) { 1.0 } else { r.range(0.1, 2.0) } as f32;
            if r.chance(0.15) { -mag } else { mag }
        };
        let (sx, sy) = (axis(r), axis(r));
        let scale = if r.chance(0.3) { Vec2::new(sx, if sy == 0.0 { sx } else { sx.abs().copysign(sy) }) } else { Vec2::new(sx, sy) };
        let rotation = match r.int(0, 19) {
            0..=8 => 0.0,
            9..=11 => FRAC_PI_2,
            _ => r.range(0.0, 2.0 * std::f64::consts::PI) as f32,
        };
        Placement { size, anchor, position, scale, rotation }
    }

    fn crop(&mut self, size: Vec2) -> RectF {
        let (w, h) = (size.x as f64, size.y as f64);
        let r = &mut self.rng;
        match r.int(0, 19) {
            0..=7 => RectF::new(0.0, 0.0, size.x, size.y),
            8..=16 => {
                let (x0, y0) = (r.range(0.0, 0.8), r.range(0.0, 0.8));
                let (x1, y1) = (r.range(x0 + 0.05, 1.0), r.range(y0 + 0.05, 1.0));
                RectF::new((x0 * w) as f32, (y0 * h) as f32, (x1 * w) as f32, (y1 * h) as f32)
            }
            _ => {
                // Extends beyond the content on some sides, but still overlaps it.
                let (x0, y0) = (r.range(-0.3, 0.6), r.range(-0.3, 0.6));
                let (x1, y1) = (r.range(x0.max(0.0) + 0.1, 1.4), r.range(y0.max(0.0) + 0.1, 1.4));
                RectF::new((x0 * w) as f32, (y0 * h) as f32, (x1 * w) as f32, (y1 * h) as f32)
            }
        }
    }

    fn id(&mut self) -> LayerId {
        self.next_id += 1;
        LayerId(self.next_id)
    }

    fn layer(&mut self, allow_transition: bool) -> (Layer, LayerInput) {
        if allow_transition && self.rng.chance(0.3) {
            return self.transition();
        }
        let kind = self.rng.f();
        // Full-frame fast-path candidates: a frame as big as the output, placed to fill the canvas.
        if kind < 0.15 {
            let frame = Arc::new(self.frame(self.out.w, self.out.h));
            let placement = Placement::fill(self.canvas);
            let (opacity, blend) = (if self.rng.chance(0.7) { 1.0 } else { self.opacity() }, self.blend(0.7));
            let effects = if self.rng.chance(0.7) { vec![] } else { self.effects() };
            let media = MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Video, display_size: self.canvas, color: frame.color };
            let layer = Layer { id: self.id(), content: LayerContent::Media { media, source_time: Time::ZERO }, crop: placement.full_crop(), placement, opacity, blend, effects };
            return (layer, LayerInput::Cpu(frame));
        }
        let placement = self.placement();
        let crop = self.crop(placement.size);
        let (opacity, blend, effects) = (self.opacity(), self.blend(0.6), self.effects());
        let (content, input) = if kind < 0.65 {
            let (w, h) = (self.rng.int(1, 64), self.rng.int(1, 64));
            let frame = Arc::new(self.frame(w, h));
            let media = MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Image, display_size: SizeU::new(w, h), color: frame.color };
            (LayerContent::Media { media, source_time: Time::ZERO }, LayerInput::Cpu(frame))
        } else if kind < 0.85 {
            (LayerContent::Solid(self.colour(false)), LayerInput::None)
        } else {
            let media = MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Video, display_size: self.canvas, color: ColorInfo::WORKING_SDR };
            (LayerContent::Media { media, source_time: Time::ZERO }, LayerInput::Missing(MissingReason::Offline))
        };
        (Layer { id: self.id(), content, placement, crop, opacity, blend, effects }, input)
    }

    fn transition(&mut self) -> (Layer, LayerInput) {
        let op = match self.rng.int(0, 2) {
            0 => TransitionOp::Dissolve,
            1 => TransitionOp::DipToColor(self.colour(true)),
            _ => {
                let angle = match self.rng.int(0, 9) {
                    0 => 0.0,
                    1 => FRAC_PI_2,
                    2 => PI,
                    _ => self.rng.range(0.0, 2.0 * std::f64::consts::PI) as f32,
                };
                let softness = if self.rng.chance(0.25) { 0.0 } else { self.rng.range(0.0, 20.0) as f32 };
                TransitionOp::Wipe { angle, softness }
            }
        };
        let progress = match self.rng.int(0, 9) {
            0 => 0.0,
            1 => 1.0,
            2 => 0.5,
            _ => self.rng.f() as f32,
        };
        let side = |g: &mut Gen| {
            let n = g.rng.int(0, 2);
            (0..n).map(|_| g.layer(false)).unzip::<Layer, LayerInput, Vec<_>, Vec<_>>()
        };
        let (from, from_in) = side(self);
        let (to, to_in) = side(self);
        let placement = Placement::fill(self.canvas);
        let (opacity, blend) = if self.rng.chance(0.75) { (1.0, BlendMode::Normal) } else { (self.opacity(), self.blend(0.3)) };
        let layer = Layer {
            id: self.id(),
            content: LayerContent::Transition(Box::new(TransitionLayer { op, progress, from, to })),
            crop: placement.full_crop(),
            placement,
            opacity,
            blend,
            effects: vec![],
        };
        (layer, LayerInput::Transition { from: from_in, to: to_in })
    }

    fn scene(mut self) -> (FrameScene, RenderInputs) {
        let background = match self.rng.int(0, 4) {
            0 => Rgba::BLACK,
            1 | 2 => self.colour(true),
            _ => self.colour(false),
        };
        let n = self.rng.int(1, 4);
        let (layers, inputs): (Vec<Layer>, Vec<LayerInput>) = (0..n).map(|_| self.layer(true)).unzip();
        let time = Time::ZERO;
        let scene = FrameScene { time, canvas: self.canvas, output: OutputSpec::new(self.out, RenderQuality::Export), background, layers };
        (scene, RenderInputs { layers: inputs })
    }
}

fn scene_seed(base: u64, index: u64) -> u64 {
    base ^ index.wrapping_mul(0xD6E8_FEB8_6659_FD93)
}

fn env_num(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

// ───────────────────────────────────────── parity ─────────────────────────────────────────

const BASE_SEED: u64 = 0x4B41_4452_5245_4E44;

/// Renders random scenes with `CpuRenderer` and with the reference; every byte must agree
/// within 1 LSB. `PARITY_SCENES=n` changes the count, `PARITY_ONLY=i` runs just scene `i`.
#[test]
fn cpu_renderer_matches_the_reference_on_random_scenes() {
    let count = env_num("PARITY_SCENES").unwrap_or(400);
    let only = env_num("PARITY_ONLY");
    let mut renderer = CpuRenderer::new();
    let (mut max_diff, mut failing, mut fast_paths, mut drawn, mut rendered) = (0u8, 0usize, 0u64, 0u64, 0usize);
    let mut reports = Vec::new();

    for index in (0..count).filter(|i| only.is_none_or(|o| o == *i)) {
        let seed = scene_seed(BASE_SEED, index);
        let (scene, inputs) = Gen::new(seed).scene();
        let expected = render_reference(&scene, &inputs);

        let (w, h) = (scene.output.size.w, scene.output.size.h);
        let mut actual = vec![0xAAu8; w as usize * h as usize * 4];
        let frame = PreparedFrame { scene: &scene, inputs: &inputs };
        let stats = renderer.render(&frame, &mut RenderTarget::Cpu(CpuTarget::packed(w, h, &mut actual))).unwrap_or_else(|e| panic!("scene {index} (seed {seed:#x}): render failed: {e}"));
        fast_paths += stats.fast_paths as u64;
        drawn += stats.layers_drawn as u64;
        rendered += 1;

        let worst = expected.iter().zip(&actual).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        max_diff = max_diff.max(worst);
        if worst > 1 {
            failing += 1;
            if reports.len() < 3 {
                let (at, (e, a)) = expected.iter().zip(&actual).enumerate().find(|(_, (e, a))| e.abs_diff(**a) > 1).unwrap();
                let (pix, ch) = (at / 4, at % 4);
                let bad = expected.iter().zip(&actual).filter(|(e, a)| e.abs_diff(**a) > 1).count();
                reports.push(format!(
                    "scene {index} (seed {seed:#x}, output {w}x{h}, canvas {}x{}): first bad pixel ({}, {}) channel {} reference {} renderer {} (max diff {worst}, {bad} bad bytes)\n  reference px {:?}, renderer px {:?}\n  scene: {:?}\n  inputs: {:?}",
                    scene.canvas.w,
                    scene.canvas.h,
                    pix % w as usize,
                    pix / w as usize,
                    ["r", "g", "b", "a"][ch],
                    e,
                    a,
                    &expected[pix * 4..pix * 4 + 4],
                    &actual[pix * 4..pix * 4 + 4],
                    scene,
                    inputs,
                ));
            }
        }
    }

    println!("parity: {rendered} scenes, {drawn} layers drawn, {fast_paths} by a fast path, max byte difference {max_diff}, {failing} scenes over tolerance");
    for r in &reports {
        eprintln!("{r}\n");
    }
    assert_eq!(failing, 0, "{failing} of {rendered} scenes differ from the reference by more than 1 LSB (max diff {max_diff}); first ones above");
}

/// The generator itself is deterministic and covers what the parity test claims to cover.
#[test]
fn generator_is_deterministic_and_varied() {
    let a = Gen::new(scene_seed(BASE_SEED, 7)).scene().0;
    let b = Gen::new(scene_seed(BASE_SEED, 7)).scene().0;
    let shape = |s: &FrameScene| (s.canvas, s.output.size, s.background, s.layers.iter().map(|l| (l.placement, l.crop, l.opacity, l.blend)).collect::<Vec<_>>());
    assert_eq!(shape(&a), shape(&b));

    let (mut transitions, mut media, mut missing, mut solids, mut crops_beyond, mut letterbox, mut full_frame) = (0, 0, 0, 0, 0, 0, 0);
    for i in 0..400 {
        let (s, inputs) = Gen::new(scene_seed(BASE_SEED, i)).scene();
        let k = (s.output.size.w as f64 / s.canvas.w as f64).min(s.output.size.h as f64 / s.canvas.h as f64);
        if (s.output.size.w as f64 - k * s.canvas.w as f64).abs() > 1e-9 || (s.output.size.h as f64 - k * s.canvas.h as f64).abs() > 1e-9 {
            letterbox += 1;
        }
        for (l, inp) in s.layers.iter().zip(&inputs.layers) {
            match (&l.content, inp) {
                (LayerContent::Transition(_), _) => transitions += 1,
                (LayerContent::Solid(_), _) => solids += 1,
                (LayerContent::Media { .. }, LayerInput::Missing(_)) => missing += 1,
                (LayerContent::Media { .. }, LayerInput::Cpu(f)) => {
                    media += 1;
                    if f.width == s.output.size.w && f.height == s.output.size.h && l.placement == Placement::fill(s.canvas) {
                        full_frame += 1;
                    }
                }
                _ => {}
            }
            if l.crop.x0 < 0.0 || l.crop.x1 > l.placement.size.x {
                crops_beyond += 1;
            }
        }
    }
    println!("generator over 400 scenes: {media} media, {solids} solid, {missing} missing, {transitions} transitions, {crops_beyond} crops beyond content, {letterbox} scenes with margins, {full_frame} full-frame layers");
    assert!(transitions > 100 && media > 300 && missing > 20 && solids > 50 && crops_beyond > 10 && letterbox > 50 && full_frame > 20);
}
