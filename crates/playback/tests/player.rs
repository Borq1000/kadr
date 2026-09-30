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
    Ready(u64),
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
    fn ready(&mut self, generation: u64) {
        let _ = self.tx.send(Event::Ready(generation));
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
            Event::Ready(_) => panic!("no pre-roll: the clock was running when the play began"),
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

/// Presented shows, in arrival order, until `last` is presented or `timeout` passes.
fn presented_until(events: &Receiver<Event>, last: u64, timeout: Duration) -> Vec<(u64, Time, [u8; 4])> {
    let deadline = Instant::now() + timeout;
    let mut out = vec![];
    while Instant::now() < deadline {
        if let Some(Event::Presented { generation, time, pixel, .. }) = next_event(events, ms(50)) {
            out.push((generation, time, pixel));
            if generation == last {
                break;
            }
        }
    }
    out
}

fn presented_now(events: &Receiver<Event>) -> Vec<u64> {
    events.try_iter().filter_map(|e| if let Event::Presented { generation, .. } = e { Some(generation) } else { None }).collect()
}

#[test]
fn a_show_in_flight_is_finished_and_presented_before_the_newer_one() {
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
    assert!(r.player.accepts(g1), "a newer show does not make the one in flight stale");
    let shown = presented_until(&r.events, g2, ms(3000));
    assert_eq!(shown.last().map(|s| (s.0, s.1)), Some((g2, Time::from_secs(300))), "the newest show is presented last: {shown:?}");
    assert_eq!(read_fake_pixel(&shown.last().unwrap().2), (3, 7500));
    // The first show was in flight (picked up at once): it is finished, not thrown away.
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert_eq!((shown[0].0, shown[0].1), (g1, Time::from_secs(100)));
    assert_eq!(read_fake_pixel(&shown[0].2), (3, 2500));
    assert!(next_event(&r.events, ms(300)).is_none(), "nothing after the newest");
    let perf = r.perf.snapshot();
    assert!(perf.iter().all(|p| !p.dropped), "{perf:?}");
    // Seek latency is measured on the final frame only, from its own request.
    let seeks: Vec<_> = perf.iter().filter_map(|p| p.seek_latency).collect();
    assert_eq!(seeks.len(), 1, "{perf:?}");
    assert!(seeks[0] >= ms(150), "{seeks:?}");
}

#[test]
fn rapid_shows_present_in_order_ending_with_the_newest() {
    let fake = Arc::new(FakeDecoders::new(ms(30), ms(5)));
    fake.add("a.mp4", FakeMedia { tag: 3, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake.clone(), PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(600), one_clip(a, m.clone(), Time::ZERO)));
    let mut asked = vec![];
    for i in 0..40 {
        let t = Time::from_secs(i * 13 % 500);
        asked.push((r.player.show(t), t));
        std::thread::sleep(ms(2));
    }
    let last = asked.last().unwrap().0;
    // Presented before the burst ended may be any requests, in order; afterwards at most one stale frame.
    let during = presented_now(&r.events);
    let after = presented_until(&r.events, last, ms(3000));
    let all: Vec<u64> = during.iter().copied().chain(after.iter().map(|s| s.0)).collect();
    assert!(all.windows(2).all(|w| w[0] < w[1]), "presented in request order: {all:?}");
    assert_eq!(all.last(), Some(&last), "the last request is presented last");
    assert!(after.iter().filter(|s| s.0 != last).count() <= 1, "at most one stale frame after the burst: {after:?}");
    for (g, time, pixel) in &after {
        let t = asked.iter().find(|a| a.0 == *g).unwrap().1;
        assert_eq!(*time, t);
        assert_eq!(read_fake_pixel(pixel).1, FrameRate::FPS_25.time_to_frame(t), "each frame is the one its request asked for");
    }
    assert!(next_event(&r.events, ms(200)).is_none(), "nothing after the newest");
    assert!(fake.opens() < 20, "{} opens for 40 shows", fake.opens());
}

/// The playhead dragged continuously (a show every 10 ms for 1.2 s, each
/// seek costing ~45 ms): frames keep appearing during the drag.
#[test]
fn continuous_scrubbing_presents_frames_while_dragging() {
    let fake = Arc::new(FakeDecoders::new(ms(40), ms(5)));
    fake.add("a.mp4", FakeMedia { tag: 4, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(600), one_clip(a, m.clone(), Time::ZERO)));
    let started = Instant::now();
    let mut last = 0;
    let mut i = 0;
    // Far jumps every time: every show needs a fresh open.
    while started.elapsed() < ms(1200) {
        last = r.player.show(Time::from_secs(i * 37 % 590));
        i += 1;
        std::thread::sleep(ms(10));
    }
    let during = presented_now(&r.events);
    // ~45 ms per seek → ~25 frames with the machine to itself; demand a few under load.
    assert!(during.len() >= 4, "only {} frames presented during a 1.2 s drag", during.len());
    assert!(during.windows(2).all(|w| w[0] < w[1]), "{during:?}");
    let after = presented_until(&r.events, last, ms(3000));
    assert_eq!(after.last().map(|s| s.0), Some(last));
    assert!(after.len() <= 2, "{after:?}");
    let summary = r.perf.summary();
    assert!(summary.seek.count <= 2 && summary.seek.count >= 1, "seek latency only on final frames: {summary:?}");
}

#[test]
fn play_and_stop_cancel_a_show_in_flight() {
    let fake = Arc::new(FakeDecoders::new(ms(300), ms(5)));
    fake.add("a.mp4", FakeMedia { tag: 3, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(600), one_clip(a, m.clone(), Time::ZERO)));
    let g1 = r.player.show(Time::from_secs(100));
    std::thread::sleep(ms(20));
    let g2 = r.player.stop();
    assert!(!r.player.accepts(g1), "stop makes the show stale");
    assert!(r.player.accepts(g2) && g2 > g1);
    assert!(next_event(&r.events, ms(600)).is_none(), "a stopped show was presented");
    let perf = r.perf.snapshot();
    // Its wait was abandoned at once (resolver-level supersession), well before the 300 ms open.
    assert_eq!(perf.len(), 1, "{perf:?}");
    assert!(perf[0].dropped && perf[0].seek_latency.is_none(), "{perf:?}");
    assert!(perf[0].total < ms(250), "{perf:?}");
}

#[test]
fn preroll_renders_the_first_frame_before_the_clock_starts() {
    // A slow open (120 ms): the first frame is ready before the clock runs, then shown on time.
    let fake = Arc::new(FakeDecoders::new(ms(120), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 5, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(60), one_clip(a, m.clone(), Time::ZERO)));
    let from = Time::from_secs(7);
    let asked = Instant::now();
    let g = r.player.play(from);
    match next_event(&r.events, ms(3000)) {
        Some(Event::Ready(generation)) => assert_eq!(generation, g),
        other => panic!("expected ready before any frame, got {other:?}"),
    }
    let waited = asked.elapsed();
    assert!(waited >= ms(100), "ready only after the first frame was decoded ({waited:?})");
    assert!(next_event(&r.events, ms(100)).is_none(), "nothing is presented before the clock runs");
    r.clock.start(from);
    let clock_started = Instant::now();
    match next_event(&r.events, ms(1000)) {
        Some(Event::Presented { generation, time, pixel, playing, .. }) => {
            assert_eq!((generation, time, playing), (g, from, true));
            assert_eq!(read_fake_pixel(&pixel), (5, m.frame_at(from)));
            assert!(clock_started.elapsed() < ms(40), "the pre-rolled frame is shown as soon as the clock starts: {:?}", clock_started.elapsed());
        }
        other => panic!("expected the first frame, got {other:?}"),
    }
    let mut shown = 1;
    while shown < 10 {
        match next_event(&r.events, ms(1000)) {
            Some(Event::Presented { generation, .. }) => {
                assert_eq!(generation, g);
                shown += 1;
            }
            other => panic!("playback stalled after {shown} frames: {other:?}"),
        }
    }
    r.player.stop();
    r.clock.stop();
    let first = r.perf.snapshot()[0].clone();
    assert!(!first.dropped && first.decode.len() == 1, "the pre-rolled frame is the first one pushed: {first:?}");
}

#[test]
fn preroll_gives_up_after_its_timeout() {
    let fake = Arc::new(FakeDecoders::new(ms(600), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 5, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake, PlayerConfig { preroll: ms(100), ..Default::default() });
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(60), one_clip(a, m, Time::ZERO)));
    let asked = Instant::now();
    let g = r.player.play(Time::ZERO);
    match next_event(&r.events, ms(3000)) {
        Some(Event::Ready(generation)) => assert_eq!(generation, g),
        other => panic!("expected ready, got {other:?}"),
    }
    assert!(asked.elapsed() < ms(450), "ready after the pre-roll timeout, not after the open: {:?}", asked.elapsed());
    r.clock.start(Time::ZERO);
    // Frames follow once decoding catches up.
    match next_event(&r.events, ms(3000)) {
        Some(Event::Presented { generation, .. }) => assert_eq!(generation, g),
        other => panic!("expected a frame, got {other:?}"),
    }
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

/// Decoding at ~60 % of real time (a 66 ms frame at 25 fps): the picture
/// keeps moving at the rate decoding allows instead of freezing.
#[test]
fn decoding_slower_than_real_time_keeps_the_picture_moving() {
    let fake = Arc::new(FakeDecoders::new(ms(5), ms(66)));
    fake.add("a.mp4", FakeMedia { tag: 6, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    let duration = Time::from_secs(3);
    r.player.set_source(source(vec![(a, m.clone())], duration, one_clip(a, m.clone(), Time::ZERO)));
    r.clock.start(Time::ZERO);
    let started = Instant::now();
    let g = r.player.play(Time::ZERO);
    let fd = FrameRate::FPS_25.frame_duration();
    let mut shown = vec![];
    loop {
        match next_event(&r.events, ms(3000)).expect("playback finishes") {
            Event::Presented { generation, time, pixel, .. } => {
                assert_eq!(generation, g);
                let clock = Time::from_micros(started.elapsed().as_micros() as i64);
                // Presented within one frame of the clock (a few ms of channel delay allowed).
                assert!(clock + Time::from_millis(3) >= time && clock < time + fd + Time::from_millis(15), "{time:?} shown at {clock:?}");
                shown.push((time, read_fake_pixel(&pixel).1));
            }
            Event::Finished(_) => break,
            Event::Ready(_) => panic!("no pre-roll with the clock running"),
        }
    }
    let total = 75;
    eprintln!("slow decoding: {} of {total} frames presented", shown.len());
    assert!(shown.len() * 100 >= total * 40, "{} of {total} frames presented", shown.len());
    assert!(shown.windows(2).all(|w| w[0].0 < w[1].0), "in order");
    assert!(shown.windows(2).all(|w| w[0].1 < w[1].1), "every presented frame shows newer content: {shown:?}");
    for (time, frame) in &shown {
        let due = m.frame_at(*time);
        assert!(*frame <= due && *frame >= due - 25, "frame {frame} at {time:?} (due {due}): at most 1 s behind");
    }
    let perf = r.perf.snapshot();
    assert_eq!(perf.iter().filter(|p| !p.dropped).count(), shown.len());
}

#[test]
fn a_show_stuck_in_a_slow_open_is_abandoned_for_a_newer_one() {
    let fake = Arc::new(FakeDecoders::new(ms(3000), ms(5)));
    fake.add("a.mp4", FakeMedia { tag: 7, ..Default::default() });
    fake.add("b.mp4", FakeMedia { tag: 8, ..Default::default() });
    let (a, b) = (AssetId::new(), AssetId::new());
    let ma = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let mb = video_source("b.mp4", FrameRate::FPS_25, Time::from_secs(600), CANVAS);
    let r = rig(fake.clone(), PlayerConfig::default());
    let (ma2, mb2) = (ma.clone(), mb.clone());
    // a before 50 s, b after: the second show needs another media.
    let scene: SceneFn = Box::new(move |t, out| {
        let layer = if t < Time::from_secs(50) { media_layer(1, a, &ma2, t, CANVAS) } else { media_layer(2, b, &mb2, t, CANVAS) };
        FrameScene { layers: vec![layer], ..FrameScene::empty(t, CANVAS, *out) }
    });
    r.player.set_source(source(vec![(a, ma), (b, mb)], Time::from_secs(600), scene));
    let g1 = r.player.show(Time::from_secs(10));
    std::thread::sleep(ms(50));
    fake.set_delays(ms(5), ms(5));
    let asked = Instant::now();
    let g2 = r.player.show(Time::from_secs(100));
    match next_event(&r.events, ms(2000)) {
        Some(Event::Presented { generation, pixel, .. }) => {
            assert_eq!(generation, g2, "the stuck show is not presented first");
            assert_eq!(read_fake_pixel(&pixel), (8, 2500));
        }
        other => panic!("expected the newer show, got {other:?}"),
    }
    assert!(asked.elapsed() < ms(1000), "the newer show waited for the stuck open: {:?}", asked.elapsed());
    assert!(r.perf.snapshot().iter().any(|p| p.dropped), "the stuck show counts as dropped");
    assert!(g2 > g1);
}

#[test]
fn a_prerolled_frame_is_rendered_again_after_a_source_change() {
    let fake = Arc::new(FakeDecoders::new(ms(5), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 9, ..Default::default() });
    fake.add("b.mp4", FakeMedia { tag: 10, ..Default::default() });
    let (a, b) = (AssetId::new(), AssetId::new());
    let ma = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let mb = video_source("b.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, ma.clone())], Time::from_secs(60), one_clip(a, ma, Time::ZERO)));
    let g = r.player.play(Time::ZERO);
    assert!(matches!(next_event(&r.events, ms(2000)), Some(Event::Ready(x)) if x == g));
    // An edit before the clock starts: the pre-rolled picture is out of date.
    r.player.set_source(source(vec![(b, mb.clone())], Time::from_secs(60), one_clip(b, mb, Time::ZERO)));
    std::thread::sleep(ms(30));
    r.clock.start(Time::ZERO);
    match next_event(&r.events, ms(2000)) {
        Some(Event::Presented { generation, pixel, .. }) => {
            assert_eq!(generation, g);
            assert_eq!(read_fake_pixel(&pixel).0, 10, "the new source's picture");
        }
        other => panic!("expected a frame, got {other:?}"),
    }
}

#[test]
fn a_play_uses_a_source_queued_right_after_it() {
    let fake = Arc::new(FakeDecoders::new(ms(5), ms(1)));
    fake.add("a.mp4", FakeMedia { tag: 11, ..Default::default() });
    let a = AssetId::new();
    let m = video_source("a.mp4", FrameRate::FPS_25, Time::from_secs(60), CANVAS);
    let r = rig(fake, PlayerConfig::default());
    r.player.set_source(source(vec![(a, m.clone())], Time::from_secs(60), one_clip(a, m.clone(), Time::ZERO)));
    r.clock.start(Time::ZERO);
    let g = r.player.play(Time::ZERO);
    r.player.set_source(source(vec![(a, m.clone())], Time::from_millis(200), one_clip(a, m, Time::ZERO)));
    let deadline = Instant::now() + ms(2000);
    loop {
        assert!(Instant::now() < deadline, "the play kept the 60 s source");
        if let Some(Event::Finished(x)) = next_event(&r.events, ms(100)) {
            assert_eq!(x, g);
            break;
        }
    }
}
