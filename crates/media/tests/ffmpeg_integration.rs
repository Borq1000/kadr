//! Real FFmpeg round-trips. Skipped (with a message) if FFmpeg is missing.

use kadr_core::{CancelToken, FrameRate, MediaKind, Time};
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
        .open_stream(&StreamRequest { path: clip, start: Time::from_secs(4), width: 320, height: 180, rate: FrameRate::FPS_25, speed: 1.0 })
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
