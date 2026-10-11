//! Effect commands (effects spec §7): validation against the registry,
//! one instance per recipe, tracks go with their effect, one undo step.

use kadr_core::{EffectId, FrameRate, Time, TimeRange};
use kadr_project::*;
use kadr_timeline::*;
use std::collections::BTreeMap;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

/// A 10 s clip on V1 and one on A1 (source 100..110 s, timeline 0..10 s).
fn setup() -> (Project, EditEngine, kadr_core::ClipId, kadr_core::ClipId) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let v = Clip::new(kadr_core::AssetId::new(), "v", TimeRange::new(s(100), s(110)), s(0));
    let a = Clip::new(kadr_core::AssetId::new(), "a", TimeRange::new(s(100), s(110)), s(0));
    let (vid, aid) = (v.id, a.id);
    p.sequence_mut().tracks[0].clips.push(v);
    p.sequence_mut().tracks[1].clips.push(a);
    (p, EditEngine::new(), vid, aid)
}

fn zoom() -> AppliedEffect {
    AppliedEffect::new("smooth_zoom", 1, BTreeMap::from([("strength".to_string(), ParamValue::Number(0.6))]))
}

fn effects(p: &Project, id: kadr_core::ClipId) -> Vec<AppliedEffect> {
    p.sequence().clip(id).unwrap().effects.clone()
}

/// Entries the engine recorded (undo does not shorten the log).
fn entries(p: &Project) -> usize {
    p.history_log.len()
}

fn push(p: &mut Project, clip: kadr_core::ClipId, e: AppliedEffect) {
    let (ti, ci) = p.sequence().locate_clip(clip).unwrap();
    p.sequence_mut().tracks[ti].clips[ci].effects.push(e);
}

/// The clip as a file could bring it (no command, no history).
fn clip_mut(p: &mut Project, clip: kadr_core::ClipId) -> &mut Clip {
    let (ti, ci) = p.sequence().locate_clip(clip).unwrap();
    &mut p.sequence_mut().tracks[ti].clips[ci]
}

#[test]
fn add_effect_fills_defaults_and_is_one_undo_step() {
    let (mut p, mut e, v, _) = setup();
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: zoom() }).unwrap();
    let fx = effects(&p, v);
    assert_eq!(fx.len(), 1);
    assert_eq!(fx[0].params["direction"], ParamValue::Choice("in".into()), "missing parameters get defaults");
    assert_eq!(fx[0].params["point"], ParamValue::Point { x: 0.5, y: 0.5 });
    assert_eq!(entries(&p), 1);
    e.undo(&mut p);
    assert!(effects(&p, v).is_empty());
}

#[test]
fn a_recipe_the_clip_has_is_replaced_in_place_with_its_tracks_dropped() {
    let (mut p, mut e, v, _) = setup();
    let first = zoom();
    let first_id = first.id;
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: first }).unwrap();
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: AppliedEffect::new("handheld", 1, BTreeMap::new()) }).unwrap();
    let k = TrackKey::fx(first_id, "strength");
    e.execute(&mut p, EditCommand::SetKeyframe { clip: v, param: k.clone(), at: SourceTime(s(102)), value: 0.2, ease: None }).unwrap();
    let mut again = zoom();
    again.params.insert("strength".into(), ParamValue::Number(0.9));
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: again }).unwrap();
    let fx = effects(&p, v);
    assert_eq!(fx.iter().map(|x| x.recipe.as_str()).collect::<Vec<_>>(), ["smooth_zoom", "handheld"], "one instance, same place");
    assert_eq!((fx[0].id, fx[0].number("strength")), (first_id, Some(0.9)), "same id, new parameters");
    assert!(!p.sequence().clip(v).unwrap().animation.is_animated(&k), "its tracks went");
}

#[test]
fn applying_what_is_already_there_changes_nothing() {
    let (mut p, mut e, v, _) = setup();
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: zoom() }).unwrap();
    let before = entries(&p);
    assert_eq!(e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: zoom() }), Err(EditError::NoOp));
    assert_eq!(entries(&p), before, "no empty history entry");
}

#[test]
fn unknown_newer_invalid_and_misplaced_effects_are_rejected() {
    let (mut p, mut e, v, a) = setup();
    let add = |effect| EditCommand::AddEffect { clip: v, effect };
    assert_eq!(e.execute(&mut p, add(AppliedEffect::new("glitch_v2", 1, BTreeMap::new()))), Err(EditError::UnknownRecipe("glitch_v2".into())));
    assert_eq!(e.execute(&mut p, add(AppliedEffect::new("smooth_zoom", 2, BTreeMap::new()))), Err(EditError::UnknownRecipe("smooth_zoom".into())));
    let bad = AppliedEffect::new("smooth_zoom", 1, BTreeMap::from([("strength".to_string(), ParamValue::Number(1.5))]));
    assert!(matches!(e.execute(&mut p, add(bad)), Err(EditError::InvalidParam(m)) if m.contains("strength")));
    let extra = AppliedEffect::new("smooth_zoom", 1, BTreeMap::from([("speed".to_string(), ParamValue::Number(1.0))]));
    assert!(matches!(e.execute(&mut p, add(extra)), Err(EditError::InvalidParam(_))));
    assert_eq!(e.execute(&mut p, EditCommand::AddEffect { clip: a, effect: zoom() }), Err(EditError::EffectTarget), "a picture effect on a sound clip");
    p.sequence_mut().tracks[0].locked = true;
    assert!(matches!(e.execute(&mut p, add(zoom())), Err(EditError::TrackLocked(_))));
    assert_eq!(entries(&p), 0);
}

#[test]
fn removing_an_effect_takes_its_tracks_and_undo_brings_both_back() {
    let (mut p, mut e, v, _) = setup();
    let z = zoom();
    let zid = z.id;
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    let k = TrackKey::fx(zid, "strength");
    e.execute(&mut p, EditCommand::SetKeyframe { clip: v, param: k.clone(), at: SourceTime(s(101)), value: 0.3, ease: None }).unwrap();
    e.execute(&mut p, EditCommand::RemoveEffect { clip: v, id: zid }).unwrap();
    let c = p.sequence().clip(v).unwrap();
    assert!(c.effects.is_empty() && !c.animation.is_animated(&k));
    e.undo(&mut p);
    let c = p.sequence().clip(v).unwrap();
    assert!(c.effect(zid).is_some() && c.animation.is_animated(&k), "one undo step brings both back");
    assert_eq!(e.execute(&mut p, EditCommand::RemoveEffect { clip: v, id: EffectId::new() }), Err(EditError::EffectNotFound));
}

#[test]
fn update_merges_parameters_toggles_and_rejects_no_change() {
    let (mut p, mut e, v, _) = setup();
    let z = zoom();
    let zid = z.id;
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    let upd = |params: &[(&str, ParamValue)], enabled| EditCommand::UpdateEffect {
        clip: v,
        id: zid,
        params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        enabled,
    };
    e.execute(&mut p, upd(&[("strength", ParamValue::Number(0.3))], None)).unwrap();
    let fx = &effects(&p, v)[0];
    assert_eq!((fx.number("strength"), fx.params["direction"].clone()), (Some(0.3), ParamValue::Choice("in".into())), "merged, the rest kept");
    e.execute(&mut p, upd(&[], Some(false))).unwrap();
    assert!(!effects(&p, v)[0].enabled);
    assert_eq!(e.execute(&mut p, upd(&[("strength", ParamValue::Number(0.3))], Some(false))), Err(EditError::NoOp));
    assert!(matches!(e.execute(&mut p, upd(&[("direction", ParamValue::Choice("left".into()))], None)), Err(EditError::InvalidParam(_))));
    let missing = EditCommand::UpdateEffect { clip: v, id: EffectId::new(), params: BTreeMap::new(), enabled: Some(true) };
    assert_eq!(e.execute(&mut p, missing), Err(EditError::EffectNotFound));
}

/// Review focus 5: an effect from a newer Kadr is kept — moved or removed,
/// never changed (its parameters cannot be validated here).
#[test]
fn an_unknown_effect_is_kept_moved_or_removed_but_not_changed() {
    let (mut p, mut e, v, _) = setup();
    let future = AppliedEffect::new("glitch_v2", 3, BTreeMap::from([("amount".to_string(), ParamValue::Number(0.5))]));
    let fid = future.id;
    push(&mut p, v, future);
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: zoom() }).unwrap();
    let upd = EditCommand::UpdateEffect { clip: v, id: fid, params: BTreeMap::new(), enabled: Some(false) };
    assert_eq!(e.execute(&mut p, upd), Err(EditError::UnknownRecipe("glitch_v2".into())));
    e.execute(&mut p, EditCommand::MoveEffect { clip: v, id: fid, to: 1 }).unwrap();
    assert_eq!(effects(&p, v)[1].id, fid);
    e.execute(&mut p, EditCommand::RemoveEffect { clip: v, id: fid }).unwrap();
    assert_eq!(effects(&p, v).iter().map(|x| x.recipe.as_str()).collect::<Vec<_>>(), ["smooth_zoom"]);
}

#[test]
fn move_effect_reorders_clamps_and_rejects_no_change() {
    let (mut p, mut e, v, _) = setup();
    let (a, b, c) = (zoom(), AppliedEffect::new("handheld", 1, BTreeMap::new()), AppliedEffect::new("smooth_pan", 1, BTreeMap::new()));
    let ids = [a.id, b.id, c.id];
    for x in [a, b, c] {
        e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: x }).unwrap();
    }
    e.execute(&mut p, EditCommand::MoveEffect { clip: v, id: ids[0], to: 99 }).unwrap();
    assert_eq!(effects(&p, v).iter().map(|x| x.id).collect::<Vec<_>>(), [ids[1], ids[2], ids[0]], "clamped to the end");
    assert_eq!(e.execute(&mut p, EditCommand::MoveEffect { clip: v, id: ids[1], to: 0 }), Err(EditError::NoOp));
}

#[test]
fn applying_to_several_clips_is_one_undo_entry() {
    let (mut p, mut e, v, _) = setup();
    let w = Clip::new(kadr_core::AssetId::new(), "w", TimeRange::new(s(0), s(5)), s(20));
    let wid = w.id;
    p.sequence_mut().tracks[0].clips.push(w);
    let batch = EditCommand::Batch { label: "Эффект: Плавный наезд".into(), commands: vec![EditCommand::AddEffect { clip: v, effect: zoom() }, EditCommand::AddEffect { clip: wid, effect: zoom() }] };
    e.execute(&mut p, batch).unwrap();
    assert_eq!(entries(&p), 1);
    e.undo(&mut p);
    assert!(effects(&p, v).is_empty() && effects(&p, wid).is_empty());
}

#[test]
fn effect_commands_have_labels_and_error_keys() {
    let (clip, id) = (kadr_core::ClipId::new(), EffectId::new());
    assert_eq!(EditCommand::AddEffect { clip, effect: zoom() }.label(), "cmd.add_effect");
    assert_eq!(EditCommand::RemoveEffect { clip, id }.label(), "cmd.remove_effect");
    assert_eq!(EditCommand::UpdateEffect { clip, id, params: BTreeMap::new(), enabled: None }.label(), "cmd.update_effect");
    assert_eq!(EditCommand::MoveEffect { clip, id, to: 0 }.label(), "cmd.move_effect");
    for (err, key) in [
        (EditError::UnknownRecipe("x".into()), "err.edit.unknown_recipe"),
        (EditError::EffectTarget, "err.edit.effect_target"),
        (EditError::InvalidParam("x".into()), "err.edit.invalid_param"),
    ] {
        assert_eq!(EditCommand::error_key(&err), key);
    }
}

/// A lenient load can bring two effects of one recipe; applying the recipe
/// leaves one (the first, in place, with its id), the others go with their
/// tracks — one undo step brings everything back.
#[test]
fn a_clip_with_a_recipe_twice_ends_with_one_after_applying_it() {
    let (mut p, mut e, v, _) = setup();
    let (first, second) = (zoom(), zoom());
    let (k1, k2) = (TrackKey::fx(first.id, "strength"), TrackKey::fx(second.id, "strength"));
    let first_id = first.id;
    push(&mut p, v, first);
    push(&mut p, v, AppliedEffect::new("handheld", 1, BTreeMap::new()));
    push(&mut p, v, second);
    clip_mut(&mut p, v).animation.set_key(k1.clone(), SourceTime(s(101)), 0.2, None);
    clip_mut(&mut p, v).animation.set_key(k2.clone(), SourceTime(s(101)), 0.9, None);
    let before = p.sequence().clip(v).unwrap().clone();
    let mut again = zoom();
    again.params.insert("strength".into(), ParamValue::Number(0.3));
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: again }).unwrap();
    let c = p.sequence().clip(v).unwrap();
    assert_eq!(c.effects.iter().map(|x| x.recipe.as_str()).collect::<Vec<_>>(), ["smooth_zoom", "handheld"]);
    assert_eq!((c.effects[0].id, c.effects[0].number("strength")), (first_id, Some(0.3)));
    assert!(c.animation.is_empty(), "both instances' tracks went");
    e.undo(&mut p);
    assert_eq!(p.sequence().clip(v).unwrap(), &before);
}

/// An effect of a recipe this build knows, made by a newer version of it,
/// cannot be changed — but applying the recipe again replaces it whole, as
/// removing it and applying would (in its place, with its id): the result
/// is a valid effect of this build, and the effects tab's hover preview
/// (`Clip::put_effect`) shows exactly that.
#[test]
fn applying_a_recipe_over_its_newer_version_replaces_it_whole() {
    let (mut p, mut e, v, _) = setup();
    let mut newer: AppliedEffect = serde_json::from_value(serde_json::json!({
        "id": EffectId::new(), "recipe": "smooth_zoom", "version": 2,
        "params": {"strength": {"number": 0.4}, "curve": {"bezier": [0.1, 0.2]}}, "mask": {"shape": "oval"}
    }))
    .unwrap();
    newer.progress = (0.25, 1.0);
    let nid = newer.id;
    push(&mut p, v, newer);
    clip_mut(&mut p, v).animation.set_key(TrackKey::fx(nid, "strength"), SourceTime(s(101)), 0.2, None);
    let upd = EditCommand::UpdateEffect { clip: v, id: nid, params: BTreeMap::new(), enabled: Some(false) };
    assert_eq!(e.execute(&mut p, upd), Err(EditError::UnknownRecipe("smooth_zoom".into())));
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: zoom() }).unwrap();
    let c = p.sequence().clip(v).unwrap();
    assert_eq!(c.effects.len(), 1);
    let fx = &c.effects[0];
    assert_eq!((fx.id, fx.version, fx.progress, fx.enabled), (nid, 1, (0.0, 1.0), true));
    assert_eq!(fx.params.keys().map(String::as_str).collect::<Vec<_>>(), ["direction", "point", "strength"], "this build's parameters only");
    assert!(fx.extra.is_empty() && c.animation.is_empty(), "the newer fields and tracks went with it");
}

/// The stored effect is built from what was validated: fields a caller
/// adds beside them (`extra` is for files from a newer Kadr) are not kept,
/// and a disabled or partial effect is applied enabled, over the whole clip.
#[test]
fn an_applied_effect_is_stored_as_validated() {
    let (mut p, mut e, v, _) = setup();
    let mut z = zoom();
    z.extra.insert("id".into(), serde_json::json!("not-an-id"));
    z.enabled = false;
    z.progress = (0.5, 0.75);
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z.clone() }).unwrap();
    let fx = &effects(&p, v)[0];
    assert!(fx.extra.is_empty());
    assert_eq!((fx.id, fx.enabled, fx.progress), (z.id, true, (0.0, 1.0)));
    let saved = serde_json::to_value(fx).unwrap();
    assert_eq!(serde_json::from_value::<AppliedEffect>(saved).unwrap(), *fx, "it reloads as it is");
}

#[test]
fn effects_on_a_locked_track_are_not_changed() {
    let (mut p, mut e, v, _) = setup();
    let (z, h) = (zoom(), AppliedEffect::new("handheld", 1, BTreeMap::new()));
    let zid = z.id;
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: h }).unwrap();
    p.sequence_mut().tracks[0].locked = true;
    let n = entries(&p);
    let locked = |r: Result<(), EditError>| matches!(r, Err(EditError::TrackLocked(_)));
    assert!(locked(e.execute(&mut p, EditCommand::RemoveEffect { clip: v, id: zid })));
    assert!(locked(e.execute(&mut p, EditCommand::UpdateEffect { clip: v, id: zid, params: BTreeMap::new(), enabled: Some(false) })));
    assert!(locked(e.execute(&mut p, EditCommand::MoveEffect { clip: v, id: zid, to: 1 })));
    assert_eq!((effects(&p, v).len(), effects(&p, v)[0].id, effects(&p, v)[0].enabled), (2, zid, true));
    assert_eq!(entries(&p), n);
}

/// Ruling R4: keyframe commands edit only numbers of effects this build
/// runs. A newer Kadr's effect keeps its tracks as they were (review
/// focus 5); a choice, a point or a parameter the recipe does not declare
/// never gets a track; a key is within the parameter's range.
#[test]
fn keyframes_animate_only_numbers_of_effects_this_build_runs() {
    let (mut p, mut e, v, _) = setup();
    let z = zoom();
    let zid = z.id;
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    let future = AppliedEffect::new("glitch_v2", 3, BTreeMap::from([("amount".to_string(), ParamValue::Number(0.5))]));
    let fid = future.id;
    push(&mut p, v, future);
    let amount = TrackKey::fx(fid, "amount");
    let at = SourceTime(s(101));
    clip_mut(&mut p, v).animation.set_key(amount.clone(), at, 0.7, None);
    let before = p.sequence().clip(v).unwrap().clone();
    let n = entries(&p);
    let set = |param: TrackKey, value: f64| EditCommand::SetKeyframe { clip: v, param, at: SourceTime(s(102)), value, ease: None };

    let unknown = Err(EditError::UnknownRecipe("glitch_v2".into()));
    assert_eq!(e.execute(&mut p, set(amount.clone(), 0.2)), unknown);
    assert_eq!(e.execute(&mut p, EditCommand::RemoveKeyframe { clip: v, param: amount.clone(), at, keep: None }), unknown);
    assert_eq!(e.execute(&mut p, EditCommand::MoveKeyframes { clip: v, moves: vec![(amount.clone(), at, SourceTime(s(103)))] }), unknown);
    assert_eq!(e.execute(&mut p, EditCommand::SetKeyframeEase { clip: v, param: amount.clone(), at, ease: Ease::Hold }), unknown);
    assert_eq!(e.execute(&mut p, EditCommand::ClearAnimation { clip: v, statics: vec![(amount.clone(), 0.1)] }), unknown);
    let both = vec![(AnimParam::Scale.into(), at, SourceTime(s(103))), (amount.clone(), at, SourceTime(s(103)))];
    clip_mut(&mut p, v).animation.set_key(AnimParam::Scale, at, 2.0, None);
    assert_eq!(e.execute(&mut p, EditCommand::MoveKeyframes { clip: v, moves: both }), unknown, "all or nothing");
    clip_mut(&mut p, v).animation.clear(AnimParam::Scale);

    let invalid = |r: Result<(), EditError>| matches!(r, Err(EditError::InvalidParam(_)));
    assert!(invalid(e.execute(&mut p, set(TrackKey::fx(zid, "direction"), 1.0))), "a choice");
    assert!(invalid(e.execute(&mut p, set(TrackKey::fx(zid, "point"), 0.5))), "a point");
    assert!(invalid(e.execute(&mut p, set(TrackKey::fx(zid, "speed"), 0.5))), "not a parameter of the recipe");
    assert!(matches!(e.execute(&mut p, set(TrackKey::fx(zid, "strength"), 1.5)), Err(EditError::InvalidParam(m)) if m.contains("strength")));
    assert_eq!(e.execute(&mut p, set(TrackKey::fx(EffectId::new(), "strength"), 0.5)), Err(EditError::EffectNotFound));
    assert_eq!(p.sequence().clip(v).unwrap(), &before, "nothing changed");
    assert_eq!(entries(&p), n);

    e.execute(&mut p, set(TrackKey::fx(zid, "strength"), 0.2)).unwrap();
    e.execute(&mut p, set(AnimParam::Opacity.into(), 0.5)).unwrap();
}

/// Ruling R4: a parameter that cannot hold a number — a choice, one the
/// recipe does not declare (kept from a newer Kadr), a stored value this
/// build cannot read, or none at all — is named as such (not "effect not
/// found"), and removing the last key never writes a number into it.
#[test]
fn a_parameter_without_a_number_is_named_and_never_gets_one() {
    let (mut p, mut e, v, _) = setup();
    let z = zoom();
    let zid = z.id;
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    let odd: ParamValue = serde_json::from_value(serde_json::json!({"curve": {"bezier": [0.1, 0.2]}})).unwrap();
    let c = clip_mut(&mut p, v);
    c.effects[0].params.insert("future_amount".into(), ParamValue::Number(2.5));
    c.effects[0].params.insert("strength".into(), odd.clone());
    let at = SourceTime(s(104));
    let keys = ["direction", "future_amount", "strength"].map(|k| TrackKey::fx(zid, k));
    for k in &keys {
        c.animation.set_key(k.clone(), at, 0.5, None);
    }
    let before = p.sequence().clip(v).unwrap().clone();
    for k in &keys {
        let r = e.execute(&mut p, EditCommand::ClearAnimation { clip: v, statics: vec![(k.clone(), 0.5)] });
        assert!(matches!(&r, Err(EditError::InvalidParam(m)) if m.contains(&k.as_fx().unwrap().key)), "{k:?}: {r:?}");
        let r = e.execute(&mut p, EditCommand::RemoveKeyframe { clip: v, param: k.clone(), at, keep: None });
        assert!(matches!(r, Err(EditError::InvalidParam(_))), "{k:?}: {r:?}");
        let r = e.execute(&mut p, EditCommand::SetKeyframeEase { clip: v, param: k.clone(), at, ease: Ease::Hold });
        assert!(matches!(r, Err(EditError::InvalidParam(_))), "{k:?}: {r:?}");
    }
    let fx = &p.sequence().clip(v).unwrap().effects[0];
    assert_eq!((fx.params["direction"].clone(), fx.number("future_amount"), fx.params["strength"].clone()), (ParamValue::Choice("in".into()), Some(2.5), odd));
    assert_eq!(p.sequence().clip(v).unwrap(), &before);
    clip_mut(&mut p, v).effects[0].params.remove("strength");
    let r = e.execute(&mut p, EditCommand::RemoveKeyframe { clip: v, param: keys[2].clone(), at, keep: Some(0.3) });
    assert!(matches!(r, Err(EditError::InvalidParam(_))), "no stored value: {r:?}");
    assert!(!p.sequence().clip(v).unwrap().effects[0].params.contains_key("strength"));
    let gone = EditCommand::ClearAnimation { clip: v, statics: vec![(TrackKey::fx(EffectId::new(), "strength"), 0.5)] };
    assert_eq!(e.execute(&mut p, gone), Err(EditError::EffectNotFound));
}

/// The static value a keyframe edit writes for an effect number is kept in
/// the parameter's range — the value the picture showed, as the evaluator
/// clamps a track from a file the same way.
#[test]
fn an_effect_number_left_by_its_keys_stays_in_range() {
    let (mut p, mut e, v, _) = setup();
    let z = zoom();
    let k = TrackKey::fx(z.id, "strength");
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    let strength = |p: &Project| effects(p, v)[0].number("strength");
    clip_mut(&mut p, v).animation.set_key(k.clone(), SourceTime(s(101)), 3.0, None);
    e.execute(&mut p, EditCommand::RemoveKeyframe { clip: v, param: k.clone(), at: SourceTime(s(101)), keep: None }).unwrap();
    assert_eq!(strength(&p), Some(1.0));
    e.execute(&mut p, EditCommand::SetKeyframe { clip: v, param: k.clone(), at: SourceTime(s(102)), value: 0.4, ease: None }).unwrap();
    e.execute(&mut p, EditCommand::ClearAnimation { clip: v, statics: vec![(k.clone(), -2.0)] }).unwrap();
    assert_eq!(strength(&p), Some(0.0));
    e.execute(&mut p, EditCommand::SetKeyframe { clip: v, param: k.clone(), at: SourceTime(s(102)), value: 0.4, ease: None }).unwrap();
    e.execute(&mut p, EditCommand::RemoveKeyframe { clip: v, param: k.clone(), at: SourceTime(s(102)), keep: Some(0.25) }).unwrap();
    assert_eq!(strength(&p), Some(0.25));
}

/// Ruling R8: a gesture on a keyframe moment edits the tracks it may and
/// leaves read-only ones — a newer Kadr's effect, an orphan `fx:` track —
/// where they are, instead of failing whole.
#[test]
fn a_moment_s_editable_keys_skip_read_only_tracks() {
    let (mut p, mut e, v, _) = setup();
    let z = zoom();
    let strength = TrackKey::fx(z.id, "strength");
    e.execute(&mut p, EditCommand::AddEffect { clip: v, effect: z }).unwrap();
    let future = AppliedEffect::new("glitch_v2", 3, BTreeMap::from([("amount".to_string(), ParamValue::Number(0.5))]));
    let newer = TrackKey::fx(future.id, "amount");
    push(&mut p, v, future);
    let orphan = TrackKey::fx(EffectId::new(), "strength");
    let (at, to) = (SourceTime(s(104)), SourceTime(s(106)));
    let c = clip_mut(&mut p, v);
    for k in [newer.clone(), orphan.clone()] {
        c.animation.set_key(k, at, 0.5, None);
    }
    assert!(editable_keys_at(c, at).is_empty(), "only read-only keys: nothing to edit");
    c.animation.set_key(AnimParam::Scale, at, 2.0, None);
    c.animation.set_key(strength.clone(), at, 0.3, None);
    let keys = editable_keys_at(c, at);
    assert_eq!(keys, [TrackKey::from(AnimParam::Scale), strength.clone()]);
    let moves = keys.into_iter().map(|k| (k, at, to)).collect();
    e.execute(&mut p, EditCommand::MoveKeyframes { clip: v, moves }).unwrap();
    let c = p.sequence().clip(v).unwrap();
    assert_eq!(c.animation.keys_at(to), [TrackKey::from(AnimParam::Scale), strength]);
    assert_eq!(c.animation.keys_at(at).len(), 2, "the read-only keys stay");
    assert!(c.animation.keys_at(at).contains(&newer) && c.animation.keys_at(at).contains(&orphan));
}
