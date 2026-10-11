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
            sar: (1, 1),
            color: None,
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
    clip.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_millis(5_500)), 0.5, Some(Ease::EaseInOut));
    clip.video_fade = EdgeFades { fade_in: Time::from_millis(500), fade_out: Time::ZERO };
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

/// Review focus 5 at the project level: an effect from a newer Kadr — an
/// unknown recipe with an unknown parameter variant, a field this build
/// lacks and an `fx:` track of its own — comes through `io::parse` and a
/// save as the same JSON.
#[test]
fn an_unknown_effect_with_its_track_survives_parse_and_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = sample_project(dir.path());
    let id = kadr_core::EffectId::new();
    let future: AppliedEffect = serde_json::from_value(serde_json::json!({
        "id": id, "recipe": "glitch_v2", "version": 4, "enabled": true, "progress": [0.25, 1.0],
        "params": {"amount": {"number": 0.3}, "curve": {"bezier": [0.1, 0.2]}},
        "mask": {"shape": "oval", "feather": 0.2}
    }))
    .unwrap();
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    c.effects.push(future);
    c.animation.set_key(TrackKey::fx(id, "amount"), SourceTime(Time::from_secs(6)), 0.8, Some(Ease::Linear));
    let json = |p: &Project| serde_json::from_slice::<serde_json::Value>(&io::to_json(p)).unwrap();
    let saved = json(&p);
    let clip = &saved["sequences"][0]["tracks"][0]["clips"][0];
    assert_eq!(clip["effects"][0]["mask"], serde_json::json!({"shape": "oval", "feather": 0.2}), "the newer Kadr's field is written");
    assert_eq!(clip["effects"][0]["params"]["curve"], serde_json::json!({"bezier": [0.1, 0.2]}), "and its parameter variant");
    assert!(clip["animation"].get(format!("fx:{id}:amount")).is_some(), "and its track");
    let loaded = io::parse(&io::to_json(&p), std::path::Path::new("future.kadr")).unwrap();
    assert_eq!(json(&loaded), saved, "parse → save gives the same JSON");
    assert_eq!(loaded, p);
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

/// R25 / Review Focus 2: garbage animation data never fails the project
/// load, and nothing that survives can panic the time conversions.
#[test]
fn garbage_clip_animation_never_fails_the_load() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.kadr");
    let p = sample_project(dir.path());
    let with_animation = |anim: serde_json::Value| {
        let mut v: serde_json::Value = serde_json::from_slice(&io::to_json(&p)).unwrap();
        v["sequences"][0]["tracks"][0]["clips"][0]["animation"] = anim;
        serde_json::to_vec(&v).unwrap()
    };
    for garbage in [json!(null), json!([]), json!("x")] {
        let loaded = io::parse(&with_animation(garbage.clone()), &path).unwrap_or_else(|e| panic!("{garbage}: {e}"));
        let clip = &loaded.sequences[0].tracks[0].clips[0];
        assert!(clip.animation.is_stored_empty(), "{garbage}");
        assert_eq!(clip.transform.scale, 1.25, "the rest of the clip loads");
        assert_eq!(io::parse(&io::to_json(&loaded), &path).unwrap(), loaded, "{garbage}: round trip");
    }

    let extreme = json!({"scale": [{"at": i64::MAX, "value": 2.0}]});
    let loaded = io::parse(&with_animation(extreme.clone()), &path).unwrap();
    let clip = &loaded.sequences[0].tracks[0].clips[0];
    assert!(clip.animation.is_empty(), "never executed");
    assert_eq!(clip.anim_value(AnimParam::Scale, Time::from_secs(1)), 1.25, "the static value applies");
    let saved: serde_json::Value = serde_json::from_slice(&io::to_json(&loaded)).unwrap();
    assert_eq!(saved["sequences"][0]["tracks"][0]["clips"][0]["animation"], extreme, "kept verbatim");
}

#[test]
fn old_project_json_still_loads() {
    // A project written before decisions / angle descriptions existed.
    let mut v = serde_json::to_value(Project::new("old")).unwrap();
    v.as_object_mut().unwrap().remove("jev_decisions");
    v["multicam_groups"] = serde_json::json!([{"id": kadr_core::MulticamId::new(), "name": "g",
        "angles": [{"asset": kadr_core::AssetId::new(), "label": "CAM1", "sync_offset": 0}]}]);
    v["preference_events"] = serde_json::json!([{"at_ms": 1, "action": null, "kind": "camera_changed", "context": {},
        "features": {}, "ai_choice": "CAM1", "ai_confidence": 0.5, "human_choice": "CAM2"}]);
    let p: Project = serde_json::from_value(v).unwrap();
    assert!(p.jev_decisions.is_empty());
    assert_eq!(p.multicam_groups[0].angles[0].description, "");
    assert_eq!(p.preference_events[0].decision, None);
}
