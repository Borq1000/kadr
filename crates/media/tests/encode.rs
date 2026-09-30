//! Frame encoder (`start_encode`) against real FFmpeg: frame count, size,
//! colour tags, audio, the BT.709 colour round trip, abort and errors (M5
//! plan, task 1). Skipped (with a message) if FFmpeg is missing.

use kadr_core::color::{AlphaMode, ColorInfo, Matrix, Primaries, Range, Transfer};
use kadr_core::{FrameRate, Time};
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

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-nostdin", "-v", "error", "-y"]).args(args).output().unwrap();
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `key=value` lines of an ffprobe run.
fn ffprobe(path: &Path, args: &[&str]) -> Vec<(String, String)> {
    let out = Command::new("ffprobe")
        .args(["-hide_banner", "-v", "error"])
        .args(args)
        .args(["-of", "default=nw=1"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "ffprobe {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

fn get<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str())
}

fn job(output: &Path, width: u32, height: u32, rate: FrameRate, frames: i64, audio: Vec<ExportAudio>) -> EncodeJob {
    let total = rate.frame_to_time(frames);
    EncodeJob {
        output: output.to_path_buf(),
        width,
        height,
        rate,
        frames,
        total,
        audio,
        settings: ExportSettings { width, height, rate, preset: "ultrafast".into(), ..ExportSettings::default() },
    }
}

fn tone(dir: &Path, secs: u32) -> PathBuf {
    let out = dir.join("tone.wav");
    ffmpeg(&["-f", "lavfi", "-i", &format!("sine=frequency=440:duration={secs}:sample_rate=48000"), out.to_str().unwrap()]);
    out
}

fn part_of(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

// ---- 1. gradient + tone -------------------------------------------------------

fn gradient(buf: &mut [u8], w: u32, h: u32, n: u32) {
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            buf[i] = ((x * 255 / w + n * 5) % 256) as u8;
            buf[i + 1] = (y * 255 / h) as u8;
            buf[i + 2] = (n * 255 / 50) as u8;
            buf[i + 3] = 255;
        }
    }
}

#[test]
fn encodes_frames_and_audio_with_the_right_shape_and_tags() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let wav = tone(dir.path(), 2);
    let out = dir.path().join("out.mp4");
    let (w, h) = (320, 180);
    let audio = vec![ExportAudio {
        path: wav,
        source_start: Time::ZERO,
        timeline_start: Time::ZERO,
        duration: Time::from_secs(2),
        speed: 1.0,
        gain_db: 0.0,
        pan: 0.0,
        fade_in: Time::ZERO,
        fade_out: Time::ZERO,
    }];
    let mut enc = ff.start_encode(&job(&out, w, h, FrameRate::FPS_25, 50, audio)).unwrap();
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for n in 0..50 {
        gradient(&mut buf, w, h, n);
        enc.write_frame(&buf).unwrap();
        assert_eq!(enc.frames_written(), n as i64 + 1);
    }
    assert!(!out.exists(), "the result appears only on finish");
    assert!(part_of(&out).exists());
    enc.finish().unwrap();
    assert!(out.is_file());
    assert!(!part_of(&out).exists());

    let v = ffprobe(
        &out,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,nb_read_frames,pix_fmt,color_space,color_primaries,color_transfer,color_range",
        ],
    );
    assert_eq!((get(&v, "width"), get(&v, "height")), ("320", "180"));
    assert_eq!(get(&v, "nb_read_frames"), "50");
    assert_eq!(get(&v, "pix_fmt"), "yuv420p");
    assert_eq!(get(&v, "color_space"), "bt709");
    assert_eq!(get(&v, "color_primaries"), "bt709");
    assert_eq!(get(&v, "color_transfer"), "bt709");
    assert_eq!(get(&v, "color_range"), "tv");

    let a = ffprobe(&out, &["-select_streams", "a", "-show_entries", "stream=codec_name,sample_rate"]);
    assert_eq!(get(&a, "codec_name"), "aac");
    assert_eq!(get(&a, "sample_rate"), "48000");

    let f = ffprobe(&out, &["-show_entries", "format=duration"]);
    let duration: f64 = get(&f, "duration").parse().unwrap();
    assert!((duration - 2.0).abs() < 0.1, "duration {duration}");
}

// ---- 2. colour round trip -----------------------------------------------------

const BARS: [[u8; 3]; 9] = [
    [191, 191, 191], // 75 % white
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
    [255, 255, 255],
    [0, 0, 0],
];
const BAR_W: u32 = 64;

#[test]
fn colour_round_trips_through_bt709_limited_yuv() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("bars.mp4");
    let (w, h) = (BAR_W * 10, 96); // nine bars and a mid grey
    let mut frame = vec![255u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let rgb = BARS.get((x / BAR_W) as usize).copied().unwrap_or([128, 128, 128]);
            let i = ((y * w + x) * 4) as usize;
            frame[i..i + 3].copy_from_slice(&rgb);
        }
    }
    let mut j = job(&out, w, h, FrameRate::FPS_25, 8, vec![]);
    j.settings.crf = 6;
    let mut enc = ff.start_encode(&j).unwrap();
    for _ in 0..8 {
        enc.write_frame(&frame).unwrap();
    }
    enc.finish().unwrap();

    let color = ColorInfo { primaries: Primaries::Bt709, transfer: Transfer::Bt709, matrix: Matrix::Bt709, range: Range::Limited, alpha: AlphaMode::Opaque };
    let req = SourceRequest { path: out, rate: FrameRate::FPS_25, start_frame: 0, width: w, height: h, color, hwaccel: false };
    let mut src = ff.open_source(&req).unwrap();
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut frames = 0;
    while src.read_into(&mut buf).unwrap() {
        frames += 1;
        if frames != 5 {
            continue;
        }
        for bar in 0..10u32 {
            let want = BARS.get(bar as usize).copied().unwrap_or([128, 128, 128]);
            let i = (((h / 2) * w + bar * BAR_W + BAR_W / 2) * 4) as usize;
            let got = &buf[i..i + 3];
            for c in 0..3 {
                assert!((got[c] as i32 - want[c] as i32).abs() <= 3, "bar {bar}: got {got:?}, want {want:?}");
            }
        }
    }
    assert_eq!(frames, 8);
}

// ---- 3. abort -----------------------------------------------------------------

#[test]
fn abort_and_drop_leave_no_files_behind() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (64, 64);
    let frame = vec![90u8; (w * h * 4) as usize];

    let out = dir.path().join("aborted.mp4");
    let mut enc = ff.start_encode(&job(&out, w, h, FrameRate::FPS_25, 100, vec![])).unwrap();
    for _ in 0..5 {
        enc.write_frame(&frame).unwrap();
    }
    assert_eq!(enc.frames_written(), 5);
    enc.abort();
    assert!(!out.exists());
    assert!(!part_of(&out).exists());

    let out = dir.path().join("dropped.mp4");
    let mut enc = ff.start_encode(&job(&out, w, h, FrameRate::FPS_25, 100, vec![])).unwrap();
    enc.write_frame(&frame).unwrap();
    drop(enc);
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing left in the output directory");
}

// ---- 4. errors ----------------------------------------------------------------

#[test]
fn wrong_buffer_size_is_an_error_and_the_encoder_stays_usable() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mp4");
    let (w, h) = (64, 48);
    let mut enc = ff.start_encode(&job(&out, w, h, FrameRate::FPS_25, 2, vec![])).unwrap();
    let frame = vec![10u8; (w * h * 4) as usize];
    assert!(enc.write_frame(&frame[..frame.len() - 1]).is_err());
    assert!(enc.write_frame(&[]).is_err());
    let mut big = frame.clone();
    big.push(0);
    assert!(enc.write_frame(&big).is_err());
    assert_eq!(enc.frames_written(), 0);
    enc.write_frame(&frame).unwrap();
    enc.write_frame(&frame).unwrap();
    enc.finish().unwrap();
    assert!(out.is_file());
}

#[test]
fn invalid_jobs_are_rejected_before_anything_starts() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mp4");
    for (w, h) in [(0, 64), (64, 0), (63, 64), (64, 63)] {
        assert!(ff.start_encode(&job(&out, w, h, FrameRate::FPS_25, 2, vec![])).is_err(), "{w}x{h}");
    }
    let mut empty = job(&out, 64, 64, FrameRate::FPS_25, 2, vec![]);
    empty.total = Time::ZERO;
    assert!(ff.start_encode(&empty).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_dying_encoder_reports_ffmpegs_message_and_leaves_no_output() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mp4");
    let (w, h) = (64, 64);
    let mut j = job(&out, w, h, FrameRate::FPS_25, 500, vec![]);
    j.settings.video_codec = "no_such_codec".into();
    let mut enc = ff.start_encode(&j).unwrap();
    let frame = vec![0u8; (w * h * 4) as usize];
    let mut err = None;
    for _ in 0..500 {
        if let Err(e) = enc.write_frame(&frame) {
            err = Some(e);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let err = err.expect("the encoder is gone, writes must fail").to_string();
    assert!(err.contains("no_such_codec"), "{err}");
    // A failed encode stays failed, and finishing it reports the same.
    assert!(enc.write_frame(&frame).is_err());
    assert!(enc.finish().is_err());
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
}
