//! Transition commands (effects spec §7, D3) and transitions following
//! their cuts through every clip command (D2): one undo entry each, and
//! undo restores clips and transitions together.

use kadr_core::{ClipId, FrameRate, MediaInfo, MediaKind, Time, TimeRange, TransitionId, VideoInfo};
use kadr_project::*;
use kadr_timeline::*;
use std::collections::BTreeMap;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}
fn ms(x: i64) -> Time {
    Time::from_millis(x)
}

fn video(dur: Time) -> MediaInfo {
    let v = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaInfo { kind: MediaKind::Video, duration: dur, container: "mp4".into(), size_bytes: 0, video: Some(v), audio: None, timecode: None }
}

/// V1: a 0..4 s, b 4..8 s, c 8..12 s (each its own 60 s medium, source
/// from 10 s), an empty V2 above, A1; a dissolve on a|b, a wipe on b|c.
fn setup() -> (Project, EditEngine, [ClipId; 3], [TransitionId; 2]) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    let mut ids = vec![];
    for (i, start) in [0, 4, 8].into_iter().enumerate() {
        let a = MediaAsset::new(format!("m{i}.mp4"), video(s(60)));
        let c = Clip::new(a.id, format!("c{i}"), TimeRange::new(s(10), s(14)), s(start));
        ids.push(c.id);
        p.assets.push(a);
        p.sequence_mut().tracks[0].clips.push(c);
    }
    let ids = [ids[0], ids[1], ids[2]];
    let (t1, t2) = (Transition::legacy(TransitionKind::CrossDissolve, ids[0], ids[1], s(1)), Transition::legacy(TransitionKind::Wipe, ids[1], ids[2], s(1)));
    let tids = [t1.id, t2.id];
    p.sequence_mut().transitions = vec![t1, t2];
    (p, EditEngine::new(), ids, tids)
}

/// Every transition as (id, from, to, cut): the cut `None` where its clips do not meet.
fn cuts(p: &Project) -> Vec<(TransitionId, ClipId, ClipId, Option<Time>)> {
    let seq = p.sequence();
    seq.transitions.iter().map(|t| (t.id, t.from, t.to, seq.transition_cut(t).map(|(_, at)| at))).collect()
}

/// Which of `tids` are still there, in order.
fn kept(p: &Project, tids: [TransitionId; 2]) -> Vec<TransitionId> {
    p.sequence().transitions.iter().map(|t| t.id).filter(|id| tids.contains(id)).collect()
}

fn set(from: ClipId, to: ClipId, recipe: &str, params: &[(&str, ParamValue)], duration: Time) -> EditCommand {
    let version = kadr_effects::recipe(recipe).map_or(1, |d| d.version);
    EditCommand::SetTransition { from, to, recipe: recipe.into(), version, params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), duration }
}

type Cmd = dyn Fn(&Project, [ClipId; 3]) -> EditCommand;
type Want = dyn Fn(&Project, [ClipId; 3], [TransitionId; 2]);

/// D2, review focus 2: every command that changes clips leaves no
/// transition between clips that do not meet — in its own one undo entry —
/// keeps (re-anchoring if need be) those whose clips still meet, and undo
/// restores the sequence exactly.
#[test]
fn transitions_follow_every_clip_command() {
    let check = |name: &str, cmd: &Cmd, want: &Want| {
        let (mut p, mut e, ids, tids) = setup();
        let before = p.sequence().clone();
        let entries = p.history_log.len();
        let c = cmd(&p, ids);
        e.execute(&mut p, c).unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(p.history_log.len(), entries + 1, "{name}: one undo entry");
        assert!(cuts(&p).iter().all(|c| c.3.is_some()), "{name}: a transition between clips that do not meet: {:?}", cuts(&p));
        want(&p, ids, tids);
        e.undo(&mut p);
        assert_eq!(p.sequence(), &before, "{name}: undo restores clips and transitions together");
    };
    check("split of from", &|_, _| EditCommand::Split { at: s(2), clips: None }, &|p, [a, b, _], [t1, _]| {
        let t = p.sequence().transition(t1).unwrap();
        assert_ne!(t.from, a, "re-anchored to the right half");
        assert_eq!((p.sequence().clip(t.from).unwrap().timeline_out, t.to), (s(4), b));
    });
    check("split of to", &|_, [_, b, _]| EditCommand::Split { at: s(6), clips: Some(vec![b]) }, &|p, [a, b, _], [t1, t2]| {
        let (x, y) = (p.sequence().transition(t1).unwrap(), p.sequence().transition(t2).unwrap());
        assert_eq!((x.from, x.to), (a, b), "b keeps its start");
        assert_ne!(y.from, b, "b|c follows b's right half");
    });
    check("delete from", &|_, [a, _, _]| EditCommand::DeleteClips { clips: vec![a], ripple: false }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t2]));
    check("ripple delete to", &|_, [_, b, _]| EditCommand::DeleteClips { clips: vec![b], ripple: true }, &|p, _, tids| assert!(kept(p, tids).is_empty(), "a and c now meet, without a transition"));
    check("ripple delete after the pair", &|_, [_, _, c]| EditCommand::DeleteClips { clips: vec![c], ripple: true }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t1]));
    check("delete range over a cut", &|_, _| EditCommand::DeleteRange { range: TimeRange::new(s(3), s(5)), ripple: false }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t2]));
    check("ripple delete range inside a clip", &|_, _| EditCommand::DeleteRange { range: TimeRange::new(s(9), s(10)), ripple: true }, &|p, _, tids| assert_eq!(kept(p, tids), tids.to_vec(), "c lost a middle part; b|c still meets"));
    check("move to away", &|_, [_, _, c]| EditCommand::MoveClips { clips: vec![c], delta: s(10), track_delta: 0 }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t1]));
    check("move the pairs together to V2", &|_, ids| EditCommand::MoveClips { clips: ids.to_vec(), delta: s(20), track_delta: 1 }, &|p, _, tids| {
        assert_eq!(kept(p, tids), tids.to_vec());
        assert_eq!(cuts(p).iter().map(|c| c.3).collect::<Vec<_>>(), [Some(s(24)), Some(s(28))]);
        assert_eq!(p.sequence().transition_cut(&p.sequence().transitions[0]).unwrap().0, 1, "on V2 now");
    });
    check("trim opening a gap", &|_, [a, _, _]| EditCommand::TrimClip { clip: a, edge: TrimEdge::End, to: s(3), ripple: false }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t2]));
    check("ripple trim keeping the pair", &|_, [a, _, _]| EditCommand::TrimClip { clip: a, edge: TrimEdge::End, to: s(3), ripple: true }, &|p, _, tids| {
        assert_eq!(kept(p, tids), tids.to_vec());
        assert_eq!(cuts(p)[0].3, Some(s(3)), "the cut moved with the trim");
    });
    check("trim start of to", &|_, [_, b, _]| EditCommand::TrimClip { clip: b, edge: TrimEdge::Start, to: s(5), ripple: false }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t2]));
    check("speed of from", &|_, [a, _, _]| EditCommand::SetClipProperty { clip: a, prop: ClipProperty::Speed(2.0) }, &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t2], "a got shorter: a gap"));
    check(
        "insert at the cut",
        &|p, _| {
            let mut c = Clip::new(p.assets[2].id, "x", TimeRange::new(s(30), s(32)), s(4));
            c.id = ClipId::new();
            EditCommand::InsertClip { track: p.sequence().tracks[0].id, clip: c, mode: InsertMode::Insert }
        },
        &|p, _, [t1, t2]| assert_eq!(kept(p, [t1, t2]), vec![t2], "a clip between a and b"),
    );
    check("switching b off keeps both", &|_, [_, b, _]| EditCommand::SetClipProperty { clip: b, prop: ClipProperty::Enabled(false) }, &|p, _, tids| assert_eq!(kept(p, tids), tids.to_vec()));
    check(
        "a batch splitting from twice",
        &|_, _| EditCommand::Batch { label: "x".into(), commands: vec![EditCommand::Split { at: s(1), clips: None }, EditCommand::Split { at: s(3), clips: None }] },
        &|p, _, [t1, _]| assert_eq!(p.sequence().clip(p.sequence().transition(t1).unwrap().from).unwrap().timeline_in, s(3), "the last piece"),
    );
}

/// `setup` with `a` an angle of a two-camera group: CAM1 is `a`'s medium,
/// CAM2 (60 s) started 2 s later (sync offset −2 s).
fn setup_multicam() -> (Project, EditEngine, [ClipId; 3], [TransitionId; 2]) {
    let (mut p, e, ids, tids) = setup();
    let cam2 = MediaAsset::new("cam2.mp4", video(s(60)));
    let angle = |asset, label: &str, sync_offset| MulticamAngle { asset, label: label.into(), description: String::new(), sync_offset, sync_method: SyncMethod::Manual };
    let group = MulticamGroup { id: kadr_core::MulticamId::new(), name: "show".into(), angles: vec![angle(p.assets[0].id, "CAM1", s(0)), angle(cam2.id, "CAM2", s(-2))], master_audio: None };
    p.sequence_mut().tracks[0].clips[0].multicam = Some(MulticamSelection { group: group.id, angle: 0 });
    p.assets.push(cam2);
    p.multicam_groups.push(group);
    (p, e, ids, tids)
}

/// Ruling E3-R4: an edit inside the outgoing clip — a range taken out of
/// it (gap or ripple), a live camera cut, a camera range — keeps its
/// transition on the cut, re-anchored to the piece that now ends there; in
/// one undo entry that restores clips and transitions together.
#[test]
fn edits_inside_the_outgoing_clip_keep_its_transition() {
    let check = |name: &str, cmd: &dyn Fn(ClipId) -> EditCommand, cut: Time, want: &dyn Fn(&Project, &Clip)| {
        let (mut p, mut e, [a, b, _], tids) = setup_multicam();
        let before = p.sequence().clone();
        let entries = p.history_log.len();
        e.execute(&mut p, cmd(a)).unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(p.history_log.len(), entries + 1, "{name}: one undo entry");
        assert_eq!(kept(&p, tids), tids.to_vec(), "{name}: both transitions stay");
        let t = p.sequence().transition(tids[0]).unwrap();
        let from = p.sequence().clip(t.from).unwrap();
        assert_ne!(t.from, a, "{name}: re-anchored to the piece at the cut");
        assert_eq!((t.to, from.timeline_out, p.sequence().transition_cut(t).map(|c| c.1)), (b, cut, Some(cut)), "{name}");
        want(&p, from);
        e.undo(&mut p);
        assert_eq!(p.sequence(), &before, "{name}: undo restores clips and transitions together");
    };
    let range = |from, to, ripple| EditCommand::DeleteRange { range: TimeRange::new(s(from), s(to)), ripple };
    check("a range taken out of from, leaving a gap", &|_| range(1, 2, false), s(4), &|_, c| assert_eq!((c.timeline_in, c.source_in), (s(2), s(12))));
    check("a range rippled out of from", &|_| range(1, 2, true), s(3), &|_, c| assert_eq!((c.timeline_in, c.source_in), (s(1), s(12))));
    check("a live camera cut inside from", &|a| EditCommand::SwitchAngle { clip: a, angle: 1, at: Some(s(2)) }, s(4), &|p, c| {
        let cam2 = p.multicam_groups[0].angles[1].asset;
        assert_eq!((c.asset, c.timeline_in, c.multicam.as_ref().map(|m| m.angle)), (cam2, s(2), Some(1)), "the piece on CAM2");
    });
    check("a camera range inside from", &|_| EditCommand::SetAngleRange { range: TimeRange::new(s(1), s(2)), angle: 1 }, s(4), &|_, c| {
        assert_eq!((c.timeline_in, c.multicam.as_ref().map(|m| m.angle)), (s(2), Some(0)), "back on CAM1 after the range");
    });
}

/// D3: a recipe on a cut — validated, its missing parameters at their
/// defaults — replacing the cut's transition in place; the same again is
/// no entry.
#[test]
fn set_transition_validates_fills_defaults_and_replaces_in_place() {
    let (mut p, mut e, [a, b, c], [t1, _]) = setup();
    let entries = p.history_log.len();
    e.execute(&mut p, set(a, b, "smooth_push", &[], ms(500))).unwrap();
    let t = p.sequence().transition(t1).expect("the cut's transition keeps its id");
    assert_eq!((t.recipe.as_str(), t.params["direction"].clone(), t.duration), ("smooth_push", ParamValue::Choice("left".into()), ms(500)));
    assert_eq!(p.sequence().transitions.len(), 2, "one per cut");
    assert_eq!(e.execute(&mut p, set(a, b, "smooth_push", &[("direction", ParamValue::Choice("left".into()))], ms(500))), Err(EditError::NoOp), "the same again");
    assert_eq!(p.history_log.len(), entries + 1);
    e.execute(&mut p, EditCommand::RemoveTransition(t1)).unwrap();
    e.execute(&mut p, set(a, b, "light_sweep", &[("color", ParamValue::Choice("cold".into()))], s(1))).unwrap();
    let fresh = p.sequence().transitions.iter().find(|t| t.to == b).unwrap();
    assert_ne!(fresh.id, t1, "a cut without one gets a new transition");
    assert_eq!(fresh.params["direction"], ParamValue::Choice("left".into()), "defaults filled in");
    assert_eq!(e.execute(&mut p, set(a, b, "iris", &[], s(1))), Err(EditError::UnknownRecipe("iris".into())));
    assert_eq!(e.execute(&mut p, set(a, b, "look", &[], s(1))), Err(EditError::EffectTarget), "a clip recipe is no transition");
    assert!(matches!(e.execute(&mut p, set(a, b, "wipe", &[("softness", ParamValue::Number(3.0))], s(1))), Err(EditError::InvalidParam(m)) if m.contains("softness")));
    assert_eq!(e.execute(&mut p, set(a, b, "wipe", &[], ms(70))), Err(EditError::InvalidRange), "under two frames (80 ms at 25 fps)");
    assert_eq!(e.execute(&mut p, set(a, c, "wipe", &[], s(1))), Err(EditError::NotACut));
    assert_eq!(e.execute(&mut p, set(b, a, "wipe", &[], s(1))), Err(EditError::NotACut));
    let mut audio = p.clone();
    let clips = std::mem::take(&mut audio.sequence_mut().tracks[0].clips);
    let a1 = audio.sequence().tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
    audio.sequence_mut().tracks[a1].clips = clips;
    assert_eq!(EditEngine::new().execute(&mut audio, set(a, b, "wipe", &[], s(1))), Err(EditError::KindMismatch), "no transitions on sound (D11)");
}

#[test]
fn update_transition_merges_parameters_and_length_and_never_changes_the_unknown() {
    let (mut p, mut e, _, [t1, t2]) = setup();
    e.execute(&mut p, EditCommand::UpdateTransition { id: t2, params: BTreeMap::from([("softness".to_string(), ParamValue::Number(0.5))]), duration: Some(ms(600)) }).unwrap();
    let t = p.sequence().transition(t2).unwrap();
    assert_eq!((t.params["softness"].clone(), t.params["direction"].clone(), t.duration), (ParamValue::Number(0.5), ParamValue::Choice("left".into()), ms(600)), "merged");
    assert_eq!(e.execute(&mut p, EditCommand::UpdateTransition { id: t2, params: BTreeMap::new(), duration: Some(ms(600)) }), Err(EditError::NoOp));
    assert!(matches!(e.execute(&mut p, EditCommand::UpdateTransition { id: t1, params: BTreeMap::from([("style".to_string(), ParamValue::Choice("sepia".into()))]), duration: None }), Err(EditError::InvalidParam(_))));
    assert_eq!(e.execute(&mut p, EditCommand::UpdateTransition { id: t1, params: BTreeMap::new(), duration: Some(ms(40)) }), Err(EditError::InvalidRange));
    assert_eq!(e.execute(&mut p, EditCommand::UpdateTransition { id: TransitionId::new(), params: BTreeMap::new(), duration: Some(s(1)) }), Err(EditError::TransitionNotFound));
    p.sequence_mut().transitions[0].recipe = "iris".into();
    assert_eq!(e.execute(&mut p, EditCommand::UpdateTransition { id: t1, params: BTreeMap::new(), duration: Some(s(2)) }), Err(EditError::UnknownRecipe("iris".into())), "a newer Kadr's transition is only removed");
    e.execute(&mut p, EditCommand::RemoveTransition(t1)).unwrap();
    assert_eq!(e.execute(&mut p, EditCommand::RemoveTransition(t1)), Err(EditError::NoOp));
}

#[test]
fn a_locked_track_keeps_its_transitions() {
    let (mut p, mut e, [a, b, _], [t1, _]) = setup();
    p.sequence_mut().tracks[0].locked = true;
    assert!(matches!(e.execute(&mut p, set(a, b, "wipe", &[], s(1))), Err(EditError::TrackLocked(_))));
    assert!(matches!(e.execute(&mut p, EditCommand::UpdateTransition { id: t1, params: BTreeMap::new(), duration: Some(s(2)) }), Err(EditError::TrackLocked(_))));
    assert!(matches!(e.execute(&mut p, EditCommand::RemoveTransition(t1)), Err(EditError::TrackLocked(_))));
}

#[test]
fn transition_commands_have_their_labels() {
    let (_, _, [a, b, _], [t1, _]) = setup();
    assert_eq!(set(a, b, "wipe", &[], s(1)).label(), "cmd.set_transition");
    assert_eq!(EditCommand::UpdateTransition { id: t1, params: BTreeMap::new(), duration: None }.label(), "cmd.update_transition");
    assert_eq!(EditCommand::RemoveTransition(t1).label(), "cmd.remove_transition");
    assert_eq!((EditCommand::error_key(&EditError::NotACut), EditCommand::error_key(&EditError::TransitionNotFound)), ("err.edit.not_a_cut", "err.edit.transition_not_found"));
}
