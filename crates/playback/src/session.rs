//! Decoder sessions (render spec §9): a long-lived decoder for one media at
//! one decode size, on its own thread, with a current position. A target
//! ahead of the position within the forward window is read forward; any
//! other target reopens the stream there. The thread always works towards
//! the newest target: between two frames it re-reads its target, so a
//! superseded seek is abandoned after at most one frame (a blocking read or
//! open itself cannot be interrupted). Frames go into the shared
//! [`FrameCache`]; buffers come from the shared pool.

use crate::cache::{FrameCache, FrameKey};
use crate::decoders::Decoders;
use crate::source::MediaSource;
use kadr_core::{AssetId, ColorInfo, CpuFrame, FramePool, Time};
use kadr_media::{MediaError, SourceStream};
use kadr_scene::{LayerId, SizeU, SourceKind};
use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Counters {
    pub opens: AtomicU64,
    pub frames: AtomicU64,
    pub stills: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionKey {
    pub media: AssetId,
    pub size: SizeU,
}

/// A frame a session should produce. Work of a generation older than the
/// resolver's newest is void.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Work {
    pub frame: i64,
    pub generation: u64,
    /// Prefetch: never replaces a pending foreground target.
    pub background: bool,
}

pub(crate) struct SessionState {
    pub target: Option<Work>,
    /// Read ahead up to this frame (exclusive) while nothing else is wanted.
    pub ahead_until: i64,
    pub ahead_gen: u64,
    /// Next frame the open stream yields (`None` = no stream). Mirrored by
    /// the thread for the resolver's choice of session.
    pub position: Option<i64>,
    /// Forward distance (frames) still cheaper to read than to reopen.
    pub window: i64,
    /// The layer that last claimed this session, and when.
    pub owner: Option<LayerId>,
    pub last_claim: Instant,
    pub last_active: Instant,
    pub quit: bool,
    pub exited: bool,
}

pub(crate) struct Shared {
    pub state: Mutex<SessionState>,
    pub cv: Condvar,
}

/// What all sessions of one resolver share.
pub(crate) struct Env {
    pub cache: Arc<FrameCache>,
    pub decoders: Arc<dyn Decoders>,
    pub pool: FramePool,
    pub latest: Arc<AtomicU64>,
    pub counters: Arc<Counters>,
    pub idle_close: Duration,
    pub forward_window: Time,
}

pub(crate) struct Session {
    pub id: u64,
    pub key: SessionKey,
    pub shared: Arc<Shared>,
    pub thread: Option<JoinHandle<()>>,
}

impl Session {
    pub fn spawn(id: u64, key: SessionKey, media: Arc<MediaSource>, env: Arc<Env>) -> Session {
        let now = Instant::now();
        let max_window = max_window_frames(&media, env.forward_window);
        let shared = Arc::new(Shared {
            state: Mutex::new(SessionState {
                target: None,
                ahead_until: i64::MIN,
                ahead_gen: 0,
                position: None,
                window: (max_window / 2).max(2),
                owner: None,
                last_claim: now,
                last_active: now,
                quit: false,
                exited: false,
            }),
            cv: Condvar::new(),
        });
        let worker = Worker::new(key, media, shared.clone(), env, max_window);
        let thread = std::thread::Builder::new().name("kadr-decode".into()).spawn(move || worker.run()).expect("spawn decoder session thread");
        Session { id, key, shared, thread: Some(thread) }
    }

    /// Asks the thread to stop after its current read (dropping the stream
    /// kills the decoder process).
    pub fn quit(&self) {
        self.shared.state.lock().quit = true;
        self.shared.cv.notify_all();
    }
}

fn max_window_frames(media: &MediaSource, window: Time) -> i64 {
    if media.rate.num == 0 || media.rate.den == 0 {
        return 2;
    }
    media.rate.time_to_frame(window).max(2)
}

enum Action {
    Open(i64),
    Read,
    Still,
    Exit,
}

/// Marks the session exited however the thread ends; a panic counts as a
/// decode failure so waiters do not wait for a thread that is gone.
struct ExitGuard {
    shared: Arc<Shared>,
    cache: Arc<FrameCache>,
    media: AssetId,
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.cache.update(|b| {
                b.record_failure(self.media);
                ((), vec![])
            });
        }
        let mut st = self.shared.state.lock();
        st.exited = true;
        st.position = None;
    }
}

struct Worker {
    key: SessionKey,
    media: Arc<MediaSource>,
    color: ColorInfo,
    image: bool,
    shared: Arc<Shared>,
    env: Arc<Env>,
    stream: Option<Box<dyn SourceStream>>,
    /// Next frame the stream yields.
    pos: i64,
    opened_at: i64,
    /// The target the stream was opened for: read forward to it whatever the window.
    open_for: i64,
    /// Set by `Open`: the first frame's decode time includes the open.
    open_started: Option<Instant>,
    /// After a stream ended without a single frame: open this far before the target.
    back: i64,
    est_open: Option<f64>,
    est_frame: Option<f64>,
    max_window: i64,
}

impl Worker {
    fn new(key: SessionKey, media: Arc<MediaSource>, shared: Arc<Shared>, env: Arc<Env>, max_window: i64) -> Self {
        Worker {
            key,
            color: media.frame_color(),
            image: media.kind == SourceKind::Image,
            media,
            shared,
            env,
            stream: None,
            pos: 0,
            opened_at: 0,
            open_for: 0,
            open_started: None,
            back: 0,
            est_open: None,
            est_frame: None,
            max_window,
        }
    }

    fn run(mut self) {
        let _guard = ExitGuard { shared: self.shared.clone(), cache: self.env.cache.clone(), media: self.key.media };
        loop {
            match self.decide() {
                Action::Exit => break,
                Action::Open(target) => self.open(target),
                Action::Read => self.read(),
                Action::Still => self.still(),
            }
        }
        self.stream = None;
    }

    fn frame_key(&self, frame: i64) -> FrameKey {
        FrameKey { media: self.key.media, size: self.key.size, frame }
    }

    /// Next action, decided under the session lock; waits while idle and
    /// exits after `idle_close` without work.
    fn decide(&self) -> Action {
        let mut st = self.shared.state.lock();
        loop {
            if st.quit {
                return Action::Exit;
            }
            let latest = self.env.latest.load(Ordering::Acquire);
            if st.target.is_some_and(|w| w.generation < latest) {
                st.target = None;
            }
            if st.ahead_gen < latest {
                st.ahead_until = i64::MIN;
            }
            if let Some(w) = st.target {
                let f = w.frame;
                let done = {
                    let b = self.env.cache.lock();
                    b.contains(&self.frame_key(f)) || b.beyond_eof(self.key.media, f)
                };
                if done {
                    st.target = None;
                    continue;
                }
                st.last_active = Instant::now();
                if self.image {
                    return Action::Still;
                }
                let reachable = self.stream.is_some() && self.pos <= f && (f - self.pos < st.window || f <= self.open_for);
                return if reachable { Action::Read } else { Action::Open(f) };
            }
            if !self.image && self.stream.is_some() && self.pos < st.ahead_until && !self.env.cache.lock().beyond_eof(self.key.media, self.pos) {
                st.last_active = Instant::now();
                return Action::Read;
            }
            let idle = st.last_active.elapsed();
            if idle >= self.env.idle_close {
                st.exited = true;
                st.position = None;
                return Action::Exit;
            }
            self.shared.cv.wait_for(&mut st, self.env.idle_close - idle);
        }
    }

    fn open(&mut self, target: i64) {
        self.stream = None; // the old process goes first
        let start = (target - self.back).max(0);
        self.shared.state.lock().position = Some(start);
        let t0 = Instant::now();
        match self.env.decoders.open(&self.media, start, self.key.size) {
            Ok(s) => {
                self.env.counters.opens.fetch_add(1, Ordering::Relaxed);
                self.stream = Some(s);
                self.pos = start;
                self.opened_at = start;
                self.open_for = target;
                self.open_started = Some(t0);
            }
            Err(e) => self.fail(e, target),
        }
    }

    fn read(&mut self) {
        let Some(stream) = self.stream.as_mut() else { return };
        let mut frame = CpuFrame::rgba8(&self.env.pool, self.key.size.w, self.key.size.h, self.color);
        let with_open = self.open_started.take();
        let t0 = with_open.unwrap_or_else(Instant::now);
        match stream.read_into(&mut frame.data) {
            Ok(true) => {
                let spent = t0.elapsed();
                let est = if with_open.is_some() { &mut self.est_open } else { &mut self.est_frame };
                *est = Some(est.map_or(spent.as_secs_f64(), |p| p * 0.8 + spent.as_secs_f64() * 0.2));
                let key = self.frame_key(self.pos);
                self.pos += 1;
                self.back = 0;
                self.env.counters.frames.fetch_add(1, Ordering::Relaxed);
                self.env.cache.insert(key, Arc::new(frame), spent);
                let window = self.window();
                let mut st = self.shared.state.lock();
                st.position = Some(self.pos);
                st.window = window;
                st.last_active = Instant::now();
            }
            Ok(false) => {
                let end = self.pos;
                let empty = end == self.opened_at;
                self.stream = None;
                if empty && end == 0 {
                    self.fail(MediaError::Unsupported(format!("{}: no frames", self.media.path.display())), end);
                    return;
                }
                if empty {
                    // Opened past the real end: look further back next time.
                    let second = self.media.rate.time_to_frame(Time::from_secs(1)).max(1);
                    self.back = if self.back == 0 { second } else { self.back.saturating_mul(2) };
                }
                tracing::debug!(path = %self.media.path.display(), end, "source stream ended before its duration");
                self.env.cache.update(|b| {
                    b.record_eof(self.key.media, end);
                    ((), vec![])
                });
                self.shared.state.lock().position = None;
            }
            Err(e) => {
                let at = self.pos;
                self.fail(e, at);
            }
        }
    }

    fn still(&mut self) {
        let mut frame = CpuFrame::rgba8(&self.env.pool, self.key.size.w, self.key.size.h, self.color);
        let t0 = Instant::now();
        match self.env.decoders.still(&self.media, self.key.size, &mut frame.data) {
            Ok(()) => {
                self.env.counters.stills.fetch_add(1, Ordering::Relaxed);
                self.env.cache.insert(self.frame_key(0), Arc::new(frame), t0.elapsed());
            }
            Err(e) => self.fail(e, 0),
        }
    }

    /// Forward reads cheaper than a reopen: measured open cost over per-frame cost.
    fn window(&self) -> i64 {
        match (self.est_open, self.est_frame) {
            (Some(o), Some(f)) if f > 0.0 => ((o / f).round() as i64).clamp(2, self.max_window),
            _ => (self.max_window / 2).max(2),
        }
    }

    fn fail(&mut self, e: MediaError, frame: i64) {
        tracing::warn!(path = %self.media.path.display(), frame, error = %e, "decode failed");
        self.stream = None;
        self.env.cache.update(|b| {
            b.record_failure(self.key.media);
            ((), vec![])
        });
        // Every waiter of this media now answers `DecodeFailed` (and new
        // requests do for a while), so no target is worth retrying.
        let mut st = self.shared.state.lock();
        st.position = None;
        st.target = None;
        st.ahead_until = i64::MIN;
    }
}
