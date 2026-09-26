use kadr_core::{AudioInfo, FrameRate, LinkId, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_timeline::*;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}
fn ms(x: i64) -> Time {
    Time::from_millis(x)
}

fn video_info(dur: Time) -> MediaInfo {
    MediaInfo {
        kind: MediaKind::Video,
        duration: dur,
        container: "mp4".into(),
        size_bytes: 0,
        video: Some(VideoInfo {
            width: 1920,
            height: 1080,
            frame_rate: Some(FrameRate::FPS_25),
            variable_frame_rate: false,
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            rotation: 0,
        }),
        audio: Some(AudioInfo { sample_rate: 48000, channels: 2, codec: "aac".into(), channel_layout: String::new() }),
        timecode: None,
    }
}

/// Project with a 60 s linked A/V clip at 0 on V1/A1 and an external audio
/// clip on A2.
fn setup() -> (Project, EditEngine) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let asset = MediaAsset::new("cam.mp4", video_info(s(120)));
    let ext = MediaAsset::new("ext.wav", MediaInfo { kind: MediaKind::Audio, video: None, ..video_info(s(120)) });
    let (aid, eid) = (asset.id, ext.id);
    p.assets.extend([asset, ext]);
    let link = LinkId::new();
    let seq = p.sequence_mut();
    seq.tracks.push(Track::new(TrackKind::Audio, "A2"));
    let mut v = Clip::new(aid, "cam", TimeRange::new(s(0), s(60)), s(0));
    v.link = Some(link);
    let mut a = v.clone();
    a.id = kadr_core::ClipId::new();
    seq.tracks[0].clips.push(v);
    seq.tracks[1].clips.push(a);
    seq.tracks[2].clips.push(Clip::new(eid, "ext", TimeRange::new(s(0), s(60)), s(0)));
    (p, EditEngine::new())
}

fn spans(p: &Project, track: usize) -> Vec<(i64, i64)> {
    p.sequence().tracks[track].clips.iter().map(|c| (c.timeline_in.as_millis(), c.timeline_out.as_millis())).collect()
}

#[test]
fn split_then_ripple_delete_then_undo_redo() {
    let (mut p, mut e) = setup();
    let original = p.sequence().clone();
    e.execute(&mut p, EditCommand::Split { at: s(10), clips: None }).unwrap();
    e.execute(&mut p, EditCommand::Split { at: s(20), clips: None }).unwrap();
    // Middle V1 piece (linked with middle A1 piece).
    let mid = p.sequence().tracks[0].clips[1].id;
    e.execute(&mut p, EditCommand::DeleteClips { clips: vec![mid], ripple: true }).unwrap();

    assert_eq!(spans(&p, 0), vec![(0, 10_000), (10_000, 50_000)]);
    assert_eq!(spans(&p, 1), vec![(0, 10_000), (10_000, 50_000)], "linked audio follows");
    // A2 was split (Split with clips:None hits all tracks) but not ripple-deleted.
    assert_eq!(spans(&p, 2).len(), 3);
    // Source continuity after ripple: second piece starts at source 20 s.
    assert_eq!(p.sequence().tracks[0].clips[1].source_in, s(20));

    e.undo(&mut p);
    e.undo(&mut p);
    e.undo(&mut p);
    assert_eq!(*p.sequence(), original);
    e.redo(&mut p);
    e.redo(&mut p);
    e.redo(&mut p);
    assert_eq!(spans(&p, 0), vec![(0, 10_000), (10_000, 50_000)]);
}

#[test]
fn delete_range_ripple_keeps_all_tracks_in_sync() {
    let (mut p, mut e) = setup();
    e.execute(&mut p, EditCommand::DeleteRange { range: TimeRange::new(s(5), s(8)), ripple: true }).unwrap();
    for t in 0..3 {
        assert_eq!(spans(&p, t), vec![(0, 5_000), (5_000, 57_000)], "track {t}");
        assert_eq!(p.sequence().tracks[t].clips[1].source_in, s(8));
    }
}

#[test]
fn split_right_halves_are_linked_to_each_other_only() {
    let (mut p, mut e) = setup();
    e.execute(&mut p, EditCommand::Split { at: s(30), clips: None }).unwrap();
    let seq = p.sequence();
    let (vl, vr) = (&seq.tracks[0].clips[0], &seq.tracks[0].clips[1]);
    let (al, ar) = (&seq.tracks[1].clips[0], &seq.tracks[1].clips[1]);
    assert_eq!(vl.link, al.link);
    assert_eq!(vr.link, ar.link);
    assert_ne!(vl.link, vr.link);
}

#[test]
fn move_linked_clips_overwrites_destination() {
    let (mut p, mut e) = setup();
    // Put a second clip on V1/A1 at 70..80.
    let asset = p.assets[0].id;
    let v1 = p.sequence().tracks[0].id;
    e.execute(
        &mut p,
        EditCommand::InsertClip {
            track: v1,
            clip: Clip::new(asset, "b", TimeRange::new(s(0), s(10)), s(70)),
            mode: InsertMode::Overwrite,
        },
    )
    .unwrap();
    let first = p.sequence().tracks[0].clips[0].id;
    e.execute(&mut p, EditCommand::MoveClips { clips: vec![first], delta: s(15), track_delta: 0 }).unwrap();
    // 15..75 overwrote the start of 70..80 → 75..80 remains.
    assert_eq!(spans(&p, 0), vec![(15_000, 75_000), (75_000, 80_000)]);
    assert_eq!(spans(&p, 1), vec![(15_000, 75_000)], "linked audio moved too");
    // Can't move before zero: delta is clamped.
    e.execute(&mut p, EditCommand::MoveClips { clips: vec![first], delta: s(-100), track_delta: 0 }).unwrap();
    assert_eq!(spans(&p, 0)[0], (0, 60_000));
}

#[test]
fn trim_clamps_to_source_and_neighbours() {
    let (mut p, mut e) = setup();
    let v = p.sequence().tracks[0].clips[0].id;
    // Extend end beyond the 120 s source: clamps to 120.
    e.execute(&mut p, EditCommand::TrimClip { clip: v, edge: TrimEdge::End, to: s(500), ripple: false }).unwrap();
    assert_eq!(spans(&p, 0), vec![(0, 120_000)]);
    assert_eq!(spans(&p, 1), vec![(0, 120_000)], "linked trim");
    // Start can't go before source 0.
    let r = e.execute(&mut p, EditCommand::TrimClip { clip: v, edge: TrimEdge::Start, to: s(-5), ripple: false });
    assert_eq!(r, Err(EditError::NoOp));
    // Trim start with ripple: clip stays at 0, content shortens.
    e.execute(&mut p, EditCommand::TrimClip { clip: v, edge: TrimEdge::Start, to: s(20), ripple: true }).unwrap();
    assert_eq!(spans(&p, 0), vec![(0, 100_000)]);
    assert_eq!(p.sequence().tracks[0].clips[0].source_in, s(20));
}

#[test]
fn edits_are_frame_quantized() {
    let (mut p, mut e) = setup();
    // 25 fps → 40 ms frames; 10.019 s snaps to 10.02 s? No: nearest frame boundary is 10.000 or 10.040.
    e.execute(&mut p, EditCommand::Split { at: ms(10_019), clips: None }).unwrap();
    assert_eq!(p.sequence().tracks[0].clips[0].timeline_out, ms(10_000));
}

#[test]
fn locked_track_is_untouched() {
    let (mut p, mut e) = setup();
    let a2 = p.sequence().tracks[2].id;
    e.execute(&mut p, EditCommand::SetTrackFlag { track: a2, flag: TrackFlag::Lock, value: true }).unwrap();
    e.execute(&mut p, EditCommand::DeleteRange { range: TimeRange::new(s(0), s(5)), ripple: true }).unwrap();
    assert_eq!(spans(&p, 2), vec![(0, 60_000)]);
    let ext = p.sequence().tracks[2].clips[0].id;
    assert!(matches!(e.execute(&mut p, EditCommand::DeleteClips { clips: vec![ext], ripple: false }), Err(EditError::TrackLocked(_))));
}

#[test]
fn ai_batch_is_one_undo_step() {
    let (mut p, mut e) = setup();
    let original = p.sequence().clone();
    let action = kadr_core::ActionId::new();
    let cmds = (0..5)
        .rev()
        .map(|i| EditCommand::DeleteRange { range: TimeRange::new(s(i * 10), s(i * 10 + 2)), ripple: true })
        .collect();
    e.execute_as(&mut p, EditCommand::Batch { label: "AI: remove pauses".into(), commands: cmds }, EditSource::Ai, Some(action))
        .unwrap();
    assert_eq!(p.sequence().duration(), s(50));
    assert_eq!(e.undo_label(), Some("AI: remove pauses"));
    assert!(e.undo_action(&mut p, action).is_some());
    assert_eq!(*p.sequence(), original);
    assert_eq!(p.history_log.last().unwrap().source, EditSource::Ai);
}

#[test]
fn failed_command_leaves_sequence_untouched() {
    let (mut p, mut e) = setup();
    let before = p.sequence().clone();
    let bogus = kadr_core::ClipId::new();
    let batch = EditCommand::Batch {
        label: "x".into(),
        commands: vec![
            EditCommand::DeleteRange { range: TimeRange::new(s(0), s(5)), ripple: true },
            EditCommand::DeleteClips { clips: vec![bogus], ripple: false },
        ],
    };
    // The bogus clip resolves to nothing → NoOp inside with_links → still Ok? Force a real error:
    let v = p.sequence().tracks[0].clips[0].id;
    let batch2 = EditCommand::Batch {
        label: "y".into(),
        commands: vec![
            EditCommand::DeleteRange { range: TimeRange::new(s(0), s(5)), ripple: true },
            EditCommand::SetClipProperty { clip: v, prop: ClipProperty::Speed(0.0) },
        ],
    };
    let _ = e.execute(&mut p, batch);
    e.undo(&mut p);
    assert!(e.execute(&mut p, batch2).is_err());
    assert_eq!(*p.sequence(), before);
    assert!(!e.can_redo() || e.redo_label().is_some());
}

/// Deterministic fuzz: random edits, then undo everything → identical to
/// the start; redo everything → identical to the end state.
#[test]
fn random_edit_sequences_are_fully_reversible() {
    let mut rng = 0x2545F4914F6CDD1Du64;
    let mut next = move |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    for _round in 0..20 {
        let (mut p, mut e) = setup();
        let start = p.sequence().clone();
        let mut ok = 0;
        for _ in 0..40 {
            let seq = p.sequence();
            let all: Vec<_> = seq.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
            let pick = all.get(next(all.len().max(1) as u64) as usize).copied();
            let t = ms(next(90_000) as i64);
            let cmd = match (next(6), pick) {
                (0, _) => EditCommand::Split { at: t, clips: None },
                (1, Some(c)) => EditCommand::DeleteClips { clips: vec![c], ripple: next(2) == 0 },
                (2, Some(c)) => EditCommand::MoveClips { clips: vec![c], delta: ms(next(20_000) as i64 - 10_000), track_delta: 0 },
                (3, Some(c)) => EditCommand::TrimClip {
                    clip: c,
                    edge: if next(2) == 0 { TrimEdge::Start } else { TrimEdge::End },
                    to: t,
                    ripple: next(2) == 0,
                },
                (4, _) => EditCommand::DeleteRange { range: TimeRange::new(t, t + ms(next(5000) as i64 + 40)), ripple: next(2) == 0 },
                _ => EditCommand::AddMarker(Marker::new(t, "m")),
            };
            if e.execute(&mut p, cmd).is_ok() {
                ok += 1;
            }
            for tr in &p.sequence().tracks {
                assert!(tr.is_sorted_and_disjoint(), "invariant broken");
            }
        }
        let end = p.sequence().clone();
        for _ in 0..ok {
            e.undo(&mut p).unwrap();
        }
        assert_eq!(*p.sequence(), start);
        for _ in 0..ok {
            e.redo(&mut p).unwrap();
        }
        assert_eq!(*p.sequence(), end);
    }
}

#[test]
fn track_management_is_undoable_and_safe() {
    let (mut p, mut e) = setup();
    let original = p.sequence().clone();
    // Non-empty track can't be removed.
    let v1 = p.sequence().tracks[0].id;
    assert_eq!(e.execute(&mut p, EditCommand::RemoveTrack { track: v1 }), Err(EditError::TrackNotEmpty));
    e.execute(&mut p, EditCommand::AddTrack { kind: TrackKind::Video }).unwrap();
    let v2 = p.sequence().tracks[1].id;
    assert_eq!(p.sequence().tracks[1].name, "V2");
    e.execute(&mut p, EditCommand::RenameTrack { track: v2, name: "  Cam B  ".into() }).unwrap();
    assert_eq!(p.sequence().tracks[1].name, "Cam B");
    e.execute(&mut p, EditCommand::RemoveTrack { track: v2 }).unwrap();
    e.undo(&mut p);
    assert_eq!(p.sequence().tracks[1].name, "Cam B", "removed track restored with its name");
    e.undo(&mut p);
    e.undo(&mut p);
    assert_eq!(*p.sequence(), original);
}

#[test]
fn marker_update_renames_and_resorts() {
    let (mut p, mut e) = setup();
    let m = Marker::new(s(5), "a");
    e.execute(&mut p, EditCommand::AddMarker(m.clone())).unwrap();
    e.execute(&mut p, EditCommand::AddMarker(Marker::new(s(10), "b"))).unwrap();
    let mut m2 = m.clone();
    m2.name = "Song 2".into();
    m2.time = s(20);
    e.execute(&mut p, EditCommand::UpdateMarker(m2)).unwrap();
    let names: Vec<_> = p.sequence().markers.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, vec!["b", "Song 2"]);
}

#[test]
fn history_marker_returns_to_saved_state() {
    let (mut p, mut e) = setup();
    let saved = e.history_marker();
    e.execute(&mut p, EditCommand::Split { at: s(10), clips: None }).unwrap();
    assert_ne!(e.history_marker(), saved);
    e.undo(&mut p);
    assert_eq!(e.history_marker(), saved, "undo back to the saved point = clean");
    e.redo(&mut p);
    let after = e.history_marker();
    e.undo(&mut p);
    e.execute(&mut p, EditCommand::Split { at: s(20), clips: None }).unwrap();
    assert_ne!(e.history_marker(), after, "a different edit is a different state");
}

/// Two 60 s angles; CAM2 started recording 2 s later (sync_offset −2 s:
/// source = group + offset). One clip shows CAM1 for group time `g0..g1`.
fn multicam_project(g0: i64, g1: i64) -> (Project, EditEngine, kadr_core::ClipId, MulticamGroup) {
    let mut p = Project::new("mc");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let a = MediaAsset::new("cam1.mp4", video_info(s(60)));
    let b = MediaAsset::new("cam2.mp4", video_info(s(60)));
    let group = MulticamGroup {
        id: kadr_core::MulticamId::new(),
        name: "show".into(),
        angles: vec![
            MulticamAngle { asset: a.id, label: "CAM1".into(), description: String::new(), sync_offset: s(0), sync_method: SyncMethod::Manual },
            MulticamAngle { asset: b.id, label: "CAM2".into(), description: String::new(), sync_offset: s(-2), sync_method: SyncMethod::Manual },
        ],
        master_audio: None,
    };
    let mut clip = Clip::new(a.id, "CAM1", TimeRange::new(s(g0), s(g1)), s(0));
    clip.multicam = Some(MulticamSelection { group: group.id, angle: 0 });
    let id = clip.id;
    p.assets.extend([a, b]);
    p.multicam_groups.push(group.clone());
    p.sequence_mut().tracks[0].clips.push(clip);
    (p, EditEngine::new(), id, group)
}

#[test]
fn switch_angle_keeps_timing_and_maps_source() {
    let (mut p, mut e, clip, _) = multicam_project(10, 20);
    e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: None }).unwrap();
    let c = p.sequence().clip(clip).unwrap();
    assert_eq!((c.timeline_in, c.timeline_out), (s(0), s(10)));
    assert_eq!((c.source_in, c.source_out), (s(8), s(18)), "group 10 s is CAM2 source 8 s");
    assert_eq!(c.asset, p.multicam_groups[0].angles[1].asset);
    assert_eq!(c.name, "CAM2");
    assert_eq!(c.multicam.as_ref().unwrap().angle, 1);
    assert_eq!(e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: None }), Err(EditError::NoOp));
    e.undo(&mut p).unwrap();
    assert_eq!(p.sequence().clip(clip).unwrap().source_in, s(10));
}

#[test]
fn switch_to_angle_without_media_fails() {
    let (mut p, mut e, clip, _) = multicam_project(0, 10);
    let before = p.sequence().clone();
    assert_eq!(e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: None }), Err(EditError::InvalidRange));
    assert_eq!(p.sequence(), &before);
    assert_eq!(e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 7, at: None }), Err(EditError::InvalidRange));
}

#[test]
fn switch_at_a_time_is_a_live_cut() {
    let (mut p, mut e, clip, _) = multicam_project(10, 20);
    e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: Some(s(4)) }).unwrap();
    let v1 = &p.sequence().tracks[0].clips;
    assert_eq!(v1.len(), 2);
    assert_eq!((v1[0].name.as_str(), v1[0].source_in, v1[0].timeline_out), ("CAM1", s(10), s(4)));
    assert_eq!((v1[1].name.as_str(), v1[1].source_in, v1[1].timeline_out), ("CAM2", s(12), s(10)));
    e.undo(&mut p).unwrap();
    assert_eq!(p.sequence().tracks[0].clips.len(), 1, "one undo step");
}

#[test]
fn an_off_frame_live_cut_switches_the_part_after_the_cut() {
    // During playback the playhead follows the audio clock, between frames.
    let (mut p, mut e, clip, _) = multicam_project(10, 20);
    e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: Some(Time::from_millis(4_030)) }).unwrap();
    let v1: Vec<_> = p.sequence().tracks[0].clips.iter().map(|c| (c.name.clone(), c.timeline_in.as_millis())).collect();
    assert_eq!(v1, vec![("CAM1".to_string(), 0), ("CAM2".to_string(), 4_040)]);
}

#[test]
fn a_live_cut_within_half_a_frame_of_the_clip_end_does_nothing_quietly() {
    let (mut p, mut e, clip, _) = multicam_project(10, 20);
    assert_eq!(e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: Some(Time::from_millis(9_990)) }), Err(EditError::NoOp));
    // …and half a frame after the start switches the whole clip.
    e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: Some(Time::from_millis(10)) }).unwrap();
    let v1 = &p.sequence().tracks[0].clips;
    assert_eq!((v1.len(), v1[0].name.as_str()), (1, "CAM2"));
}

#[test]
fn no_group_clip_for_an_angle_whose_media_is_gone() {
    let (mut p, _, _, group) = multicam_project(10, 20);
    let cam1 = group.angles[0].asset;
    p.assets.retain(|a| a.id != cam1);
    assert!(multicam::group_clip(&group, 0, &p.assets, s(0)).is_none());
}

#[test]
fn group_span_and_clip_cover_all_angles() {
    let (p, _, _, group) = multicam_project(10, 20);
    // CAM1 covers group 0..60, CAM2 covers group 2..62.
    assert_eq!(multicam::group_span(&group, &p.assets), TimeRange::new(s(2), s(60)));
    let c = multicam::group_clip(&group, 1, &p.assets, s(5)).unwrap();
    assert_eq!((c.source_in, c.source_out, c.timeline_in, c.timeline_out), (s(0), s(58), s(5), s(63)));
    assert_eq!(multicam::group_time_at(&group, &c, s(5)), Some(s(2)));
}

#[test]
fn set_angle_range_cuts_both_edges_and_switches_inside() {
    let (mut p, mut e, _, _) = multicam_project(10, 20);
    e.execute(&mut p, EditCommand::SetAngleRange { range: TimeRange::new(s(3), s(6)), angle: 1 }).unwrap();
    let v1: Vec<_> = p.sequence().tracks[0].clips.iter().map(|c| (c.name.clone(), c.timeline_in, c.timeline_out)).collect();
    assert_eq!(v1, vec![("CAM1".into(), s(0), s(3)), ("CAM2".into(), s(3), s(6)), ("CAM1".into(), s(6), s(10))]);
    // A range already on that angle is a no-op; one undo restores everything.
    assert_eq!(e.execute(&mut p, EditCommand::SetAngleRange { range: TimeRange::new(s(3), s(6)), angle: 1 }), Err(EditError::NoOp));
    e.undo(&mut p).unwrap();
    assert_eq!(p.sequence().tracks[0].clips.len(), 1);
}

#[test]
fn set_angle_range_outside_multicam_fails() {
    let (mut p, mut e, _, _) = multicam_project(10, 20);
    assert_eq!(e.execute(&mut p, EditCommand::SetAngleRange { range: TimeRange::new(s(12), s(14)), angle: 1 }), Err(EditError::ClipNotFound));
}
