//! Resolver, sessions and cache on the fake decoder.

use kadr_core::perf::FramePerf;
use kadr_core::{AssetId, CpuFrame, FrameRate, Time};
use kadr_playback::testing::{media_layer, read_fake_pixel, video_source, FakeDecoders, FakeMedia, FnSceneSource};
use kadr_playback::{MediaSource, Mode, Resolver, ResolverConfig, SceneSource};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{
    BlendMode, FrameScene, Layer, LayerContent, LayerId, OutputSpec, Placement, RectF, RenderQuality, Rgba, SizeU, SourceKind, TransitionLayer, TransitionOp, Vec2,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const CANVAS: SizeU = SizeU::new(64, 36);

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

type Source = FnSceneSource<fn(Time, &OutputSpec) -> FrameScene>;

fn empty_scene(t: Time, out: &OutputSpec) -> FrameScene {
    FrameScene::empty(t, CANVAS, *out)
}

fn source(media: Vec<(AssetId, MediaSource)>) -> Source {
    FnSceneSource { media: media.into_iter().collect::<HashMap<_, _>>(), rate: FrameRate::FPS_25, duration: Time::from_secs(60), canvas: CANVAS, scene: empty_scene }
}

fn scene(layers: Vec<Layer>) -> FrameScene {
    FrameScene { layers, ..FrameScene::empty(Time::ZERO, CANVAS, OutputSpec::new(CANVAS, RenderQuality::PreviewHigh)) }
}

fn frame_of(input: &LayerInput) -> &Arc<CpuFrame> {
    match input {
        LayerInput::Cpu(f) => f,
        other => panic!("expected a frame, got {other:?}"),
    }
}

/// (tag, source frame) the fake decoder wrote into this input.
fn which(input: &LayerInput) -> (u8, i64) {
    read_fake_pixel(frame_of(input).row(0))
}

fn resolver(fake: &Arc<FakeDecoders>, config: ResolverConfig) -> Resolver {
    Resolver::new(fake.clone(), config)
}

fn fake(open: u64, frame: u64) -> Arc<FakeDecoders> {
    Arc::new(FakeDecoders::new(ms(open), ms(frame)))
}

fn prepare(r: &Resolver, s: &FrameScene, src: &dyn SceneSource, mode: Mode) -> (RenderInputs, FramePerf) {
    let mut perf = FramePerf::default();
    let inputs = r.prepare(s, src, mode, &mut perf);
    (inputs, perf)
}

#[test]
fn each_layer_gets_its_exact_source_frame_including_speed_and_rate() {
    let fake = fake(1, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("b.mp4", FakeMedia { tag: 2, ..Default::default() });
    let (a, b) = (AssetId::new(), AssetId::new());
    let ma = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let mb = video_source("b.mp4", FrameRate::FPS_29_97, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, ma.clone()), (b, mb.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    for t in [Time::ZERO, Time::from_millis(39), Time::from_millis(40), Time::from_millis(1234), Time::from_secs(7)] {
        // Layer b plays at double speed: source time 2t.
        let s = scene(vec![media_layer(1, a, &ma, t, CANVAS), media_layer(2, b, &mb, Time(t.flicks() * 2), CANVAS)]);
        let (inputs, _) = prepare(&r, &s, &src, Mode::Export);
        assert_eq!(which(&inputs.layers[0]), (1, FrameRate::FPS_25.time_to_frame(t)), "a at {t:?}");
        assert_eq!(which(&inputs.layers[1]), (2, FrameRate::FPS_29_97.time_to_frame(Time(t.flicks() * 2))), "b at {t:?}");
    }
}

#[test]
fn frames_are_working_space_with_the_source_alpha() {
    let fake = fake(0, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, Time::ZERO, CANVAS)]), &src, Mode::Export);
    let f = frame_of(&inputs.layers[0]);
    assert_eq!(f.color, m.frame_color());
    assert_eq!(f.color.alpha, kadr_core::color::AlphaMode::Opaque);
}

#[test]
fn at_and_after_the_media_end_the_last_frame_is_shown() {
    let fake = fake(1, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(4), CANVAS); // frames 0..=99
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    for t in [Time::from_secs(4), Time::from_secs(4) + Time(1), Time::from_secs(9)] {
        let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, t, CANVAS)]), &src, Mode::Export);
        assert_eq!(which(&inputs.layers[0]), (1, 99), "at {t:?}");
    }
}

#[test]
fn a_stream_ending_early_serves_its_last_frame_for_later_indices() {
    let fake = fake(1, 0);
    // The container says 4 s (100 frames) but only 90 decode.
    fake.add("a.mp4", FakeMedia { tag: 1, frames: Some(90), fail: false });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(4), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let at = |f: i64| FrameRate::FPS_25.frame_to_time(f);
    let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, at(95), CANVAS)]), &src, Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 89));
    let opens = fake.opens();
    let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, at(99), CANVAS)]), &src, Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 89));
    assert_eq!(fake.opens(), opens, "the end is remembered: no new decoding");
    let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, at(50), CANVAS)]), &src, Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 50), "earlier frames are unaffected");
    // Reading forward into the end also finds it.
    fake.add("b.mp4", FakeMedia { tag: 2, frames: Some(10), fail: false });
    let b = AssetId::new();
    let mb = video_source("b.mp4", FrameRate::FPS_25, Time::from_secs(4), CANVAS);
    let src = source(vec![(b, mb.clone())]);
    for f in 0..14 {
        let (inputs, _) = prepare(&r, &scene(vec![media_layer(2, b, &mb, at(f), CANVAS)]), &src, Mode::Export);
        assert_eq!(which(&inputs.layers[0]), (2, f.min(9)), "frame {f}");
    }
}

#[test]
fn layers_are_decoded_at_their_quantised_footprint() {
    let fake = fake(0, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let display = SizeU::new(128, 72);
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), display);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let mut layer = media_layer(1, a, &m, Time::ZERO, CANVAS);
    // Full canvas (64 wide) of a 128-wide source: ½.
    let (inputs, _) = prepare(&r, &scene(vec![layer.clone()]), &src, Mode::Export);
    let f = frame_of(&inputs.layers[0]);
    assert_eq!((f.width, f.height), (64, 36));
    // Scaled to 0.3: 19.2 px of 128 = 0.15 → ¼.
    layer.placement.scale = kadr_scene::Vec2::new(0.3, 0.3);
    let (inputs, _) = prepare(&r, &scene(vec![layer]), &src, Mode::Export);
    let f = frame_of(&inputs.layers[0]);
    assert_eq!((f.width, f.height), (32, 18));
}

#[test]
fn the_same_media_at_two_times_uses_two_sessions() {
    let fake = fake(5, 1);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    // A transition inside one file: 100 s and 300 s apart, played for 20 frames.
    for i in 0..20 {
        let t1 = FrameRate::FPS_25.frame_to_time(2500 + i);
        let t2 = FrameRate::FPS_25.frame_to_time(7500 + i);
        let s = scene(vec![media_layer(1, a, &m, t1, CANVAS), media_layer(2, a, &m, t2, CANVAS)]);
        let (inputs, _) = prepare(&r, &s, &src, Mode::Deadline(Instant::now() + Duration::from_secs(5)));
        assert_eq!(which(&inputs.layers[0]), (1, 2500 + i));
        assert_eq!(which(&inputs.layers[1]), (1, 7500 + i));
    }
    assert_eq!(r.stats().sessions, 2);
    assert_eq!(fake.opens(), 2, "each layer keeps reading forward in its own session");
}

#[test]
fn transition_inputs_are_resolved_recursively() {
    let fake = fake(1, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("b.mp4", FakeMedia { tag: 2, ..Default::default() });
    let (a, b) = (AssetId::new(), AssetId::new());
    let ma = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let mb = video_source("b.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, ma.clone()), (b, mb.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let t = TransitionLayer {
        op: TransitionOp::Dissolve,
        progress: 0.5,
        from: vec![media_layer(1, a, &ma, Time::from_secs(10), CANVAS)],
        to: vec![media_layer(2, b, &mb, Time::ZERO, CANVAS), Layer { content: LayerContent::Solid(Rgba::BLACK), ..media_layer(3, b, &mb, Time::ZERO, CANVAS) }],
    };
    let tl = Layer {
        id: LayerId(9),
        content: LayerContent::Transition(Box::new(t)),
        placement: Placement::fill(CANVAS),
        crop: Placement::fill(CANVAS).full_crop(),
        opacity: 1.0,
        blend: BlendMode::Normal,
        effects: vec![],
    };
    let (inputs, perf) = prepare(&r, &scene(vec![tl]), &src, Mode::Export);
    let LayerInput::Transition { from, to } = &inputs.layers[0] else { panic!("{:?}", inputs.layers[0]) };
    assert_eq!(which(&from[0]), (1, 250));
    assert_eq!(which(&to[0]), (2, 0));
    assert!(matches!(to[1], LayerInput::None));
    assert_eq!(perf.decode.len(), 2, "one decode timing per media layer");
    assert_eq!(perf.cache_misses, 2);
}

#[test]
fn an_image_is_decoded_once() {
    let fake = fake(1, 0);
    fake.add("logo.png", FakeMedia { tag: 7, ..Default::default() });
    let a = AssetId::new();
    let m = MediaSource { kind: SourceKind::Image, color: kadr_core::ColorInfo::IMAGE_SRGB, ..video_source("logo.png", FrameRate::FPS_25, Time::ZERO, CANVAS) };
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    for i in 0..5 {
        let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, Time::from_secs(i), CANVAS)]), &src, Mode::Scrub { generation: i as u64 + 1 });
        assert_eq!(which(&inputs.layers[0]), (7, 0));
        assert_eq!(frame_of(&inputs.layers[0]).color.alpha, kadr_core::color::AlphaMode::Straight);
    }
    assert_eq!((fake.stills(), fake.opens()), (1, 0));
}

#[test]
fn a_repeated_request_is_a_cache_hit_without_decoding() {
    let fake = fake(1, 1);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &m, Time::from_secs(3), CANVAS)]);
    let (_, first) = prepare(&r, &s, &src, Mode::Scrub { generation: 1 });
    assert_eq!((first.cache_hits, first.cache_misses), (0, 1));
    assert!(first.decode[0].time >= ms(2), "open + first frame: {:?}", first.decode);
    let frames = fake.frames();
    let (inputs, second) = prepare(&r, &s, &src, Mode::Scrub { generation: 2 });
    assert_eq!((second.cache_hits, second.cache_misses), (1, 0));
    assert_eq!(second.decode[0].time, Duration::ZERO, "the decode was reported with the first delivery");
    assert_eq!(which(&inputs.layers[0]), (1, 75));
    assert_eq!(fake.frames(), frames, "nothing decoded");
}

#[test]
fn offline_or_unknown_media_is_missing_without_decoding() {
    let fake = fake(0, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let (a, unknown) = (AssetId::new(), AssetId::new());
    let m = MediaSource { online: false, ..video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS) };
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &m, Time::ZERO, CANVAS), media_layer(2, unknown, &m, Time::ZERO, CANVAS)]);
    let (inputs, _) = prepare(&r, &s, &src, Mode::Export);
    assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::Offline)));
    assert!(matches!(inputs.layers[1], LayerInput::Missing(MissingReason::Offline)));
    assert_eq!(fake.opens(), 0);
}

#[test]
fn a_failing_decoder_is_missing_and_not_retried_in_a_loop() {
    let fake = fake(1, 0);
    fake.add("bad.mp4", FakeMedia { tag: 1, frames: None, fail: true });
    let a = AssetId::new();
    let m = video_source("bad.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &m, Time::ZERO, CANVAS)]);
    let started = Instant::now();
    let (inputs, _) = prepare(&r, &s, &src, Mode::Export);
    assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::DecodeFailed)));
    let waited = started.elapsed();
    // Export retries after 50, 200 and 500 ms, then gives up.
    assert!(waited >= ms(750) && waited < ms(2000), "Export retries a bounded number of times: {waited:?}");
    assert_eq!(fake.open_calls(), 4, "the first open and three retries");
    // Preview: the failure is remembered, nothing is retried.
    for g in 1..20 {
        let t0 = Instant::now();
        let (inputs, _) = prepare(&r, &s, &src, Mode::Scrub { generation: g });
        assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::DecodeFailed)));
        assert!(t0.elapsed() < ms(50), "answered at once: {:?}", t0.elapsed());
    }
    std::thread::sleep(ms(50));
    assert_eq!(fake.open_calls(), 4, "scrubbing did not retry");
    assert_eq!(r.stats().streams_opened, 0);
    // Export gave up on this media: a later frame is tried once, without retries.
    let t0 = Instant::now();
    let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, Time::from_secs(1), CANVAS)]), &src, Mode::Export);
    assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::DecodeFailed)));
    assert!(t0.elapsed() < ms(300), "{:?}", t0.elapsed());
    assert_eq!(fake.open_calls(), 5);
}

/// A transient failure: preview reports it (and keeps reporting it for a
/// while without retrying); export, in that same window, reopens the
/// stream at the frame and gets it.
#[test]
fn export_retries_a_transient_failure_that_preview_reports() {
    let fake = fake(1, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.fail_opens("a.mp4", 1);
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &m, FrameRate::FPS_25.frame_to_time(10), CANVAS)]);
    let (inputs, _) = prepare(&r, &s, &src, Mode::Scrub { generation: 1 });
    assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::DecodeFailed)));
    let calls = fake.open_calls();
    let t0 = Instant::now();
    let (inputs, _) = prepare(&r, &s, &src, Mode::Scrub { generation: 2 });
    assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::DecodeFailed)), "within the 2 s window");
    assert!(t0.elapsed() < ms(50), "{:?}", t0.elapsed());
    assert_eq!(fake.open_calls(), calls, "preview does not retry");
    let (inputs, _) = prepare(&r, &s, &src, Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 10), "export is not short-circuited by the failure memory");

    // Two failed reads in a row at another frame: two retries, then the frame.
    fake.fail_reads("a.mp4", 2);
    let t0 = Instant::now();
    let s = scene(vec![media_layer(1, a, &m, FrameRate::FPS_25.frame_to_time(900), CANVAS)]);
    let (inputs, _) = prepare(&r, &s, &src, Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 900));
    let waited = t0.elapsed();
    assert!(waited >= ms(250) && waited < ms(2000), "backoffs of 50 and 200 ms: {waited:?}");
}

/// Media layers inside a transition that draw nothing whatever their input
/// (opacity 0, zero scale, empty crop, zero size) are never decoded; their
/// input is a frame the renderer accepts and draws nothing with.
#[test]
fn invisible_layers_inside_a_transition_are_not_decoded() {
    let fake = fake(1, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("bad.mp4", FakeMedia { tag: 2, frames: None, fail: true });
    let (a, bad) = (AssetId::new(), AssetId::new());
    let ma = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let mb = video_source("bad.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, ma.clone()), (bad, mb.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let hidden = |id: u128, f: &dyn Fn(&mut Layer)| {
        let mut l = media_layer(id, bad, &mb, Time::ZERO, CANVAS);
        f(&mut l);
        l
    };
    let t = TransitionLayer {
        op: TransitionOp::Dissolve,
        progress: 0.5,
        from: vec![media_layer(1, a, &ma, Time::from_secs(10), CANVAS), hidden(2, &|l| l.opacity = 0.0), hidden(3, &|l| l.opacity = f32::NAN)],
        to: vec![
            hidden(4, &|l| l.placement.scale = Vec2::new(0.0, 1.0)),
            hidden(5, &|l| l.crop = RectF::new(10.0, 0.0, 10.0, 36.0)),
            hidden(6, &|l| l.placement.size = Vec2::new(0.0, 36.0)),
        ],
    };
    let tl = Layer {
        id: LayerId(9),
        content: LayerContent::Transition(Box::new(t)),
        placement: Placement::fill(CANVAS),
        crop: Placement::fill(CANVAS).full_crop(),
        opacity: 1.0,
        blend: BlendMode::Normal,
        effects: vec![],
    };
    let s = scene(vec![tl]);
    let (inputs, perf) = prepare(&r, &s, &src, Mode::Export);
    let LayerInput::Transition { from, to } = &inputs.layers[0] else { panic!("{:?}", inputs.layers[0]) };
    assert_eq!(which(&from[0]), (1, 250));
    for input in from[1..].iter().chain(to) {
        let f = frame_of(input);
        assert_eq!((f.width, f.height), (1, 1), "a blank frame, not a decode");
    }
    assert_eq!(fake.open_calls(), 1, "only the visible layer was decoded");
    assert_eq!(perf.decode.len(), 1);
    let mut buf = vec![0u8; CANVAS.w as usize * CANVAS.h as usize * 4];
    let mut target = RenderTarget::Cpu(CpuTarget::packed(CANVAS.w, CANVAS.h, &mut buf));
    CpuRenderer::new().render(&PreparedFrame { scene: &s, inputs: &inputs }, &mut target).expect("the renderer accepts the blank inputs");
}

#[test]
fn past_the_deadline_a_frame_is_not_ready() {
    let fake = fake(200, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &m, Time::ZERO, CANVAS)]);
    let started = Instant::now();
    let (inputs, _) = prepare(&r, &s, &src, Mode::Deadline(Instant::now() + ms(20)));
    assert!(matches!(inputs.layers[0], LayerInput::Missing(MissingReason::NotReady)));
    let waited = started.elapsed();
    assert!(waited >= ms(20) && waited < ms(150), "waited {waited:?}");
}

#[test]
fn prefetch_opens_the_next_clip_before_it_is_needed() {
    let fake = fake(30, 1);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(5, a, &m, Time::from_secs(40), CANVAS)]);
    r.prefetch(&s, &src);
    // Decoded with no one asking for it.
    let key = kadr_playback::FrameKey { media: a, size: CANVAS, frame: 1000 };
    assert!(r.cache().wait_for(&key, Some(Instant::now() + Duration::from_secs(5))).is_some());
    let (inputs, perf) = prepare(&r, &s, &src, Mode::Deadline(Instant::now() + ms(5)));
    assert_eq!(which(&inputs.layers[0]), (1, 1000));
    assert_eq!((perf.cache_hits, perf.cache_misses), (1, 0));
}

/// 50 rapid scrubs to far-apart positions while opening takes 300 ms and each
/// frame 50 ms: superseded requests return at once (well before their frame
/// could be ready), sessions give up superseded targets, and the last
/// request gets its frame.
#[test]
fn scrubbing_abandons_superseded_requests() {
    let fake = fake(300, 50);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(3600), CANVAS);
    let src = Arc::new(source(vec![(a, m.clone())]));
    let r = Arc::new(resolver(&fake, ResolverConfig::default()));
    let frame_for = |g: u64| (g as i64 * 7919) % 60_000; // the fake pixel holds 16 bits
    let mut threads = vec![];
    for g in 1..=50u64 {
        let (r, src, m) = (r.clone(), src.clone(), m.clone());
        threads.push(std::thread::spawn(move || {
            let s = scene(vec![media_layer(1, a, &m, FrameRate::FPS_25.frame_to_time(frame_for(g)), CANVAS)]);
            let started = Instant::now();
            let (inputs, _) = prepare(&r, &s, &*src, Mode::Scrub { generation: g });
            (started, Instant::now(), inputs)
        }));
        std::thread::sleep(ms(3));
    }
    let results: Vec<(Instant, Instant, RenderInputs)> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    for (i, (started, returned, inputs)) in results.iter().enumerate().take(49) {
        // Superseded once a newer generation has started (threads may start out of order).
        let superseded = results[i + 1..].iter().map(|r| r.0).min().unwrap().max(*started);
        let late = returned.saturating_duration_since(superseded);
        // Waiting for its frame would take ≥ 350 ms (open + frame); anything here is scheduling slack.
        assert!(late < ms(200), "generation {} returned {late:?} after being superseded", i + 1);
        if let LayerInput::Cpu(_) = &inputs.layers[0] {
            assert_eq!(which(&inputs.layers[0]).1, frame_for(i as u64 + 1), "a frame, if any, is the right one");
        }
    }
    assert_eq!(which(&results[49].2.layers[0]), (1, frame_for(50)), "the last request is served");
    let (opens, frames) = (fake.opens(), fake.frames());
    assert!(opens <= 5, "{opens} streams opened for 50 requests");
    assert!(frames <= 5, "{frames} frames decoded for 50 requests");
    assert_eq!(r.stats().sessions, 1, "one layer, one session");
}

/// Sequential playback in steady state reuses pooled buffers: after warm-up
/// the pool allocates nothing new.
#[test]
fn steady_state_playback_allocates_no_new_frame_buffers() {
    let fake = fake(2, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let frame_bytes = 64 * 36 * 4;
    let r = resolver(&fake, ResolverConfig { cache_bytes: frame_bytes * 48, ..ResolverConfig::default() });
    let mut after_warmup = 0;
    let mut misses = 0;
    for f in 0..300 {
        if f > 0 {
            // Read-ahead produces the next frame before it is asked for.
            let key = kadr_playback::FrameKey { media: a, size: CANVAS, frame: f };
            assert!(r.cache().wait_for(&key, Some(Instant::now() + Duration::from_secs(5))).is_some(), "frame {f} was not read ahead");
        }
        let s = scene(vec![media_layer(1, a, &m, FrameRate::FPS_25.frame_to_time(f), CANVAS)]);
        let (inputs, perf) = prepare(&r, &s, &src, Mode::Deadline(Instant::now() + Duration::from_secs(2)));
        assert_eq!(which(&inputs.layers[0]), (1, f));
        misses += perf.cache_misses;
        if f == 100 {
            after_warmup = r.pool().allocations();
        }
    }
    assert_eq!(r.pool().allocations(), after_warmup, "no new buffers after warm-up");
    assert!(r.cache().bytes() <= frame_bytes * 48);
    assert_eq!(fake.opens(), 1, "one stream read forward all the way");
    assert_eq!(misses, 1, "after the first frame playback runs from the cache");
}

#[test]
fn idle_sessions_close_and_dropping_the_resolver_stops_everything() {
    let fake = fake(1, 0);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = source(vec![(a, m.clone())]);
    let r = resolver(&fake, ResolverConfig { idle_close: ms(100), ..ResolverConfig::default() });
    prepare(&r, &scene(vec![media_layer(1, a, &m, Time::ZERO, CANVAS)]), &src, Mode::Export);
    assert_eq!(fake.live_streams(), 1);
    std::thread::sleep(ms(400));
    assert_eq!(fake.live_streams(), 0, "closed when idle");
    assert_eq!(r.stats().sessions, 0);
    // A new request after that simply opens again.
    let (inputs, _) = prepare(&r, &scene(vec![media_layer(1, a, &m, Time::from_secs(1), CANVAS)]), &src, Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 25));
    drop(r);
    assert_eq!(fake.live_streams(), 0, "drop joins the sessions and closes their streams");
}

#[test]
fn a_changed_media_path_drops_stale_frames() {
    let fake = fake(1, 0);
    fake.add("old.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("new.mp4", FakeMedia { tag: 2, ..Default::default() });
    let a = AssetId::new();
    let old = video_source("old.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let new = video_source("new.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &old, Time::ZERO, CANVAS)]);
    let (inputs, _) = prepare(&r, &s, &source(vec![(a, old)]), Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (1, 0));
    let (inputs, _) = prepare(&r, &s, &source(vec![(a, new)]), Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (2, 0), "relinked: decoded from the new file");
}

/// A relink while the old file's session is inside a read: that frame, and
/// what the old session learns (a failure, an early end), must not outlive
/// the relink — the new file's frames and records are its own.
#[test]
fn a_relink_during_a_decode_never_serves_the_old_files_frame_or_failure() {
    let fake = fake(0, 150);
    fake.add("old.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("new.mp4", FakeMedia { tag: 2, ..Default::default() });
    let a = AssetId::new();
    let old = video_source("old.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let new = video_source("new.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = resolver(&fake, ResolverConfig::default());
    let s = scene(vec![media_layer(1, a, &old, Time::ZERO, CANVAS)]);
    // The old file's session starts reading frame 0 (150 ms); the request gives up first.
    prepare(&r, &s, &source(vec![(a, old.clone())]), Mode::Deadline(Instant::now() + ms(10)));
    // Relinked while that read is in flight; the old read finishes first.
    // (Whatever the timing, only the new file's frame may be served.)
    let (inputs, _) = prepare(&r, &s, &source(vec![(a, new.clone())]), Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (2, 0), "relinked: decoded from the new file");
    let (inputs, _) = prepare(&r, &s, &source(vec![(a, new)]), Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (2, 0), "and the cache holds the new file's frame");

    // The old file fails (to open, 150 ms) after the relink to a good file.
    let slow_open = Arc::new(FakeDecoders::new(ms(150), ms(0)));
    slow_open.add("bad.mp4", FakeMedia { tag: 3, frames: None, fail: true });
    slow_open.add("good.mp4", FakeMedia { tag: 4, ..Default::default() });
    let b = AssetId::new();
    let bad = video_source("bad.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let good = video_source("good.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = resolver(&slow_open, ResolverConfig::default());
    let s = scene(vec![media_layer(1, b, &bad, Time::ZERO, CANVAS)]);
    prepare(&r, &s, &source(vec![(b, bad)]), Mode::Deadline(Instant::now() + ms(10)));
    let (inputs, _) = prepare(&r, &s, &source(vec![(b, good)]), Mode::Export);
    assert_eq!(which(&inputs.layers[0]), (4, 0), "the old file's failure is not the new file's");
}

/// The decode time of the first frame after an open is the open plus that
/// frame — not the time the open stream then sat idle (a scrub moved on
/// during the open and came back later).
#[test]
fn decode_time_excludes_idle_time_after_an_open() {
    let fake = fake(40, 1);
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let src = Arc::new(source(vec![(a, m.clone())]));
    let r = Arc::new(resolver(&fake, ResolverConfig::default()));
    let s = scene(vec![media_layer(1, a, &m, FrameRate::FPS_25.frame_to_time(100), CANVAS)]);
    // Superseded while the stream opens: the open completes, its frame is not read.
    let waiter = {
        let (r, src, s) = (r.clone(), src.clone(), s.clone());
        std::thread::spawn(move || prepare(&r, &s, &*src, Mode::Scrub { generation: 1 }).0)
    };
    std::thread::sleep(ms(10));
    r.supersede(2);
    assert!(matches!(waiter.join().unwrap().layers[0], LayerInput::Missing(MissingReason::NotReady)));
    std::thread::sleep(ms(400));
    let (inputs, perf) = prepare(&r, &s, &*src, Mode::Scrub { generation: 3 });
    assert_eq!(which(&inputs.layers[0]), (1, 100));
    assert_eq!(fake.opens(), 1, "the stream opened for the first request serves the second");
    assert!(perf.decode[0].time < ms(250), "open + one frame, not the idle time since: {:?}", perf.decode[0].time);
}
