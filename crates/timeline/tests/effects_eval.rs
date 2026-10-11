//! The evaluator with effects (effects spec §1, §3, §9 «evaluator»).

use kadr_core::{EffectId, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_scene::{Effect, Layer, OutputSpec, Placement, RenderQuality, SizeU};
use kadr_timeline::ops::{split_clip, trim_start, LinkRemap};
use kadr_timeline::scene::evaluate;
use std::collections::BTreeMap;

const F30: Time = Time(705_600_000 / 30);

fn hd() -> MediaAsset {
    let video = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaAsset::new("m", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(600), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
}

/// One clip on V1: timeline 10..20 s showing source 30..40 s, with `effects`.
fn project(effects: Vec<AppliedEffect>) -> (Project, kadr_core::ClipId) {
    let mut p = Project::new("fx");
    let a = hd();
    let mut c = Clip::new(a.id, "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10));
    c.effects = effects;
    let id = c.id;
    p.assets.push(a);
    p.sequence_mut().tracks[0].clips.push(c);
    (p, id)
}

fn fx(recipe: &str, set: &[(&str, ParamValue)]) -> AppliedEffect {
    let params: BTreeMap<String, ParamValue> = set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    AppliedEffect::new(recipe, 1, params)
}

fn clip_mut(p: &mut Project, id: kadr_core::ClipId) -> &mut Clip {
    let (ti, ci) = p.sequence().locate_clip(id).unwrap();
    &mut p.sequence_mut().tracks[ti].clips[ci]
}

/// The same project with the clip's effects (and their `fx:` tracks) gone.
fn without_effects(p: &Project, id: kadr_core::ClipId) -> Project {
    let mut plain = p.clone();
    let c = clip_mut(&mut plain, id);
    for e in std::mem::take(&mut c.effects) {
        c.animation.remove_effect(e.id);
    }
    plain
}

fn layer(p: &Project, t: Time) -> Layer {
    let out = OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewHigh);
    evaluate(p, p.sequence(), t, &out).layers.into_iter().next().expect("one layer")
}

/// Where the clip picture's corners land on the canvas.
fn corners(l: &Layer) -> [(f64, f64); 4] {
    let m = l.placement.to_canvas();
    let (w, h) = (l.placement.size.x as f64, l.placement.size.y as f64);
    [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| m.apply(x, y))
}

fn same_place(a: &Layer, b: &Layer) -> bool {
    corners(a).iter().zip(corners(b)).all(|(p, q)| (p.0 - q.0).abs() < 0.01 && (p.1 - q.1).abs() < 0.01)
}

/// Where the picture point at fractions `(x, y)` lands on the canvas.
fn at_fraction(l: &Layer, (x, y): (f64, f64)) -> (f64, f64) {
    l.placement.to_canvas().apply(x * l.placement.size.x as f64, y * l.placement.size.y as f64)
}

fn same_point(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
}

fn scale(p: &Project, ms: i64) -> f64 {
    layer(p, Time::from_millis(ms)).placement.scale.x as f64
}

/// Spec §3 and review focus 5: strength 0, a disabled effect, an unknown
/// recipe and a newer version change nothing — not one field of the layer.
#[test]
fn zero_strength_disabled_and_unknown_effects_change_nothing() {
    let t = Time::from_secs(15);
    let mut off = fx("smooth_zoom", &[]);
    off.enabled = false;
    let mut newer = fx("smooth_zoom", &[]);
    newer.version = 2;
    for e in [fx("smooth_zoom", &[("strength", ParamValue::Number(0.0))]), off, newer, fx("glitch_v2", &[("amount", ParamValue::Number(1.0))])] {
        let (p, id) = project(vec![e.clone()]);
        assert_eq!(layer(&p, t), layer(&without_effects(&p, id), t), "{e:?}");
    }
}

/// Review focus 5 and the keyframes of effect parameters: an `fx:` track is
/// read only for a declared number parameter of an effect the clip has and
/// this build runs — an orphan track, one of an unknown or newer recipe and
/// one of an undeclared or non-number parameter change nothing.
#[test]
fn fx_tracks_of_absent_unknown_or_undeclared_parameters_change_nothing() {
    let t = Time::from_secs(15);
    let off = || fx("smooth_zoom", &[("strength", ParamValue::Number(0.0))]);
    let mut newer = off();
    newer.version = 2;
    let unknown = fx("glitch_v2", &[("strength", ParamValue::Number(0.0))]);
    // Each case: the clip's one effect and the key of a track at 1.0 (from the effect's id).
    type KeyOf = fn(EffectId) -> TrackKey;
    let cases: [(&str, AppliedEffect, KeyOf); 5] = [
        ("orphan", off(), |_| TrackKey::fx(EffectId::new(), "strength")),
        ("unknown recipe", unknown, |e| TrackKey::fx(e, "strength")),
        ("newer version", newer, |e| TrackKey::fx(e, "strength")),
        ("undeclared", off(), |e| TrackKey::fx(e, "amount")),
        ("not a number", off(), |e| TrackKey::fx(e, "direction")),
    ];
    for (what, e, key) in cases {
        let k = key(e.id);
        let (mut p, id) = project(vec![e]);
        clip_mut(&mut p, id).animation.set_key(k, SourceTime(Time::from_secs(30)), 1.0, None);
        let mut plain = p.clone();
        let c = clip_mut(&mut plain, id);
        c.effects.clear();
        c.animation = Animation::default();
        assert_eq!(layer(&p, t), layer(&plain, t), "{what}");
    }
}

#[test]
fn smooth_zoom_follows_the_visible_clip_about_its_point() {
    let (p, _) = project(vec![fx("smooth_zoom", &[("point", ParamValue::Point { x: 0.25, y: 0.75 })])]);
    let (plain, _) = project(vec![]);
    assert!(same_place(&layer(&p, Time::from_secs(10)), &layer(&plain, Time::from_secs(10))), "progress 0: no zoom yet");
    let mid = layer(&p, Time::from_secs(15));
    assert!((mid.placement.scale.x - 1.05).abs() < 1e-6, "{:?}", mid.placement.scale);
    let at_point = |l: &Layer| l.placement.to_canvas().apply(0.25 * l.placement.size.x as f64, 0.75 * l.placement.size.y as f64);
    let (a, b) = (at_point(&mid), at_point(&layer(&plain, Time::from_secs(15))));
    assert!((a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01, "the point stays put: {a:?} vs {b:?}");
}

/// Spec §1 / review focus 2: a trim stretches the effect over the new
/// visible clip, within its range.
#[test]
fn trimming_stretches_the_effect_over_the_new_visible_clip() {
    let (mut p, id) = project(vec![fx("smooth_zoom", &[])]);
    let (ti, ci) = p.sequence().locate_clip(id).unwrap();
    trim_start(&mut p.sequence_mut().tracks[ti].clips[ci], Time::from_secs(14)); // visible 14..20 s
    assert!((scale(&p, 14_000) - 1.0).abs() < 1e-6, "the new first frame starts the zoom");
    assert!((scale(&p, 17_000) - 1.05).abs() < 1e-6, "half way through the shorter clip");
}

/// Ruling R6 with a trim: a cropped clip's effect stretches over the new
/// visible clip too, and zooms about its Point clamped into the crop window
/// — the window comes from the clip's crop.
#[test]
fn trimming_a_cropped_clip_stretches_the_effect_about_its_window() {
    let (mut p, id) = project(vec![fx("smooth_zoom", &[("point", ParamValue::Point { x: 0.25, y: 0.5 })])]);
    let c = clip_mut(&mut p, id);
    (c.transform.crop_left, c.transform.crop_top, c.transform.crop_bottom) = (0.6, 0.1, 0.2);
    trim_start(clip_mut(&mut p, id), Time::from_secs(14)); // visible 14..20 s
    let plain = without_effects(&p, id);
    assert!(same_place(&layer(&p, Time::from_secs(14)), &layer(&plain, Time::from_secs(14))), "the new first frame starts the zoom");
    let (mid, base) = (layer(&p, Time::from_secs(17)), layer(&plain, Time::from_secs(17)));
    assert!((mid.placement.scale.x - 1.05).abs() < 1e-6, "half way through the shorter clip: {:?}", mid.placement.scale);
    assert_eq!(mid.crop, base.crop, "the crop is the clip's");
    assert!(same_point(at_fraction(&mid, (0.6, 0.5)), at_fraction(&base, (0.6, 0.5))), "the fixed point is the Point clamped into the window");
    assert!(!same_point(at_fraction(&mid, (0.25, 0.5)), at_fraction(&base, (0.25, 0.5))), "the hidden Point itself moves");
}

/// Ruling R6: the window is the crop at the frame — an animated crop moves
/// it along (source-bound, like the crop's own keys) — also through the
/// public `place_with_effects`.
#[test]
fn the_crop_window_is_the_crop_at_the_frame() {
    let (mut p, id) = project(vec![fx("cinematic_push", &[("point", ParamValue::Point { x: 0.25, y: 0.5 })])]);
    let c = clip_mut(&mut p, id);
    c.animation.set_key(AnimParam::CropLeft, SourceTime(Time::from_secs(30)), 0.0, Some(Ease::Hold));
    c.animation.set_key(AnimParam::CropLeft, SourceTime(Time::from_secs(35)), 0.6, None); // timeline 15 s
    let plain = without_effects(&p, id);
    let (early, early_base) = (layer(&p, Time::from_secs(14)), layer(&plain, Time::from_secs(14)));
    assert!(early.placement.scale.x > 1.0);
    assert!(same_point(at_fraction(&early, (0.25, 0.5)), at_fraction(&early_base, (0.25, 0.5))), "uncropped: about the Point");
    let (late, late_base) = (layer(&p, Time::from_secs(18)), layer(&plain, Time::from_secs(18)));
    assert!(same_point(at_fraction(&late, (0.6, 0.5)), at_fraction(&late_base, (0.6, 0.5))), "cropped to 0.6: about the window's edge");
    assert!(!same_point(at_fraction(&late, (0.25, 0.5)), at_fraction(&late_base, (0.25, 0.5))));
    let c = p.sequence().clip(id).unwrap();
    let placed = kadr_timeline::fx::place_with_effects(c, Time::from_secs(18), SizeU::new(1920, 1080), late_base.placement);
    assert_eq!(placed, late.placement, "the public function reads the same window");
}

/// Spec §1: «Момент» is source-bound; ruling R17: the punch is fully in on
/// the moment's own frame (its 0.12 s ramp ends there). Review focus 4: a
/// moment before the visible start is done on the first frame; one past
/// the end never comes.
#[test]
fn the_punch_moment_stays_on_its_source_frame() {
    // Source 33 s = timeline 13 s; the ramp runs 12.88..13 s.
    let (mut p, id) = project(vec![fx("punch_in", &[("moment", ParamValue::Moment(SourceTime(Time::from_secs(33))))])]);
    assert!((scale(&p, 12_850) - 1.0).abs() < 1e-6);
    assert!(scale(&p, 12_950) > 1.0 && scale(&p, 12_950) < 1.15, "on its way in");
    assert!((scale(&p, 13_000) - 1.15).abs() < 1e-5, "the moment's own frame: in");
    assert!((scale(&p, 13_200) - 1.15).abs() < 1e-5);
    let (ti, ci) = p.sequence().locate_clip(id).unwrap();
    trim_start(&mut p.sequence_mut().tracks[ti].clips[ci], Time::from_secs(12));
    assert!((scale(&p, 12_850) - 1.0).abs() < 1e-6, "the content frame decides, not the clip start");
    assert!((scale(&p, 13_000) - 1.15).abs() < 1e-5);
    assert!((scale(&p, 13_200) - 1.15).abs() < 1e-5);
    trim_start(&mut p.sequence_mut().tracks[ti].clips[ci], Time::from_secs(14));
    assert!((scale(&p, 14_000) - 1.15).abs() < 1e-5, "punched before the visible start: held from the first frame");
    let (late, _) = project(vec![fx("punch_in", &[("moment", ParamValue::Moment(SourceTime(Time::from_secs(45))))])]);
    assert!((scale(&late, 19_900) - 1.0).abs() < 1e-6, "a moment past the end never comes");
}

#[test]
fn an_fx_track_overrides_the_stored_strength() {
    let e = fx("smooth_zoom", &[("strength", ParamValue::Number(0.0))]);
    let k = TrackKey::fx(e.id, "strength");
    let (mut p, id) = project(vec![e]);
    let (ti, ci) = p.sequence().locate_clip(id).unwrap();
    let c = &mut p.sequence_mut().tracks[ti].clips[ci];
    c.animation.set_key(k.clone(), SourceTime(Time::from_secs(30)), 0.6, Some(Ease::Hold));
    c.animation.set_key(k, SourceTime(Time::from_secs(36)), 0.0, None); // timeline 16 s
    assert!((scale(&p, 15_000) - 1.05).abs() < 1e-6, "the track's 0.6 before source 36 s");
    assert!((scale(&p, 17_000) - 1.0).abs() < 1e-6, "its 0 after: off");
}

/// A small xorshift: the property tests' cases are reproducible.
fn rng() -> impl FnMut(u64) -> u64 {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    move |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    }
}

/// Spec §1 / review focus 2: a split never changes the picture — every
/// frame of both halves places the clip where the whole clip did, for
/// every recipe, at 1× and 2× (Handheld's noise and Punch In's moment too).
#[test]
fn a_split_never_moves_the_picture() {
    let mut rnd = rng();
    let dirs = ["in", "out", "left", "right", "up", "down"];
    for case in 0..60 {
        let def = &kadr_effects::registry()[rnd(5) as usize];
        let set: Vec<(&str, ParamValue)> = [
            ("strength", ParamValue::Number(rnd(101) as f64 / 100.0)),
            ("point", ParamValue::Point { x: rnd(101) as f64 / 100.0, y: rnd(101) as f64 / 100.0 }),
            ("moment", ParamValue::Moment(SourceTime(Time::from_millis(30_000 + rnd(10_000) as i64)))),
            ("direction", ParamValue::Choice(dirs[rnd(6) as usize].into())),
        ]
        .into_iter()
        .filter(|(k, _)| def.param(k).is_some())
        .collect();
        let (mut p, id) = project(vec![fx(def.id, &set)]);
        if case % 3 == 1 {
            let (ti, ci) = p.sequence().locate_clip(id).unwrap();
            p.sequence_mut().tracks[ti].clips[ci].source_out = Time::from_secs(50); // 2×
        }
        let whole = p.clone();
        let c = p.sequence().clip(id).unwrap().clone();
        let cut = c.timeline_in + F30.mul_ratio(1 + rnd(298) as i64, 1);
        let (l, r) = split_clip(&c, cut, &mut LinkRemap::default());
        p.sequence_mut().tracks[0].clips = vec![l, r];
        for f in 0..300 {
            let t = c.timeline_in + F30.mul_ratio(f, 1);
            assert!(same_place(&layer(&p, t), &layer(&whole, t)), "case {case} {} frame {f} (cut at {cut:?})", def.id);
        }
    }
}

/// Review focus 2 with ruling R6: a split keeps the picture of a cropped
/// clip too — a static crop, a letterbox, an animated crop — with two
/// effects stacked, at 1× and 2×.
#[test]
fn a_split_keeps_a_cropped_clip_s_picture() {
    let mut rnd = rng();
    let dirs = ["in", "out", "left", "right", "up", "down"];
    let registry = kadr_effects::registry();
    for case in 0..40 {
        let effects: Vec<AppliedEffect> = [rnd(5), rnd(5)]
            .iter()
            .map(|&i| {
                let def = &registry[i as usize];
                let set: Vec<(&str, ParamValue)> = [
                    ("strength", ParamValue::Number((20 + rnd(81)) as f64 / 100.0)),
                    ("point", ParamValue::Point { x: rnd(101) as f64 / 100.0, y: rnd(101) as f64 / 100.0 }),
                    ("moment", ParamValue::Moment(SourceTime(Time::from_millis(30_000 + rnd(10_000) as i64)))),
                    ("direction", ParamValue::Choice(dirs[rnd(6) as usize].into())),
                ]
                .into_iter()
                .filter(|(k, _)| def.param(k).is_some())
                .collect();
                fx(def.id, &set)
            })
            .collect();
        let (mut p, id) = project(effects);
        let c = clip_mut(&mut p, id);
        match case % 4 {
            0 => (c.transform.crop_left, c.transform.crop_right) = (rnd(60) as f64 / 100.0, rnd(30) as f64 / 100.0),
            1 => (c.transform.crop_top, c.transform.crop_bottom) = (0.12, 0.12),
            2 => {
                (c.transform.crop_left, c.transform.crop_top, c.transform.crop_right, c.transform.crop_bottom) = (0.1, 0.2, 0.15, 0.05);
                c.source_out = Time::from_secs(50); // 2×
            }
            _ => {
                c.animation.set_key(AnimParam::CropLeft, SourceTime(Time::from_secs(30)), 0.0, None);
                c.animation.set_key(AnimParam::CropLeft, SourceTime(Time::from_secs(40)), 0.6, None);
                c.animation.set_key(AnimParam::CropBottom, SourceTime(Time::from_secs(34)), 0.3, None);
            }
        }
        let whole = p.clone();
        let c = p.sequence().clip(id).unwrap().clone();
        let cut = c.timeline_in + F30.mul_ratio(1 + rnd(298) as i64, 1);
        let (l, r) = split_clip(&c, cut, &mut LinkRemap::default());
        p.sequence_mut().tracks[0].clips = vec![l, r];
        for f in 0..300 {
            let t = c.timeline_in + F30.mul_ratio(f, 1);
            let (a, b) = (layer(&p, t), layer(&whole, t));
            assert!(same_place(&a, &b) && a.crop == b.crop, "case {case} frame {f} (cut at {cut:?}): {:?} vs {:?}", a.placement, b.placement);
        }
    }
}

/// Transition handles ask for a clip past its edges: still finite.
#[test]
fn an_effect_evaluated_past_the_clip_edge_stays_finite() {
    let (p, id) = project(vec![fx("handheld", &[]), fx("smooth_zoom", &[])]);
    let c = p.sequence().clip(id).unwrap();
    let base = Placement::fill(SizeU::new(1920, 1080));
    for t in [Time::from_secs(-5), Time::from_secs(9), Time::from_secs(25)] {
        let pl = kadr_timeline::fx::place_with_effects(c, t, SizeU::new(1920, 1080), base);
        assert!([pl.position.x, pl.position.y, pl.scale.x, pl.rotation].iter().all(|v| v.is_finite()), "{t:?}: {pl:?}");
    }
}

/// A degenerate clip (zero length) or an absurd progress range from a file
/// never yields a non-finite progress or placement.
#[test]
fn degenerate_clips_and_ranges_stay_finite() {
    let (p, id) = project(vec![fx("handheld", &[]), fx("smooth_pan", &[])]);
    let mut c = p.sequence().clip(id).unwrap().clone();
    c.timeline_out = c.timeline_in; // zero length
    let base = Placement::fill(SizeU::new(1920, 1080));
    for t in [Time::from_secs(9), Time::from_secs(10), Time::from_secs(11)] {
        for e in &c.effects {
            assert!(kadr_timeline::fx::effect_progress(&c, e, t).is_finite(), "{t:?}");
        }
        let pl = kadr_timeline::fx::place_with_effects(&c, t, SizeU::new(1920, 1080), base);
        assert!([pl.position.x, pl.position.y, pl.scale.x, pl.rotation].iter().all(|v| v.is_finite()), "{t:?}: {pl:?}");
    }
    let (mut p, id) = project(vec![fx("handheld", &[])]);
    clip_mut(&mut p, id).effects[0].progress = (-1e308, 1e308);
    let c = p.sequence().clip(id).unwrap().clone();
    let (l, r) = split_clip(&c, Time::from_secs(15), &mut LinkRemap::default());
    for h in [&l, &r] {
        let (a, b) = h.effects[0].progress;
        assert!(a.is_finite() && b.is_finite(), "{:?}", h.effects[0].progress);
    }
    let pl = kadr_timeline::fx::place_with_effects(&l, Time::from_secs(10), SizeU::new(1920, 1080), base);
    assert!([pl.position.x, pl.position.y, pl.scale.x, pl.rotation].iter().all(|v| v.is_finite()), "{pl:?}");
}

/// The colour primitives of a layer, by kind, in order.
fn kinds(l: &Layer) -> Vec<&'static str> {
    l.effects
        .iter()
        .map(|e| match e {
            Effect::ColorAdjust(_) => "adjust",
            Effect::ToneCurve(_) => "curve",
            Effect::SplitTone(_) => "split",
            Effect::Vignette(_) => "vignette",
            Effect::Grain(_) => "grain",
            _ => "other",
        })
        .collect()
}

fn look(preset: &str) -> AppliedEffect {
    fx("look", &[("preset", ParamValue::Choice(preset.into()))])
}

/// Spec §3: correction first, then the Look, in D4's order.
#[test]
fn a_look_follows_the_inspector_correction_in_its_fixed_order() {
    let (mut p, id) = project(vec![look("vintage")]);
    clip_mut(&mut p, id).color.saturation = 0.5;
    let l = layer(&p, Time::from_secs(15));
    assert_eq!(kinds(&l), ["adjust", "adjust", "curve", "split", "vignette", "grain"]);
    let Effect::ColorAdjust(first) = &l.effects[0] else { unreachable!() };
    assert_eq!(first.saturation, 0.5, "the inspector's correction comes first");
}

#[test]
fn colour_effects_follow_the_clip_s_list_order() {
    let (p, _) = project(vec![fx("vignette", &[]), look("natural")]);
    assert_eq!(kinds(&layer(&p, Time::from_secs(15))), ["vignette", "adjust", "curve"]);
}

/// Review focus 3 and 4: strength 0, a switched-off Look and a preset this
/// build lacks change nothing — not one field of the layer.
#[test]
fn a_look_at_zero_switched_off_or_of_an_unknown_preset_adds_nothing() {
    let t = Time::from_secs(15);
    let mut off = look("warm");
    off.enabled = false;
    for e in [fx("look", &[("strength", ParamValue::Number(0.0))]), off, look("bleach_bypass"), fx("vignette", &[("strength", ParamValue::Number(0.0))])] {
        let (p, id) = project(vec![e.clone()]);
        assert_eq!(layer(&p, t), layer(&without_effects(&p, id), t), "{e:?}");
    }
}

#[test]
fn motion_and_a_look_both_apply() {
    let (p, _) = project(vec![fx("smooth_zoom", &[("strength", ParamValue::Number(1.0))]), look("dramatic")]);
    let l = layer(&p, Time::from_millis(19_900));
    assert!(l.placement.scale.x > 1.1, "zoomed: {}", l.placement.scale.x);
    assert_eq!(kinds(&l), ["adjust", "curve", "vignette"]);
}

/// Review focus 2: grain is new every frame, and a split or a start-trim
/// never changes a frame's primitives (the seed is the effect id's and the frame's).
#[test]
fn grain_changes_every_frame_and_a_split_or_trim_keeps_it() {
    let (p, id) = project(vec![look("soft_film")]);
    let grain = |p: &Project, t: Time| layer(p, t).effects.iter().find_map(|e| if let Effect::Grain(g) = e { Some(*g) } else { None }).expect("Мягкая плёнка has grain");
    let at = Time::from_secs(15);
    assert_ne!(grain(&p, at).seed, grain(&p, at + F30).seed, "the next frame has new grain");
    assert_eq!(grain(&p, at), grain(&p, at), "the same frame, the same grain");
    let c = p.sequence().clip(id).unwrap().clone();
    let (l, r) = split_clip(&c, Time::from_secs(14), &mut LinkRemap::default());
    let mut split = p.clone();
    split.sequence_mut().tracks[0].clips = vec![l, r];
    let mut trimmed = p.clone();
    trim_start(clip_mut(&mut trimmed, id), Time::from_secs(12));
    for t in [Time::from_secs(13), Time::from_secs(14), Time::from_secs(14) + F30, Time::from_secs(18)] {
        assert_eq!(layer(&split, t).effects, layer(&p, t).effects, "split, {t:?}");
        assert_eq!(layer(&trimmed, t).effects, layer(&p, t).effects, "trimmed, {t:?}");
    }
}
