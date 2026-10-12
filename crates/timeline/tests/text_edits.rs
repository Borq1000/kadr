//! Text clips through the edit engine (titles spec §2.1, §3, §7): where
//! `AddText` puts a title, the text property, trim before source 0, split
//! of the animations, and one undo entry per command.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_timeline::*;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

/// V1 holds a 0..10 s video, V2 above it is empty, A1 below.
fn setup() -> (Project, EditEngine) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    let v = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("m.mp4", MediaInfo { kind: MediaKind::Video, duration: s(60), container: "mp4".into(), size_bytes: 0, video: Some(v), audio: None, timecode: None });
    p.sequence_mut().tracks[0].clips.push(Clip::new(a.id, "m", TimeRange::new(Time::ZERO, s(10)), Time::ZERO));
    p.assets.push(a);
    (p, EditEngine::new())
}

fn title(at: i64, dur: i64) -> Clip {
    Clip::new_text(TextDoc::new("Гол!"), s(at), s(dur))
}

fn track_of(p: &Project, id: kadr_core::ClipId) -> String {
    let (ti, _) = p.sequence().locate_clip(id).unwrap();
    p.sequence().tracks[ti].name.clone()
}

#[test]
fn add_text_goes_on_the_topmost_video_track_or_a_new_one_above() {
    let (mut p, mut e) = setup();
    let a = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: a.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, a.id), "V2", "the topmost video track is free over 2..7 s");
    assert_eq!(e.undo_label(), Some("cmd.add_text"));
    let b = title(4, 5);
    e.execute(&mut p, EditCommand::AddText { clip: b.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, b.id), "V3", "V2 is taken at 4 s: a new track above all");
    let kinds: Vec<_> = p.sequence().tracks.iter().map(|t| (t.name.as_str(), t.kind)).collect();
    assert_eq!(kinds, [("V1", TrackKind::Video), ("V2", TrackKind::Video), ("V3", TrackKind::Video), ("A1", TrackKind::Audio)]);
    let c = title(20, 5);
    e.execute(&mut p, EditCommand::AddText { clip: c.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, c.id), "V3", "the topmost track, free at 20 s");
    e.undo(&mut p);
    e.undo(&mut p);
    assert_eq!(p.sequence().tracks.len(), 3, "one undo takes the title and its new track");
    assert!(p.sequence().clip(b.id).is_none());
}

#[test]
fn add_text_on_a_given_track_checks_it() {
    let (mut p, mut e) = setup();
    let (v1, v2, a1) = (p.sequence().tracks[0].id, p.sequence().tracks[1].id, p.sequence().tracks[2].id);
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: title(2, 5), track: Some(v1) }), Err(EditError::Overlap));
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: title(2, 5), track: Some(a1) }), Err(EditError::KindMismatch));
    p.sequence_mut().tracks[1].locked = true;
    assert!(matches!(e.execute(&mut p, EditCommand::AddText { clip: title(2, 5), track: Some(v2) }), Err(EditError::TrackLocked(_))));
    let media = p.sequence().tracks[0].clips[0].clone();
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: media, track: None }), Err(EditError::KindMismatch), "only text clips");
    let t = title(12, 3);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: Some(v1) }).unwrap();
    assert_eq!(track_of(&p, t.id), "V1");
    let u = title(2, 3);
    e.execute(&mut p, EditCommand::AddText { clip: u.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, u.id), "V3", "the topmost (V2) is locked: a new track");
}

#[test]
fn the_text_property_changes_the_doc_once() {
    let (mut p, mut e) = setup();
    let t = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    let mut doc = TextDoc::new("Победа");
    doc.seed = 7;
    let set = |d: &TextDoc| EditCommand::SetClipProperty { clip: t.id, prop: ClipProperty::Text(Box::new(d.clone())) };
    e.execute(&mut p, set(&doc)).unwrap();
    assert_eq!(p.sequence().clip(t.id).unwrap().text.as_deref(), Some(&doc));
    assert_eq!(e.execute(&mut p, set(&doc)), Err(EditError::NoOp));
    let media = p.sequence().tracks[0].clips[0].id;
    assert_eq!(e.execute(&mut p, EditCommand::SetClipProperty { clip: media, prop: ClipProperty::Text(Box::new(doc)) }), Err(EditError::KindMismatch));
}

#[test]
fn a_text_clip_trims_before_source_zero_and_a_video_does_not() {
    let (mut p, mut e) = setup();
    let t = title(5, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    e.execute(&mut p, EditCommand::TrimClip { clip: t.id, edge: TrimEdge::Start, to: s(3), ripple: false }).unwrap();
    let c = p.sequence().clip(t.id).unwrap();
    assert_eq!((c.timeline_in, c.source_in, c.source_out), (s(3), s(-2), s(5)));
    e.execute(&mut p, EditCommand::TrimClip { clip: t.id, edge: TrimEdge::End, to: s(40), ripple: false }).unwrap();
    assert_eq!(p.sequence().clip(t.id).unwrap().timeline_out, s(40), "no source end either");
    let v = p.sequence().tracks[0].clips[0].id;
    e.execute(&mut p, EditCommand::MoveClips { clips: vec![v], delta: s(5), track_delta: 0 }).unwrap();
    assert_eq!(e.execute(&mut p, EditCommand::TrimClip { clip: v, edge: TrimEdge::Start, to: s(1), ripple: false }), Err(EditError::NoOp), "a video stops at its source start");
}

#[test]
fn split_hands_the_appearance_left_and_the_disappearance_right() {
    let (mut p, mut e) = setup();
    let mut doc = TextDoc::new("раз два три");
    let rev = |preset: &str| Reveal { preset: preset.into(), unit: Unit::Word, duration: Time::from_millis(400), stagger: Time::from_millis(100), ease: "smooth".into(), extra: Default::default() };
    doc.anim.reveal_in = Some(rev("rise"));
    doc.anim.reveal_out = Some(rev("fade"));
    doc.anim.highlight = Some(Highlight { colour: Rgba8::WHITE, scale: 1.1, span: (0.0, 1.0), extra: Default::default() });
    let t = Clip::new_text(doc.clone(), s(2), s(4));
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    e.execute(&mut p, EditCommand::Split { at: s(3), clips: Some(vec![t.id]) }).unwrap();
    let v2 = &p.sequence().tracks[1].clips;
    assert_eq!(v2.len(), 2);
    let (l, r) = (v2[0].text.as_deref().unwrap(), v2[1].text.as_deref().unwrap());
    assert_eq!((l.anim.reveal_in.is_some(), l.anim.reveal_out.is_some(), r.anim.reveal_in.is_some(), r.anim.reveal_out.is_some()), (true, false, false, true));
    assert_eq!((l.anim.highlight.as_ref().unwrap().span, r.anim.highlight.as_ref().unwrap().span), ((0.0, 0.25), (0.25, 1.0)));
    assert_eq!((l.seed, r.seed, l.plain(), r.plain()), (doc.seed, doc.seed, doc.plain(), doc.plain()));
    assert_eq!((v2[1].source_in, v2[1].timeline_in), (s(1), s(3)), "the right piece continues the text's own time");
}

#[test]
fn a_text_clip_cannot_go_on_an_audio_track() {
    let (mut p, mut e) = setup();
    let a1 = p.sequence().tracks[2].id;
    let cmd = EditCommand::InsertClip { track: a1, clip: title(1, 2), mode: InsertMode::Overwrite };
    assert_eq!(e.execute(&mut p, cmd), Err(EditError::KindMismatch));
}

#[test]
fn a_document_with_a_non_finite_number_is_refused() {
    let (mut p, mut e) = setup();
    let mut nan = TextDoc::new("x");
    nan.style.size = f32::NAN;
    let bad = Clip::new_text(nan.clone(), s(2), s(5));
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: bad.clone(), track: None }), Err(EditError::InvalidRange));
    let v2 = p.sequence().tracks[1].id;
    assert_eq!(e.execute(&mut p, EditCommand::InsertClip { track: v2, clip: bad, mode: InsertMode::Overwrite }), Err(EditError::InvalidRange));
    let t = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    let set = EditCommand::SetClipProperty { clip: t.id, prop: ClipProperty::Text(Box::new(nan)) };
    assert_eq!(e.execute(&mut p, set), Err(EditError::InvalidRange));
    assert_eq!(p.sequence().clip(t.id).unwrap().text.as_ref().unwrap().style.size, 96.0, "unchanged");
}

#[test]
fn the_text_property_stores_a_normalised_doc() {
    let (mut p, mut e) = setup();
    let t = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    let mut split = p.sequence().clip(t.id).unwrap().text.as_deref().unwrap().clone();
    split.runs = vec![TextRun { text: "Г".into(), ..Default::default() }, TextRun { text: "ол!".into(), ..Default::default() }];
    let cmd = EditCommand::SetClipProperty { clip: t.id, prop: ClipProperty::Text(Box::new(split)) };
    assert_eq!(e.execute(&mut p, cmd), Err(EditError::NoOp), "the same text in two runs of one style is the same document");
}

#[test]
fn a_titles_name_follows_its_text_until_renamed() {
    let (mut p, mut e) = setup();
    let t = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    let doc_of = |p: &Project| p.sequence().clip(t.id).unwrap().text.as_deref().unwrap().clone();
    let set_text = |p: &Project, text: &str| {
        let mut d = doc_of(p);
        d.set_plain(text);
        EditCommand::SetClipProperty { clip: t.id, prop: ClipProperty::Text(Box::new(d)) }
    };
    let name = |p: &Project| p.sequence().clip(t.id).unwrap().name.clone();
    let cmd = set_text(&p, "Победа!\nвторая строка");
    e.execute(&mut p, cmd).unwrap();
    assert_eq!(name(&p), "Победа!", "the automatic name follows the first line");
    e.execute(&mut p, EditCommand::SetClipProperty { clip: t.id, prop: ClipProperty::Name("Финал".into()) }).unwrap();
    let cmd = set_text(&p, "Ничья");
    e.execute(&mut p, cmd).unwrap();
    assert_eq!(name(&p), "Финал", "a name the user gave stays");
}

#[test]
fn text_clips_always_carry_the_text_asset() {
    let (mut p, mut e) = setup();
    let other = p.assets[0].id;
    let mut a = title(12, 2);
    a.asset = other;
    e.execute(&mut p, EditCommand::AddText { clip: a.clone(), track: None }).unwrap();
    assert_eq!(p.sequence().clip(a.id).unwrap().asset, TEXT_ASSET);
    let mut b = title(20, 2);
    b.asset = other;
    let v1 = p.sequence().tracks[0].id;
    e.execute(&mut p, EditCommand::InsertClip { track: v1, clip: b.clone(), mode: InsertMode::Overwrite }).unwrap();
    assert_eq!(p.sequence().clip(b.id).unwrap().asset, TEXT_ASSET);
}

#[test]
fn add_text_skips_a_hidden_topmost_track() {
    let (mut p, mut e) = setup();
    p.sequence_mut().tracks[1].muted = true;
    let t = title(2, 3);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, t.id), "V3", "V2 is hidden: a title there would not be seen");
    let v2 = p.sequence().tracks[1].id;
    let u = title(2, 3);
    e.execute(&mut p, EditCommand::AddText { clip: u.clone(), track: Some(v2) }).unwrap();
    assert_eq!(track_of(&p, u.id), "V2", "asked for by name: allowed");
}
