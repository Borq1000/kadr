//! The motion recipes (effects spec §5 «Движение»): each turns its
//! parameters and the frame's progress into a `PlacementDelta`. Numbers
//! are at intensity `k = 1` («Обычно»); every motion keeps the picture
//! covering the frame it filled ("без полей").

use crate::params::{intensity, Params};
use crate::registry::{CompileCtx, Compiled};
use crate::{Noise1D, PlacementDelta};
use kadr_core::ease::Curve;
use kadr_core::Time;

/// Плавный наезд: scale 1.00 → 1.10.
pub const ZOOM_AMOUNT: f64 = 0.10;
/// Кино-наезд: scale 1.00 → 1.05.
pub const PUSH_AMOUNT: f64 = 0.05;
/// Резкий наезд: scale 1.00 → 1.15 …
pub const PUNCH_AMOUNT: f64 = 0.15;
/// … in 0.12 timeline seconds.
pub const PUNCH_SECS: f64 = 0.12;
/// Плавная панорама: travel 6 % of the frame (with the same scale-up).
pub const PAN_TRAVEL: f64 = 0.06;
/// Ручная камера: 0.6 % of the frame, 0.4°, ~0.7 Hz.
pub const SHAKE_SHIFT: f64 = 0.006;
pub const SHAKE_DEG: f64 = 0.4;
pub const SHAKE_HZ: f64 = 0.7;
const SALT_Y: u64 = 0x9E37_79B9_7F4A_7C15;
const SALT_ROT: u64 = 0xC2B2_AE3D_27D4_EB4F;

fn zoom(scale: f64, p: &Params) -> Compiled {
    Compiled::motion(PlacementDelta { scale, anchor: p.point("point"), ..PlacementDelta::IDENTITY })
}

pub fn smooth_zoom(p: &Params, ctx: &CompileCtx) -> Compiled {
    let u = Curve::EaseInOut.apply(ctx.progress);
    let w = if p.choice("direction") == "out" { 1.0 - u } else { u };
    zoom(1.0 + ZOOM_AMOUNT * intensity(p.number("strength")) * w, p)
}

pub fn cinematic_push(p: &Params, ctx: &CompileCtx) -> Compiled {
    zoom(1.0 + PUSH_AMOUNT * intensity(p.number("strength")) * Curve::Cinematic.apply(ctx.progress), p)
}

/// In at the source moment, held to the end; no moment — no punch. The
/// 0.12 s are on screen: source time since the moment ÷ speed.
pub fn punch_in(p: &Params, ctx: &CompileCtx) -> Compiled {
    let w = p.moment("moment").map_or(0.0, |m| {
        // saturating: a moment from a damaged file never overflows a frame
        let since = Time(ctx.source_time.0.flicks().saturating_sub(m.0.flicks()));
        let elapsed = since.as_secs_f64() / ctx.speed.max(1e-9);
        if elapsed < 0.0 { 0.0 } else { Curve::Snap.apply(elapsed / PUNCH_SECS) }
    });
    zoom(1.0 + PUNCH_AMOUNT * intensity(p.number("strength")) * w, p)
}

/// The camera moves `direction` (refinement 7): the picture slides the
/// other way, from +travel/2 to −travel/2, scaled up by the travel so no
/// edge shows.
pub fn smooth_pan(p: &Params, ctx: &CompileCtx) -> Compiled {
    let travel = PAN_TRAVEL * intensity(p.number("strength"));
    let (dx, dy) = match p.choice("direction") {
        "left" => (-1.0, 0.0),
        "up" => (0.0, -1.0),
        "down" => (0.0, 1.0),
        _ => (1.0, 0.0),
    };
    let s = travel * (0.5 - Curve::EaseInOut.apply(ctx.progress));
    Compiled::motion(PlacementDelta { scale: 1.0 + travel, anchor: (0.5, 0.5), offset: (dx * s, dy * s), rotation_deg: 0.0 })
}

/// Noise on x, y and rotation over the effect's own clock; one constant
/// cover scale for the largest shift and turn: a `w × h` picture turned by
/// θ and shifted by `d` (fractions) stays over the frame when
/// `s ≥ (1 + 2d)(1 + sin θ · max(w/h, h/w))`.
pub fn handheld(p: &Params, ctx: &CompileCtx) -> Compiled {
    let k = intensity(p.number("strength"));
    let (shift, deg) = (SHAKE_SHIFT * k, SHAKE_DEG * k);
    let t = ctx.progress * ctx.clip_secs;
    let noise = |salt: u64, amplitude: f64| Noise1D { seed: ctx.seed ^ salt, frequency: SHAKE_HZ, amplitude }.at(t);
    let aspect = if ctx.aspect.is_finite() && ctx.aspect > 0.0 { ctx.aspect } else { 16.0 / 9.0 };
    let cover = (1.0 + 2.0 * shift) * (1.0 + deg.to_radians().sin() * aspect.max(1.0 / aspect));
    Compiled::motion(PlacementDelta { scale: cover, anchor: (0.5, 0.5), offset: (noise(0, shift), noise(SALT_Y, shift)), rotation_deg: noise(SALT_ROT, deg) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{recipe, registry, CompileCtx};
    use kadr_core::Time;
    use kadr_project::{ParamValue, SourceTime};
    use kadr_scene::{Placement, SizeU, Vec2};

    fn ctx(progress: f64) -> CompileCtx {
        CompileCtx { progress, clip_secs: 10.0, source_time: SourceTime(Time::from_secs(5)), speed: 1.0, canvas: SizeU::new(1920, 1080), aspect: 16.0 / 9.0, frame: Time::ZERO, seed: 7 }
    }

    fn run(id: &str, set: &[(&str, ParamValue)], c: &CompileCtx) -> PlacementDelta {
        let def = recipe(id).unwrap();
        let stored = set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        (def.compile)(&Params::resolve(def.params, &stored, |_| None), c).placement
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn normal_strength_matches_the_spec_table() {
        assert!(close(run("smooth_zoom", &[], &ctx(0.0)).scale, 1.0));
        assert!(close(run("smooth_zoom", &[], &ctx(1.0)).scale, 1.10));
        assert!(close(run("smooth_zoom", &[], &ctx(0.5)).scale, 1.05), "Smooth = ease in-out: half way at ½");
        let out = [("direction", ParamValue::Choice("out".into()))];
        assert!(close(run("smooth_zoom", &out, &ctx(0.0)).scale, 1.10));
        assert!(close(run("smooth_zoom", &out, &ctx(1.0)).scale, 1.0));
        let at = [("point", ParamValue::Point { x: 0.2, y: 0.9 })];
        assert_eq!(run("smooth_zoom", &at, &ctx(1.0)).anchor, (0.2, 0.9));
        assert!(close(run("cinematic_push", &[], &ctx(1.0)).scale, 1.05));
        assert!(close(run("cinematic_push", &[], &ctx(0.5)).scale, 1.0 + 0.05 * Curve::Cinematic.apply(0.5)));
        let pan = run("smooth_pan", &[], &ctx(0.0));
        assert!(close(pan.scale, 1.06) && close(pan.offset.0, 0.03) && pan.offset.1 == 0.0, "{pan:?}");
        assert!(close(run("smooth_pan", &[], &ctx(1.0)).offset.0, -0.03), "the camera pans right: the picture slides left");
        let up = run("smooth_pan", &[("direction", ParamValue::Choice("up".into()))], &ctx(0.0));
        assert!(close(up.offset.1, -0.03) && up.offset.0 == 0.0, "{up:?}");
    }

    #[test]
    fn punch_in_happens_at_its_moment_in_timeline_time_and_holds() {
        let m = [("moment", ParamValue::Moment(SourceTime(Time::from_secs(5))))];
        let at = |src_ms: i64, speed: f64| {
            let mut c = ctx(0.5);
            c.source_time = SourceTime(Time::from_millis(src_ms));
            c.speed = speed;
            run("punch_in", &m, &c).scale
        };
        assert!(close(at(4_999, 1.0), 1.0), "before the moment");
        assert!(close(at(5_000, 1.0), 1.0), "the moment itself starts it");
        assert!(close(at(5_060, 1.0), 1.0 + 0.15 * Curve::Snap.apply(0.5)));
        assert!(close(at(5_120, 1.0), 1.15), "0.12 s later: in");
        assert!(close(at(9_000, 1.0), 1.15), "and it holds");
        assert!(close(at(5_120, 2.0), 1.0 + 0.15 * Curve::Snap.apply(0.5)), "at 2× 0.12 s of source is 0.06 s on screen");
        assert!(close(run("punch_in", &[], &ctx(0.5)).scale, 1.0), "no moment: no punch");
    }

    /// `Params::resolve` passes a stored moment through unchecked: one at the
    /// ends of the i64 range (a damaged file) must not overflow a frame.
    #[test]
    fn punch_in_survives_a_moment_at_the_ends_of_time() {
        let at = |m: i64| run("punch_in", &[("moment", ParamValue::Moment(SourceTime(Time(m))))], &ctx(0.5)).scale;
        assert!(close(at(i64::MIN), 1.15), "long ago: in");
        assert!(close(at(i64::MAX), 1.0), "never comes");
    }

    #[test]
    fn strength_scales_the_motion_and_zero_is_none() {
        for d in registry() {
            let zero = run(d.id, &[("strength", ParamValue::Number(0.0)), ("moment", ParamValue::Moment(SourceTime(Time::ZERO)))], &ctx(0.7));
            assert!(zero.is_identity(), "{}: {zero:?}", d.id);
        }
        let strong = run("smooth_zoom", &[("strength", ParamValue::Number(1.0))], &ctx(1.0));
        assert!(close(strong.scale, 1.0 + 0.10 / 0.6), "100 % = ×1.67");
    }

    /// Spec §5 "без полей": at any strength and progress the moved picture
    /// still covers the frame it filled — no black edge or corner shows.
    #[test]
    fn motion_never_shows_a_border() {
        for aspect in [16.0 / 9.0, 9.0 / 16.0, 1.0] {
            let (w, h) = (100.0 * aspect, 100.0);
            let base = Placement { size: Vec2::new(w as f32, h as f32), anchor: Vec2::new(0.5, 0.5), position: Vec2::new((w / 2.0) as f32, (h / 2.0) as f32), scale: Vec2::new(1.0, 1.0), rotation: 0.0 };
            for d in registry() {
                for s in [0.3, 0.6, 1.0] {
                    for point in [(0.5, 0.5), (0.0, 0.0), (1.0, 0.3)] {
                        for dir in ["in", "out", "left", "right", "up", "down"] {
                            for i in 0..=40 {
                                let mut c = ctx(i as f64 / 40.0);
                                c.aspect = aspect;
                                c.source_time = SourceTime(Time::from_millis(4_900 + i * 10));
                                let set: Vec<(&str, ParamValue)> = [
                                    ("strength", ParamValue::Number(s)),
                                    ("point", ParamValue::Point { x: point.0, y: point.1 }),
                                    ("moment", ParamValue::Moment(SourceTime(Time::from_secs(5)))),
                                    ("direction", ParamValue::Choice(dir.into())),
                                ]
                                .into_iter()
                                .filter(|(k, _)| d.param(k).is_some())
                                .collect();
                                let moved = run(d.id, &set, &c).apply(&base);
                                let inv = moved.to_canvas().inverse().unwrap();
                                for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
                                    let (lx, ly) = inv.apply(x, y);
                                    let eps = 1e-3;
                                    assert!(lx >= -eps && lx <= w + eps && ly >= -eps && ly <= h + eps, "{} s={s} p={point:?} {dir} i={i} aspect={aspect}: corner ({x}, {y}) shows ({lx}, {ly})", d.id);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn handheld_is_seeded_bounded_and_smooth() {
        let at = |seed: u64, secs: f64| {
            let mut c = ctx(secs / 10.0);
            c.seed = seed;
            run("handheld", &[], &c)
        };
        let mut prev = at(7, 0.0);
        for f in 1..300 {
            let d = at(7, f as f64 / 30.0);
            assert!(d.offset.0.abs() <= 0.006 + 1e-12 && d.offset.1.abs() <= 0.006 + 1e-12 && d.rotation_deg.abs() <= 0.4 + 1e-12, "{d:?}");
            assert!((d.offset.0 - prev.offset.0).abs() < 0.001 && (d.rotation_deg - prev.rotation_deg).abs() < 0.05, "frame {f}: a shake, not a jump");
            prev = d;
        }
        assert_eq!(at(7, 3.3), at(7, 3.3), "deterministic");
        assert_ne!(at(7, 3.3), at(8, 3.3), "the seed (the effect's id) picks the shake");
        assert_eq!(at(7, 1.0).scale, at(7, 2.0).scale, "a constant cover scale: no breathing");
    }
}
