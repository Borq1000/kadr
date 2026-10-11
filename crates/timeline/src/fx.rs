//! Effects in the evaluator (effects spec §3): for each enabled, known
//! effect of a clip in list order — progress (§1), the parameter values at
//! the frame (`fx:` tracks over stored values), `compile` — the motion delta
//! composed over the clip's placement and the colour primitives collected
//! in order.

use kadr_core::Time;
use kadr_effects::{definition, CompileCtx, Params};
use kadr_project::{AppliedEffect, Clip, SourceTime, Transform};
use kadr_scene::{Effect, Placement, SizeU};

/// Where `t` lies in the clip's visible length, `(t − timeline_in) /
/// duration` — integer flicks into one f64; 0 for a clip of no length (never
/// 0 / 0). `t` may lie outside the clip (transition handles).
pub(crate) fn visible_fraction(clip: &Clip, t: Time) -> f64 {
    let d = clip.duration().flicks();
    if d > 0 { (t - clip.timeline_in).flicks() as f64 / d as f64 } else { 0.0 }
}

/// Spec §1: `from + (to − from) · (t − timeline_in) / duration`.
pub fn effect_progress(clip: &Clip, e: &AppliedEffect, t: Time) -> f64 {
    e.progress_at(visible_fraction(clip, t))
}

/// The noise seed of an effect: from its id, so both halves of a split shake alike.
pub fn effect_seed(e: &AppliedEffect) -> u64 {
    let u = e.id.0.as_u128();
    (u as u64) ^ ((u >> 64) as u64)
}

/// The visible crop window of a clip's (resolved) transform, as
/// `CompileCtx::window`: `(crop_left, crop_top, 1 − crop_right, 1 −
/// crop_bottom)`, each crop clamped into 0..1 like the layer's crop (a
/// non-finite one counts as none); the whole picture when nothing is left
/// (such a clip draws nothing anyway).
pub fn crop_window(tr: &Transform) -> (f64, f64, f64, f64) {
    let f = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let w = (f(tr.crop_left), f(tr.crop_top), 1.0 - f(tr.crop_right), 1.0 - f(tr.crop_bottom));
    if w.0 < w.2 && w.1 < w.3 { w } else { CompileCtx::UNCROPPED }
}

/// `base` — the clip's own placement at `t` — with the clip's motion
/// effects applied in list order, in the crop window the clip has at `t`
/// (its animated crop). Unknown recipes, newer versions and unknown
/// variants are skipped; strength 0 compiles nothing (§3).
pub fn place_with_effects(clip: &Clip, t: Time, canvas: SizeU, base: Placement) -> Placement {
    if clip.effects.is_empty() || clip.duration() <= Time::ZERO {
        return base;
    }
    let window = crop_window(&crate::scene::clip_state_at(clip, t, Time::ZERO).transform);
    apply_effects(clip, t, canvas, base, window).0
}

/// Every enabled, known effect of `clip` at `t`, in list order (spec §3,
/// D3): its placement delta composed over `base` in the crop `window`
/// (`crop_window` of the clip's state at `t`), its colour primitives
/// collected — `clip_layer` puts them after the inspector's correction.
pub(crate) fn apply_effects(clip: &Clip, t: Time, canvas: SizeU, base: Placement, window: (f64, f64, f64, f64)) -> (Placement, Vec<Effect>) {
    // A clip of no length shows nothing (and maps no source time).
    if clip.effects.is_empty() || clip.duration() <= Time::ZERO {
        return (base, Vec::new());
    }
    let source_time = SourceTime(clip.source_time_at(t));
    let aspect = if base.size.y > 0.0 { base.size.x as f64 / base.size.y as f64 } else { 1.0 };
    let secs = clip.duration().as_secs_f64();
    let speed = Some(clip.speed()).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
    let mut placement = base;
    let mut colour = Vec::new();
    for e in clip.effects.iter().filter(|e| e.enabled) {
        // Unknown recipe, newer version or variant: kept, never run (spec §2)
        // — nor are its `fx:` tracks read. Only declared number parameters of
        // the clip's own effects are looked up, so orphan tracks never count.
        let Some(def) = definition(e) else { continue };
        let params = Params::resolve(def.params, &e.params, |key| clip.animation.fx_value_at(e.id, key, source_time));
        if params.is_off() {
            continue;
        }
        // Seconds of the whole progress range at the clip's length: the
        // effect's own clock `progress × clip_secs` runs on across a split.
        let span = (e.progress.1 - e.progress.0).abs();
        let clip_secs = if span > 1e-9 { secs / span } else { secs };
        let progress = effect_progress(clip, e, t);
        if !progress.is_finite() || !clip_secs.is_finite() {
            continue;
        }
        let ctx = CompileCtx { progress, clip_secs, source_time, speed, canvas, aspect, window, frame: t, seed: effect_seed(e) };
        let compiled = (def.compile)(&params, &ctx);
        placement = compiled.placement.apply(&placement);
        colour.extend(compiled.effects);
    }
    (placement, colour)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, TimeRange};
    use std::collections::BTreeMap;

    #[test]
    fn the_crop_window_is_the_visible_part_of_the_picture() {
        let mut tr = Transform::default();
        assert_eq!(crop_window(&tr), CompileCtx::UNCROPPED);
        (tr.crop_left, tr.crop_top, tr.crop_right, tr.crop_bottom) = (0.25, 0.125, 0.5, 0.375);
        assert_eq!(crop_window(&tr), (0.25, 0.125, 0.5, 0.625));
        (tr.crop_left, tr.crop_right) = (-1.0, 2.0);
        assert_eq!(crop_window(&tr), CompileCtx::UNCROPPED, "nothing left: the whole picture");
        (tr.crop_left, tr.crop_right) = (f64::NAN, 0.5);
        assert_eq!(crop_window(&tr), (0.0, 0.125, 0.5, 0.625), "a non-finite crop is none");
        (tr.crop_left, tr.crop_right) = (0.6, 0.5);
        assert_eq!(crop_window(&tr), CompileCtx::UNCROPPED, "inverted: the whole picture");
    }

    #[test]
    fn progress_runs_over_the_visible_clip_and_a_split_meets_at_the_cut() {
        let c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10));
        let mut e = AppliedEffect::new("smooth_zoom", 1, BTreeMap::new());
        e.progress = (0.5, 1.0);
        assert_eq!(effect_progress(&c, &e, Time::from_secs(10)), 0.5);
        assert_eq!(effect_progress(&c, &e, Time::from_secs(15)), 0.75);
        assert_eq!(effect_progress(&c, &e, Time::from_secs(20)), 1.0);
        assert_eq!(effect_progress(&c, &e, Time::from_secs(0)), 0.0, "past the edge: extrapolated");
        let mut empty = c.clone();
        empty.timeline_out = empty.timeline_in;
        assert_eq!(effect_progress(&empty, &e, Time::from_secs(12)), 0.5, "no length: the range's start");
        let other = AppliedEffect::new("smooth_zoom", 1, BTreeMap::new());
        assert_ne!(effect_seed(&e), effect_seed(&other));
        assert_eq!(effect_seed(&e), effect_seed(&e.clone()));
    }
}
