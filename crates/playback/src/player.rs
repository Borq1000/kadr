//! `PreviewPlayer`: a thread that turns show/play/stop requests into
//! rendered, paced frames (render spec §8, §10).
//!
//! Every request increments the player's generation. The thread coalesces
//! queued requests (the newest wins), resolves the scene (paused: `Scrub`,
//! so a newer request abandons the wait at once; playing: `Deadline`),
//! renders into a buffer the [`FrameSink`] lends it and presents only if the
//! generation is still current — checked under the same lock that
//! [`PreviewPlayer::show`]/`play`/`stop` take to increment it, so once one of
//! them returns no frame of an older generation is presented any more.
//! Every presented or dropped frame pushes a [`FramePerf`] into the shared
//! [`PerfRing`].

use crate::pace::{duration_of, Clock, Pace, PlaySchedule, Step};
use crate::resolver::{Mode, Resolver};
use crate::source::SceneSource;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use kadr_core::perf::{FramePerf, PerfRing};
use kadr_core::Time;
use kadr_render::{CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{FrameScene, OutputSpec, SizeU};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What a presented frame is.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    /// The request it answers (compare with [`PreviewPlayer::generation`]).
    pub generation: u64,
    /// Timeline time shown.
    pub time: Time,
    pub playing: bool,
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
}

#[derive(Clone, Debug)]
pub struct PlayerConfig {
    /// During playback the scene this far ahead is prefetched, so the next
    /// clip's decoder is open before the cut. `Time::ZERO` disables it.
    pub lookahead: Time,
    /// How often to look at a clock that is not running yet.
    pub clock_poll: Duration,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        PlayerConfig { lookahead: Time::from_millis(750), clock_poll: Duration::from_millis(5) }
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
    fn supersedes(&self) -> bool {
        matches!(self, Cmd::Show { .. } | Cmd::Play { .. } | Cmd::Stop | Cmd::Quit)
    }
}

/// The generation counter; incremented and checked-then-presented under one lock.
struct Generation {
    lock: Mutex<()>,
    value: AtomicU64,
}

impl Generation {
    fn current(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }
}

pub struct PreviewPlayer {
    tx: Sender<Cmd>,
    generation: Arc<Generation>,
    resolver: Arc<Resolver>,
    perf: Arc<PerfRing>,
    thread: Option<JoinHandle<()>>,
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
        let generation = Arc::new(Generation { lock: Mutex::new(()), value: AtomicU64::new(resolver.generation()) });
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
        };
        let thread = std::thread::Builder::new().name("kadr-player".into()).spawn(move || worker.run()).expect("spawn player thread");
        PreviewPlayer { tx, generation, resolver, perf, thread: Some(thread) }
    }

    /// Scenes and media from now on (an edit replaces the snapshot). Does
    /// not re-render by itself: follow with `show` when paused.
    pub fn set_source(&self, source: Arc<dyn SceneSource>) {
        let _ = self.tx.send(Cmd::Source(source));
    }

    /// Output size and quality from now on (the sink's buffers must match).
    pub fn set_output(&self, output: OutputSpec) {
        let _ = self.tx.send(Cmd::Output(output));
    }

    /// Shows the frame at timeline time `t` (paused). Returns its generation.
    pub fn show(&self, t: Time) -> u64 {
        let generation = self.bump();
        let _ = self.tx.send(Cmd::Show { generation, t, asked: Instant::now() });
        generation
    }

    /// Plays from `from`, paced against the clock (start the clock at
    /// `from` too). Returns the play's generation.
    pub fn play(&self, from: Time) -> u64 {
        let generation = self.bump();
        let _ = self.tx.send(Cmd::Play { generation, from });
        generation
    }

    /// Stops playback (or a pending show); the last presented frame stays.
    pub fn stop(&self) -> u64 {
        let generation = self.bump();
        let _ = self.tx.send(Cmd::Stop);
        generation
    }

    /// The current generation; frames of older ones are never presented.
    pub fn generation(&self) -> u64 {
        self.generation.current()
    }

    pub fn perf(&self) -> &Arc<PerfRing> {
        &self.perf
    }

    pub fn resolver(&self) -> &Arc<Resolver> {
        &self.resolver
    }

    fn bump(&self) -> u64 {
        let g = {
            let _l = self.generation.lock.lock();
            self.generation.value.fetch_add(1, Ordering::AcqRel) + 1
        };
        // Wakes a paused-frame wait and stops session work of older generations.
        self.resolver.supersede(g);
        g
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        self.bump();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

enum Job {
    Idle,
    Show { generation: u64, t: Time, asked: Instant },
    Play { generation: u64, sched: PlaySchedule },
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
                    self.show(generation, t, asked);
                    Job::Idle
                }
                Job::Play { generation, mut sched } => {
                    if self.play_step(generation, &mut sched) {
                        Job::Play { generation, sched }
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
                *job = match &self.source {
                    Some(s) => Job::Play { generation, sched: PlaySchedule::new(from, s.frame_rate(), s.duration()) },
                    None => Job::Idle,
                }
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

    fn superseded(&self, generation: u64) -> bool {
        self.generation.current() != generation || self.stash.iter().any(Cmd::supersedes)
    }

    fn dropped(&self, mut perf: FramePerf, t0: Instant) {
        perf.dropped = true;
        perf.total = t0.elapsed();
        self.perf.push(perf);
    }

    fn show(&mut self, generation: u64, t: Time, asked: Instant) {
        let Some(source) = self.source.clone() else { return };
        if self.superseded(generation) {
            return;
        }
        let t0 = Instant::now();
        let mut perf = FramePerf::default();
        let scene = source.scene_at(t, &self.output);
        perf.evaluate = t0.elapsed();
        let inputs = self.resolver.prepare(&scene, &*source, Mode::Scrub { generation }, &mut perf);
        if self.superseded(generation) || !self.render(&scene, &inputs, &mut perf) {
            return self.dropped(perf, t0);
        }
        self.present(generation, t, false, perf, Some(asked), t0);
    }

    /// One scheduling step of a play; false when it ended.
    fn play_step(&mut self, generation: u64, sched: &mut PlaySchedule) -> bool {
        let Some(source) = self.source.clone() else { return false };
        let Some(now) = self.clock.now() else {
            self.wait(self.config.clock_poll);
            return true;
        };
        match sched.poll(now) {
            Step::End => {
                self.sink.finished(generation, source.duration());
                false
            }
            Step::Wait(d) => {
                self.wait(duration_of(d).min(Duration::from_millis(50)));
                true
            }
            Step::Render { index, time, skipped } => {
                for _ in 0..skipped {
                    self.perf.push(FramePerf { dropped: true, ..Default::default() });
                }
                self.play_frame(generation, sched, &*source, index, time, now);
                true
            }
        }
    }

    fn play_frame(&mut self, generation: u64, sched: &mut PlaySchedule, source: &dyn SceneSource, index: i64, time: Time, now: Time) {
        let t0 = Instant::now();
        let fd = sched.frame_duration();
        if self.config.lookahead > Time::ZERO {
            let ahead = time + self.config.lookahead;
            if ahead < source.duration() {
                self.resolver.prefetch(&source.scene_at(ahead, &self.output), source);
            }
        }
        let mut perf = FramePerf::default();
        let e0 = Instant::now();
        let scene = source.scene_at(time, &self.output);
        perf.evaluate = e0.elapsed();
        // Wait for decoding at most until the frame would be late, keeping time to render it.
        let left = duration_of(time + fd - now).saturating_sub(self.render_estimate);
        let inputs = self.resolver.prepare(&scene, source, Mode::Deadline(t0 + left), &mut perf);
        if self.superseded(generation) {
            return self.dropped(perf, t0);
        }
        // A layer not decoded in time: keep the previous picture rather than flash MISSING.
        if any_not_ready(&inputs.layers) || !self.render(&scene, &inputs, &mut perf) {
            sched.drop_frame(index);
            return self.dropped(perf, t0);
        }
        self.render_estimate = (self.render_estimate * 3 + perf.composite) / 4;
        loop {
            let Some(now) = self.clock.now() else {
                sched.drop_frame(index);
                return self.dropped(perf, t0);
            };
            match sched.after_render(index, now) {
                Pace::Wait(d) => {
                    self.wait(duration_of(d));
                    if self.superseded(generation) {
                        return self.dropped(perf, t0);
                    }
                }
                Pace::Present => return self.present(generation, time, true, perf, None, t0),
                Pace::Drop => return self.dropped(perf, t0),
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

    fn present(&mut self, generation: u64, time: Time, playing: bool, perf: FramePerf, asked: Option<Instant>, t0: Instant) {
        let p0 = Instant::now();
        let shown = {
            let _l = self.generation.lock.lock();
            if self.generation.current() != generation {
                Err(perf)
            } else {
                let mut info = FrameInfo { generation, time, playing, perf };
                self.sink.present(&mut info);
                Ok(info.perf)
            }
        };
        match shown {
            Ok(mut p) => {
                p.present = p0.elapsed();
                p.seek_latency = asked.map(|a| a.elapsed());
                p.total = t0.elapsed();
                self.perf.push(p);
            }
            Err(p) => self.dropped(p, t0),
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
