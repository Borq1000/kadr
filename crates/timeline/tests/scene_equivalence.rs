//! For projects the legacy model could show (opaque, untransformed video),
//! the evaluator shows exactly what `video_at` showed: same clip, same
//! source time. Multi-track: the opaque upper clip hides the lower, as before.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::{Clip, MediaAsset, Project, Track, TrackKind};
use kadr_scene::{LayerContent, LayerId, OutputSpec, RenderQuality, SizeU};
use kadr_timeline::composition::video_at;
use kadr_timeline::scene::evaluate;

fn hd() -> MediaAsset {
    let video = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaAsset::new("m", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(120), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
}

fn place(p: &mut Project, track: usize, src_ms: i64, tl_ms: i64, dur_ms: i64) {
    let a = hd();
    p.assets.push(a.clone());
    let c = Clip::new(a.id, "c", TimeRange::new(Time::from_millis(src_ms), Time::from_millis(src_ms + dur_ms)), Time::from_millis(tl_ms));
    let clips = &mut p.sequence_mut().tracks[track].clips;
    clips.push(c);
    clips.sort_by_key(|c| c.timeline_in);
}

fn assert_same(p: &Project) {
    let seq = p.sequence();
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::Export);
    let step = Time::from_millis(1000).mul_ratio(1, 7);
    let mut t = Time::ZERO;
    while t < seq.duration() {
        let scene = evaluate(p, seq, t, &out);
        match video_at(seq, t) {
            None => assert!(scene.layers.is_empty(), "t={t:?}"),
            Some(v) => {
                assert_eq!(scene.layers.len(), 1, "t={t:?}");
                assert_eq!(scene.layers[0].id, LayerId::from(v.clip), "t={t:?}");
                match &scene.layers[0].content {
                    LayerContent::Media { source_time, .. } => assert_eq!(*source_time, v.source_start, "t={t:?}"),
                    other => panic!("{other:?}"),
                }
            }
        }
        t += step;
    }
}

#[test]
fn single_track_with_gaps_matches_video_at() {
    let mut p = Project::new("t");
    place(&mut p, 0, 0, 0, 3_000);
    place(&mut p, 0, 5_000, 3_000, 2_500);
    place(&mut p, 0, 20_000, 7_000, 4_000);
    place(&mut p, 0, 1_000, 11_000, 900);
    assert_same(&p);
}

#[test]
fn opaque_upper_track_hides_the_lower_like_before() {
    let mut p = Project::new("t");
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    place(&mut p, 0, 0, 0, 10_000);
    place(&mut p, 1, 30_000, 2_000, 3_000);
    place(&mut p, 1, 40_000, 7_000, 1_000);
    assert_same(&p);
}

#[test]
fn a_muted_upper_track_shows_nothing_in_both() {
    let mut p = Project::new("t");
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    place(&mut p, 0, 0, 0, 10_000);
    place(&mut p, 1, 30_000, 2_000, 3_000);
    p.sequence_mut().tracks[1].muted = true;
    assert_same(&p);
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::Export);
    let v1 = p.sequence().tracks[0].clips[0].id;
    let scene = evaluate(&p, p.sequence(), Time::from_millis(3_000), &out);
    assert_eq!(scene.layers.len(), 1);
    assert_eq!(scene.layers[0].id, LayerId::from(v1), "the muted V2 clip is not shown; V1 is");
}

#[test]
fn a_muted_only_track_is_black_in_both() {
    let mut p = Project::new("t");
    place(&mut p, 0, 0, 0, 4_000);
    p.sequence_mut().tracks[0].muted = true;
    assert_same(&p);
    assert!(video_at(p.sequence(), Time::from_millis(1_000)).is_none());
}

#[test]
fn a_disabled_clip_is_a_gap_in_both() {
    let mut p = Project::new("t");
    place(&mut p, 0, 0, 0, 3_000);
    place(&mut p, 0, 5_000, 3_000, 2_500);
    place(&mut p, 0, 20_000, 5_500, 3_000);
    p.sequence_mut().tracks[0].clips[1].enabled = false;
    assert_same(&p);
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::Export);
    assert!(evaluate(&p, p.sequence(), Time::from_millis(4_000), &out).layers.is_empty(), "the disabled clip shows nothing");
}

#[test]
fn a_disabled_upper_clip_lets_the_track_below_show_in_both() {
    let mut p = Project::new("t");
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    place(&mut p, 0, 0, 0, 10_000);
    place(&mut p, 1, 30_000, 2_000, 3_000);
    place(&mut p, 1, 40_000, 6_000, 2_000);
    p.sequence_mut().tracks[1].clips[0].enabled = false;
    assert_same(&p);
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::Export);
    let v1 = p.sequence().tracks[0].clips[0].id;
    let scene = evaluate(&p, p.sequence(), Time::from_millis(3_000), &out);
    assert_eq!(scene.layers.iter().map(|l| l.id).collect::<Vec<_>>(), vec![LayerId::from(v1)]);
}
