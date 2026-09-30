//! `PreviewPlayer`: a thread that turns show/play/stop requests into
//! rendered, paced frames (render spec §8, §10).
//!
//! Every request increments the player's generation. The thread coalesces
//! queued requests (the newest wins), resolves the scene (paused: `Scrub`;
//! playing: `Deadline`), renders into a buffer the [`FrameSink`] lends it
//! and presents it if it may still be shown — checked under the same lock
//! that [`PreviewPlayer::show`]/`play`/`stop` take to increment the
//! generation. Every presented or dropped frame pushes a [`FramePerf`] into
//! the shared [`PerfRing`].
//!
//! # Scrubbing
//!
//! At most one show is in flight. A newer `show` does not cancel it at
//! once: the frame being prepared is finished and presented, then the
//! newest pending show is next (the ones in between are skipped, never
//! attempted). So a continuous drag of the playhead shows frames at seek
//! speed instead of nothing until the pointer stops, frames are presented
//! in request order, and after a burst of shows at most one frame older
//! than the last request is presented. A show still waiting for decoding
//! [`PlayerConfig::show_patience`] after it began while a newer show is
//! pending is abandoned (a slow open must not hold the newest request
//! back). `play`, `stop` (and dropping the player) cancel an in-flight show
//! at once, at the resolver too: no frame of an older generation is
//! presented once one of them returned. `set_source`/`set_output` do not:
//! live inspector drags replace the source on every step and must keep
//! frames coming just like scrubbing. `seek_latency` is recorded on frames
//! that were the newest request when presented (the final frame of a
//! scrub), measured from that request.
//!
//! # Playback slower than real time
//!
//! A media layer not decoded by its frame's deadline shows the newest frame
//! its decoder already produced (at most [`PlayerConfig::max_behind`]
//! earlier), so the picture keeps moving at whatever rate decoding allows
//! instead of freezing. A frame is presented only if some layer shows newer
//! content than the frame before it; otherwise it is dropped (the picture
//! on screen is the same). Presented frames stay within one frame of the
//! clock; only their content lags.
//!
//! # Pre-roll
//!
//! When `play` starts while the [`Clock`] is not running (`now()` is
//! `None`), the player prepares and renders the play's first frame first,
//! waiting for decoding up to [`PlayerConfig::preroll`], then calls
//! [`FrameSink::ready`]: that is the moment to start the clock (the audio).
//! The pre-rolled frame is presented when the clock reaches it — unless the
//! source or output changed meanwhile, then it is rendered again. The app
//! starts the clock on `ready` or after its own timeout, whichever comes
//! first, so a slow open never holds playback for more than that.

use crate::cache::FrameKey;
use crate::pace::{duration_of, Clock, Pace, PlaySchedule, Step};
use crate::resolver::{decode_size, Mode, Resolver};
use crate::source::SceneSource;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use kadr_core::perf::{FramePerf, PerfRing};
use kadr_core::{AssetId, Time};
use kadr_render::{CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{FrameScene, Layer, LayerContent, LayerId, OutputSpec, SizeU};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What a presented frame is.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    /// The request it answers (check with [`PreviewPlayer::accepts`]).
    pub generation: u64,
    /// Timeline time shown.
    pub time: Time,
    pub playing: bool,
    /// The newest request when it was presented (a paused frame that is not
    /// final is an intermediate scrub frame: a newer show follows).
    pub latest: bool,
    /// Telemetry so far (evaluate, resolve, decode, composite, cache,
    /// allocations). The sink may add the allocations and copies it makes
    /// itself; the player then adds `present`, `total` and `seek_latency`
    /// and pushes it into the ring.
    pub perf: FramePerf,
}

/// The display end of the player. All methods run on the player thread.
pub trait FrameSink: Send {
    /// A premultiplied RGBA8 buffer of exactly `size` pixels to render the
    /// next frame into — not the one currently on screen (the next frame is
    /// rendered while the current one is shown). `None` skips the frame.
    fn target(&mut self, size: SizeU) -> Option<CpuTarget<'_>>;
    /// The buffer last handed out by [`FrameSink::target`] holds a finished
    /// frame: show it. Called with the generation lock held: hand the frame
    /// to the UI thread without waiting for it (post, never block on the UI
    /// thread — it may be inside `show()` waiting for this lock).
    fn present(&mut self, info: &mut FrameInfo);
    /// Playback of `generation` reached the end of the source at `at`.
    fn finished(&mut self, _generation: u64, _at: Time) {}
    /// A play of `generation` that began with the clock stopped has its
    /// first frame rendered (or gave up waiting after
    /// [`PlayerConfig::preroll`]): start the clock now. At most once per
    /// play; never for a play that began with the clock running. Must not
    /// block (post to the UI thread).
    fn ready(&mut self, _generation: u64) {}
}

#[derive(Clone, Debug)]
pub struct PlayerConfig {
    /// During playback the scene this far ahead is prefetched, so the next
    /// clip's decoder is open before the cut. `Time::ZERO` disables it.
    pub lookahead: Time,
    /// The lookahead scene is prefetched when its media change, and at
    /// least every this many frames (to keep its sessions warm).
    pub prefetch_every: u32,
    /// How often to look at a clock that is not running yet.
    pub clock_poll: Duration,
    /// Longest wait for the first frame of a play started with the clock
    /// stopped before [`FrameSink::ready`] is called anyway.
    pub preroll: Duration,
    /// A show still waiting this long while a newer show is pending is abandoned.
    pub show_patience: Duration,
    /// Playback: a layer not decoded in time may show a frame of its media
    /// at most this much older (source time) instead.
    pub max_behind: Time,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        PlayerConfig {
            lookahead: Time::from_millis(750),
            prefetch_every: 12,
            clock_poll: Duration::from_millis(5),
            preroll: Duration::from_millis(250),
            show_patience: Duration::from_millis(200),
            max_behind: Time::from_secs(1),
        }
    }
}

enum Cmd {
    Source(Arc<dyn SceneSource>),
    Output(OutputSpec),
    Show { generation: u64, t: Time, asked: Instant },
    Play { generation: u64, from: Time },
    Stop,
    Quit,
}

impl Cmd {
    /// Ends the current job (a play is replaced by anything, a show only by these).
    fn supersedes(&self) -> bool {
        matches!(self, Cmd::Show { .. } | Cmd::Play { .. } | Cmd::Stop | Cmd::Quit)
    }

    /// Cancels an in-flight show.
    fn cancels(&self) -> bool {
        matches!(self, Cmd::Play { .. } | Cmd::Stop | Cmd::Quit)
    }
}

/// The generation counter; incremented and checked-then-presented under one lock.
struct Generation {
    lock: Mutex<()>,
    value: AtomicU64,
    /// The newest play/stop/quit: frames of older generations are never presented.
    barrier: AtomicU64,
}

impl Generation {
    fn current(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    fn barrier(&self) -> u64 {
        self.barrier.load(Ordering::Acquire)
    }
}

/// The show being prepared, watched so a slow one can be abandoned for a newer one.
#[derive(Default)]
struct WatchState {
    in_flight: Option<(u64, Instant)>,
    quit: bool,
}

#[derive(Default)]
struct ShowWatch {
    state: Mutex<WatchState>,
    cv: Condvar,
}

impl ShowWatch {
    fn set(&self, v: Option<(u64, Instant)>) {
        self.state.lock().in_flight = v;
        self.cv.notify_all();
    }

    /// Thread body: abandons (at the resolver) a show that has waited
    /// `patience` while a newer request is pending.
    fn run(&self, generation: &Generation, resolver: &Resolver, patience: Duration) {
        let mut st = self.state.lock();
        loop {
            if st.quit {
                return;
            }
            match st.in_flight {
                Some((g, started)) if generation.current() > g => {
                    let due = started + patience;
                    if Instant::now() >= due {
                        resolver.supersede(generation.current());
                        st.in_flight = None;
                    } else {
                        self.cv.wait_until(&mut st, due);
                    }
                }
                _ => {
                    self.cv.wait(&mut st);
                }
            }
        }
    }
}

pub struct PreviewPlayer {
    tx: Sender<Cmd>,
    generation: Arc<Generation>,
    resolver: Arc<Resolver>,
    perf: Arc<PerfRing>,
    watch: Arc<ShowWatch>,
    thread: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
}

impl PreviewPlayer {
    /// Starts the player thread. `resolver` should serve this player only
    /// (the player drives its generations). Nothing is shown until
    /// [`PreviewPlayer::set_source`] and a request.
    pub fn new(
        resolver: Arc<Resolver>,
        renderer: Box<dyn Renderer + Send>,
        clock: Arc<dyn Clock>,
        sink: Box<dyn FrameSink>,
        output: OutputSpec,
        perf: Arc<PerfRing>,
        config: PlayerConfig,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let g = resolver.generation();
        let generation = Arc::new(Generation { lock: Mutex::new(()), value: AtomicU64::new(g), barrier: AtomicU64::new(g) });
        let watch = Arc::new(ShowWatch::default());
        let watcher = {
            let (w, g, r, p) = (watch.clone(), generation.clone(), resolver.clone(), config.show_patience);
            std::thread::Builder::new().name("kadr-player-watch".into()).spawn(move || w.run(&g, &r, p)).expect("spawn player watch thread")
        };
        let worker = Worker {
            rx,
            generation: generation.clone(),
            resolver: resolver.clone(),
            renderer,
            clock,
            sink,
            perf: perf.clone(),
            config,
            source: None,
            output,
            stash: VecDeque::new(),
            render_estimate: Duration::from_millis(5),
            watch: watch.clone(),
        };
        let thread = std::thread::Builder::new().name("kadr-player".into()).spawn(move || worker.run()).expect("spawn player thread");
        PreviewPlayer { tx, generation, resolver, perf, watch, thread: Some(thread), watcher: Some(watcher) }
    }

    /// Scenes and media from now on (an edit replaces the snapshot). Does
    /// not re-render by itself: follow with `show` when paused. A show
    /// already in flight finishes with the previous source.
    pub fn set_source(&self, source: Arc<dyn SceneSource>) {
        let _ = self.tx.send(Cmd::Source(source));
    }

    /// Output size and quality from now on (the sink's buffers must match).
    pub fn set_output(&self, output: OutputSpec) {
        let _ = self.tx.send(Cmd::Output(output));
    }

    /// Shows the frame at timeline time `t` (paused). Returns its generation.
    /// A show in flight is finished first (see the module docs).
    pub fn show(&self, t: Time) -> u64 {
        let generation = self.bump(false);
        let _ = self.tx.send(Cmd::Show { generation, t, asked: Instant::now() });
        // The watch re-checks the show in flight against the newer request.
        // Notified under its lock: between its check of the generation and
        // its wait, a notify without the lock would be lost (the stuck show
        // would then hold this request back until the next one).
        let _st = self.watch.state.lock();
        self.watch.cv.notify_all();
        generation
    }

    /// Plays from `from`, paced against the clock. Start the clock at `from`
    /// either before (no pre-roll) or on [`FrameSink::ready`] (pre-roll).
    /// Returns the play's generation.
    pub fn play(&self, from: Time) -> u64 {
        let generation = self.bump(true);
        let _ = self.tx.send(Cmd::Play { generation, from });
        generation
    }

    /// Stops playback (or a pending show); the last presented frame stays.
    pub fn stop(&self) -> u64 {
        let generation = self.bump(true);
        let _ = self.tx.send(Cmd::Stop);
        generation
    }

    /// The newest generation (the last request).
    pub fn generation(&self) -> u64 {
        self.generation.current()
    }

    /// Whether a frame of `generation` handed to the sink may still be
    /// shown: false once a newer play or stop was requested. The UI checks
    /// this when the frame arrives (a frame posted just before `play`/`stop`
    /// may reach it after). A frame that is not of the newest generation
    /// is an intermediate scrub frame; it is still in order.
    pub fn accepts(&self, generation: u64) -> bool {
        generation >= self.generation.barrier() && generation <= self.generation.current()
    }

    pub fn perf(&self) -> &Arc<PerfRing> {
        &self.perf
    }

    pub fn resolver(&self) -> &Arc<Resolver> {
        &self.resolver
    }

    fn bump(&self, cancel: bool) -> u64 {
        let g = {
            let _l = self.generation.lock.lock();
            let g = self.generation.value.fetch_add(1, Ordering::AcqRel) + 1;
            if cancel {
                self.generation.barrier.store(g, Ordering::Release);
            }
            g
        };
        if cancel {
            // Wakes a paused-frame wait and stops session work of older generations.
            self.resolver.supersede(g);
        }
        g
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        self.bump(true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.watch.state.lock().quit = true;
        self.watch.cv.notify_all();
        if let Some(t) = self.watcher.take() {
            let _ = t.join();
        }
    }
}

/// The first frame of a play started with the clock stopped.
enum Preroll {
    /// Not attempted yet (the clock was not running when the play began).
    Pending,
    /// Rendered into the sink's target, to be presented when the clock gets
    /// there — with the source and output it was rendered for.
    Ready(Box<Prerolled>),
    /// Done, or not needed.
    Done,
}

struct Prerolled {
    perf: FramePerf,
    t0: Instant,
    source: Arc<dyn SceneSource>,
    output: OutputSpec,
    shows: Vec<(LayerId, i64)>,
}

/// A play in progress.
struct Play {
    from: Time,
    /// Built at the first step, from the source current then (a `Source`
    /// queued right after the `Play` counts).
    sched: Option<PlaySchedule>,
    preroll: Preroll,
    /// Per media layer, the source frame last presented.
    shown: HashMap<LayerId, i64>,
    /// The media (and decode sizes) of the last prefetched lookahead scene,
    /// and frames since that prefetch.
    prefetched: Vec<(AssetId, SizeU)>,
    since_prefetch: u32,
}

enum Job {
    Idle,
    Show { generation: u64, t: Time, asked: Instant },
    Play { generation: u64, play: Box<Play> },
}

struct Worker {
    rx: Receiver<Cmd>,
    generation: Arc<Generation>,
    resolver: Arc<Resolver>,
    renderer: Box<dyn Renderer + Send>,
    clock: Arc<dyn Clock>,
    sink: Box<dyn FrameSink>,
    perf: Arc<PerfRing>,
    config: PlayerConfig,
    source: Option<Arc<dyn SceneSource>>,
    output: OutputSpec,
    /// Commands received while waiting inside a job.
    stash: VecDeque<Cmd>,
    /// Recent render time, kept free before a playback frame's deadline.
    render_estimate: Duration,
    watch: Arc<ShowWatch>,
}

impl Worker {
    fn run(mut self) {
        let mut job = Job::Idle;
        loop {
            let first = match self.stash.pop_front() {
                Some(c) => Some(c),
                None if matches!(job, Job::Idle) => match self.rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                },
                None => self.rx.try_recv().ok(),
            };
            if let Some(c) = first {
                if !self.apply(c, &mut job) {
                    return;
                }
                while let Some(c) = self.stash.pop_front().or_else(|| self.rx.try_recv().ok()) {
                    if !self.apply(c, &mut job) {
                        return;
                    }
                }
            }
            job = match job {
                Job::Idle => Job::Idle,
                Job::Show { generation, t, asked } => {
                    self.watch.set(Some((generation, Instant::now())));
                    self.show(generation, t, asked);
                    self.watch.set(None);
                    Job::Idle
                }
                Job::Play { generation, mut play } => {
                    if self.play_step(generation, &mut play) {
                        Job::Play { generation, play }
                    } else {
                        Job::Idle
                    }
                }
            };
        }
    }

    /// False on quit.
    fn apply(&mut self, c: Cmd, job: &mut Job) -> bool {
        match c {
            Cmd::Source(s) => self.source = Some(s),
            Cmd::Output(o) => self.output = o,
            Cmd::Show { generation, t, asked } => *job = Job::Show { generation, t, asked },
            Cmd::Play { generation, from } => {
                let preroll = if self.clock.now().is_none() { Preroll::Pending } else { Preroll::Done };
                let play = Play { from, sched: None, preroll, shown: HashMap::new(), prefetched: vec![], since_prefetch: 0 };
                *job = Job::Play { generation, play: Box::new(play) };
            }
            Cmd::Stop => *job = Job::Idle,
            Cmd::Quit => return false,
        }
        true
    }

    /// Waits up to `d` for a command (kept for the main loop).
    fn wait(&mut self, d: Duration) {
        match self.rx.recv_timeout(d.max(Duration::from_micros(100))) {
            Ok(c) => self.stash.push_back(c),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => self.stash.push_back(Cmd::Quit),
        }
    }

    /// A play frame of `generation` is not wanted any more.
    fn superseded(&self, generation: u64) -> bool {
        self.generation.current() != generation || self.stash.iter().any(Cmd::supersedes)
    }

    /// A show of `generation` is not wanted any more (a newer show does not cancel it).
    fn cancelled(&self, generation: u64) -> bool {
        self.generation.barrier() > generation || self.stash.iter().any(Cmd::cancels)
    }

    fn dropped(&self, mut perf: FramePerf, t0: Instant) {
        perf.dropped = true;
        perf.total = t0.elapsed();
        self.perf.push(perf);
    }

    fn show(&mut self, generation: u64, t: Time, asked: Instant) {
        let Some(source) = self.source.clone() else { return };
        if self.cancelled(generation) {
            return;
        }
        let t0 = Instant::now();
        let mut perf = FramePerf::default();
        let scene = source.scene_at(t, &self.output);
        perf.evaluate = t0.elapsed();
        let inputs = self.resolver.prepare(&scene, &*source, Mode::Scrub { generation }, &mut perf);
        // Not ready in scrub mode = abandoned for a newer request.
        if self.cancelled(generation) || any_not_ready(&inputs.layers) || !self.render(&scene, &inputs, &mut perf) {
            return self.dropped(perf, t0);
        }
        self.present(generation, t, false, perf, Some(asked), t0);
    }

    /// One scheduling step of a play; false when it ended.
    fn play_step(&mut self, generation: u64, play: &mut Play) -> bool {
        let Some(source) = self.source.clone() else { return false };
        let sched = play.sched.get_or_insert_with(|| PlaySchedule::new(play.from, source.frame_rate(), source.duration())).clone();
        let Some(now) = self.clock.now() else {
            if matches!(play.preroll, Preroll::Pending) {
                play.preroll = self.preroll(generation, &sched, &*source, play);
            } else {
                self.wait(self.config.clock_poll);
            }
            return true;
        };
        let mut sched = sched;
        let step = if let Preroll::Ready(p) = std::mem::replace(&mut play.preroll, Preroll::Done) {
            let Prerolled { perf, t0, source: rendered_for, output, shows } = *p;
            if self.superseded(generation) {
                self.dropped(perf, t0);
            } else if !Arc::ptr_eq(&rendered_for, &source) || output != self.output {
                // The picture it holds is out of date: render the frame again.
                self.dropped(perf, t0);
            } else if self.pace_and_present(generation, &mut sched, 0, play.from, perf, t0) {
                play.shown.extend(shows);
            }
            None
        } else {
            Some(sched.poll(now))
        };
        let go_on = match step {
            None => true,
            Some(Step::End) => {
                self.sink.finished(generation, source.duration());
                false
            }
            Some(Step::Wait(d)) => {
                self.wait(duration_of(d).min(Duration::from_millis(50)));
                true
            }
            Some(Step::Render { index, time, skipped }) => {
                for _ in 0..skipped {
                    self.perf.push(FramePerf { dropped: true, ..Default::default() });
                }
                self.play_frame(generation, &mut sched, play, &*source, index, time, now);
                true
            }
        };
        play.sched = Some(sched);
        go_on
    }

    /// Prefetches the scene one lookahead after `time` when its media
    /// changed since the last prefetch, or every `prefetch_every` frames.
    fn prefetch_ahead(&self, play: &mut Play, source: &dyn SceneSource, time: Time) {
        if self.config.lookahead <= Time::ZERO {
            return;
        }
        let ahead = time + self.config.lookahead;
        if ahead >= source.duration() {
            return;
        }
        let scene = source.scene_at(ahead, &self.output);
        let mut media = vec![];
        visit_media(&scene.layers, &mut |l| {
            if let LayerContent::Media { media: m, .. } = &l.content
                && let Some(ms) = source.media(m.media)
            {
                media.push((m.media, decode_size(&l.placement, scene.canvas, scene.output.size, ms.display_size)));
            }
        });
        media.sort_by_key(|(id, s)| (id.0, s.w, s.h));
        media.dedup();
        play.since_prefetch += 1;
        if media != play.prefetched || play.since_prefetch >= self.config.prefetch_every.max(1) {
            self.resolver.prefetch(&scene, source);
            play.prefetched = media;
            play.since_prefetch = 0;
        }
    }

    /// Renders the play's first frame while the clock is stopped, then
    /// tells the sink to start the clock.
    fn preroll(&mut self, generation: u64, sched: &PlaySchedule, source: &dyn SceneSource, play: &mut Play) -> Preroll {
        let t0 = Instant::now();
        let time = sched.time_of(0);
        if time >= source.duration() {
            self.sink.ready(generation);
            return Preroll::Done;
        }
        self.prefetch_ahead(play, source, time);
        let mut perf = FramePerf::default();
        let scene = source.scene_at(time, &self.output);
        perf.evaluate = t0.elapsed();
        let mut inputs = self.resolver.prepare(&scene, source, Mode::Deadline(t0 + self.config.preroll), &mut perf);
        if self.superseded(generation) {
            return Preroll::Done;
        }
        let shows = self.fill_behind(&scene, source, &mut inputs.layers, &play.shown);
        // Not decoded in time: the clock starts anyway and the schedule
        // renders the frame again (it is not counted twice).
        let ready = shows.is_some() && self.render(&scene, &inputs, &mut perf);
        self.sink.ready(generation);
        match (ready, self.source.clone()) {
            (true, Some(current)) => {
                self.render_estimate = (self.render_estimate * 3 + perf.composite + perf.effects) / 4;
                Preroll::Ready(Box::new(Prerolled { perf, t0, source: current, output: self.output, shows: shows.unwrap_or_default() }))
            }
            _ => Preroll::Done,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn play_frame(&mut self, generation: u64, sched: &mut PlaySchedule, play: &mut Play, source: &dyn SceneSource, index: i64, time: Time, now: Time) {
        let t0 = Instant::now();
        let fd = sched.frame_duration();
        self.prefetch_ahead(play, source, time);
        let mut perf = FramePerf::default();
        let e0 = Instant::now();
        let scene = source.scene_at(time, &self.output);
        perf.evaluate = e0.elapsed();
        // Wait for decoding at most until half the frame's period is over,
        // keeping time to render it: a layer still not decoded then shows an
        // older frame, and waking up late (timer granularity) still leaves
        // time to present within the period.
        let left = duration_of(time + Time(fd.flicks() / 2) - now).saturating_sub(self.render_estimate);
        let mut inputs = self.resolver.prepare(&scene, source, Mode::Deadline(t0 + left), &mut perf);
        if self.superseded(generation) {
            return self.dropped(perf, t0);
        }
        // A layer with nothing to show (not even an older frame), or nothing
        // newer than what is on screen: keep the previous picture.
        let Some(shows) = self.fill_behind(&scene, source, &mut inputs.layers, &play.shown) else {
            sched.drop_frame(index);
            return self.dropped(perf, t0);
        };
        if !self.render(&scene, &inputs, &mut perf) {
            sched.drop_frame(index);
            return self.dropped(perf, t0);
        }
        self.render_estimate = (self.render_estimate * 3 + perf.composite + perf.effects) / 4;
        if self.pace_and_present(generation, sched, index, time, perf, t0) {
            play.shown.extend(shows);
        }
    }

    /// Playback inputs made presentable: a media layer not decoded in time
    /// gets the newest frame of its media (at its decode size) already in
    /// the cache, at most `max_behind` earlier. Returns the source frame each
    /// media layer shows, or `None` when a layer has nothing to show or when
    /// a substitute was needed and no layer shows anything newer than `shown`.
    fn fill_behind(&self, scene: &FrameScene, source: &dyn SceneSource, inputs: &mut [LayerInput], shown: &HashMap<LayerId, i64>) -> Option<Vec<(LayerId, i64)>> {
        let mut shows = vec![];
        let mut substituted = false;
        let mut missing = false;
        fill(self, scene, source, &scene.layers, inputs, &mut shows, &mut substituted, &mut missing);
        if missing {
            return None;
        }
        if substituted && !shows.iter().any(|(id, f)| shown.get(id).is_none_or(|s| f > s)) {
            return None;
        }
        Some(shows)
    }

    /// Presents rendered frame `index` when the clock reaches it (or drops
    /// it when late); true when presented.
    fn pace_and_present(&mut self, generation: u64, sched: &mut PlaySchedule, index: i64, time: Time, perf: FramePerf, t0: Instant) -> bool {
        loop {
            let Some(now) = self.clock.now() else {
                sched.drop_frame(index);
                self.dropped(perf, t0);
                return false;
            };
            match sched.after_render(index, now) {
                Pace::Wait(d) => {
                    self.wait(duration_of(d));
                    if self.superseded(generation) {
                        self.dropped(perf, t0);
                        return false;
                    }
                }
                Pace::Present => return self.present(generation, time, true, perf, None, t0),
                Pace::Drop => {
                    self.dropped(perf, t0);
                    return false;
                }
            }
        }
    }

    fn render(&mut self, scene: &FrameScene, inputs: &RenderInputs, perf: &mut FramePerf) -> bool {
        let size = scene.output.size;
        let Some(target) = self.sink.target(size) else { return false };
        let frame = PreparedFrame { scene, inputs };
        let mut rt = RenderTarget::Cpu(target);
        let renderer = &mut self.renderer;
        match std::panic::catch_unwind(AssertUnwindSafe(|| renderer.render(&frame, &mut rt))) {
            Ok(Ok(stats)) => {
                perf.composite = stats.composite;
                perf.effects = stats.effects;
                true
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "preview render failed");
                false
            }
            Err(_) => {
                tracing::error!("preview renderer panicked");
                false
            }
        }
    }

    /// A play frame is presented only while its play is the newest request;
    /// a show (`asked` is set) while no play or stop came after it. True when presented.
    fn present(&mut self, generation: u64, time: Time, playing: bool, perf: FramePerf, asked: Option<Instant>, t0: Instant) -> bool {
        let p0 = Instant::now();
        let shown = {
            let _l = self.generation.lock.lock();
            let current = self.generation.current();
            let ok = if playing { current == generation } else { generation >= self.generation.barrier() };
            if !ok {
                Err(perf)
            } else {
                let mut info = FrameInfo { generation, time, playing, latest: current == generation, perf };
                self.sink.present(&mut info);
                Ok((info.perf, info.latest))
            }
        };
        match shown {
            Ok((mut p, latest)) => {
                p.present = p0.elapsed();
                p.seek_latency = asked.filter(|_| latest).map(|a| a.elapsed());
                p.total = t0.elapsed();
                self.perf.push(p);
                true
            }
            Err(p) => {
                self.dropped(p, t0);
                false
            }
        }
    }
}

/// [`Worker::fill_behind`] over a layer tree (transitions recursively).
#[allow(clippy::too_many_arguments)]
fn fill(
    w: &Worker,
    scene: &FrameScene,
    source: &dyn SceneSource,
    layers: &[Layer],
    inputs: &mut [LayerInput],
    shows: &mut Vec<(LayerId, i64)>,
    substituted: &mut bool,
    missing: &mut bool,
) {
    for (l, input) in layers.iter().zip(inputs.iter_mut()) {
        match (&l.content, input) {
            (LayerContent::Transition(t), LayerInput::Transition { from, to }) => {
                fill(w, scene, source, &t.from, from, shows, substituted, missing);
                fill(w, scene, source, &t.to, to, shows, substituted, missing);
            }
            (LayerContent::Media { media, source_time }, input) => {
                let Some(m) = source.media(media.media) else { continue };
                let size = decode_size(&l.placement, scene.canvas, scene.output.size, m.display_size);
                let want = m.frame_at(*source_time);
                match input {
                    LayerInput::Cpu(_) => shows.push((l.id, want)),
                    LayerInput::Missing(MissingReason::NotReady) => {
                        let behind = if m.rate.num > 0 && m.rate.den > 0 { m.rate.time_to_frame(w.config.max_behind).max(1) } else { 1 };
                        let cache = w.resolver.cache();
                        let found = (1..=behind.min(want)).find_map(|k| cache.get(&FrameKey { media: media.media, size, frame: want - k }).map(|f| (want - k, f)));
                        match found {
                            Some((frame, f)) => {
                                *input = LayerInput::Cpu(f);
                                shows.push((l.id, frame));
                                *substituted = true;
                            }
                            None => *missing = true,
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn visit_media(layers: &[Layer], f: &mut impl FnMut(&Layer)) {
    for l in layers {
        match &l.content {
            LayerContent::Transition(t) => {
                visit_media(&t.from, f);
                visit_media(&t.to, f);
            }
            _ => f(l),
        }
    }
}

fn any_not_ready(layers: &[LayerInput]) -> bool {
    layers.iter().any(|l| match l {
        LayerInput::Missing(MissingReason::NotReady) => true,
        LayerInput::Transition { from, to } => any_not_ready(from) || any_not_ready(to),
        _ => false,
    })
}
