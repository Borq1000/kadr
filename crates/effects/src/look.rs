//! Looks and light (effects spec §5): the `look` recipe — one of the presets
//! in `crates/effects/looks.json`, data and not code (§3: a new Look is a
//! record plus its two catalog names) — and `vignette`. Both compile to
//! colour primitives; at strength `s` every preset value moves from its
//! neutral by `k = s / 0.6` (§2): `k = 1` is the preset as designed.

use crate::params::{intensity, Params};
use crate::registry::{CompileCtx, Compiled};
use kadr_core::hash::fmix64;
use kadr_scene::{ColorAdjust, Effect, Grain, SizeU, SplitTone, ToneCurve, Vignette};
use serde::Deserialize;
use std::sync::LazyLock;

/// The preset a new Look starts with (spec §5's first).
pub const DEFAULT_PRESET: &str = "cinematic";
/// A Look's vignette has the Vignette effect's shape at softness 0.5 (refinement 7).
pub const LOOK_VIGNETTE_MIDPOINT: f64 = 0.45;
pub const LOOK_VIGNETTE_FEATHER: f64 = 0.5;
/// Grain cells: 2 canvas pixels per 1080 pixels of the canvas's short side,
/// at least 1 (refinement 6, ruling E2-R4).
pub const GRAIN_CELL_PER_1080: f64 = 2.0;
/// The Vignette effect at `k = 1` (D4): amount 0.35, midpoint 0.45,
/// feather `0.15 + 0.7 · softness`.
pub const VIGNETTE_AMOUNT: f64 = 0.35;
pub const VIGNETTE_MIDPOINT: f64 = 0.45;
pub const VIGNETTE_FEATHER_MIN: f64 = 0.15;
pub const VIGNETTE_FEATHER_SPAN: f64 = 0.7;

/// One record of `looks.json`; missing parts are neutral.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookPreset {
    /// The `preset` choice, the card `look:<key>`, the names `fx.look.<key>`.
    pub key: String,
    /// Master tone curve `(x, y)`: 2–5 points, x from 0 rising to 1, y never falling.
    pub curve: Vec<(f64, f64)>,
    #[serde(default)]
    pub split_tone: Option<SplitToneSpec>,
    /// Vignette amount, 0..1.
    #[serde(default)]
    pub vignette: f64,
    /// Grain amount, 0..0.2.
    #[serde(default)]
    pub grain: f64,
    #[serde(default)]
    pub adjust: AdjustSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitToneSpec {
    /// Degrees, 0..360.
    pub shadow_hue: f64,
    pub shadow_amount: f64,
    pub highlight_hue: f64,
    pub highlight_amount: f64,
    #[serde(default)]
    pub balance: f64,
}

/// The preset's colour correction (`ColorAdjust`'s meaning); absent fields are neutral.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdjustSpec {
    pub exposure: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub temperature: f64,
    pub tint: f64,
}

impl Default for AdjustSpec {
    /// `ColorAdjust::NEUTRAL`: an absent field changes nothing.
    fn default() -> Self {
        let n = ColorAdjust::NEUTRAL;
        AdjustSpec { exposure: n.exposure as f64, contrast: n.contrast as f64, saturation: n.saturation as f64, temperature: n.temperature as f64, tint: n.tint as f64 }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Looks {
    presets: Vec<LookPreset>,
}

/// Compiled in; `looks_json_is_valid` keeps it parseable and in range.
static LOOKS: LazyLock<Vec<LookPreset>> =
    LazyLock::new(|| serde_json::from_str::<Looks>(include_str!("../looks.json")).map(|l| l.presets).expect("crates/effects/looks.json is checked by looks_json_is_valid"));
static KEYS: LazyLock<Vec<&'static str>> = LazyLock::new(|| presets().iter().map(|p| p.key.as_str()).collect());

/// Every preset, in card order.
pub fn presets() -> &'static [LookPreset] {
    &LOOKS
}

pub fn preset(key: &str) -> Option<&'static LookPreset> {
    presets().iter().find(|p| p.key == key)
}

/// The `preset` options: the presets' keys in card order.
pub fn preset_keys() -> &'static [&'static str] {
    &KEYS
}

/// The grain seed of frame `ctx.frame`: the effect's (from its id) mixed
/// with the frame's time — new grain every frame, the same while paused and
/// on both halves of a split (D1).
pub fn frame_seed(ctx: &CompileCtx) -> u64 {
    ctx.seed ^ fmix64(ctx.frame.flicks() as u64)
}

/// The grain cell for a canvas: `GRAIN_CELL_PER_1080` per 1080 pixels of
/// its short side, at least 1 pixel — a vertical 1080 × 1920 canvas gets the
/// grain of a 1920 × 1080 one (E2-R4).
pub fn grain_cell(canvas: SizeU) -> f64 {
    (GRAIN_CELL_PER_1080 * canvas.w.min(canvas.h) as f64 / 1080.0).max(1.0)
}

/// `preset` at intensity `k` (D4): its correction, curve, split tone,
/// vignette and grain, in that order, each moved from neutral by `k` — the
/// curve's y to `x + k·(y − x)`, clamped into [0, 1] and kept rising; the
/// correction from `ColorAdjust::NEUTRAL`; amounts × k, at most 1 — neutral
/// ones dropped.
pub fn look_effects(preset: &LookPreset, k: f64, ctx: &CompileCtx) -> Vec<Effect> {
    let lerp = |neutral: f64, v: f64| neutral + k * (v - neutral);
    let mut out = Vec::new();
    let (a, n) = (&preset.adjust, AdjustSpec::default());
    let adjust = ColorAdjust {
        exposure: lerp(n.exposure, a.exposure) as f32,
        contrast: lerp(n.contrast, a.contrast).max(0.0) as f32,
        saturation: lerp(n.saturation, a.saturation).max(0.0) as f32,
        temperature: lerp(n.temperature, a.temperature).clamp(-1.0, 1.0) as f32,
        tint: lerp(n.tint, a.tint).clamp(-1.0, 1.0) as f32,
    };
    if !adjust.is_neutral() {
        out.push(Effect::ColorAdjust(adjust));
    }
    let mut rising = 0.0f64;
    let mut points = Vec::with_capacity(preset.curve.len());
    for &(x, y) in &preset.curve {
        rising = lerp(x, y).clamp(0.0, 1.0).max(rising);
        points.push((x as f32, rising as f32));
    }
    let curve = ToneCurve { points };
    if !curve.is_neutral() {
        out.push(Effect::ToneCurve(curve));
    }
    if let Some(s) = preset.split_tone {
        let split = SplitTone {
            shadow_hue_deg: s.shadow_hue as f32,
            shadow_amount: (s.shadow_amount * k).min(1.0) as f32,
            highlight_hue_deg: s.highlight_hue as f32,
            highlight_amount: (s.highlight_amount * k).min(1.0) as f32,
            balance: s.balance as f32,
        };
        if !split.is_neutral() {
            out.push(Effect::SplitTone(split));
        }
    }
    let vignette = Vignette { amount: (preset.vignette * k).min(1.0) as f32, midpoint: LOOK_VIGNETTE_MIDPOINT as f32, feather: LOOK_VIGNETTE_FEATHER as f32 };
    if !vignette.is_neutral() {
        out.push(Effect::Vignette(vignette));
    }
    let grain = Grain { amount: (preset.grain * k) as f32, size: grain_cell(ctx.canvas) as f32, seed: frame_seed(ctx) };
    if !grain.is_neutral() {
        out.push(Effect::Grain(grain));
    }
    out
}

/// The `look` recipe (spec §5): its preset at the strength's intensity. A
/// preset this build lacks never gets here (`definition` skips the effect),
/// and `Params::resolve` against the Look's own parameters always yields one
/// of its keys; the empty result stays only for `Params` resolved for
/// another recipe (a caller's mistake) — nothing drawn rather than a panic
/// in the middle of a render.
pub fn look(p: &Params, ctx: &CompileCtx) -> Compiled {
    let effects = preset(p.choice("preset")).map_or_else(Vec::new, |found| look_effects(found, intensity(p.number("strength")), ctx));
    Compiled::colour(effects)
}

/// The `vignette` recipe (spec §5 «Свет»): Сила and Мягкость.
pub fn vignette(p: &Params, _ctx: &CompileCtx) -> Compiled {
    let k = intensity(p.number("strength"));
    let v = Vignette {
        amount: (VIGNETTE_AMOUNT * k).min(1.0) as f32,
        midpoint: VIGNETTE_MIDPOINT as f32,
        feather: (VIGNETTE_FEATHER_MIN + VIGNETTE_FEATHER_SPAN * p.number("softness")) as f32,
    };
    Compiled::colour(if v.is_neutral() { vec![] } else { vec![Effect::Vignette(v)] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::intensity;
    use crate::registry::recipe;
    use kadr_core::Time;
    use kadr_project::ParamValue;

    fn hd() -> CompileCtx {
        CompileCtx::still(SizeU::new(1920, 1080), Time::ZERO, 7)
    }

    #[test]
    fn looks_json_is_valid() {
        let keys: Vec<&str> = presets().iter().map(|p| p.key.as_str()).collect();
        // The one pin of the six keys: other tests compare with `preset_keys()`.
        assert_eq!(keys, ["cinematic", "warm", "soft_film", "dramatic", "vintage", "natural"], "spec §5's six, in card order");
        assert!(keys.contains(&DEFAULT_PRESET));
        assert_eq!(preset_keys(), keys);
        let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), keys.len(), "every key once: a card id, a choice option");
        for p in presets() {
            let k = &p.key;
            assert!(!k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{k}: a key fit for a card id and i18n");
            assert!((2..=5).contains(&p.curve.len()), "{k}: 2–5 curve points");
            assert_eq!((p.curve[0].0, p.curve[p.curve.len() - 1].0), (0.0, 1.0), "{k}: the curve spans 0..1");
            assert!(p.curve.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 <= w[1].1), "{k}: x rising, y never falling");
            assert!(p.curve.iter().all(|&(x, y)| (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)), "{k}");
            if let Some(s) = p.split_tone {
                assert!([s.shadow_amount, s.highlight_amount].iter().all(|a| (0.0..=1.0).contains(a)) && (-1.0..=1.0).contains(&s.balance), "{k}");
                assert!([s.shadow_hue, s.highlight_hue].iter().all(|h| (0.0..360.0).contains(h)), "{k}");
            }
            assert!((0.0..=1.0).contains(&p.vignette) && (0.0..=0.2).contains(&p.grain), "{k}");
            let a = p.adjust;
            assert!((-2.0..=2.0).contains(&a.exposure) && (0.0..=2.0).contains(&a.contrast) && (0.0..=2.0).contains(&a.saturation), "{k}");
            assert!((-1.0..=1.0).contains(&a.temperature) && (-1.0..=1.0).contains(&a.tint), "{k}");
        }
        assert!(serde_json::from_str::<Looks>(r#"{"presets": [{"key": "x", "curve": [[0, 0], [1, 1]], "vignete": 0.3}]}"#).is_err(), "a typo is refused, not silently ignored");
    }

    /// Spec §5's table at «Обычно» (k = 1). The numbers are tuned with the
    /// owner at live acceptance: a spec value that moves updates this test.
    #[test]
    fn presets_have_their_spec_ingredients() {
        let at_normal = |key: &str| look_effects(preset(key).unwrap(), 1.0, &hd());
        let adjust = |fx: &[Effect]| fx.iter().find_map(|e| if let Effect::ColorAdjust(a) = e { Some(*a) } else { None }).unwrap_or(ColorAdjust::NEUTRAL);
        let vignette = |fx: &[Effect]| fx.iter().find_map(|e| if let Effect::Vignette(v) = e { Some(v.amount) } else { None }).unwrap_or(0.0);
        let grain = |fx: &[Effect]| fx.iter().find_map(|e| if let Effect::Grain(g) = e { Some(g.amount) } else { None }).unwrap_or(0.0);
        let curve = |fx: &[Effect]| fx.iter().find_map(|e| if let Effect::ToneCurve(c) = e { Some(c.points.clone()) } else { None }).expect("every preset has a curve");
        let split = |fx: &[Effect]| fx.iter().find_map(|e| if let Effect::SplitTone(s) = e { Some(*s) } else { None });

        let c = at_normal("cinematic");
        assert_eq!((adjust(&c).saturation, vignette(&c)), (0.9, 0.25), "Кино: saturation −10 %, vignette 0.25");
        let s = split(&c).expect("Кино: cold shadows, warm highlights");
        assert!((180.0..=240.0).contains(&s.shadow_hue_deg) && (20.0..=60.0).contains(&s.highlight_hue_deg));
        let k = curve(&c);
        assert!(k[1].1 < k[1].0 && k[3].1 > k[3].0, "a soft S: shadows down, highlights up");

        // Tuned up from the spec's 0.25 / 0.05, invisible on real footage (E2-R5).
        let w = at_normal("warm");
        assert!(adjust(&w).temperature >= 0.5 && adjust(&w).tint > 0.0, "Тёплый: clearly warm, {:?}", adjust(&w));
        assert!(curve(&w).iter().any(|&(x, y)| x == 0.5 && y > 0.5), "the mids lifted");
        let ws = split(&w).expect("warm highlights and a touch in the shadows");
        assert!((20.0..=60.0).contains(&ws.highlight_hue_deg) && (20.0..=60.0).contains(&ws.shadow_hue_deg));
        assert!(ws.highlight_amount > ws.shadow_amount && ws.shadow_amount > 0.0);

        let f = at_normal("soft_film");
        let fc = curve(&f);
        assert_eq!((fc[0].1, grain(&f)), (0.06, 0.04), "Мягкая плёнка: raised black 0.06, grain 0.04");
        assert!(fc[fc.len() - 1].1 < 1.0, "the highlights roll off");

        let d = at_normal("dramatic");
        assert_eq!((adjust(&d).saturation, vignette(&d)), (0.75, 0.4), "Драма");

        let v = at_normal("vintage");
        let vc = curve(&v);
        assert_eq!((vc[0].1, vc[vc.len() - 1].1), (0.1, 0.92), "Винтаж: faded black 0.1, white 0.92");
        assert_eq!((grain(&v), vignette(&v)), (0.05, 0.3));
        assert!(split(&v).is_some_and(|s| s.shadow_amount > 0.0 && (20.0..=60.0).contains(&s.shadow_hue_deg)), "warm shadows");

        assert_eq!(adjust(&at_normal("natural")).saturation, 0.95, "Естественный: saturation −5 %");
    }

    #[test]
    fn strength_moves_every_value_from_neutral() {
        for p in presets() {
            assert!(look_effects(p, 0.0, &hd()).is_empty(), "{}: k = 0, nothing at all", p.key);
        }
        let p = preset("cinematic").unwrap();
        let half = look_effects(p, 0.5, &hd());
        let Effect::ColorAdjust(a) = &half[0] else { panic!("{half:?}") };
        assert!((a.saturation - 0.95).abs() < 1e-6);
        let Some(Effect::ToneCurve(c)) = half.iter().find(|e| matches!(e, Effect::ToneCurve(_))) else { panic!("{half:?}") };
        assert!((c.points[1].1 - (0.25 + 0.5 * (0.22 - 0.25)) as f32).abs() < 1e-6, "y = x + k·(y_p − x)");
        let Some(Effect::Vignette(v)) = half.iter().find(|e| matches!(e, Effect::Vignette(_))) else { panic!("{half:?}") };
        assert!((v.amount - 0.125).abs() < 1e-6);
        let Some(Effect::SplitTone(s)) = half.iter().find(|e| matches!(e, Effect::SplitTone(_))) else { panic!("{half:?}") };
        assert!((s.shadow_amount - 0.1).abs() < 1e-6 && (s.highlight_amount - 0.075).abs() < 1e-6);
        let film = look_effects(preset("soft_film").unwrap(), 0.5, &hd());
        let Some(Effect::Grain(g)) = film.iter().find(|e| matches!(e, Effect::Grain(_))) else { panic!("{film:?}") };
        assert!((g.amount - 0.02).abs() < 1e-6, "Мягкая плёнка: half its grain, {}", g.amount);
        let warm = look_effects(preset("warm").unwrap(), 0.5, &hd());
        let Effect::ColorAdjust(w) = &warm[0] else { panic!("{warm:?}") };
        assert!((w.temperature - 0.35).abs() < 1e-6 && (w.tint - 0.04).abs() < 1e-6, "Тёплый: half its temperature and tint, {w:?}");
        let order: Vec<&str> = look_effects(preset("vintage").unwrap(), 1.0, &hd())
            .iter()
            .map(|e| match e {
                Effect::ColorAdjust(_) => "adjust",
                Effect::ToneCurve(_) => "curve",
                Effect::SplitTone(_) => "split",
                Effect::Vignette(_) => "vignette",
                Effect::Grain(_) => "grain",
                _ => "other",
            })
            .collect();
        assert_eq!(order, ["adjust", "curve", "split", "vignette", "grain"], "D4's order");
    }

    #[test]
    fn a_strong_look_keeps_its_curve_in_range_and_rising() {
        for p in presets() {
            for e in look_effects(p, intensity(1.0), &hd()) {
                if let Effect::ToneCurve(c) = &e {
                    assert!(c.points.windows(2).all(|w| w[0].1 <= w[1].1), "{}: {:?}", p.key, c.points);
                    assert!(c.points.iter().all(|&(_, y)| (0.0..=1.0).contains(&y)), "{}: {:?}", p.key, c.points);
                }
                if let Effect::Vignette(v) = &e {
                    assert!(v.amount <= 1.0, "{}", p.key);
                }
            }
        }
        // Amounts stay within 1 however strong a preset is pushed.
        let mut bold = preset("cinematic").unwrap().clone();
        bold.split_tone = Some(SplitToneSpec { shadow_hue: 200.0, shadow_amount: 0.9, highlight_hue: 35.0, highlight_amount: 0.8, balance: 0.0 });
        bold.vignette = 0.9;
        let fx = look_effects(&bold, intensity(1.0), &hd());
        let Some(Effect::SplitTone(s)) = fx.iter().find(|e| matches!(e, Effect::SplitTone(_))) else { panic!("{fx:?}") };
        assert_eq!((s.shadow_amount, s.highlight_amount), (1.0, 1.0));
    }

    /// Refinement 7: a Look's vignette has the Vignette effect's shape at
    /// Мягкость 0.5 — only its amount is the preset's.
    #[test]
    fn a_look_s_vignette_is_the_vignette_effect_s_shape_at_softness_half() {
        let def = recipe("vignette").unwrap();
        let stored = [("softness".to_string(), ParamValue::Number(0.5))].into_iter().collect();
        let [Effect::Vignette(v)] = (def.compile)(&Params::resolve(def.params, &stored, |_| None), &hd()).effects[..] else { panic!("one vignette") };
        let mut with_one = 0;
        for p in presets() {
            if let Some(l) = look_effects(p, 1.0, &hd()).into_iter().find_map(|e| if let Effect::Vignette(l) = e { Some(l) } else { None }) {
                assert_eq!((l.midpoint, l.feather), (v.midpoint, v.feather), "{}", p.key);
                with_one += 1;
            }
        }
        assert!(with_one >= 3, "Кино, Драма, Винтаж");
    }

    #[test]
    fn grain_is_new_every_frame_and_sized_by_the_canvas() {
        let p = preset("soft_film").unwrap();
        let grain = |canvas: SizeU, frame: Time| {
            look_effects(p, 1.0, &CompileCtx::still(canvas, frame, 7)).into_iter().find_map(|e| if let Effect::Grain(g) = e { Some(g) } else { None }).unwrap()
        };
        let hd = SizeU::new(1920, 1080);
        assert_eq!(grain(hd, Time::ZERO).seed, 7, "frame 0: fmix64(0) = 0");
        assert_ne!(grain(hd, Time::ZERO).seed, grain(hd, Time::from_millis(33)).seed, "the next frame, new grain");
        assert_eq!(grain(hd, Time::from_secs(2)), grain(hd, Time::from_secs(2)), "the same frame, the same grain");
        assert_eq!(grain(hd, Time::ZERO).size, 2.0);
        assert_eq!(grain(SizeU::new(3840, 2160), Time::ZERO).size, 4.0);
        assert_eq!(grain(SizeU::new(640, 360), Time::ZERO).size, 1.0, "never below one canvas pixel");
        assert_eq!(grain(SizeU::new(1080, 1920), Time::ZERO).size, grain(hd, Time::ZERO).size, "the short side: vertical as landscape (E2-R4)");
        assert_eq!(grain(SizeU::new(2160, 3840), Time::ZERO).size, 4.0);
        assert_eq!(grain(SizeU::new(1080, 1080), Time::ZERO).size, 2.0, "square");
    }

    #[test]
    fn the_look_recipe_runs_its_preset_and_the_vignette_its_softness() {
        let ctx = CompileCtx::still(SizeU::new(1920, 1080), Time::from_secs(3), 11);
        let run = |id: &str, set: &[(&str, ParamValue)]| {
            let def = recipe(id).unwrap();
            let stored = set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
            (def.compile)(&Params::resolve(def.params, &stored, |_| None), &ctx)
        };
        let warm = run("look", &[("preset", ParamValue::Choice("warm".into()))]);
        assert!(warm.placement.is_identity(), "a Look never moves the picture");
        assert_eq!(warm.effects, look_effects(preset("warm").unwrap(), 1.0, &ctx));
        assert_eq!(run("look", &[]).effects, look_effects(preset(DEFAULT_PRESET).unwrap(), 1.0, &ctx), "the default: Кино");
        let v = |set: &[(&str, ParamValue)]| match &run("vignette", set).effects[..] {
            [Effect::Vignette(v)] => *v,
            other => panic!("{other:?}"),
        };
        assert_eq!(v(&[]), Vignette { amount: 0.35, midpoint: 0.45, feather: 0.5 }, "D4 at «Обычно», softness 0.5");
        assert_eq!(v(&[("softness", ParamValue::Number(0.0))]).feather, 0.15);
        assert!((v(&[("softness", ParamValue::Number(1.0))]).feather - 0.85).abs() < 1e-6);
        assert!((v(&[("strength", ParamValue::Number(1.0))]).amount - 0.35 / 0.6).abs() < 1e-6);
    }
}
