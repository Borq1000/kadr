//! The motion recipes (effects spec §5 «Движение»): each turns its
//! parameters and the frame's progress into a `PlacementDelta`. Numbers
//! are at intensity `k = 1` («Обычно»). Every motion keeps the clip's
//! visible crop window covering the footprint it filled ("без полей",
//! ruling R6): a zoom's fixed point stays inside the window; pan and shake
//! move by the window's size, about its centre, at its aspect.

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
/// … in the 0.12 timeline seconds that end at its moment.
pub const PUNCH_SECS: f64 = 0.12;
/// Плавная панорама: travel 6 % of the visible window (with the same scale-up).
pub const PAN_TRAVEL: f64 = 0.06;
/// Ручная камера: shifts up to 0.6 % of the visible window, turns up to 0.4° …
pub const SHAKE_SHIFT: f64 = 0.006;
pub const SHAKE_DEG: f64 = 0.4;
/// … swaying at ~0.7 Hz (ruling R7). This is the value noise's lattice
/// rate: smoothstep value noise crosses zero about every 2 lattice steps and
/// turns about every 1.5, so 2.4 Hz sways at ≈0.6 Hz by zero crossings and
/// ≈0.8 Hz by turns (measured over 600 s, `handheld_sways_at_about_0_7_hz`).
pub const SHAKE_HZ: f64 = 2.4;
const SALT_Y: u64 = 0x9E37_79B9_7F4A_7C15;
const SALT_ROT: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// `CompileCtx::window`, checked: edges clamped into 0..1; an empty or
/// non-finite window counts as the whole picture.
#[derive(Clone, Copy)]
struct Window {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Window {
    fn of(ctx: &CompileCtx) -> Window {
        let (x0, y0, x1, y1) = ctx.window;
        let c = |v: f64| v.clamp(0.0, 1.0);
        let (x0, y0, x1, y1) = (c(x0), c(y0), c(x1), c(y1));
        // NaN fails both comparisons
        if x0 < x1 && y0 < y1 { Window { x0, y0, x1, y1 } } else { Window { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 } }
    }

    fn centre(self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    /// Width and height as fractions of the picture's.
    fn size(self) -> (f64, f64) {
        (self.x1 - self.x0, self.y1 - self.y0)
    }

    /// The window's width / height on screen, for a picture of `aspect`.
    fn aspect(self, aspect: f64) -> f64 {
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 16.0 / 9.0 };
        let (w, h) = self.size();
        aspect * w / h
    }

    /// `point` (fractions of the picture) moved into the window.
    fn clamp(self, (x, y): (f64, f64)) -> (f64, f64) {
        (x.clamp(self.x0, self.x1), y.clamp(self.y0, self.y1))
    }
}

/// A zoom by `scale ≥ 1` about the Point, kept inside the window: every
/// window point then moves away from it, so the window still covers itself.
fn zoom(scale: f64, p: &Params, ctx: &CompileCtx) -> Compiled {
    Compiled::motion(PlacementDelta { scale, anchor: Window::of(ctx).clamp(p.point("point")), ..PlacementDelta::IDENTITY })
}

pub fn smooth_zoom(p: &Params, ctx: &CompileCtx) -> Compiled {
    let u = Curve::EaseInOut.apply(ctx.progress);
    let w = if p.choice("direction") == "out" { 1.0 - u } else { u };
    zoom(1.0 + ZOOM_AMOUNT * intensity(p.number("strength")) * w, p, ctx)
}

pub fn cinematic_push(p: &Params, ctx: &CompileCtx) -> Compiled {
    zoom(1.0 + PUSH_AMOUNT * intensity(p.number("strength")) * Curve::Cinematic.apply(ctx.progress), p, ctx)
}

/// Fully in AT the source moment (ruling R17): the Snap ramp runs in the
/// 0.12 s before it, so the moment's own frame — a beat, a hit — already
/// shows the whole punch; held to the end; no moment — no punch. The 0.12 s
/// are on screen: source time since the moment ÷ speed.
pub fn punch_in(p: &Params, ctx: &CompileCtx) -> Compiled {
    let w = p.moment("moment").map_or(0.0, |m| {
        // saturating: a moment from a damaged file never overflows a frame
        let since = Time(ctx.source_time.0.flicks().saturating_sub(m.0.flicks()));
        let elapsed = since.as_secs_f64() / ctx.speed.max(1e-9);
        // 0 until 0.12 s before the moment, 1 from the moment on
        Curve::Snap.apply(((elapsed + PUNCH_SECS) / PUNCH_SECS).clamp(0.0, 1.0))
    });
    zoom(1.0 + PUNCH_AMOUNT * intensity(p.number("strength")) * w, p, ctx)
}

/// The camera moves `direction` (refinement 7): the picture slides the
/// other way, from +travel/2 to −travel/2 of the window, scaled up by the
/// travel about the window's centre so no edge of the window shows.
pub fn smooth_pan(p: &Params, ctx: &CompileCtx) -> Compiled {
    let window = Window::of(ctx);
    let (ww, wh) = window.size();
    let travel = PAN_TRAVEL * intensity(p.number("strength"));
    let (dx, dy) = match p.choice("direction") {
        "left" => (-1.0, 0.0),
        "up" => (0.0, -1.0),
        "down" => (0.0, 1.0),
        _ => (1.0, 0.0),
    };
    let s = travel * (0.5 - Curve::EaseInOut.apply(ctx.progress));
    Compiled::motion(PlacementDelta { scale: 1.0 + travel, anchor: window.centre(), offset: (dx * s * ww, dy * s * wh), rotation_deg: 0.0 })
}

/// Noise on x, y and rotation over the effect's own clock, about the
/// window's centre, with a cover scale for the largest shift and turn: a
/// `w × h` window turned by θ and shifted by `d` (fractions of its size)
/// still covers its footprint when `s ≥ (1 + 2d)(1 + sin θ · max(w/h, h/w))`.
/// The scale is constant at a constant strength; an animated strength (or
/// crop) moves it along.
pub fn handheld(p: &Params, ctx: &CompileCtx) -> Compiled {
    let window = Window::of(ctx);
    let (ww, wh) = window.size();
    let k = intensity(p.number("strength"));
    let (shift, deg) = (SHAKE_SHIFT * k, SHAKE_DEG * k);
    let t = ctx.progress * ctx.clip_secs;
    let noise = |salt: u64, amplitude: f64| Noise1D { seed: ctx.seed ^ salt, frequency: SHAKE_HZ, amplitude }.at(t);
    let aspect = window.aspect(ctx.aspect);
    let cover = (1.0 + 2.0 * shift) * (1.0 + deg.to_radians().sin() * aspect.max(1.0 / aspect));
    Compiled::motion(PlacementDelta { scale: cover, anchor: window.centre(), offset: (noise(0, shift) * ww, noise(SALT_Y, shift) * wh), rotation_deg: noise(SALT_ROT, deg) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{recipe, registry, Category, CompileCtx};
    use kadr_core::Time;
    use kadr_project::{ParamValue, SourceTime};
    use kadr_scene::{Placement, SizeU, Vec2};
    use std::collections::BTreeMap;

    fn ctx(progress: f64) -> CompileCtx {
        CompileCtx { progress, clip_secs: 10.0, source_time: SourceTime(Time::from_secs(5)), speed: 1.0, canvas: SizeU::new(1920, 1080), aspect: 16.0 / 9.0, window: CompileCtx::UNCROPPED, frame: Time::ZERO, seed: 7 }
    }

    /// A picture `100·aspect × 100` exactly filling its canvas, so local
    /// pixels are canvas pixels.
    fn filling(aspect: f64) -> Placement {
        let (w, h) = (100.0 * aspect, 100.0);
        Placement { size: Vec2::new(w as f32, h as f32), anchor: Vec2::new(0.5, 0.5), position: Vec2::new((w / 2.0) as f32, (h / 2.0) as f32), scale: Vec2::new(1.0, 1.0), rotation: 0.0 }
    }

    /// Whether `d` keeps the crop `window` of `base` (fractions of the
    /// picture) covering the footprint it had: every corner of that
    /// footprint still shows a point of the window. `Err` names the corner.
    fn covers(d: &PlacementDelta, base: &Placement, window: (f64, f64, f64, f64)) -> Result<(), String> {
        let (w, h) = (base.size.x as f64, base.size.y as f64);
        let (x0, y0, x1, y1) = (window.0 * w, window.1 * h, window.2 * w, window.3 * h);
        let (before, inv) = (base.to_canvas(), d.apply(base).to_canvas().inverse().unwrap());
        let eps = 1e-3;
        for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            let (cx, cy) = before.apply(x, y);
            let (lx, ly) = inv.apply(cx, cy);
            if !(lx >= x0 - eps && lx <= x1 + eps && ly >= y0 - eps && ly <= y1 + eps) {
                return Err(format!("corner ({cx}, {cy}) shows ({lx}, {ly}) outside {x0}..{x1} × {y0}..{y1}"));
            }
        }
        Ok(())
    }

    /// Crop windows (x0, y0, x1, y1): none, one side (small and over half),
    /// letterbox, all four sides.
    const WINDOWS: [(f64, f64, f64, f64); 9] = [
        CompileCtx::UNCROPPED,
        (0.05, 0.0, 1.0, 1.0),
        (0.1, 0.0, 1.0, 1.0),
        (0.6, 0.0, 1.0, 1.0),
        (0.0, 0.0, 0.4, 1.0),
        (0.0, 0.0, 1.0, 0.3),
        (0.0, 0.12, 1.0, 0.88),
        (0.1, 0.2, 0.85, 0.95),
        (0.3, 0.05, 0.45, 0.9),
    ];

    fn run(id: &str, set: &[(&str, ParamValue)], c: &CompileCtx) -> PlacementDelta {
        let def = recipe(id).unwrap();
        let stored = set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        def.compile_clip(&Params::resolve(def.params, &stored, |_| None), c).placement
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
        assert!(close(at(4_000, 1.0), 1.0), "well before the moment");
        assert!(close(at(4_879, 1.0), 1.0), "before the ramp");
        assert!(close(at(4_880, 1.0), 1.0), "the ramp starts 0.12 s before the moment");
        assert!(close(at(4_940, 1.0), 1.0 + 0.15 * Curve::Snap.apply(0.5)));
        let just_before = at(4_990, 1.0);
        assert!(just_before > 1.14 && just_before < 1.15, "almost in a frame before: {just_before}");
        assert!(close(at(5_000, 1.0), 1.15), "the moment's own frame shows the whole punch");
        assert!(close(at(9_000, 1.0), 1.15), "and it holds");
        assert!(close(at(4_880, 2.0), 1.0 + 0.15 * Curve::Snap.apply(0.5)), "at 2× 0.12 s of source is 0.06 s on screen");
        assert!(close(at(4_760, 2.0), 1.0), "and the ramp starts 0.12 s on screen before the moment");
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
        for d in registry().iter().filter(|d| d.category == Category::Motion) {
            let zero = run(d.id, &[("strength", ParamValue::Number(0.0)), ("moment", ParamValue::Moment(SourceTime(Time::ZERO)))], &ctx(0.7));
            assert!(zero.is_identity(), "{}: {zero:?}", d.id);
        }
        let strong = run("smooth_zoom", &[("strength", ParamValue::Number(1.0))], &ctx(1.0));
        assert!(close(strong.scale, 1.0 + 0.10 / 0.6), "100 % = ×1.67");
    }

    /// Spec §5 "без полей", of the visible crop window (ruling R6): at any
    /// strength, progress, point and crop the moved picture still covers the
    /// footprint its window had — no black edge or corner shows.
    #[test]
    fn motion_never_shows_a_border() {
        for aspect in [16.0 / 9.0, 9.0 / 16.0, 1.0] {
            let base = filling(aspect);
            for window in WINDOWS {
                for d in registry().iter().filter(|d| d.category == Category::Motion) {
                    let points: &[(f64, f64)] = if d.param("point").is_some() { &[(0.5, 0.5), (0.0, 0.0), (1.0, 0.3)] } else { &[(0.5, 0.5)] };
                    let dirs: &[&str] = if d.param("direction").is_some() { &["in", "out", "left", "right", "up", "down"] } else { &["in"] };
                    for s in [0.3, 0.6, 1.0] {
                        for &point in points {
                            for &dir in dirs {
                                for i in 0..=40 {
                                    let mut c = ctx(i as f64 / 40.0);
                                    (c.aspect, c.window) = (aspect, window);
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
                                    if let Err(e) = covers(&run(d.id, &set, &c), &base, window) {
                                        panic!("{} s={s} p={point:?} {dir} i={i} aspect={aspect} window={window:?}: {e}", d.id);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// A window from elsewhere never breaks a frame: edges are clamped into
    /// the picture, an empty or non-finite one is the whole picture.
    #[test]
    fn a_broken_window_counts_as_the_whole_picture() {
        for id in ["smooth_zoom", "smooth_pan", "handheld"] {
            let whole = run(id, &[("point", ParamValue::Point { x: 0.1, y: 0.9 })], &ctx(0.3));
            for window in [(f64::NAN, 0.0, 1.0, 1.0), (0.8, 0.0, 0.2, 1.0), (0.0, 0.5, 1.0, 0.5), (-0.5, -1.0, 1.5, 2.0)] {
                let mut c = ctx(0.3);
                c.window = window;
                assert_eq!(run(id, &[("point", ParamValue::Point { x: 0.1, y: 0.9 })], &c), whole, "{id} {window:?}");
            }
        }
    }

    /// Handheld's cover scale against the worst a shake can do: both shifts
    /// and the turn at their maxima together, in every sign combination.
    #[test]
    fn handheld_covers_its_worst_case() {
        for aspect in [9.0 / 16.0, 16.0 / 9.0, 1.0, 1.0 / 3.0] {
            let base = filling(aspect);
            for window in WINDOWS {
                for s in [0.3, 0.6, 1.0] {
                    let mut c = ctx(0.5);
                    (c.aspect, c.window) = (aspect, window);
                    let shake = run("handheld", &[("strength", ParamValue::Number(s))], &c);
                    let (shift, deg) = (SHAKE_SHIFT * intensity(s), SHAKE_DEG * intensity(s));
                    let (ww, wh) = (window.2 - window.0, window.3 - window.1);
                    for sx in [-1.0, 1.0] {
                        for sy in [-1.0, 1.0] {
                            for sr in [-1.0, 1.0] {
                                let worst = PlacementDelta { offset: (sx * shift * ww, sy * shift * wh), rotation_deg: sr * deg, ..shake };
                                if let Err(e) = covers(&worst, &base, window) {
                                    panic!("s={s} aspect={aspect} window={window:?} signs=({sx}, {sy}, {sr}): {e}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Ruling R7: the shake reads as ~0.7 Hz. Over 600 s, on each axis and
    /// for several seeds, both ways of counting a sway land in 0.55..0.85 Hz:
    /// half the zero crossings per second (a back-and-forth crosses the rest
    /// position twice) and half the turns of direction per second.
    #[test]
    fn handheld_sways_at_about_0_7_hz() {
        let def = recipe("handheld").unwrap();
        let p = Params::resolve(def.params, &BTreeMap::new(), |_| None);
        const SECS: usize = 600;
        const RATE: usize = 100;
        for seed in [1u64, 7, 42, 0xDEAD_BEEF, u64::MAX] {
            let mut c = ctx(0.0);
            (c.seed, c.clip_secs) = (seed, SECS as f64);
            let shakes: Vec<PlacementDelta> = (0..=SECS * RATE)
                .map(|i| {
                    c.progress = i as f64 / (SECS * RATE) as f64;
                    def.compile_clip(&p, &c).placement
                })
                .collect();
            for axis in ["x", "y", "turn"] {
                let v: Vec<f64> = shakes.iter().map(|d| match axis { "x" => d.offset.0, "y" => d.offset.1, _ => d.rotation_deg }).collect();
                let crossings = v.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
                let steps: Vec<f64> = v.windows(2).map(|w| w[1] - w[0]).filter(|d| *d != 0.0).collect();
                let turns = steps.windows(2).filter(|s| (s[0] < 0.0) != (s[1] < 0.0)).count();
                let (by_crossings, by_turns) = (crossings as f64 / 2.0 / SECS as f64, turns as f64 / 2.0 / SECS as f64);
                assert!((0.55..=0.85).contains(&by_crossings) && (0.55..=0.85).contains(&by_turns), "seed {seed} {axis}: {by_crossings:.3} Hz by zero crossings, {by_turns:.3} Hz by turns");
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
        // Value noise moves at most amplitude × frequency × the largest
        // lattice step (2: values in −1..1) × smoothstep's steepest slope
        // (1.5) per second — this much per 1/30 s frame:
        let step = |amplitude: f64| amplitude * SHAKE_HZ * 2.0 * 1.5 / 30.0 + 1e-12;
        let mut prev = at(7, 0.0);
        for f in 1..300 {
            let d = at(7, f as f64 / 30.0);
            assert!(d.offset.0.abs() <= 0.006 + 1e-12 && d.offset.1.abs() <= 0.006 + 1e-12 && d.rotation_deg.abs() <= 0.4 + 1e-12, "{d:?}");
            let moved = ((d.offset.0 - prev.offset.0).abs(), (d.offset.1 - prev.offset.1).abs(), (d.rotation_deg - prev.rotation_deg).abs());
            assert!(moved.0 <= step(0.006) && moved.1 <= step(0.006) && moved.2 <= step(0.4), "frame {f}: a shake, not a jump: {moved:?}");
            prev = d;
        }
        assert_eq!(at(7, 3.3), at(7, 3.3), "deterministic");
        assert_ne!(at(7, 3.3), at(8, 3.3), "the seed (the effect's id) picks the shake");
        assert_eq!(at(7, 1.0).scale, at(7, 2.0).scale, "a constant cover scale at a constant strength: no breathing");
    }
}
