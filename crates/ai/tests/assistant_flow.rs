use kadr_ai::command::{self, Permissions, ValidationError};
use kadr_ai::privacy::PrivacyMode;
use kadr_ai::*;
use kadr_core::{AudioInfo, LinkId, MediaInfo, MediaKind, Time, TimeRange, VideoInfo, FrameRate};
use kadr_project::*;
use kadr_timeline::{EditCommand, EditEngine};

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

/// 60 s linked A/V clip whose audio is silent at 10–13 s (3 s), 30–31 s (1 s)
/// and 45–50 s (5 s) of source time; placed on the timeline at 0.
fn project() -> Project {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let info = MediaInfo {
        kind: MediaKind::Video,
        duration: s(60),
        container: "mp4".into(),
        size_bytes: 0,
        video: Some(VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0 }),
        audio: Some(AudioInfo { sample_rate: 48000, channels: 2, codec: "aac".into(), channel_layout: String::new() }),
        timecode: None,
    };
    let a = MediaAsset::new("talk.mp4", info);
    let aid = a.id;
    p.assets.push(a);
    p.analysis.push(AnalysisResult {
        asset: aid,
        algo_version: 1,
        data: AnalysisData::Silence {
            threshold_db: -40.0,
            min_duration: Time::from_millis(300),
            ranges: vec![TimeRange::new(s(10), s(13)), TimeRange::new(s(30), s(31)), TimeRange::new(s(45), s(50))],
        },
    });
    let link = LinkId::new();
    let seq = p.sequence_mut();
    let mut v = Clip::new(aid, "talk", TimeRange::new(s(0), s(60)), s(0));
    v.link = Some(link);
    let mut au = v.clone();
    au.id = kadr_core::ClipId::new();
    seq.tracks[0].clips.push(v);
    seq.tracks[1].clips.push(au);
    p
}

#[test]
fn remove_pauses_end_to_end_locally_and_undo_in_one_step() {
    kadr_i18n::set_lang(kadr_i18n::Lang::Ru);
    let mut p = project();
    let original = p.sequence().clone();
    let ai = Assistant::new(AiSettings::default()); // LocalOnly by default
    assert_eq!(ai.status_label(), "AI: LOCAL");

    let reply = ai.handle("Удали паузы длиннее двух секунд", &p, &EditorState::default());
    let Reply::Plan(plan) = reply else { panic!("expected plan, got {reply:?}") };
    assert_eq!(plan.tier, Tier::Local);
    assert_eq!(plan.cost_usd, 0.0);
    assert_eq!(plan.items.len(), 2, "3 s and 5 s pauses; the 1 s one stays");
    assert!(plan.findings.iter().any(|f| f.contains("1") && f.contains("оставлен")), "{:?}", plan.findings);
    // Latest first (ripple safety).
    assert!(plan.items[0].start > plan.items[1].start);

    let cmds = command::validate(&plan.enabled_commands(), &p, &Permissions::default()).unwrap();
    let mut engine = EditEngine::new();
    engine
        .execute_as(&mut p, EditCommand::Batch { label: "AI: Удаление пауз".into(), commands: cmds }, EditSource::Ai, Some(plan.id))
        .unwrap();
    // Removed (3 − 0.3) + (5 − 0.3) s, with 150 ms padding quantized to
    // 25 fps frames (10.15→10.16, 12.85→12.84 …) = 2.68 + 4.68 = 7.36 s.
    assert_eq!(p.sequence().duration(), Time::from_millis(52_640));
    assert_eq!(plan.after_duration, Some(Time::from_millis(52_640)), "plan preview matches the result");
    // A/V stay in sync on both tracks.
    let v: Vec<_> = p.sequence().tracks[0].clips.iter().map(|c| (c.timeline_in, c.source_in)).collect();
    let a: Vec<_> = p.sequence().tracks[1].clips.iter().map(|c| (c.timeline_in, c.source_in)).collect();
    assert_eq!(v, a);
    assert_eq!(v.len(), 3);

    assert!(engine.undo_action(&mut p, plan.id).is_some());
    assert_eq!(*p.sequence(), original);
}

#[test]
fn review_can_disable_items() {
    let p = project();
    let ai = Assistant::new(AiSettings::default());
    let Reply::Plan(mut plan) = ai.handle("удали паузы длиннее 2 секунд", &p, &EditorState::default()) else { panic!() };
    plan.items[0].enabled = false;
    assert_eq!(plan.enabled_commands().len(), 1);
}

#[test]
fn missing_analysis_is_reported_not_guessed() {
    let mut p = project();
    p.analysis.clear();
    let ai = Assistant::new(AiSettings::default());
    assert!(matches!(ai.handle("удали паузы", &p, &EditorState::default()), Reply::NeedsAnalysis { .. }));
}

#[test]
fn unknown_request_in_local_only_mode_is_blocked_before_any_network() {
    let p = project();
    let ai = Assistant::new(AiSettings::default());
    let Reply::CloudOffer(offer) = ai.handle("Оставь только лучшие моменты", &p, &EditorState::default()) else { panic!() };
    assert_eq!(offer.blocked_reason().as_deref(), Some("privacy.deny.local_only"));
    assert!(offer.estimate.high_usd > 0.0, "cost preview is still computed");
    // Even if the UI tried to run it, the assistant refuses.
    let (tx, rx) = std::sync::mpsc::channel();
    ai.run_cloud(offer, p, true, kadr_core::CancelToken::new(), move |r| tx.send(r.is_err()).unwrap());
    assert!(rx.recv().unwrap());
}

#[test]
fn ai_off_mode() {
    let p = project();
    let mut st = AiSettings::default();
    st.mode = PrivacyMode::Off;
    let ai = Assistant::new(st);
    assert!(matches!(ai.handle("удали паузы", &p, &EditorState::default()), Reply::Message(_)));
    assert_eq!(ai.status_label(), "AI: OFF");
}

#[test]
fn split_here_uses_playhead() {
    let p = project();
    let ai = Assistant::new(AiSettings::default());
    let st = EditorState { playhead: s(7), ..Default::default() };
    match ai.handle("Разрежь здесь", &p, &st) {
        Reply::Execute { command: EditCommand::Split { at, clips: None }, .. } => assert_eq!(at, s(7)),
        r => panic!("{r:?}"),
    }
}

#[test]
fn llm_commands_are_strictly_validated() {
    let p = project();
    let perms = Permissions::default();
    // Unknown command type → schema error.
    assert!(matches!(command::parse_commands(r#"[{"type":"run_shell","cmd":"rm -rf /"}]"#), Err(ValidationError::Schema { .. })));
    // Unknown field → schema error (deny_unknown_fields).
    assert!(command::parse_commands(r#"[{"type":"add_marker","at_ms":1,"name":"x","path":"C:/"}]"#).is_err());
    // Out-of-range time.
    let c = command::parse_commands(r#"[{"type":"delete_range","start_ms":59000,"end_ms":99000,"ripple":true}]"#).unwrap();
    assert!(matches!(command::validate(&c, &p, &perms), Err(ValidationError::Range { .. })));
    // Unknown clip id.
    let c = command::parse_commands(r#"[{"type":"delete_clip","clip_id":"00000000-0000-0000-0000-000000000000"}]"#).unwrap();
    assert!(matches!(command::validate(&c, &p, &perms), Err(ValidationError::UnknownClip { .. })));
    // Destructive edits can be forbidden.
    let c = command::parse_commands(r#"{"commands":[{"type":"delete_range","start_ms":0,"end_ms":1000}]}"#).unwrap();
    let ro = Permissions { allow_destructive: false, ..Default::default() };
    assert!(matches!(command::validate(&c, &p, &ro), Err(ValidationError::Permission { .. })));
    // Reserved commands are rejected until implemented.
    let c = command::parse_commands(r#"[{"type":"select_camera","start_ms":0,"end_ms":10,"angle":"CAM_B"}]"#).unwrap();
    assert!(matches!(command::validate(&c, &p, &perms), Err(ValidationError::Unsupported { .. })));
}
