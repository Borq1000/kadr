use kadr_core::{FrameRate, Time, TimeRange};
use kadr_project::*;
use kadr_timeline::*;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

/// One 10 s clip (source 100..110 s) on V1 with fades 3 s / 4 s and a keyframe.
fn setup() -> (Project, EditEngine, kadr_core::ClipId) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let mut c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(s(100), s(110)), s(0));
    c.video_fade = EdgeFades { fade_in: s(3), fade_out: s(4) };
    c.audio.fade_in = s(3);
    c.audio.fade_out = s(4);
    c.animation.set_key(AnimParam::Scale, SourceTime(s(105)), 2.0, None);
    let id = c.id;
    p.sequence_mut().tracks[0].clips.push(c);
    (p, EditEngine::new(), id)
}

fn fades(p: &Project, id: kadr_core::ClipId) -> (i64, i64) {
    let c = p.sequence().clip(id).unwrap();
    (c.video_fade.fade_in.as_millis(), c.video_fade.fade_out.as_millis())
}

#[test]
fn trimming_the_start_shrinks_fade_in_first_and_keeps_keyframes() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::TrimClip { clip: id, edge: TrimEdge::Start, to: s(5), ripple: false }).unwrap();
    assert_eq!(fades(&p, id), (1_000, 4_000));
    let c = p.sequence().clip(id).unwrap();
    assert_eq!(c.animation.times(), vec![SourceTime(s(105))], "source-bound: untouched");
    assert_eq!((c.audio.fade_in, c.audio.fade_out), (s(1), s(4)), "audio fades follow the same rule");
    e.undo(&mut p);
    assert_eq!(fades(&p, id), (3_000, 4_000), "undo restores the stored values");
}

#[test]
fn trimming_the_end_or_speeding_up_shrinks_fade_out_first() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::TrimClip { clip: id, edge: TrimEdge::End, to: s(5), ripple: false }).unwrap();
    assert_eq!(fades(&p, id), (3_000, 2_000));
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Speed(2.0) }).unwrap();
    assert_eq!(fades(&p, id), (3_000, 2_000), "5 s long now; fade durations are not scaled");
    assert_eq!(p.sequence().clip(id).unwrap().animation.times(), vec![SourceTime(s(105))]);
}

#[test]
fn split_zeroes_the_inner_fades_and_copies_the_animation() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::Split { at: s(6), clips: Some(vec![id]) }).unwrap();
    let t = &p.sequence().tracks[0].clips;
    assert_eq!((t[0].video_fade.fade_in, t[0].video_fade.fade_out), (s(3), Time::ZERO));
    assert_eq!((t[1].video_fade.fade_in, t[1].video_fade.fade_out), (Time::ZERO, s(4)));
    assert_eq!(t[0].animation, t[1].animation, "each half has a full copy");
}

#[test]
fn moving_a_clip_touches_neither_fades_nor_keyframes() {
    let (mut p, mut e, id) = setup();
    let before = p.sequence().clip(id).unwrap().clone();
    e.execute(&mut p, EditCommand::MoveClips { clips: vec![id], delta: s(20), track_delta: 0 }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    assert_eq!((&c.video_fade, &c.animation), (&before.video_fade, &before.animation));
}

fn scale_track(p: &Project, id: kadr_core::ClipId) -> Vec<(i64, f64)> {
    p.sequence().clip(id).unwrap().animation.track(AnimParam::Scale).iter().map(|k| (k.at.0.as_millis(), k.value)).collect()
}

#[test]
fn set_and_remove_keyframe_are_one_undo_step_each() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(108)), value: 3.0, ease: None }).unwrap();
    assert_eq!(scale_track(&p, id), vec![(105_000, 2.0), (108_000, 3.0)]);
    e.undo(&mut p);
    assert_eq!(scale_track(&p, id), vec![(105_000, 2.0)]);
}

#[test]
fn removing_the_last_keyframe_keeps_the_picture_at_the_playhead() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::RemoveKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(105)), keep: Some(1.5) }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    assert!(!c.animation.is_animated(AnimParam::Scale));
    assert_eq!(c.transform.scale, 1.5, "the caller's value at the playhead wins");
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::RemoveKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(105)), keep: None }).unwrap();
    assert_eq!(p.sequence().clip(id).unwrap().transform.scale, 2.0, "without keep, the removed value");
}

#[test]
fn a_failed_move_inside_a_batch_loses_no_keyframe() {
    let (mut p, mut e, id) = setup();
    let batch = EditCommand::Batch {
        label: "b".into(),
        commands: vec![
            EditCommand::MoveKeyframes {
                clip: id,
                moves: vec![
                    (AnimParam::Scale, SourceTime(s(105)), SourceTime(s(106))),
                    (AnimParam::Scale, SourceTime(s(101)), SourceTime(s(102))), // no such keyframe
                ],
            },
            EditCommand::SetKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(108)), value: 3.0, ease: None },
        ],
    };
    e.execute(&mut p, batch).unwrap();
    assert_eq!(scale_track(&p, id), vec![(105_000, 2.0), (108_000, 3.0)], "the first move was not half-applied");
}

#[test]
fn moving_keyframes_onto_another_is_rejected_whole() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(108)), value: 3.0, ease: None }).unwrap();
    let before = p.sequence().clone();
    let r = e.execute(&mut p, EditCommand::MoveKeyframes { clip: id, moves: vec![(AnimParam::Scale, SourceTime(s(105)), SourceTime(s(108)))] });
    assert_eq!(r, Err(EditError::KeyframeCollision));
    assert_eq!(p.sequence(), &before);
    assert_eq!(EditCommand::error_key(&EditError::KeyframeCollision), "err.edit.keyframe_collision");
}

#[test]
fn ease_and_clear_animation() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframeEase { clip: id, param: AnimParam::Scale, at: SourceTime(s(105)), ease: Ease::Hold }).unwrap();
    assert_eq!(p.sequence().clip(id).unwrap().animation.track(AnimParam::Scale)[0].ease, Ease::Hold);
    e.execute(&mut p, EditCommand::ClearAnimation { clip: id, statics: vec![(AnimParam::Scale, 1.0), (AnimParam::X, 0.0)] }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    assert!(c.animation.is_empty());
    assert_eq!((c.transform.scale, c.transform.x), (1.0, 0.0));
}

#[test]
fn keyframes_are_video_only_and_values_finite() {
    let (mut p, mut e, id) = setup();
    let bad = EditCommand::SetKeyframe { clip: id, param: AnimParam::X, at: SourceTime(s(101)), value: f64::NAN, ease: None };
    assert_eq!(e.execute(&mut p, bad), Err(EditError::InvalidRange));
    let a = Clip::new(kadr_core::AssetId::new(), "a", TimeRange::new(s(0), s(5)), s(0));
    let aid = a.id;
    p.sequence_mut().tracks[1].clips.push(a); // A1
    let on_audio = EditCommand::SetKeyframe { clip: aid, param: AnimParam::X, at: SourceTime(s(1)), value: 1.0, ease: None };
    assert_eq!(e.execute(&mut p, on_audio), Err(EditError::KindMismatch));
}

#[test]
fn video_fade_property_is_validated() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetClipProperty { clip: id, prop: ClipProperty::VideoFade(EdgeFades { fade_in: s(1), fade_out: s(2) }) }).unwrap();
    assert_eq!(fades(&p, id), (1_000, 2_000));
    let too_long = EditCommand::SetClipProperty { clip: id, prop: ClipProperty::VideoFade(EdgeFades { fade_in: s(6), fade_out: s(5) }) };
    assert_eq!(e.execute(&mut p, too_long), Err(EditError::InvalidRange));
}
