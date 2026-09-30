//! Real FFmpeg round-trips. Skipped (with a message) if FFmpeg is missing.

use kadr_core::{CancelToken, FrameRate, MediaKind, Time};
use kadr_media::export::{ExportVideoSource, VideoLook};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn backend() -> Option<FfmpegCli> {
    match FfmpegCli::locate() {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

/// 6 s of 25 fps test pattern with a 440 Hz tone that is silent 2–4 s.
fn make_clip(dir: &Path) -> PathBuf {
    let out = dir.join("clip.mp4");
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6:sample_rate=48000"])
        .args(["-af", "volume='if(between(t,2,4),0,1)':eval=frame"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success());
    out
}

#[test]
fn probe_decode_thumbnails() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());

    let info = ff.probe(&clip).unwrap();
    assert_eq!(info.kind, MediaKind::Video);
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (640, 360));
    assert_eq!(v.frame_rate, Some(FrameRate::FPS_25));
    assert!((info.duration.as_secs_f64() - 6.0).abs() < 0.1, "{:?}", info.duration);
    assert_eq!(info.audio.as_ref().unwrap().sample_rate, 48_000);

    let f = ff.decode_frame(&clip, Time::from_secs(3), 320, 320).unwrap();
    assert_eq!((f.width, f.height), (320, 180), "aspect preserved");
    assert_eq!(f.data.len(), 320 * 180 * 4);

    let thumbs = ff.thumbnails(&clip, &[Time::ZERO, Time::from_secs(5)], 72, &CancelToken::new()).unwrap();
    assert_eq!(thumbs.len(), 2);
    assert_eq!(thumbs[0].height, 72);
}

#[test]
fn stream_yields_frames_at_requested_rate() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());
    let mut s = ff
        .open_stream(&StreamRequest { path: clip, start: Time::from_secs(4), width: 320, height: 180, rate: FrameRate::FPS_25, speed: 1.0, look: VideoLook::default(), px_scale: 0.5 })
        .unwrap();
    let mut n = 0;
    while let Some(f) = s.next_frame().unwrap() {
        assert_eq!(f.data.len(), 320 * 180 * 4);
        n += 1;
    }
    assert!((48..=52).contains(&n), "2 s at 25 fps ≈ 50 frames, got {n}");
}

#[test]
fn pcm_extraction_is_exact_length() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());
    let out = dir.path().join("a.pcm");
    let last = std::sync::Mutex::new(0.0f32);
    ff.extract_pcm(&clip, &out, 48_000, 2, Time::from_secs(6), &|p| *last.lock().unwrap() = p, &CancelToken::new()).unwrap();
    let bytes = std::fs::metadata(&out).unwrap().len();
    let secs = bytes as f64 / (48_000.0 * 2.0 * 2.0);
    assert!((secs - 6.0).abs() < 0.05, "{secs}");
    assert_eq!(*last.lock().unwrap(), 1.0);
}

#[test]
fn export_cut_with_gap_has_expected_duration() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());
    let output = dir.path().join("out.mp4");
    let src = |start: i64, look: VideoLook| Some(ExportVideoSource { path: clip.clone(), source_start: Time::from_millis(start), speed: 1.0, look });
    let plan = ExportPlan {
        output: output.clone(),
        total: Time::from_millis(4_000),
        video: vec![
            ExportVideo { duration: Time::from_millis(1_000), source: src(0, VideoLook::default()), transition_in: None },
            ExportVideo { duration: Time::from_millis(1_000), source: None, transition_in: None },
            ExportVideo {
                duration: Time::from_millis(2_000),
                source: src(4_000, VideoLook { scale: 0.5, x: 100.0, opacity: 0.8, contrast: 1.2, ..Default::default() }),
                transition_in: None,
            },
        ],
        audio: vec![ExportAudio {
            path: clip.clone(),
            source_start: Time::ZERO,
            timeline_start: Time::from_millis(500),
            duration: Time::from_millis(3_000),
            speed: 1.0,
            gain_db: -6.0,
            pan: 0.3,
            fade_in: Time::from_millis(200),
            fade_out: Time::from_millis(200),
        }],
        settings: ExportSettings { width: 640, height: 360, rate: FrameRate::FPS_25, preset: "ultrafast".into(), ..Default::default() },
    };
    ff.export(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let info = ff.probe(&output).unwrap();
    let v = info.video.unwrap();
    assert_eq!((v.width, v.height), (640, 360));
    assert!((info.duration.as_secs_f64() - 4.0).abs() < 0.1, "{:?}", info.duration);

    // Exactly 100 frames at 25 fps.
    let frames = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&frames.stdout).trim(), "100");
}

#[test]
fn cancelled_export_leaves_no_output() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());
    let output = dir.path().join("out.mp4");
    let cancel = CancelToken::new();
    cancel.cancel();
    let plan = ExportPlan {
        output: output.clone(),
        total: Time::from_secs(6),
        video: vec![ExportVideo {
            duration: Time::from_secs(6),
            source: Some(ExportVideoSource { path: clip, source_start: Time::ZERO, speed: 1.0, look: VideoLook::default() }),
            transition_in: None,
        }],
        audio: vec![],
        settings: ExportSettings { width: 640, height: 360, rate: FrameRate::FPS_25, ..Default::default() },
    };
    assert!(matches!(ff.export(&plan, &|_| {}, &cancel), Err(MediaError::Cancelled)));
    assert!(!output.exists());
}

fn mean_luma(f: &RgbaFrame) -> f64 {
    let px = f.data.chunks_exact(4);
    let n = px.len() as f64;
    px.map(|p| 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64).sum::<f64>() / n
}

#[test]
fn export_dissolve_blends_across_the_cut_and_keeps_duration() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());
    let output = dir.path().join("out.mp4");
    let dissolve = |ms| Some(ExportTransition { kind: ExportTransitionKind::Dissolve, duration: Time::from_millis(ms) });
    let plan = ExportPlan {
        output: output.clone(),
        total: Time::from_millis(3_000),
        video: vec![
            ExportVideo { duration: Time::from_millis(1_000), source: None, transition_in: None },
            // Source starts at 0: the head handle before the cut must be synthesised.
            ExportVideo {
                duration: Time::from_millis(1_000),
                source: Some(ExportVideoSource { path: clip.clone(), source_start: Time::ZERO, speed: 1.0, look: VideoLook::default() }),
                transition_in: dissolve(800),
            },
            ExportVideo { duration: Time::from_millis(1_000), source: None, transition_in: dissolve(400) },
        ],
        audio: vec![],
        settings: ExportSettings { width: 320, height: 180, rate: FrameRate::FPS_25, preset: "ultrafast".into(), ..Default::default() },
    };
    ff.export(&plan, &|_| {}, &CancelToken::new()).unwrap();

    let frames = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&frames.stdout).trim(), "75", "a dissolve must not change the length");

    let luma = |ms| mean_luma(&ff.decode_frame(&output, Time::from_millis(ms), 320, 180).unwrap());
    let (black, early, cut, full, late) = (luma(300), luma(700), luma(1_000), luma(1_500), luma(2_500));
    assert!(black < 20.0, "pure black before the dissolve: {black}");
    assert!(full > 60.0, "picture fully visible after the dissolve: {full}");
    assert!(early > black + 5.0 && early < cut && cut < full - 5.0, "ramps up across the cut: {early} {cut} {full}");
    assert!(late < 20.0, "second dissolve finished into black: {late}");
}

/// Exports cut, gap, dissolve-in, cut (segment lengths in ms) at `rate`;
/// returns the frame count ffprobe reads back.
fn export_cuts_then_dissolve(ff: &FfmpegCli, rate: FrameRate, segments_ms: [i64; 4]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(dir.path());
    let output = dir.path().join("out.mp4");
    let source = |start_ms| Some(ExportVideoSource { path: clip.clone(), source_start: Time::from_millis(start_ms), speed: 1.0, look: VideoLook::default() });
    let dissolve = Some(ExportTransition { kind: ExportTransitionKind::Dissolve, duration: Time::from_millis(800) });
    let [a, gap, b, c] = segments_ms.map(Time::from_millis);
    let plan = ExportPlan {
        output: output.clone(),
        total: a + gap + b + c,
        video: vec![
            ExportVideo { duration: a, source: source(0), transition_in: None },
            ExportVideo { duration: gap, source: None, transition_in: None },
            ExportVideo { duration: b, source: source(2_000), transition_in: dissolve },
            ExportVideo { duration: c, source: source(3_000), transition_in: None },
        ],
        audio: vec![],
        settings: ExportSettings { width: 320, height: 180, rate, preset: "ultrafast".into(), ..Default::default() },
    };
    ff.export(&plan, &|_| {}, &CancelToken::new()).unwrap();

    let frames = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(&output)
        .output()
        .unwrap();
    String::from_utf8_lossy(&frames.stdout).trim().to_string()
}

/// Plain cuts are concatenated before the transition (concat outputs a 1/1000000 time
/// base, xfade needs both inputs on the same one) and another run follows it.
#[test]
fn export_with_cuts_before_a_dissolve_succeeds_and_keeps_frame_count() {
    let Some(ff) = backend() else { return };
    // Transitions use handles around the cut, so the length is the sum of the segments: 4 x 25 frames.
    assert_eq!(export_cuts_then_dissolve(&ff, FrameRate::FPS_25, [1_000; 4]), "100", "a dissolve must not change the length");
}

/// The same at 29.97 fps, where segment lengths are not whole frames: the
/// output has exactly the frames `build_graph` counts, each segment rounded
/// to the nearest frame.
#[test]
fn export_with_cuts_before_a_dissolve_keeps_frame_count_at_29_97() {
    let Some(ff) = backend() else { return };
    let rate = FrameRate::FPS_29_97;
    let segments_ms = [1_000, 700, 1_300, 1_000];
    let expected: i64 = segments_ms.iter().map(|&ms| rate.time_to_frame_round(Time::from_millis(ms)).max(1)).sum();
    assert_eq!(expected, 30 + 21 + 39 + 30);
    assert_eq!(export_cuts_then_dissolve(&ff, rate, segments_ms), expected.to_string(), "a dissolve must not change the length");
}

/// Same loop as the editor's analysis job: 4 fps grey proxy → stats.
fn video_overview(ff: &FfmpegCli, path: &Path) -> kadr_analysis::video::VideoOverview {
    use kadr_analysis::video::*;
    let mut s = ff
        .open_stream(&StreamRequest {
            path: path.to_path_buf(),
            start: Time::ZERO,
            width: ANALYSIS_W,
            height: ANALYSIS_H,
            rate: FrameRate::new(ANALYSIS_FPS, 1),
            speed: 1.0,
            look: VideoLook::default(),
            px_scale: 1.0,
        })
        .unwrap();
    let mut frames = vec![];
    let mut prev: Option<Vec<u8>> = None;
    while let Some(f) = s.next_frame().unwrap() {
        let g = rgba_to_gray(&f.data);
        frames.push(analyze_gray_frame(&g, f.width, f.height, prev.as_deref()));
        prev = Some(g);
    }
    VideoOverview { fps: ANALYSIS_FPS, frames }
}

fn lavfi_clip(dir: &Path, name: &str, filter: &str) -> PathBuf {
    let out = dir.join(name);
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", filter, "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success());
    out
}

#[test]
fn video_analysis_buckets_real_footage() {
    use kadr_analysis::video::*;
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let sharp = make_clip(dir.path());
    let blurry = lavfi_clip(dir.path(), "blur.mp4", "testsrc2=size=640x360:rate=25:duration=3,boxblur=12:4");
    let black = lavfi_clip(dir.path(), "black.mp4", "color=c=black:s=640x360:r=25:d=3");

    let ov = video_overview(&ff, &sharp);
    assert!((22..=26).contains(&ov.frames.len()), "6 s at 4 fps, got {}", ov.frames.len());
    let shots = detect_shots(&ov, Time::from_secs(1));
    eprintln!("sharp: {:?}", shots.iter().map(|s| (s.sharpness, s.luma, s.motion)).collect::<Vec<_>>());
    assert!(shots.iter().all(|s| bucket_sharpness(s.sharpness) == "sharp" && !s.black));
    assert_eq!(bucket_exposure(shots[0].luma), "normal");

    let b = detect_shots(&video_overview(&ff, &blurry), Time::from_secs(1));
    eprintln!("blurry: {:?}", b.iter().map(|s| (s.sharpness, s.luma, s.motion)).collect::<Vec<_>>());
    assert_ne!(bucket_sharpness(b[0].sharpness), "sharp");

    let k = detect_shots(&video_overview(&ff, &black), Time::from_secs(1));
    assert!(k.len() == 1 && k[0].black && bucket_exposure(k[0].luma) == "black");
}
