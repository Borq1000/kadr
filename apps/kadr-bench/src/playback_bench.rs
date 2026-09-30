//! M3: seek latency and sequential decode through the real playback path
//! (`Resolver` over FFmpeg decoder sessions, then `CpuRenderer`), on the M0
//! clips and sizes so the numbers sit next to the M0 baseline.

use crate::alloc;
use crate::baseline::seek_times;
use crate::media::{self, TestClip};
use crate::report::Report;
use kadr_core::perf::{FramePerf, Stats};
use kadr_core::{AssetId, FrameRate, Time};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_playback::testing::{media_layer, video_source, FnSceneSource};
use kadr_playback::{Decoders, FfmpegDecoders, MediaSource, Mode, Resolver, ResolverConfig, SceneSource};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderTarget, Renderer};
use kadr_scene::{FrameScene, OutputSpec, RenderQuality, SizeU};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Frames decoded before the sequential measurement starts (session open, read-ahead and pool fill).
const WARMUP_FRAMES: usize = 30;

/// One row: a clip shown full-frame on a canvas of its own size, rendered at `output`.
struct Case {
    asset: AssetId,
    media: MediaSource,
    canvas: SizeU,
    output: SizeU,
}

impl Case {
    fn new(path: &std::path::Path, size: SizeU, secs: u32, output: SizeU) -> Self {
        let media = video_source(&path.to_string_lossy(), FrameRate::FPS_30, Time::from_secs(secs as i64), size);
        Case { asset: AssetId::new(), media, canvas: size, output }
    }

    /// The one-layer scene showing the clip at source time `t` (timeline time = source time).
    fn scene_at(&self, t: Time) -> FrameScene {
        let out = OutputSpec::new(self.output, RenderQuality::PreviewHigh);
        FrameScene { layers: vec![media_layer(1, self.asset, &self.media, t, self.canvas)], ..FrameScene::empty(t, self.canvas, out) }
    }

    fn source(&self) -> impl SceneSource + use<> {
        let (asset, media, canvas) = (self.asset, self.media.clone(), self.canvas);
        FnSceneSource {
            media: [(asset, media.clone())].into_iter().collect(),
            rate: FrameRate::FPS_30,
            duration: media.duration,
            canvas,
            scene: move |t: Time, out: &OutputSpec| FrameScene { layers: vec![media_layer(1, asset, &media, t, canvas)], ..FrameScene::empty(t, canvas, *out) },
        }
    }

    fn output_bytes(&self) -> usize {
        self.output.w as usize * self.output.h as usize * 4
    }
}

/// The two output sizes of every clip, as in M0: `full` (the clip's size) and `half`.
fn sizes(clip: &TestClip) -> [(&'static str, SizeU); 2] {
    [("full", SizeU::new(clip.width, clip.height)), ("half", SizeU::new(clip.width / 2, clip.height / 2))]
}

/// Prepares and renders one scene; the result must be a real frame (a missing layer would make the row meaningless).
fn prepare_and_render(resolver: &Resolver, renderer: &mut CpuRenderer, src: &dyn SceneSource, scene: &FrameScene, mode: Mode, out: &mut [u8]) -> Result<(), String> {
    let inputs = resolver.prepare(scene, src, mode, &mut FramePerf::default());
    match inputs.layers.first() {
        Some(LayerInput::Cpu(_)) => {}
        other => return Err(format!("no frame at {:?}: {other:?}", scene.time)),
    }
    let size = scene.output.size;
    renderer
        .render(&PreparedFrame { scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(size.w, size.h, out)))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Request → rendered frame for each of `times`, one resolver for all of them (as the app would have),
/// every request a new generation. Also returns how many streams the resolver opened.
fn seek_latency(decoders: Arc<dyn Decoders>, case: &Case, times: &[Time]) -> Result<(Stats, u64), String> {
    let resolver = Resolver::new(decoders, ResolverConfig::default());
    let src = case.source();
    let mut renderer = CpuRenderer::new();
    let mut out = vec![0u8; case.output_bytes()];
    let mut samples = Vec::with_capacity(times.len());
    for (i, &t) in times.iter().enumerate() {
        let started = Instant::now();
        let scene = src.scene_at(t, &OutputSpec::new(case.output, RenderQuality::PreviewHigh));
        prepare_and_render(&resolver, &mut renderer, &src, &scene, Mode::Scrub { generation: i as u64 + 1 }, &mut out)?;
        samples.push(started.elapsed());
    }
    std::hint::black_box(&out);
    Ok((Stats::of(samples), resolver.stats().streams_opened))
}

/// One measured window of a sequential run.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Window {
    fps: f64,
    /// Allocations of at least 1 MiB in the whole process, per frame.
    large_allocs_per_frame: f64,
    /// Buffers the resolver's frame pool allocated, per frame.
    pool_allocs_per_frame: f64,
}

/// A sequential run: the first `frames` frames after the warm-up, and the same number once the cache is full.
#[derive(Debug, PartialEq)]
struct Decode {
    /// Frames 30 to 30 + N: the cache is still filling, so the pool still grows.
    early: Window,
    /// After the cache is full (its capacity in frames plus the warm-up): steady state.
    steady: Window,
    /// Frames that fit into the resolver's cache at the decode size.
    cache_frames: usize,
}

/// The frame index where the steady-state window starts: the warm-up plus the frames that fill the cache,
/// pulled back so the window ends inside a clip of `clip_frames`.
fn steady_start(warmup: usize, frames: usize, cache_frames: usize, clip_frames: usize) -> usize {
    (warmup + cache_frames).min(clip_frames.saturating_sub(frames)).max(warmup)
}

/// Consecutive frames from frame 0 (prepare + render, playback-style read-ahead, the deadline far away).
/// Two windows of `frames` frames are measured: one after `warmup` frames, one after the cache has filled.
fn sequential_decode(decoders: Arc<dyn Decoders>, case: &Case, config: ResolverConfig, warmup: usize, frames: usize, clip_frames: usize) -> Result<Decode, String> {
    let cache_frames = config.cache_bytes / case.output_bytes().max(1);
    let late = steady_start(warmup, frames, cache_frames, clip_frames);
    let resolver = Resolver::new(decoders, config);
    let src = case.source();
    let mut renderer = CpuRenderer::new();
    let mut out = vec![0u8; case.output_bytes()];
    let rate = FrameRate::FPS_30;
    let mode = Mode::Deadline(Instant::now() + Duration::from_secs(3600));
    let mark = || (Instant::now(), alloc::large_allocs(), resolver.stats().pool_allocations);
    let window = |from: (Instant, u64, u64), to: (Instant, u64, u64)| Window {
        fps: frames as f64 / to.0.duration_since(from.0).as_secs_f64().max(1e-9),
        large_allocs_per_frame: (to.1 - from.1) as f64 / frames as f64,
        pool_allocs_per_frame: (to.2 - from.2) as f64 / frames as f64,
    };
    let (e0, e1, s0, s1) = (warmup, warmup + frames, late, late + frames);
    let mut marks = std::collections::HashMap::new();
    for n in 0..=s1 {
        if [e0, e1, s0, s1].contains(&n) {
            marks.insert(n, mark());
        }
        if n < s1 {
            prepare_and_render(&resolver, &mut renderer, &src, &case.scene_at(rate.frame_to_time(n as i64)), mode, &mut out)?;
        }
    }
    std::hint::black_box(&out);
    Ok(Decode { early: window(marks[&e0], marks[&e1]), steady: window(marks[&s0], marks[&s1]), cache_frames })
}

pub fn run(quick: bool) -> Result<Report, String> {
    let ff = FfmpegCli::locate().map_err(|e| e.to_string())?;
    let decoders: Arc<dyn Decoders> = Arc::new(FfmpegDecoders::new(Arc::new(ff)));
    let clips = media::ensure(&media::bench_dir()).map_err(|e| e.to_string())?;
    let (seeks, frames) = if quick { (5, 60) } else { (15, 300) };
    let mut r = Report::new("m3-playback");
    for clip in &clips {
        let display = SizeU::new(clip.width, clip.height);
        for (label, output) in sizes(clip) {
            let case = Case::new(&clip.path, display, clip.secs, output);
            let name = format!("{} {label}", clip.name);
            let (stats, opened) = seek_latency(decoders.clone(), &case, &seek_times(clip.secs, seeks))?;
            r.stats("seek", &name, &stats);
            r.push("seek", &name, "streams_opened", opened as f64, "n");
            let d = sequential_decode(decoders.clone(), &case, ResolverConfig::default(), WARMUP_FRAMES, frames, clip.secs as usize * 30)?;
            r.push("decode", &name, "fps", d.steady.fps, "fps");
            r.push("decode", &name, "large_allocs_per_frame", d.steady.large_allocs_per_frame, "n");
            r.push("decode", &name, "pool_allocs_per_frame", d.steady.pool_allocs_per_frame, "n");
            r.push("decode", &name, "cache_frames", d.cache_frames as f64, "n");
            r.push("decode", &name, "early_fps", d.early.fps, "fps");
            r.push("decode", &name, "early_large_allocs_per_frame", d.early.large_allocs_per_frame, "n");
            r.push("decode", &name, "early_pool_allocs_per_frame", d.early.pool_allocs_per_frame, "n");
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_playback::decode_size;
    use kadr_playback::testing::{FakeDecoders, FakeMedia};
    use kadr_scene::LayerContent;

    fn fake_case(output: SizeU) -> (Arc<FakeDecoders>, Case) {
        let fake = Arc::new(FakeDecoders::new(Duration::ZERO, Duration::ZERO));
        fake.add("clip.mp4", FakeMedia { tag: 7, ..Default::default() });
        (fake, Case::new(std::path::Path::new("clip.mp4"), SizeU::new(64, 36), 60, output))
    }

    #[test]
    fn the_bench_scene_is_one_full_frame_layer_decoded_at_the_output_size() {
        for output in [SizeU::new(64, 36), SizeU::new(32, 18)] {
            let (_, case) = fake_case(output);
            let scene = case.scene_at(Time::from_secs(3));
            assert_eq!((scene.canvas, scene.output.size, scene.time), (case.canvas, output, Time::from_secs(3)));
            assert_eq!(scene.layers.len(), 1);
            let l = &scene.layers[0];
            let LayerContent::Media { source_time, .. } = &l.content else { panic!("a media layer") };
            assert_eq!(*source_time, Time::from_secs(3));
            assert_eq!(decode_size(&l.placement, scene.canvas, output, case.media.display_size), output, "the layer is decoded at the output size");
        }
    }

    #[test]
    fn the_steady_window_starts_after_the_cache_fills_and_ends_inside_the_clip() {
        assert_eq!(steady_start(30, 300, 128, 1800), 158, "warm-up plus the cache");
        assert_eq!(steady_start(30, 300, 517, 600), 300, "pulled back to end at the clip end");
        assert_eq!(steady_start(30, 300, 5, 100), 30, "never before the warm-up");
        assert_eq!(steady_start(30, 300, 0, 1800), 30);
    }

    #[test]
    fn half_is_half_the_clip_size() {
        let clip = TestClip { name: "x", path: "x.mp4".into(), width: 3840, height: 2160, secs: 20 };
        assert_eq!(sizes(&clip), [("full", SizeU::new(3840, 2160)), ("half", SizeU::new(1920, 1080))]);
    }

    #[test]
    fn a_seek_run_measures_every_request_with_one_resolver() {
        let (fake, case) = fake_case(SizeU::new(64, 36));
        let times = seek_times(60, 4);
        let (stats, opened) = seek_latency(fake.clone(), &case, &times).unwrap();
        assert_eq!(stats.count, 4);
        assert_eq!(opened, fake.opens(), "the reported streams are the ones the decoder opened");
        assert!((1..=4).contains(&opened), "{opened}");
    }

    #[test]
    fn a_sequential_run_reads_forward_in_one_stream_and_counts_after_warmup() {
        let (fake, case) = fake_case(SizeU::new(64, 36));
        let config = ResolverConfig { cache_bytes: case.output_bytes() * 12, ..ResolverConfig::default() };
        let d = sequential_decode(fake.clone(), &case, config, 5, 20, 10_000).unwrap();
        assert_eq!(d.cache_frames, 12);
        assert!(d.early.fps > 0.0 && d.steady.fps > 0.0);
        assert_eq!(fake.opens(), 1, "one stream read forward");
        assert!(d.steady.pool_allocs_per_frame <= d.early.pool_allocs_per_frame, "{d:?}");
    }

    #[test]
    fn a_missing_frame_fails_the_run_instead_of_timing_nothing() {
        let (_, case) = fake_case(SizeU::new(64, 36));
        let unknown = Arc::new(FakeDecoders::new(Duration::ZERO, Duration::ZERO)); // knows no media: decode fails
        assert!(seek_latency(unknown, &case, &[Time::from_secs(1)]).is_err());
    }
}
