//! Transitions (effects spec §5 «Переходы», D4, D5): recipes on a cut. A
//! transition's `compile` turns the window's progress into a mix of the two
//! sides (a `TransitionOp`), a canvas shift of each side, colour primitives
//! on each side and a generator above the mix — a [`TransitionBuild`]; the
//! evaluator (`kadr-timeline::scene`) applies it to the clips' own layers.
//! `left` means the pictures (or the edge) move leftwards: the incoming one
//! comes from the right.

use crate::params::{ParamKind, Params};
use crate::registry::{Overlay, RecipeDef};
use kadr_core::ease::Curve;
use kadr_core::{FrameRate, Time, TransitionId};
use kadr_project::ParamValue;
use kadr_scene::{BlendMode, ColorAdjust, Effect, Generator, Gradient, GradientShape, GradientStop, Rgba, SizeU, TransitionOp};
use std::collections::BTreeMap;

/// A new transition's length (spec §5): 0.5 s.
pub const DEFAULT_SECS: f64 = 0.5;
/// «Плёнка»: the exposure lift half way, EV (`FILM_LIFT_EV · sin(π·p)`).
pub const FILM_LIFT_EV: f64 = 0.35;
/// Шторка at softness 1: a band this fraction of the canvas diagonal wide.
pub const WIPE_SOFT_DIAGONAL: f64 = 0.25;
/// Световая волна: the band's length along its direction, as a fraction of
/// the canvas's extent there, and its peak opacity.
pub const SWEEP_BAND: f64 = 0.35;
pub const SWEEP_OPACITY: f64 = 0.85;

/// What a transition's `compile` knows (D5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionCtx {
    /// Linear progress through the (clamped) window, 0..1.
    pub progress: f64,
    pub canvas: SizeU,
    /// The output being made: Сдвиг puts its seam on its pixel grid.
    pub output: SizeU,
    /// From the transition's id ([`transition_seed`]).
    pub seed: u64,
}

/// One frame of a transition (D5, refinement 5).
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionBuild {
    pub op: TransitionOp,
    /// The mix's progress.
    pub progress: f32,
    /// How far each side moves on the canvas, in canvas widths (x) and
    /// heights (y) — its clip's overlays with it.
    pub from_shift: (f64, f64),
    pub to_shift: (f64, f64),
    /// Colour primitives appended to each side's clip layer.
    pub from_effects: Vec<Effect>,
    pub to_effects: Vec<Effect>,
    /// A full-canvas generator above the mix.
    pub overlay: Option<Overlay>,
}

impl TransitionBuild {
    /// A plain mix: `op` at `progress` (clamped into 0..1, NaN → 0), nothing else.
    pub fn mix(op: TransitionOp, progress: f64) -> TransitionBuild {
        let p = if progress.is_nan() { 0.0 } else { progress.clamp(0.0, 1.0) };
        TransitionBuild { op, progress: p as f32, from_shift: (0.0, 0.0), to_shift: (0.0, 0.0), from_effects: vec![], to_effects: vec![], overlay: None }
    }
}

/// Every declared parameter of a cut recipe at its default (a cut has no
/// point or moment).
pub fn transition_params(def: &RecipeDef) -> BTreeMap<String, ParamValue> {
    def.params
        .iter()
        .filter_map(|d| {
            let v = match d.kind {
                ParamKind::Number { default, .. } => ParamValue::Number(default),
                ParamKind::Choice { default, .. } => ParamValue::Choice(default.to_string()),
                ParamKind::Point | ParamKind::Moment => return None,
            };
            Some((d.key.to_string(), v))
        })
        .collect()
}

/// A new transition's length at `rate`: [`DEFAULT_SECS`] on whole frames, at least two.
pub fn default_duration(rate: FrameRate) -> Time {
    rate.snap(Time::from_secs_f64(DEFAULT_SECS)).max(rate.frame_to_time(2))
}

/// A transition's noise seed: from its id.
pub fn transition_seed(id: TransitionId) -> u64 {
    let u = id.0.as_u128();
    (u as u64) ^ ((u >> 64) as u64)
}

/// The unit direction a key names (y down): `left` (the default), `right`, `up`, `down`.
fn direction(key: &str) -> (f64, f64) {
    match key {
        "right" => (1.0, 0.0),
        "up" => (0.0, -1.0),
        "down" => (0.0, 1.0),
        _ => (-1.0, 0.0),
    }
}

/// Растворение (Clean Fade): a dissolve; «Плёнка» lifts both sides'
/// exposure towards the middle, `FILM_LIFT_EV · sin(π·p)`.
pub fn clean_fade(p: &Params, ctx: &TransitionCtx) -> TransitionBuild {
    let mut b = TransitionBuild::mix(TransitionOp::Dissolve, ctx.progress);
    if p.choice("style") == "film" {
        let lift = FILM_LIFT_EV * (std::f64::consts::PI * b.progress as f64).sin();
        if lift > 1e-6 {
            let e = Effect::ColorAdjust(ColorAdjust { exposure: lift as f32, ..ColorAdjust::NEUTRAL });
            (b.from_effects, b.to_effects) = (vec![e.clone()], vec![e]);
        }
    }
    b
}

/// Через цвет (Fade Through Color): a dip to black or white.
pub fn fade_through_color(p: &Params, ctx: &TransitionCtx) -> TransitionBuild {
    let colour = if p.choice("color") == "white" { Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 } } else { Rgba::BLACK };
    TransitionBuild::mix(TransitionOp::DipToColor(colour), ctx.progress)
}

/// Сдвиг (Smooth Push): both pictures slide one canvas length in the
/// direction (Cinematic) — the outgoing one leaves, the incoming one
/// follows it in, edge to edge, `Over` — with the seam on a whole output
/// pixel (refinement 4).
pub fn smooth_push(p: &Params, ctx: &TransitionCtx) -> TransitionBuild {
    let (dx, dy) = direction(p.choice("direction"));
    let e = seam_on_pixels(Curve::Cinematic.apply(ctx.progress), dx + dy > 0.0, dx != 0.0, ctx);
    TransitionBuild { from_shift: (dx * e, dy * e), to_shift: (dx * (e - 1.0), dy * (e - 1.0)), ..TransitionBuild::mix(TransitionOp::Over, ctx.progress) }
}

/// `e` (how far the pictures have moved, in canvas lengths) moved so the
/// seam between them — at `e` of the canvas from its start when they move
/// right or down, `1 − e` when left or up — falls on a whole output pixel
/// of the contract's map `output = k·canvas + o`. 0 and 1 stay; an empty
/// output changes nothing.
fn seam_on_pixels(e: f64, towards_positive: bool, horizontal: bool, ctx: &TransitionCtx) -> f64 {
    let (cw, ch) = (ctx.canvas.w as f64, ctx.canvas.h as f64);
    let (ow, oh) = (ctx.output.w as f64, ctx.output.h as f64);
    let k = (ow / cw).min(oh / ch);
    if !(k.is_finite() && k > 0.0) || e <= 0.0 || e >= 1.0 {
        return e;
    }
    let (len, o) = if horizontal { (cw, (ow - k * cw) / 2.0) } else { (ch, (oh - k * ch) / 2.0) };
    let seam = if towards_positive { e * len } else { (1.0 - e) * len };
    let snapped = ((k * seam + o).round() - o) / k;
    (if towards_positive { snapped / len } else { 1.0 - snapped / len }).clamp(0.0, 1.0)
}

/// Световая волна's colours: warm (the default), white, cold.
fn sweep_colour(key: &str) -> Rgba {
    match key {
        "white" => Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
        "cold" => Rgba { r: 0.72, g: 0.86, b: 1.0, a: 1.0 },
        _ => Rgba { r: 1.0, g: 0.86, b: 0.62, a: 1.0 },
    }
}

/// Световая волна (Light Sweep): a dissolve (Smooth) under a soft band of
/// light crossing the canvas in the direction, in Screen — a band before the
/// canvas at the start of the window, past it at the end.
pub fn light_sweep(p: &Params, ctx: &TransitionCtx) -> TransitionBuild {
    let u = Curve::EaseInOut.apply(ctx.progress);
    let (dx, dy) = direction(p.choice("direction"));
    let (cw, ch) = (ctx.canvas.w as f64, ctx.canvas.h as f64);
    // The canvas's extent along the direction: its corners' dot products.
    let dots = [0.0, cw * dx, ch * dy, cw * dx + ch * dy];
    let lo = dots.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = dots.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let band = SWEEP_BAND * (hi - lo);
    // A canvas pixel of margin at either end: the band's clear ends stay off
    // the canvas though its angle and offset are stored as f32 (π, ±π/2 are
    // not exact there); the peak still crosses the centre half way.
    let offset = (lo - band - 1.0) + (hi - lo + band + 2.0) * u;
    let c = sweep_colour(p.choice("color"));
    let clear = Rgba { a: 0.0, ..c };
    let stops = vec![GradientStop { pos: 0.0, colour: clear }, GradientStop { pos: 0.5, colour: c }, GradientStop { pos: 1.0, colour: clear }];
    let gradient = Gradient { shape: GradientShape::Linear { angle: dy.atan2(dx) as f32, offset: offset as f32, length: band as f32 }, stops };
    TransitionBuild {
        overlay: Some(Overlay { generator: Generator::Gradient(gradient), blend: BlendMode::Screen, opacity: SWEEP_OPACITY as f32 }),
        ..TransitionBuild::mix(TransitionOp::Dissolve, u)
    }
}

/// Шторка (Wipe): the straight edge in the direction; softness 1 = a band
/// a quarter of the canvas diagonal wide, 0 a hard edge. `left` is the old
/// wipe (`xfade=wipeleft`, angle π).
pub fn wipe(p: &Params, ctx: &TransitionCtx) -> TransitionBuild {
    use std::f32::consts::{FRAC_PI_2, PI};
    let angle = match p.choice("direction") {
        "right" => 0.0,
        "up" => -FRAC_PI_2,
        "down" => FRAC_PI_2,
        _ => PI,
    };
    let diagonal = (ctx.canvas.w as f64).hypot(ctx.canvas.h as f64);
    let softness = (p.number("softness") * WIPE_SOFT_DIAGONAL * diagonal) as f32;
    TransitionBuild::mix(TransitionOp::Wipe { angle, softness }, ctx.progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::recipe;
    use kadr_scene::Generator;

    fn ctx(progress: f64) -> TransitionCtx {
        TransitionCtx { progress, canvas: SizeU::new(1920, 1080), output: SizeU::new(1920, 1080), seed: 7 }
    }

    fn choice(v: &str) -> ParamValue {
        ParamValue::Choice(v.into())
    }

    fn build(id: &str, set: &[(&str, ParamValue)], c: &TransitionCtx) -> TransitionBuild {
        let def = recipe(id).unwrap();
        let mut stored = transition_params(def);
        stored.extend(set.iter().map(|(k, v)| (k.to_string(), v.clone())));
        def.compile_cut(&Params::resolve(def.params, &stored, |_| None), c).expect("a cut recipe")
    }

    /// D1, saved semantics: the recipes the old kinds became draw what the
    /// kinds drew — the same op, the same linear progress, nothing else.
    #[test]
    fn the_old_kinds_compile_to_their_old_ops_with_linear_progress() {
        let plain = |b: &TransitionBuild| b.from_shift == (0.0, 0.0) && b.to_shift == (0.0, 0.0) && b.from_effects.is_empty() && b.to_effects.is_empty() && b.overlay.is_none();
        for p in [0.0, 0.25, 0.5, 0.987_654_321, 1.0] {
            let fade = build("clean_fade", &[("style", choice("clean"))], &ctx(p));
            assert_eq!((fade.op, fade.progress), (TransitionOp::Dissolve, p as f32));
            let dip = build("fade_through_color", &[("color", choice("black"))], &ctx(p));
            assert_eq!((dip.op, dip.progress), (TransitionOp::DipToColor(Rgba::BLACK), p as f32));
            let wipe = build("wipe", &[("direction", choice("left")), ("softness", ParamValue::Number(0.0))], &ctx(p));
            assert_eq!((wipe.op, wipe.progress), (TransitionOp::Wipe { angle: std::f32::consts::PI, softness: 0.0 }, p as f32));
            assert!(plain(&fade) && plain(&dip) && plain(&wipe), "{p}");
        }
    }

    #[test]
    fn film_lifts_both_sides_towards_the_middle_and_white_dips_to_white() {
        let film = |p: f64| build("clean_fade", &[("style", choice("film"))], &ctx(p));
        let lift = |b: &TransitionBuild| match b.from_effects.as_slice() {
            [Effect::ColorAdjust(a)] => a.exposure as f64,
            [] => 0.0,
            other => panic!("{other:?}"),
        };
        let mid = film(0.5);
        assert!((lift(&mid) - FILM_LIFT_EV).abs() < 1e-6 && mid.from_effects == mid.to_effects, "both sides, peaking half way");
        assert!(lift(&film(0.25)) > 0.0 && lift(&film(0.25)) < FILM_LIFT_EV * 0.75);
        for p in [0.0, 1.0] {
            assert!(film(p).from_effects.is_empty() && film(p).to_effects.is_empty(), "no lift at the window's ends: {p}");
        }
        assert_eq!(build("fade_through_color", &[("color", choice("white"))], &ctx(0.3)).op, TransitionOp::DipToColor(Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }));
    }

    /// Сдвиг: the outgoing picture leaves, the incoming one follows edge to
    /// edge (Cinematic), and the seam lands on a whole output pixel.
    #[test]
    fn a_push_slides_both_sides_one_canvas_and_puts_the_seam_on_a_pixel() {
        for (dir, axis) in [("left", (-1.0, 0.0)), ("right", (1.0, 0.0)), ("up", (0.0, -1.0)), ("down", (0.0, 1.0))] {
            let start = build("smooth_push", &[("direction", choice(dir))], &ctx(0.0));
            assert_eq!((start.op, start.from_shift, start.to_shift), (TransitionOp::Over, (0.0, 0.0), (-axis.0, -axis.1)), "{dir}: the incoming one waits a canvas away");
            let end = build("smooth_push", &[("direction", choice(dir))], &ctx(1.0));
            assert_eq!((end.from_shift, end.to_shift), (axis, (0.0, 0.0)), "{dir}");
            for p in [0.1, 0.37, 0.5, 0.9] {
                let b = build("smooth_push", &[("direction", choice(dir))], &ctx(p));
                assert!(((b.from_shift.0 - b.to_shift.0) - axis.0).abs() < 1e-12 && ((b.from_shift.1 - b.to_shift.1) - axis.1).abs() < 1e-12, "{dir} {p}: edge to edge");
                let moved = b.from_shift.0.abs() + b.from_shift.1.abs();
                assert!((moved - Curve::Cinematic.apply(p)).abs() <= 1.0 / 1080.0, "{dir} {p}: Cinematic, give or take a pixel");
            }
        }
        // 1920 × 1080 into 1000 × 600: k = 0.52083, letterboxed by 18.75 px.
        let c = TransitionCtx { output: SizeU::new(1000, 600), ..ctx(0.43) };
        let k = 1000.0 / 1920.0;
        let left = build("smooth_push", &[("direction", choice("left"))], &c);
        let seam = k * 1920.0 * (1.0 + left.from_shift.0);
        assert!((seam - seam.round()).abs() < 1e-6, "the seam on a whole pixel: {seam}");
        let down = build("smooth_push", &[("direction", choice("down"))], &c);
        let seam_y = k * 1080.0 * down.from_shift.1 + (600.0 - k * 1080.0) / 2.0;
        assert!((seam_y - seam_y.round()).abs() < 1e-6, "{seam_y}");
    }

    #[test]
    fn a_wipe_runs_in_its_direction_with_a_band_of_its_softness() {
        use std::f32::consts::{FRAC_PI_2, PI};
        let angle = |dir: &str| match build("wipe", &[("direction", choice(dir))], &ctx(0.5)).op {
            TransitionOp::Wipe { angle, .. } => angle,
            other => panic!("{other:?}"),
        };
        assert_eq!((angle("left"), angle("right"), angle("up"), angle("down")), (PI, 0.0, -FRAC_PI_2, FRAC_PI_2));
        let soft = |s: f64| match build("wipe", &[("softness", ParamValue::Number(s))], &ctx(0.5)).op {
            TransitionOp::Wipe { softness, .. } => softness as f64,
            other => panic!("{other:?}"),
        };
        assert_eq!(soft(0.0), 0.0, "a hard edge");
        assert!((soft(1.0) - WIPE_SOFT_DIAGONAL * 1920f64.hypot(1080.0)).abs() < 1e-3);
    }

    #[test]
    fn the_sweep_band_crosses_the_canvas_above_a_dissolve() {
        let band = |p: f64, dir: &str| {
            let b = build("light_sweep", &[("direction", choice(dir))], &ctx(p));
            assert_eq!(b.op, TransitionOp::Dissolve);
            assert!((b.progress as f64 - Curve::EaseInOut.apply(p)).abs() < 1e-6);
            let o = b.overlay.expect("a band");
            assert_eq!((o.blend, o.opacity), (BlendMode::Screen, SWEEP_OPACITY as f32));
            let Generator::Gradient(g) = o.generator else { panic!("a gradient") };
            g
        };
        let points = [(0.0, 0.0), (1920.0, 0.0), (0.0, 1080.0), (1920.0, 1080.0), (960.0, 540.0)];
        for dir in ["left", "right", "up", "down"] {
            for p in [0.0, 1.0] {
                let g = band(p, dir);
                assert!(points.iter().all(|&(x, y)| g.at(x, y)[3] < 1e-9), "{dir} at {p}: off the canvas");
            }
            let g = band(0.5, dir);
            assert!((g.t(960.0, 540.0) - 0.5).abs() < 1e-3 && (g.at(960.0, 540.0)[3] - 1.0).abs() < 1e-3, "{dir}: its peak crosses the centre half way");
        }
        assert!(band(0.3, "right").t(960.0, 540.0) > 0.5, "moving right: at 0.3 the peak is still left of the centre");
        let white = band(0.5, "right");
        let b = build("light_sweep", &[("color", choice("white"))], &ctx(0.5));
        let Some(Overlay { generator: Generator::Gradient(g), .. }) = b.overlay else { panic!() };
        assert_eq!(g.stops[1].colour, Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 });
        assert_ne!(white.stops[1].colour, g.stops[1].colour, "the default is warm");
    }

    #[test]
    fn a_cut_recipe_s_defaults_and_a_new_transition_s_length() {
        assert_eq!(transition_params(recipe("light_sweep").unwrap()), BTreeMap::from([("direction".to_string(), choice("left")), ("color".to_string(), choice("warm"))]));
        assert_eq!(transition_params(recipe("wipe").unwrap())["softness"], ParamValue::Number(0.25));
        assert_eq!(default_duration(FrameRate::FPS_30), Time::from_millis(500));
        assert_eq!(default_duration(FrameRate::FPS_29_97), FrameRate::FPS_29_97.frame_to_time(15), "0.5 s on whole frames");
        let id = TransitionId::new();
        assert_eq!(transition_seed(id), transition_seed(id));
        assert_ne!(transition_seed(id), transition_seed(TransitionId::new()));
    }
}
