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
