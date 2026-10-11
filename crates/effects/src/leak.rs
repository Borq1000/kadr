//! Засветка (Light Leak, effects spec §5 «Свет», D7): light leaking into
//! the picture from a corner — coarse value noise and a soft radial glow,
//! both tinted, in Screen over the clip's own picture (also on a
//! picture-in-picture), drifting slowly with the effect's progress.
//! Strength scales their opacity by `k = s / 0.6`. The numbers are tuned
//! with the owner (refinement 8): only here.

use crate::params::{intensity, Params};
use crate::registry::{CompileCtx, Compiled, Overlay};
use kadr_core::hash::fmix64;
use kadr_scene::{BlendMode, Generator, Gradient, GradientShape, GradientStop, Noise, Rgba, Vec2};

/// The glow's and the noise's opacity at «Обычно».
pub const GLOW_OPACITY: f64 = 0.55;
pub const NOISE_OPACITY: f64 = 0.4;
/// The glow's centre travels this fraction of the picture over the whole progress.
pub const DRIFT: f64 = 0.2;
/// The noise's time over the whole progress, in slices.
pub const NOISE_SLICES: f64 = 2.0;
/// The noise lattice: this fraction of the picture's short side.
pub const NOISE_CELL: f64 = 0.45;
pub const NOISE_CONTRAST: f64 = 1.6;
/// The glow's radius: this fraction of the picture's diagonal.
pub const GLOW_RADIUS: f64 = 0.85;

/// The tint of a colour key: warm (the default), pink, cold.
pub fn leak_colour(key: &str) -> Rgba {
    match key {
        "pink" => Rgba { r: 1.0, g: 0.45, b: 0.62, a: 1.0 },
        "cold" => Rgba { r: 0.45, g: 0.7, b: 1.0, a: 1.0 },
        _ => Rgba { r: 1.0, g: 0.6, b: 0.25, a: 1.0 },
    }
}

/// A variant's corner (fractions of the picture) and its glow's drift over
/// the whole progress: "1" top left, "2" top right, "3" bottom left.
fn path(variant: &str) -> ((f64, f64), (f64, f64)) {
    match variant {
        "2" => ((1.0, 0.0), (-DRIFT, DRIFT / 2.0)),
        "3" => ((0.0, 1.0), (DRIFT, -DRIFT / 2.0)),
        _ => ((0.0, 0.0), (DRIFT, DRIFT / 2.0)),
    }
}

/// The `light_leak` recipe: noise, then the glow, both in Screen at their
/// opacity × k (at most 1), seeded by the effect and its variant.
pub fn light_leak(p: &Params, ctx: &CompileCtx) -> Compiled {
    let k = intensity(p.number("strength"));
    let colour = leak_colour(p.choice("color"));
    let variant = p.choice("variant");
    let (w, h) = ctx.picture();
    let u = if ctx.progress.is_finite() { ctx.progress.clamp(0.0, 1.0) } else { 0.0 };
    let ((x0, y0), (dx, dy)) = path(variant);
    let centre = Vec2::new(((x0 + dx * u) * w) as f32, ((y0 + dy * u) * h) as f32);
    let stop = |pos: f32, a: f32| GradientStop { pos, colour: Rgba { a, ..colour } };
    let glow = Gradient { shape: GradientShape::Radial { centre, radius: (GLOW_RADIUS * w.hypot(h)) as f32 }, stops: vec![stop(0.0, 1.0), stop(0.4, 0.45), stop(1.0, 0.0)] };
    let salt = variant.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
    let noise = Noise { cell: (NOISE_CELL * w.min(h)) as f32, seed: ctx.seed ^ fmix64(salt), time: (u * NOISE_SLICES) as f32, colour, amount: 1.0, contrast: NOISE_CONTRAST as f32 };
    let opacity = |base: f64| (base * k).clamp(0.0, 1.0) as f32;
    Compiled::overlays(vec![
        Overlay { generator: Generator::Noise(noise), blend: BlendMode::Screen, opacity: opacity(NOISE_OPACITY) },
        Overlay { generator: Generator::Gradient(glow), blend: BlendMode::Screen, opacity: opacity(GLOW_OPACITY) },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::recipe;
    use kadr_core::Time;
    use kadr_project::ParamValue;
    use kadr_scene::{GradientShape, SizeU};

    fn ctx(progress: f64, aspect: f64) -> CompileCtx {
        CompileCtx { progress, aspect, ..CompileCtx::still(SizeU::new(1920, 1080), Time::ZERO, 99) }
    }

    fn leak(set: &[(&str, ParamValue)], c: &CompileCtx) -> Vec<Overlay> {
        let def = recipe("light_leak").unwrap();
        let stored = set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let out = def.compile_clip(&Params::resolve(def.params, &stored, |_| None), c);
        assert!(out.placement.is_identity() && out.effects.is_empty(), "a leak only adds light");
        out.overlays
    }

    #[test]
    fn a_light_leak_is_noise_and_a_glow_in_screen_on_the_picture() {
        let o = leak(&[], &ctx(0.0, 16.0 / 9.0));
        assert_eq!(o.len(), 2);
        assert!(o.iter().all(|o| o.blend == BlendMode::Screen));
        assert_eq!((o[0].opacity, o[1].opacity), (NOISE_OPACITY as f32, GLOW_OPACITY as f32), "«Обычно»: the leak as designed");
        let Generator::Noise(n) = &o[0].generator else { panic!("noise first") };
        assert_eq!((n.cell, n.colour), ((NOISE_CELL * 1080.0) as f32, leak_colour("warm")));
        let Generator::Gradient(g) = &o[1].generator else { panic!("then the glow") };
        assert_eq!(g.shape, GradientShape::Radial { centre: Vec2::new(0.0, 0.0), radius: (GLOW_RADIUS * 1920f64.hypot(1080.0)) as f32 }, "variant 1: the top-left corner at progress 0");
    }

    #[test]
    fn the_leak_lies_on_the_picture_of_the_clip_s_aspect() {
        let c = CompileCtx::still(SizeU::new(1920, 1080), Time::ZERO, 1);
        assert_eq!(c.picture(), (1920.0, 1080.0));
        assert_eq!(CompileCtx { aspect: 9.0 / 16.0, ..c }.picture(), (607.5, 1080.0), "a vertical clip, pillarboxed");
        assert_eq!(CompileCtx { aspect: 4.0, ..c }.picture(), (1920.0, 480.0), "a wide one, letterboxed");
        assert_eq!(CompileCtx { aspect: f64::NAN, ..c }.picture(), (1920.0, 1080.0), "no usable aspect: the canvas's");
        let o = leak(&[("variant", ParamValue::Choice("2".into()))], &ctx(1.0, 9.0 / 16.0));
        let Generator::Gradient(g) = &o[1].generator else { panic!() };
        let GradientShape::Radial { centre, .. } = g.shape else { panic!() };
        assert!((0.0..=607.5).contains(&centre.x) && (0.0..=1080.0).contains(&centre.y), "on the vertical picture: {centre:?}");
        let Generator::Noise(n) = &o[0].generator else { panic!() };
        assert_eq!(n.cell, (NOISE_CELL * 607.5) as f32, "the picture's short side, not the canvas's");
    }

    #[test]
    fn strength_scales_the_leak_and_colours_variants_and_progress_change_it() {
        let at = |s: f64| leak(&[("strength", ParamValue::Number(s))], &ctx(0.5, 16.0 / 9.0));
        assert_eq!(at(0.3)[1].opacity, (GLOW_OPACITY * crate::intensity(0.3)) as f32, "«Мягко» = half");
        assert_eq!(at(1.0)[1].opacity, (GLOW_OPACITY * crate::intensity(1.0)).min(1.0) as f32);
        let colour = |c: &str| match &leak(&[("color", ParamValue::Choice(c.into()))], &ctx(0.5, 16.0 / 9.0))[0].generator {
            Generator::Noise(n) => n.colour,
            _ => panic!(),
        };
        assert!(colour("warm") != colour("pink") && colour("pink") != colour("cold"));
        assert!(colour("warm").r > colour("warm").b && colour("cold").b > colour("cold").r);
        let seed = |v: &str| match &leak(&[("variant", ParamValue::Choice(v.into()))], &ctx(0.5, 16.0 / 9.0))[0].generator {
            Generator::Noise(n) => n.seed,
            _ => panic!(),
        };
        assert!(seed("1") != seed("2") && seed("2") != seed("3"), "each variant its own noise");
        assert_ne!(leak(&[], &ctx(0.2, 16.0 / 9.0)), leak(&[], &ctx(0.8, 16.0 / 9.0)), "it drifts with progress");
        assert_eq!(leak(&[], &ctx(0.5, 16.0 / 9.0)), leak(&[], &ctx(0.5, 16.0 / 9.0)), "deterministic");
    }
}
