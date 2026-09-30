//! The resolver on real FFmpeg: a generated clip, a hand-built one-layer
//! scene (no timeline), frames at several times through `FfmpegDecoders`,
//! rendered with `CpuRenderer`. Skipped (with a message) without FFmpeg or
//! while source decoding is not implemented.

use kadr_core::perf::FramePerf;
use kadr_core::{AssetId, FrameRate, Time};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::{MediaBackend, MediaError, SourceRequest};
use kadr_playback::testing::{media_layer, FnSceneSource};
use kadr_playback::{FfmpegDecoders, MediaSource, Mode, Resolver, ResolverConfig};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderTarget, Renderer};
use kadr_scene::{FrameScene, OutputSpec, RenderQuality, SizeU, SourceKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const SIZE: SizeU = SizeU::new(320, 180);

fn ffmpeg_exe() -> PathBuf {
    let exe = format!("ffmpeg{}", std::env::consts::EXE_SUFFIX);
    match std::env::var_os("KADR_FFMPEG_DIR") {
        Some(d) => PathBuf::from(d).join(exe),
        None => PathBuf::from(exe),
    }
}

/// testsrc2 320×180, 25 fps, 4 s (100 frames).
fn generate(dir: &Path) -> Option<PathBuf> {
    let out = dir.join("clip.mp4");
    for codec in [&["-c:v", "libx264", "-preset", "veryfast", "-g", "25"][..], &["-c:v", "mpeg4", "-q:v", "2", "-g", "25"][..]] {
        let ok = Command::new(ffmpeg_exe())
            .args(["-hide_banner", "-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=4"])
            .args(codec)
            .args(["-pix_fmt", "yuv420p"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Some(out);
        }
    }
    None
}

fn setup() -> Option<(tempfile::TempDir, Arc<FfmpegCli>, MediaSource)> {
    let Ok(backend) = FfmpegCli::locate() else {
        eprintln!("SKIP: FFmpeg not found");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = generate(dir.path()) else {
        eprintln!("SKIP: cannot generate a test clip with FFmpeg");
        return None;
    };
    let info = backend.probe(&path).expect("probe");
    let v = info.video.as_ref().expect("video stream");
    let (w, h) = v.display_size();
    let media = MediaSource {
        path,
        kind: SourceKind::Video,
        rate: v.frame_rate.expect("frame rate"),
        duration: info.duration,
        display_size: SizeU::new(w, h),
        color: info.source_color(),
        online: true,
    };
    let probe = SourceRequest { path: media.path.clone(), rate: media.rate, start_frame: 0, width: 32, height: 18, color: media.color, hwaccel: false };
    match backend.open_source(&probe) {
        Err(MediaError::Unsupported(m)) => {
            eprintln!("SKIP: source decoding not available yet ({m})");
            return None;
        }
        r => drop(r.expect("open_source")),
    }
    Some((dir, Arc::new(backend), media))
}

/// Every frame, decoded sequentially from the start.
fn reference(backend: &FfmpegCli, media: &MediaSource) -> Vec<Vec<u8>> {
    let req = SourceRequest { path: media.path.clone(), rate: media.rate, start_frame: 0, width: SIZE.w, height: SIZE.h, color: media.color, hwaccel: false };
    let mut s = backend.open_source(&req).unwrap();
    let mut frames = vec![];
    loop {
        let mut buf = vec![0u8; SIZE.w as usize * SIZE.h as usize * 4];
        if !s.read_into(&mut buf).unwrap() {
            break;
        }
        frames.push(buf);
    }
    frames
}

#[test]
fn resolves_and_renders_exact_frames_from_a_real_clip() {
    let Some((_dir, backend, media)) = setup() else { return };
    assert_eq!(media.rate, FrameRate::FPS_25);
    let reference = reference(&backend, &media);
    assert_eq!(reference.len(), 100, "4 s at 25 fps");

    let a = AssetId::new();
    let src = FnSceneSource {
        media: [(a, media.clone())].into_iter().collect(),
        rate: FrameRate::FPS_25,
        duration: Time::from_secs(4),
        canvas: SIZE,
        scene: |t: Time, out: &OutputSpec| FrameScene::empty(t, SIZE, *out),
    };
    let decoders = Arc::new(FfmpegDecoders::new(backend.clone()));
    let resolver = Resolver::new(decoders, ResolverConfig::default());
    let out = OutputSpec::new(SIZE, RenderQuality::PreviewHigh);
    let scene_at = |t: Time| FrameScene { layers: vec![media_layer(1, a, &media, t, SIZE)], ..FrameScene::empty(t, SIZE, out) };
    let mut renderer = CpuRenderer::new();
    let mut can_render = true;
    // Seeks back and forth, a frame boundary, reading forward, the end and past it.
    let times = [(1000, 25), (1039, 25), (1040, 26), (3960, 99), (4000, 99), (5000, 99), (0, 0), (2520, 63), (2600, 65), (1000, 25)];
    for (g, (t_ms, expect)) in times.into_iter().enumerate() {
        let scene = scene_at(Time::from_millis(t_ms));
        let mut perf = FramePerf::default();
        let inputs = resolver.prepare(&scene, &src, Mode::Scrub { generation: g as u64 + 1 }, &mut perf);
        let LayerInput::Cpu(frame) = &inputs.layers[0] else { panic!("{t_ms} ms: {:?}", inputs.layers[0]) };
        assert_eq!((frame.width, frame.height), (SIZE.w, SIZE.h));
        assert!(frame.data[..] == reference[expect][..], "{t_ms} ms must be source frame {expect}");
        if can_render {
            let mut target = vec![0u8; SIZE.w as usize * SIZE.h as usize * 4];
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                renderer.render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(SIZE.w, SIZE.h, &mut target)))
            }));
            match r {
                Ok(stats) => {
                    stats.expect("render");
                    let worst = target.iter().zip(&frame.data[..]).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                    assert!(worst <= 1, "an opaque full-canvas layer renders as the frame itself (worst {worst})");
                }
                Err(_) => {
                    eprintln!("SKIP rendering: CpuRenderer is not implemented yet");
                    can_render = false;
                }
            }
        }
    }
    let stats = resolver.stats();
    assert!(stats.streams_opened <= 6, "{stats:?}");
    // A half-size output decodes at half size.
    let half = OutputSpec::new(SizeU::new(160, 90), RenderQuality::PreviewFast);
    let scene = FrameScene { layers: vec![media_layer(1, a, &media, Time::from_secs(2), SIZE)], ..FrameScene::empty(Time::from_secs(2), SIZE, half) };
    let inputs = resolver.prepare(&scene, &src, Mode::Export, &mut FramePerf::default());
    let LayerInput::Cpu(frame) = &inputs.layers[0] else { panic!("{:?}", inputs.layers[0]) };
    assert_eq!((frame.width, frame.height), (160, 90));
}

#[test]
fn sequential_playback_through_ffmpeg_reads_forward_in_one_stream() {
    let Some((_dir, backend, media)) = setup() else { return };
    let reference = reference(&backend, &media);
    let a = AssetId::new();
    let src = FnSceneSource {
        media: [(a, media.clone())].into_iter().collect(),
        rate: FrameRate::FPS_25,
        duration: Time::from_secs(4),
        canvas: SIZE,
        scene: |t: Time, out: &OutputSpec| FrameScene::empty(t, SIZE, *out),
    };
    let resolver = Resolver::new(Arc::new(FfmpegDecoders::new(backend)), ResolverConfig::default());
    let out = OutputSpec::new(SIZE, RenderQuality::PreviewHigh);
    for f in 10..60 {
        let t = FrameRate::FPS_25.frame_to_time(f);
        let scene = FrameScene { layers: vec![media_layer(1, a, &media, t, SIZE)], ..FrameScene::empty(t, SIZE, out) };
        let inputs = resolver.prepare(&scene, &src, Mode::Export, &mut FramePerf::default());
        let LayerInput::Cpu(frame) = &inputs.layers[0] else { panic!("{:?}", inputs.layers[0]) };
        assert!(frame.data[..] == reference[f as usize][..], "frame {f}");
    }
    assert_eq!(resolver.stats().streams_opened, 1);
}
