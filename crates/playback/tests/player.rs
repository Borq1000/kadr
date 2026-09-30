//! PreviewPlayer on real threads: fake decoder, CpuRenderer, wall clock.

use crossbeam_channel::{Receiver, Sender};
use kadr_core::perf::PerfRing;
use kadr_core::{AssetId, FrameRate, Time};
use kadr_playback::testing::{media_layer, read_fake_pixel, video_source, FakeDecoders, FakeMedia, FnSceneSource};
use kadr_playback::{FrameInfo, FrameSink, MediaSource, PlayerConfig, PreviewPlayer, Resolver, ResolverConfig, SceneSource, WallClock};
use kadr_render::{CpuRenderer, CpuTarget};
use kadr_scene::{FrameScene, OutputSpec, RenderQuality, SizeU};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const CANVAS: SizeU = SizeU::new(64, 36);

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

#[derive(Debug)]
enum Event {
    Presented { generation: u64, time: Time, pixel: [u8; 4], playing: bool, cache_hits: u32 },
    Finished(u64),
}

struct Sink {
    buf: Vec<u8>,
    tx: Sender<Event>,
}

impl FrameSink for Sink {
    fn target(&mut self, size: SizeU) -> Option<CpuTarget<'_>> {
        self.buf.resize(size.w as usize * size.h as usize * 4, 0);
        Some(CpuTarget::packed(size.w, size.h, &mut self.buf))
    }
    fn present(&mut self, info: &mut FrameInfo) {
        let pixel = [self.buf[0], self.buf[1], self.buf[2], self.buf[3]];
        let _ = self.tx.send(Event::Presented { generation: info.generation, time: info.time, pixel, playing: info.playing, cache_hits: info.perf.cache_hits });
    }
    fn finished(&mut self, generation: u64, _at: Time) {
        let _ = self.tx.send(Event::Finished(generation));
    }
}

struct Rig {
    player: PreviewPlayer,
    events: Receiver<Event>,
    clock: Arc<WallClock>,
    perf: Arc<PerfRing>,
}

fn output() -> OutputSpec {
    OutputSpec::new(CANVAS, RenderQuality::PreviewHigh)
}

/// A player over a source of `duration` at 25 fps whose scene is built by `layers`.
fn rig(fake: Arc<FakeDecoders>, config: PlayerConfig) -> Rig {
    let (tx, events) = crossbeam_channel::unbounded();
    let resolver = Arc::new(Resolver::new(fake, ResolverConfig::default()));
    let clock = Arc::new(WallClock::new());
    let perf = Arc::new(PerfRing::new(1000));
    let player = PreviewPlayer::new(resolver, Box::new(CpuRenderer::new()), clock.clone(), Box::new(Sink { buf: vec![], tx }), output(), perf.clone(), config);
    Rig { player, events, clock, perf }
}

type SceneFn = Box<dyn Fn(Time, &OutputSpec) -> FrameScene + Send + Sync>;

fn source(media: Vec<(AssetId, MediaSource)>, duration: Time, scene: SceneFn) -> Arc<dyn SceneSource> {
    Arc::new(FnSceneSource { media: media.into_iter().collect::<HashMap<_, _>>(), rate: FrameRate::FPS_25, duration, canvas: CANVAS, scene })
}

/// One clip of `m` (asset `a`) whose source time is the timeline time + `offset`.
fn one_clip(a: AssetId, m: MediaSource, offset: Time) -> SceneFn {
    Box::new(move |t, out| FrameScene { layers: vec![media_layer(1, a, &m, t + offset, CANVAS)], ..FrameScene::empty(t, CANVAS, *out) })
}

fn next_event(events: &Receiver<Event>, timeout: Duration) -> Option<Event> {
    events.recv_timeout(timeout).ok()
}

#[test]
fn plays_the_exact_frames_on_time_and_records_telemetry() {
    let fake = Arc::new(FakeDecoders::new(ms(5), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake.clone(), PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(2), one_clip(a, m.clone(), Time::from_secs(10))));
    r.clock.start(Time::ZERO);
    let started = Instant::now();
    let g = r.player.play(Time::ZERO);
    let mut shown = vec![];
    loop {
        match next_event(&r.events, ms(3000)).expect("playback finishes") {
            Event::Presented { generation, time, pixel, playing, .. } => {
                assert_eq!(generation, g);
                assert!(playing);
                // What the clock said when it was shown is within one frame of its time.
                let clock = Time::from_micros(started.elapsed().as_micros() as i64);
                assert!(clock + Time::from_millis(40) >= time, "shown too early: {time:?} at {clock:?}");
                shown.push((time, read_fake_pixel(&pixel)));
            }
            Event::Finished(generation) => {
                assert_eq!(generation, g);
                break;
            }
        }
    }
    // Real time on a shared machine: most frames, not all (the simulated-clock test is exact).
    assert!(shown.len() >= 30, "{} of 50 frames shown", shown.len());
    for w in shown.windows(2) {
        assert!(w[0].0 < w[1].0, "frames in order");
    }
    for (time, (tag, frame)) in &shown {
        assert_eq!((*tag, *frame), (1, m.frame_at(*time + Time::from_secs(10))), "exact source frame at {time:?}");
    }
    let perf = r.perf.snapshot();
    assert_eq!(perf.len(), 50, "every frame is pushed, shown or dropped");
    assert_eq!(perf.iter().filter(|p| !p.dropped).count(), shown.len());
    for p in perf.iter().filter(|p| !p.dropped) {
        assert_eq!(p.decode.len(), 1);
        assert_eq!(p.cache_hits + p.cache_misses, 1, "one media layer");
        assert!(p.total >= p.present && p.total > Duration::ZERO);
        assert_eq!(p.seek_latency, None, "no seek latency during playback");
    }
    assert_eq!(fake.opens(), 1, "one stream read forward; prefetch does not move the layer's session");
}

/// Random show/play/stop/source changes from another thread, with slow and
/// failing media: no deadlock, and the final show presents the right frame.
#[test]
fn random_requests_never_deadlock_and_the_last_one_wins() {
    let fake = Arc::new(FakeDecoders::new(ms(8), ms(2)));
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("b.mp4", FakeMedia { tag: 2, frames: Some(300), fail: false });
    fake.add("bad.mp4", FakeMedia { tag: 3, frames: None, fail: true });
    let ids = [AssetId::new(), AssetId::new(), AssetId::new()];
    let medias: Vec<MediaSource> = ["a.mp4", "b.mp4", "bad.mp4"].iter().map(|p| video_source(p, FrameRate::FPS_25, Time::from_secs(60), CANVAS)).collect();
    let r = rig(fake, PlayerConfig::default());
    let table: Vec<(AssetId, MediaSource)> = ids.iter().copied().zip(medias.iter().cloned()).collect();
    let (i0, i1, i2) = (ids[0], ids[1], ids[2]);
    let (m0, m1, m2) = (medias[0].clone(), medias[1].clone(), medias[2].clone());
    // Three layers, bottom to top: a failing one, a, and b 5 s later (ends early: frame 299 at most).
    let scene: SceneFn = Box::new(move |t, out| FrameScene {
        layers: vec![media_layer(3, i2, &m2, t, CANVAS), media_layer(1, i0, &m0, t, CANVAS), media_layer(2, i1, &m1, t + Time::from_secs(5), CANVAS)],
        ..FrameScene::empty(t, CANVAS, *out)
    });
    let src = source(table, Time::from_secs(60), scene);
    r.player.set_source(src.clone());
    let mut rng = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for _ in 0..300 {
        let t = Time::from_millis((next() % 50_000) as i64);
        match next() % 6 {
            0..=2 => {
                r.player.show(t);
            }
            3 => {
                r.clock.start(t);
                r.player.play(t);
            }
            4 => {
                r.player.stop();
            }
            _ => r.player.set_source(src.clone()),
        }
        std::thread::sleep(Duration::from_micros(next() % 4000));
    }
    let _ = r.events.try_iter().count();
    let g = r.player.show(Time::from_secs(12));
    let deadline = Instant::now() + ms(5000);
    loop {
        assert!(Instant::now() < deadline, "the last show was never presented");
        if let Some(Event::Presented { generation, pixel, .. }) = next_event(&r.events, ms(100)) {
            if generation == g {
                // The top layer, b at 17 s = frame 425, past its real end: its last frame.
                assert_eq!(read_fake_pixel(&pixel), (2, 299));
                break;
            }
            assert!(generation < g);
        }
    }
    assert!(r.perf.snapshot().iter().filter(|p| p.seek_latency.is_some()).all(|p| !p.dropped));
}

#[test]
fn a_superseded_show_never_presents_its_frame() {
    let fake = Arc::new(FakeDecoders::new(ms(150), ms(5)));
    fake.add("a.mp4", FakeMedia { tag: 3, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(600), one_clip(a, m.clone(), Time::ZERO)));
    let g1 = r.player.show(Time::from_secs(100));
    std::thread::sleep(ms(20));
    let g2 = r.player.show(Time::from_secs(300));
    assert!(g2 > g1);
    match next_event(&r.events, ms(3000)) {
        Some(Event::Presented { generation, time, pixel, playing, .. }) => {
            assert_eq!((generation, time, playing), (g2, Time::from_secs(300), false));
            assert_eq!(read_fake_pixel(&pixel), (3, 7500));
        }
        other => panic!("expected the second show, got {other:?}"),
    }
    assert!(next_event(&r.events, ms(300)).is_none(), "nothing else, the first show least of all");
    let perf = r.perf.snapshot();
    // The first show is dropped (or, if the second arrived before it started, never attempted).
    let (shown, dropped): (Vec<_>, Vec<_>) = perf.iter().partition(|p| !p.dropped);
    assert!(dropped.len() <= 1 && dropped.iter().all(|p| p.seek_latency.is_none()), "dropped frames carry no seek latency: {dropped:?}");
    assert_eq!(shown.len(), 1);
    assert!(shown[0].seek_latency.is_some_and(|s| s >= ms(150)), "{:?}", shown[0].seek_latency);
    assert_eq!(r.perf.summary().seek.count, 1);
}

#[test]
fn rapid_shows_present_only_the_newest() {
    let fake = Arc::new(FakeDecoders::new(ms(30), ms(5)));
    fake.add("a.mp4", FakeMedia { tag: 3, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake.clone(), PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(600), one_clip(a, m.clone(), Time::ZERO)));
    let mut last = 0;
    for i in 0..40 {
        last = r.player.show(Time::from_secs(i * 13 % 500));
        std::thread::sleep(ms(2));
    }
    let deadline = Instant::now() + ms(3000);
    let mut final_seen = false;
    while Instant::now() < deadline && !final_seen {
        if let Some(Event::Presented { generation, pixel, .. }) = next_event(&r.events, ms(100)) {
            // Anything presented was current when presented; only the last can be current now.
            assert_eq!(generation, last, "a stale show was presented");
            assert_eq!(read_fake_pixel(&pixel).1, FrameRate::FPS_25.time_to_frame(Time::from_secs(39 * 13 % 500)));
            final_seen = true;
        }
    }
    assert!(final_seen);
    assert!(fake.opens() < 15, "{} opens for 40 shows", fake.opens());
}

#[test]
fn stop_means_no_more_frames_of_that_play() {
    let fake = Arc::new(FakeDecoders::new(ms(5), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(60), one_clip(a, m, Time::ZERO)));
    r.clock.start(Time::ZERO);
    let g = r.player.play(Time::ZERO);
    for _ in 0..3 {
        assert!(next_event(&r.events, ms(3000)).is_some(), "it plays");
    }
    r.player.stop();
    r.clock.stop();
    let _ = r.events.try_iter().count();
    std::thread::sleep(ms(200));
    let after: Vec<Event> = r.events.try_iter().collect();
    assert!(after.iter().all(|e| !matches!(e, Event::Presented { generation, .. } if *generation == g)), "presented after stop: {after:?}");
}

#[test]
fn failing_and_offline_media_still_present_a_frame() {
    let fake = Arc::new(FakeDecoders::new(ms(1), ms(1)));
    fake.add("bad.mp4", FakeMedia { tag: 1, frames: None, fail: true });
    let (bad, gone) = (AssetId::new(), AssetId::new());
    let mb = video_source("bad.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let mg = MediaSource { online: false, ..video_source("gone.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS) };
    let r = rig(fake, PlayerConfig::default());
    // Media's MISSING colour, opaque: round(0.45·255), round(0.05·255), round(0.08·255).
    let missing = [115, 13, 20, 255];
    for (a, m) in [(bad, mb), (gone, mg)] {
        r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(60), one_clip(a, m, Time::ZERO)));
        let g = r.player.show(Time::from_secs(1));
        match next_event(&r.events, ms(2000)) {
            Some(Event::Presented { generation, pixel, .. }) => {
                assert_eq!(generation, g);
                assert_eq!(pixel, missing);
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }
    // Playback with a failing decoder presents frames too (not dropped as late).
    r.player.set_source(source(vec![(bad, video_source("bad.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS))], Time::from_millis(400), {
        let m = video_source("bad.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
        one_clip(bad, m, Time::ZERO)
    }));
    r.clock.start(Time::ZERO);
    let g = r.player.play(Time::ZERO);
    let mut presented = 0;
    while let Some(e) = next_event(&r.events, ms(2000)) {
        match e {
            Event::Presented { generation, pixel, .. } if generation == g => {
                assert_eq!(pixel, missing);
                presented += 1;
            }
            Event::Finished(_) => break,
            _ => {}
        }
    }
    assert!(presented >= 7, "{presented} of 10");
}

#[test]
fn prefetch_opens_the_next_clip_before_the_cut() {
    // Clip B takes 200 ms to open; the lookahead (750 ms) hides it.
    let fake = Arc::new(FakeDecoders::new(ms(5), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 1, ..Default::default() });
    fake.add("b.mp4", FakeMedia { tag: 2, ..Default::default() });
    let (a, b) = (AssetId::new(), AssetId::new());
    let ma = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let mb = video_source("b.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake.clone(), PlayerConfig::default());
    let (ma2, mb2) = (ma.clone(), mb.clone());
    let cut = Time::from_secs(1);
    let scene: SceneFn = Box::new(move |t, out| {
        let layer = if t < cut { media_layer(1, a, &ma2, t, CANVAS) } else { media_layer(2, b, &mb2, t - cut, CANVAS) };
        FrameScene { layers: vec![layer], ..FrameScene::empty(t, CANVAS, *out) }
    });
    r.player.set_source(source(vec![(a, ma), (b, mb)], Time::from_millis(1600), scene));
    std::thread::sleep(ms(50));
    fake.set_delays(ms(200), ms(1)); // a's session is not open yet either: it gets the slow open too, before playback starts
    r.player.show(Time::ZERO);
    let _ = next_event(&r.events, ms(2000));
    r.clock.start(Time::ZERO);
    let g = r.player.play(Time::ZERO);
    let mut after_cut = vec![];
    while let Some(e) = next_event(&r.events, ms(3000)) {
        match e {
            Event::Presented { generation, time, pixel, cache_hits, .. } if generation == g && time >= cut => after_cut.push((time, read_fake_pixel(&pixel), cache_hits)),
            Event::Finished(_) => break,
            _ => {}
        }
    }
    // Without prefetch the first frame after the cut comes ~200 ms (5 frames) late.
    let (time, (tag, frame), hits) = after_cut[0];
    assert!(time <= cut + FrameRate::FPS_25.frame_to_time(2), "the next clip starts on time, not after its open: {time:?}");
    assert_eq!((tag, frame), (2, FrameRate::FPS_25.time_to_frame(time - cut)));
    assert_eq!(hits, 1, "its frame was decoded before it was needed");
}
