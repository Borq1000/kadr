//! ExportRunner (M5 plan, task 2): fake decoders and a recording encoder
//! (frame count, order, content, transitions, cancel, missing media, encoder
//! errors, steady-state allocations), plus one run on real FFmpeg (skipped
//! without it).

use kadr_core::{AssetId, CancelToken, FrameRate, Time};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::{EncodeJob, ExportAudio, ExportSettings, FrameEncoder, MediaBackend, MediaError, SourceRequest};
use kadr_playback::testing::{media_layer, read_fake_pixel, video_source, FakeDecoders, FakeMedia, FnSceneSource};
use kadr_playback::{export, EncoderFactory, ExportError, ExportRequest, FfmpegDecoders, MediaSource, MissingPolicy, ResolverConfig, SceneSource};
use kadr_render::MissingReason;
use kadr_scene::{BlendMode, FrameScene, Layer, LayerContent, LayerId, OutputSpec, Placement, SizeU, SourceKind, TransitionLayer, TransitionOp};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const CANVAS: SizeU = SizeU::new(64, 36);
const RATE: FrameRate = FrameRate::FPS_25;
/// 4 s at 25 fps.
const FRAMES: i64 = 100;
const TAG_A: u8 = 10;
const TAG_B: u8 = 200;

// ---- recording encoder ---------------------------------------------------------

#[derive(Default)]
struct Record {
    starts: u32,
    job_frames: i64,
    /// Per frame written: its first pixel and a hash of all its bytes.
    frames: Vec<([u8; 4], u64)>,
    finished: bool,
    aborted: bool,
}

#[derive(Clone, Default)]
struct Recorder {
    rec: Arc<Mutex<Record>>,
    /// `write_frame` of this frame index fails.
    fail_at: Option<i64>,
    write_delay: Duration,
}

struct RecordingEncoder {
    rec: Arc<Mutex<Record>>,
    fail_at: Option<i64>,
    delay: Duration,
    len: usize,
    written: i64,
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

impl FrameEncoder for RecordingEncoder {
    fn write_frame(&mut self, rgba: &[u8]) -> Result<(), MediaError> {
        std::thread::sleep(self.delay);
        assert_eq!(rgba.len(), self.len, "frame buffer size");
        if self.fail_at == Some(self.written) {
            return Err(MediaError::ToolFailed { tool: "ffmpeg".into(), status: "exit code 1".into(), stderr: "No space left on device".into() });
        }
        self.rec.lock().frames.push(([rgba[0], rgba[1], rgba[2], rgba[3]], hash(rgba)));
        self.written += 1;
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<(), MediaError> {
        self.rec.lock().finished = true;
        Ok(())
    }
    fn abort(self: Box<Self>) {
        self.rec.lock().aborted = true;
    }
    fn frames_written(&self) -> i64 {
        self.written
    }
}

impl EncoderFactory for Recorder {
    fn start(&self, job: &EncodeJob) -> Result<Box<dyn FrameEncoder>, MediaError> {
        let mut r = self.rec.lock();
        r.starts += 1;
        r.job_frames = job.frames;
        Ok(Box::new(RecordingEncoder {
            rec: self.rec.clone(),
            fail_at: self.fail_at,
            delay: self.write_delay,
            len: job.width as usize * job.height as usize * 4,
            written: 0,
        }))
    }
}

// ---- the timeline --------------------------------------------------------------

/// Two clips, a cut and a dissolve, 4 s at 25 fps:
/// - frames 0..25: A at source frame n + 50;
/// - cut at 1 s → frames 25..50: B at source frame n − 25;
/// - frames 40..50: dissolve from B to A (A at source frame n + 210), progress (n − 40) / 10;
/// - frames 50..100: A at source frame n + 210.
struct Timeline {
    a: AssetId,
    b: AssetId,
    ma: MediaSource,
    mb: MediaSource,
}

fn full_canvas(id: u128, content: LayerContent) -> Layer {
    Layer { id: LayerId(id), content, placement: Placement::fill(CANVAS), crop: Placement::fill(CANVAS).full_crop(), opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
}

impl Timeline {
    fn new(mb: MediaSource) -> Self {
        Timeline { a: AssetId::new(), b: AssetId::new(), ma: video_source("a.mp4", RATE, Time::from_secs(60), CANVAS), mb }
    }

    fn scene(&self, t: Time, out: &OutputSpec) -> FrameScene {
        let (ms1, ms1_6, ms2) = (Time::from_secs(1), Time::from_millis(1600), Time::from_secs(2));
        let a1 = || media_layer(1, self.a, &self.ma, t + Time::from_secs(2), CANVAS);
        let b = || media_layer(2, self.b, &self.mb, t - ms1, CANVAS);
        let a2 = || media_layer(3, self.a, &self.ma, t - ms1_6 + Time::from_secs(10), CANVAS);
        let layer = if t < ms1 {
            a1()
        } else if t < ms1_6 {
            b()
        } else if t < ms2 {
            let progress = ((t - ms1_6).as_secs_f64() / 0.4) as f32;
            full_canvas(9, LayerContent::Transition(Box::new(TransitionLayer { op: TransitionOp::Dissolve, progress, from: vec![b()], to: vec![a2()] })))
        } else {
            a2()
        };
        FrameScene { layers: vec![layer], ..FrameScene::empty(t, CANVAS, *out) }
    }

    fn source(self) -> Arc<dyn SceneSource> {
        let media = [(self.a, self.ma.clone()), (self.b, self.mb.clone())].into_iter().collect();
        Arc::new(FnSceneSource { media, rate: RATE, duration: RATE.frame_to_time(FRAMES), canvas: CANVAS, scene: move |t, out: &OutputSpec| self.scene(t, out) })
    }
}

fn fake() -> Arc<FakeDecoders> {
    let f = FakeDecoders::new(Duration::ZERO, Duration::ZERO);
    f.add("a.mp4", FakeMedia { tag: TAG_A, ..Default::default() });
    f.add("b.mp4", FakeMedia { tag: TAG_B, ..Default::default() });
    Arc::new(f)
}

fn job(frames: i64) -> EncodeJob {
    EncodeJob {
        output: PathBuf::from("unused.mp4"),
        width: CANVAS.w,
        height: CANVAS.h,
        rate: RATE,
        frames,
        total: RATE.frame_to_time(frames),
        audio: vec![],
        settings: ExportSettings::default(),
    }
}

fn b_media() -> MediaSource {
    video_source("b.mp4", RATE, Time::from_secs(60), CANVAS)
}

fn no_progress() -> impl Fn(f32) + Sync {
    |_| {}
}

// ---- tests ---------------------------------------------------------------------

/// The frames of [`Timeline`], each the right source frame, the dissolve mixed.
fn assert_timeline_frames(frames: &[([u8; 4], u64)]) {
    assert_eq!(frames.len() as i64, FRAMES, "exactly job.frames frames");
    for (n, (px, _)) in frames.iter().enumerate() {
        let n = n as i64;
        assert_eq!(px[3], 255, "frame {n} opaque");
        match n {
            0..25 => assert_eq!(read_fake_pixel(px), (TAG_A, n + 50), "frame {n}: A"),
            25..=40 => assert_eq!(read_fake_pixel(px), (TAG_B, n - 25), "frame {n}: B (the dissolve starts at progress 0)"),
            41..50 => {
                let p = (n - 40) as f32 / 10.0;
                let want = TAG_B as f32 * (1.0 - p) + TAG_A as f32 * p;
                assert!((px[0] as f32 - want).abs() <= 1.5, "frame {n}: dissolve at {p}: red {} want {want}", px[0]);
            }
            _ => assert_eq!(read_fake_pixel(px), (TAG_A, n + 210), "frame {n}: A again"),
        }
    }
    // Mixed frames really differ from each other and from both ends.
    let reds: Vec<u8> = frames[40..51].iter().map(|(px, _)| px[0]).collect();
    assert!(reds.windows(2).all(|w| w[0] > w[1]), "the dissolve moves monotonically from B to A: {reds:?}");
}

#[test]
fn every_frame_in_order_with_the_right_source_frame_and_mixed_transitions() {
    let rec = Recorder::default();
    let progress = Mutex::new(vec![]);
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    let stats = export(req, fake(), &rec, &|p| progress.lock().push(p), &CancelToken::new()).expect("export");

    let r = rec.rec.lock();
    assert_eq!((r.starts, r.job_frames), (1, FRAMES));
    assert!(r.finished && !r.aborted);
    assert_eq!(stats.frames, FRAMES);
    assert_timeline_frames(&r.frames);

    let p = progress.lock();
    assert!(p.windows(2).all(|w| w[0] <= w[1]), "progress is monotonic");
    assert!(p.iter().all(|v| (0.0..=1.0).contains(v)));
    assert_eq!(p.last(), Some(&1.0));
    assert!(p.len() <= 102, "throttled: {} calls", p.len());
    assert!(stats.fps > 0.0 && stats.render.count == FRAMES as usize, "{stats:?}");
    // A, B and A's second use, each read forward in one stream (prefetch opens them ahead).
    assert!(stats.resolver.streams_opened <= 5, "{:?}", stats.resolver);
}

#[test]
fn a_full_frame_opaque_layer_is_drawn_by_the_fast_path() {
    let rec = Recorder::default();
    let (asset, media, canvas) = (AssetId::new(), video_source("a.mp4", RATE, Time::from_secs(60), CANVAS), CANVAS);
    let src = Arc::new(FnSceneSource {
        media: [(asset, media.clone())].into_iter().collect(),
        rate: RATE,
        duration: RATE.frame_to_time(FRAMES),
        canvas,
        scene: move |t: Time, out: &OutputSpec| {
            FrameScene { layers: vec![media_layer(1, asset, &media, t, canvas)], ..FrameScene::empty(t, canvas, *out) }
        },
    });
    let stats = export(ExportRequest::new(src, job(FRAMES)), fake(), &rec, &|_| {}, &CancelToken::new()).expect("export");
    assert_eq!(stats.layers_drawn, FRAMES as u64, "{stats:?}");
    assert_eq!(stats.fast_paths, FRAMES as u64, "every frame is a plain row copy: {stats:?}");
}

#[test]
fn same_scene_same_output_bytes() {
    let run = || {
        let rec = Recorder::default();
        export(ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES)), fake(), &rec, &no_progress(), &CancelToken::new()).expect("export");
        let hashes: Vec<u64> = rec.rec.lock().frames.iter().map(|f| f.1).collect();
        hashes
    };
    assert_eq!(run(), run(), "export is deterministic");
}

#[test]
fn cancel_midway_aborts_the_encoder() {
    let rec = Recorder { write_delay: Duration::from_millis(2), ..Default::default() };
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let progress = move |p: f32| {
        if p >= 0.3 {
            c.cancel();
        }
    };
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    let err = export(req, fake(), &rec, &progress, &cancel).expect_err("cancelled");
    assert!(matches!(err, ExportError::Cancelled), "{err}");
    let r = rec.rec.lock();
    assert!(r.aborted && !r.finished);
    assert!(r.frames.len() >= 30 && (r.frames.len() as i64) < FRAMES, "stopped midway: {} frames", r.frames.len());
}

#[test]
fn cancel_interrupts_a_wait_for_a_slow_decoder() {
    let fake = fake();
    fake.set_delays(Duration::ZERO, Duration::from_millis(400));
    let rec = Recorder::default();
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c.cancel();
    });
    let t0 = Instant::now();
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    let err = export(req, fake, &rec, &no_progress(), &cancel).expect_err("cancelled");
    t.join().unwrap();
    assert!(matches!(err, ExportError::Cancelled), "{err}");
    assert!(rec.rec.lock().aborted);
    // One slow read may finish while the decoder session shuts down; not the whole export.
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
}

#[test]
fn offline_media_fails_the_export_naming_the_file_and_the_time() {
    let offline = MediaSource { path: "C:/footage/b-offline.mp4".into(), online: false, ..b_media() };
    let rec = Recorder::default();
    let req = ExportRequest::new(Timeline::new(offline).source(), job(FRAMES));
    let err = export(req, fake(), &rec, &no_progress(), &CancelToken::new()).expect_err("offline media fails");
    let msg = err.to_string();
    assert!(matches!(err, ExportError::MissingMedia { reason: MissingReason::Offline, frame: 25, .. }), "{err:?}");
    assert!(msg.contains("b-offline.mp4") && msg.contains("0:01.000"), "{msg}");
    let r = rec.rec.lock();
    assert!(r.aborted && !r.finished);
    assert!(r.frames.len() <= 25, "nothing past the missing frame: {}", r.frames.len());
}

#[test]
fn undecodable_media_fails_the_export() {
    let fake = fake();
    fake.add("b.mp4", FakeMedia { tag: TAG_B, fail: true, ..Default::default() });
    let rec = Recorder::default();
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    let t0 = Instant::now();
    let err = export(req, fake, &rec, &no_progress(), &CancelToken::new()).expect_err("decode failure fails");
    assert!(matches!(err, ExportError::MissingMedia { reason: MissingReason::DecodeFailed, frame: 25, .. }), "{err:?}");
    assert!(err.to_string().contains("b.mp4"), "{err}");
    assert!(rec.rec.lock().aborted);
    assert!(t0.elapsed() < Duration::from_secs(3), "a few bounded retries, then it fails: {:?}", t0.elapsed());
}

/// Transient failures (two failed opens of A, a failed read of B — the
/// latter hit by the prefetch that opens B ahead of its cut) are retried:
/// the export completes with the right frames, nothing missing.
#[test]
fn transient_decode_failures_are_retried_and_the_export_completes() {
    let fake = fake();
    fake.fail_opens("a.mp4", 2);
    fake.fail_reads("b.mp4", 1);
    let rec = Recorder::default();
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    let stats = export(req, fake.clone(), &rec, &no_progress(), &CancelToken::new()).expect("transient failures are retried");
    assert_eq!(stats.frames, FRAMES);
    assert_eq!(fake.open_calls() - fake.opens(), 2, "both failed opens happened");
    let r = rec.rec.lock();
    assert!(r.finished && !r.aborted);
    assert_timeline_frames(&r.frames);
}

/// With placeholders, a media that never decodes costs its retries once;
/// later frames of it are tried once each (not 750 ms of backoff per frame).
#[test]
fn undecodable_media_with_placeholders_gives_up_once() {
    let fake = fake();
    fake.add("b.mp4", FakeMedia { tag: TAG_B, fail: true, ..Default::default() });
    let rec = Recorder::default();
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES)).with_missing(MissingPolicy::DrawPlaceholder);
    let t0 = Instant::now();
    let stats = export(req, fake, &rec, &no_progress(), &CancelToken::new()).expect("placeholders allowed");
    assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
    assert_eq!(stats.frames, FRAMES);
    let r = rec.rec.lock();
    for n in 25..=40 {
        let px = r.frames[n].0;
        assert!(px[0].abs_diff(115) <= 1 && px[1].abs_diff(13) <= 1 && px[2].abs_diff(20) <= 1, "frame {n}: {px:?}");
    }
    assert_eq!(read_fake_pixel(&r.frames[10].0), (TAG_A, 60));
    assert_eq!(read_fake_pixel(&r.frames[60].0), (TAG_A, 270));
}

#[test]
fn offline_media_with_placeholders_completes_with_missing_red() {
    let offline = MediaSource { path: "b-offline.mp4".into(), online: false, ..b_media() };
    let rec = Recorder::default();
    let req = ExportRequest::new(Timeline::new(offline).source(), job(FRAMES)).with_missing(MissingPolicy::DrawPlaceholder);
    let stats = export(req, fake(), &rec, &no_progress(), &CancelToken::new()).expect("placeholders allowed");
    assert_eq!(stats.frames, FRAMES);
    let r = rec.rec.lock();
    assert!(r.finished && !r.aborted);
    assert_eq!(r.frames.len() as i64, FRAMES);
    // Rgba::MISSING = (0.45, 0.05, 0.08).
    let px = r.frames[30].0;
    assert!(px[0].abs_diff(115) <= 1 && px[1].abs_diff(13) <= 1 && px[2].abs_diff(20) <= 1, "{px:?}");
    assert_eq!(read_fake_pixel(&r.frames[60].0), (TAG_A, 270));
}

#[test]
fn encoder_errors_propagate_and_abort() {
    let rec = Recorder { fail_at: Some(30), ..Default::default() };
    let req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    let err = export(req, fake(), &rec, &no_progress(), &CancelToken::new()).expect_err("encoder failure");
    assert!(matches!(err, ExportError::Encode(MediaError::ToolFailed { .. })), "{err:?}");
    assert!(err.to_string().contains("No space left on device"), "{err}");
    let r = rec.rec.lock();
    assert!(r.aborted && !r.finished);
    assert_eq!(r.frames.len(), 30);
}

#[test]
fn a_bad_job_is_refused_before_the_encoder_starts() {
    let rec = Recorder::default();
    let req = ExportRequest::new(Timeline::new(b_media()).source(), EncodeJob { width: 0, ..job(FRAMES) });
    let err = export(req, fake(), &rec, &no_progress(), &CancelToken::new()).expect_err("empty size");
    assert!(matches!(err, ExportError::InvalidJob(_)), "{err:?}");
    assert_eq!(rec.rec.lock().starts, 0);
}

#[test]
fn no_frame_sized_allocations_once_warm() {
    let frame = CANVAS.w as usize * CANVAS.h as usize * 4;
    // A cache of 16 frames fills during the first second; from then on evicted buffers are reused.
    let mut req = ExportRequest::new(Timeline::new(b_media()).source(), job(FRAMES));
    req.resolver = ResolverConfig { cache_bytes: 16 * frame, ..ExportRequest::resolver_config() };
    let rec = Recorder::default();
    let stats = export(req, fake(), &rec, &no_progress(), &CancelToken::new()).expect("export");
    assert_eq!(stats.frames, FRAMES);
    assert!(stats.allocations > 0, "{stats:?}");
    assert_eq!(stats.late_allocations, 0, "second half (one clip, cache full) allocates nothing: {stats:?}");
    assert!(stats.resolver.frames_decoded as i64 >= FRAMES - 10, "{:?}", stats.resolver);
}

// ---- real FFmpeg ---------------------------------------------------------------

const SIZE: SizeU = SizeU::new(320, 180);

fn tool(name: &str) -> PathBuf {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    match std::env::var_os("KADR_FFMPEG_DIR") {
        Some(d) => PathBuf::from(d).join(exe),
        None => PathBuf::from(exe),
    }
}

/// testsrc2 320×180, 25 fps, 2 s, with a 440 Hz tone.
fn generate(dir: &Path) -> Option<PathBuf> {
    let out = dir.join("clip.mp4");
    let ok = Command::new(tool("ffmpeg"))
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=2"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2:sample_rate=48000"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(out)
}

/// `key=value` lines of an ffprobe run.
fn ffprobe(path: &Path, args: &[&str]) -> Vec<(String, String)> {
    let out = Command::new(tool("ffprobe")).args(["-hide_banner", "-v", "error"]).args(args).args(["-of", "default=nw=1"]).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect()
}

fn get<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str())
}

fn media_of(backend: &FfmpegCli, path: &Path) -> MediaSource {
    let info = backend.probe(path).expect("probe");
    let v = info.video.as_ref().expect("video stream");
    let (w, h) = v.display_size();
    MediaSource {
        path: path.to_path_buf(),
        kind: SourceKind::Video,
        rate: v.frame_rate.expect("frame rate"),
        duration: info.duration,
        display_size: SizeU::new(w, h),
        color: info.source_color(),
        online: true,
    }
}

fn decode_frame(backend: &FfmpegCli, media: &MediaSource, frame: i64) -> Vec<u8> {
    let req = SourceRequest { path: media.path.clone(), rate: media.rate, start_frame: frame, width: SIZE.w, height: SIZE.h, color: media.color, hwaccel: false };
    let mut s = backend.open_source(&req).expect("open_source");
    let mut buf = vec![0u8; SIZE.w as usize * SIZE.h as usize * 4];
    assert!(s.read_into(&mut buf).expect("read"), "frame {frame} of {}", media.path.display());
    buf
}

#[test]
fn exports_a_real_clip_through_ffmpeg() {
    let Ok(backend) = FfmpegCli::locate() else {
        eprintln!("SKIP: FFmpeg not found");
        return;
    };
    let backend = Arc::new(backend);
    let dir = tempfile::tempdir().unwrap();
    let Some(clip) = generate(dir.path()) else {
        eprintln!("SKIP: cannot generate a test clip with FFmpeg (libx264 + aac)");
        return;
    };
    let media = media_of(&backend, &clip);
    assert_eq!(media.rate, RATE);
    let a = AssetId::new();
    let m = media.clone();
    let total = Time::from_secs(2);
    let source = Arc::new(FnSceneSource {
        media: [(a, media.clone())].into_iter().collect(),
        rate: RATE,
        duration: total,
        canvas: SIZE,
        scene: move |t: Time, out: &OutputSpec| FrameScene { layers: vec![media_layer(1, a, &m, t, SIZE)], ..FrameScene::empty(t, SIZE, *out) },
    });
    let output = dir.path().join("out.mp4");
    let frames = RATE.time_to_frame_round(total);
    let audio = ExportAudio {
        path: clip.clone(),
        source_start: Time::ZERO,
        timeline_start: Time::ZERO,
        duration: total,
        speed: 1.0,
        gain_db: 0.0,
        pan: 0.0,
        fade_in: Time::ZERO,
        fade_out: Time::ZERO,
    };
    let job = EncodeJob {
        output: output.clone(),
        width: SIZE.w,
        height: SIZE.h,
        rate: RATE,
        frames,
        total,
        audio: vec![audio],
        settings: ExportSettings { width: SIZE.w, height: SIZE.h, rate: RATE, preset: "ultrafast".into(), ..ExportSettings::default() },
    };
    let decoders = Arc::new(FfmpegDecoders::new(backend.clone()));
    let stats = export(ExportRequest::new(source, job), decoders, &*backend, &no_progress(), &CancelToken::new()).expect("export");
    eprintln!(
        "export: {} frames in {:?} ({:.1} fps), render p50 {:?}, decode wait {:?}, encoder wait {:?}, finish {:?}, {:?}",
        stats.frames, stats.elapsed, stats.fps, stats.render.p50, stats.decode_wait, stats.encoder_wait, stats.finish, stats.resolver
    );
    assert_eq!(stats.frames, 50);
    assert!(output.is_file());

    let v = ffprobe(
        &output,
        &["-count_frames", "-select_streams", "v:0", "-show_entries", "stream=width,height,nb_read_frames,pix_fmt,color_space,color_primaries,color_transfer,color_range"],
    );
    assert_eq!((get(&v, "width"), get(&v, "height")), ("320", "180"));
    assert_eq!(get(&v, "nb_read_frames"), "50");
    assert_eq!(get(&v, "pix_fmt"), "yuv420p");
    assert_eq!(get(&v, "color_space"), "bt709");
    assert_eq!(get(&v, "color_primaries"), "bt709");
    assert_eq!(get(&v, "color_transfer"), "bt709");
    assert_eq!(get(&v, "color_range"), "tv");
    let au = ffprobe(&output, &["-select_streams", "a", "-show_entries", "stream=codec_name"]);
    assert_eq!(get(&au, "codec_name"), "aac", "audio present");

    // Frame 25 of the export matches frame 25 of the source (both decoded the same way).
    let out_media = media_of(&backend, &output);
    let got = decode_frame(&backend, &out_media, 25);
    let want = decode_frame(&backend, &media, 25);
    let (sum, n) = got.as_chunks::<4>().0.iter().zip(want.as_chunks::<4>().0).fold((0u64, 0u64), |(s, n), (g, w)| (s + (0..3).map(|i| g[i].abs_diff(w[i]) as u64).sum::<u64>(), n + 3));
    let mean = sum as f64 / n as f64;
    eprintln!("frame 25: mean abs diff {mean:.3}");
    assert!(mean < 3.0, "exported frame 25 differs from the source: mean abs diff {mean:.3}");
}
