use kadr_core::{AudioInfo, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::io;
use kadr_project::*;

fn sample_info() -> MediaInfo {
    MediaInfo {
        kind: MediaKind::Video,
        duration: Time::from_secs(90),
        container: "mov,mp4".into(),
        size_bytes: 1234,
        video: Some(VideoInfo {
            width: 3840,
            height: 2160,
            frame_rate: Some(FrameRate::FPS_23_976),
            variable_frame_rate: false,
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            rotation: 0,
        }),
        audio: Some(AudioInfo { sample_rate: 48_000, channels: 2, codec: "aac".into(), channel_layout: "stereo".into() }),
        timecode: None,
    }
}

fn sample_project(media_dir: &std::path::Path) -> Project {
    let mut p = Project::new("Concert");
    let asset = MediaAsset::new(media_dir.join("cam1.mp4"), sample_info());
    let aid = asset.id;
    p.assets.push(asset);
    let seq = p.sequence_mut();
    let mut clip = Clip::new(aid, "cam1", TimeRange::new(Time::from_secs(5), Time::from_secs(20)), Time::ZERO);
    clip.transform.scale = 1.25;
    clip.audio.gain_db = -3.0;
    clip.keyframes.push(Keyframe {
        property: "transform.opacity".into(),
        time: Time::from_millis(500),
        value: 0.5,
        interpolation: Interpolation::EaseInOut,
    });
    seq.tracks[0].clips.push(clip);
    seq.markers.push(Marker::new(Time::from_secs(3), "Song 2"));
    p.analysis.push(AnalysisResult {
        asset: aid,
        algo_version: 1,
        data: AnalysisData::Silence {
            threshold_db: -40.0,
            min_duration: Time::from_secs(2),
            ranges: vec![TimeRange::new(Time::from_secs(1), Time::from_secs(4))],
        },
    });
    p
}

#[test]
fn save_load_roundtrip_is_lossless() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concert.kadr");
    let mut p = sample_project(dir.path());
    io::save(&mut p, &path).unwrap();
    let loaded = io::load(&path).unwrap();
    assert_eq!(loaded, p);
    assert_eq!(loaded.assets[0].relative_path.as_deref(), Some(std::path::Path::new("cam1.mp4")));
}

#[test]
fn time_is_stored_as_integer_flicks() {
    let dir = tempfile::tempdir().unwrap();
    let p = sample_project(dir.path());
    let json = String::from_utf8(io::to_json(&p)).unwrap();
    assert!(json.contains("\"source_in\": 3528000000"), "5 s must serialize as exact flicks");
    assert!(json.contains("\"num\": 24000"));
}

#[test]
fn moved_project_relinks_media_by_relative_path() {
    let a = tempfile::tempdir().unwrap();
    let path = a.path().join("p.kadr");
    std::fs::write(a.path().join("cam1.mp4"), b"x").unwrap();
    let mut p = sample_project(a.path());
    io::save(&mut p, &path).unwrap();

    let b = tempfile::tempdir().unwrap();
    std::fs::copy(&path, b.path().join("p.kadr")).unwrap();
    std::fs::write(b.path().join("cam1.mp4"), b"x").unwrap();
    drop(a); // original folder gone
    let loaded = io::load(&b.path().join("p.kadr")).unwrap();
    assert_eq!(loaded.assets[0].path, b.path().join("cam1.mp4"));
}

#[test]
fn autosave_newer_than_project_is_offered_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.kadr");
    let mut p = sample_project(dir.path());
    io::save(&mut p, &path).unwrap();
    assert!(io::recovery_candidate(&path).is_none());
    std::thread::sleep(std::time::Duration::from_millis(20));
    io::write_autosave(&p, &io::autosave_path(&path)).unwrap();
    assert_eq!(io::recovery_candidate(&path), Some(io::autosave_path(&path)));
    // Explicit save clears the autosave.
    io::save(&mut p, &path).unwrap();
    assert!(io::recovery_candidate(&path).is_none());
}
