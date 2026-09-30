//! Resolver (render spec §4.2): turns a scene's media references into real
//! frames. Per media layer it picks the decode size and source frame, takes
//! the frame from the cache or asks a decoder session for it, and waits as
//! the mode says. Everything is `&self` and thread-safe: the player thread
//! prepares frames while the UI thread supersedes generations.

use crate::cache::{FrameCache, FrameKey};
use crate::decoders::Decoders;
use crate::session::{Counters, Env, Session, SessionKey, Work};
use crate::source::{MediaSource, SceneSource};
use kadr_core::perf::{FramePerf, LayerTiming};
use kadr_core::color::AlphaMode;
use kadr_core::{AssetId, ColorInfo, CpuFrame, FramePool, Time};
use kadr_render::{LayerInput, MissingReason, RenderInputs};
use kadr_scene::{FrameScene, Layer, LayerContent, LayerId, Placement, SizeU, SourceKind};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ResolverConfig {
    /// Decoded frames kept (LRU).
    pub cache_bytes: usize,
    /// Unused frame buffers kept for reuse.
    pub pool_free_bytes: usize,
    /// Decoder sessions (≈ FFmpeg processes) at most, all media together.
    pub max_sessions: usize,
    /// Sessions per (media, decode size); more only when one scene needs more at once.
    pub max_sessions_per_media: usize,
    /// A target at most this far ahead of a session's position may be read
    /// forward instead of reopening; the session narrows it to what its
    /// measured open and frame costs make worthwhile.
    pub forward_window: Time,
    /// Playback read-ahead per session (also capped at 1/16 of the cache).
    pub readahead: Time,
    /// A session without work for this long closes (its process exits).
    pub idle_close: Duration,
    /// Preview modes: after a decode failure, requests for that media answer
    /// `DecodeFailed` at once for this long instead of retrying (no retry
    /// storms while scrubbing or playing). [`Mode::Export`] ignores it.
    pub retry_failed_after: Duration,
    /// [`Mode::Export`]: after a failed decode of a frame, wait this long
    /// and reopen the stream at that frame, once per entry, before
    /// answering `DecodeFailed`. A media that used up its retries is tried
    /// once per later frame (no retries) until a frame of it decodes again.
    pub export_retry_backoff: Vec<Duration>,
    /// A session claimed by a layer this recently is not taken by another
    /// layer (or by prefetch) unless the scene needs it at the same frame.
    pub claim_hold: Duration,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        ResolverConfig {
            cache_bytes: 1 << 30,
            pool_free_bytes: 512 << 20,
            max_sessions: 16,
            max_sessions_per_media: 4,
            forward_window: Time::from_secs(2),
            readahead: Time::from_millis(500),
            idle_close: Duration::from_secs(5),
            retry_failed_after: Duration::from_secs(2),
            export_retry_backoff: vec![Duration::from_millis(50), Duration::from_millis(200), Duration::from_millis(500)],
            claim_hold: Duration::from_millis(500),
        }
    }
}

/// How long [`Resolver::prepare`] waits for frames that are not cached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Paused frame / scrubbing: wait until every frame is ready, or return
    /// at once (`NotReady`) when a newer generation arrives. Passing a
    /// generation newer than the current one supersedes older requests.
    Scrub { generation: u64 },
    /// Playback: frames not ready by the deadline are `Missing(NotReady)`.
    /// Sessions read ahead.
    Deadline(Instant),
    /// Wait for every frame, never drop (use a resolver of its own: nothing
    /// supersedes it). A failed decode is retried (see
    /// [`ResolverConfig::export_retry_backoff`]) before it is `DecodeFailed`.
    Export,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResolverStats {
    /// Streams opened (seeks included) since the start.
    pub streams_opened: u64,
    pub frames_decoded: u64,
    pub stills_decoded: u64,
    /// Live decoder sessions.
    pub sessions: usize,
    pub cache_frames: usize,
    pub cache_bytes: usize,
    pub pool_allocations: u64,
}

/// Decode fractions of the display size (render spec §4.2). Animated
/// scale moves between a few sizes instead of fragmenting the cache.
const STEPS: [(u32, u32); 6] = [(1, 8), (1, 4), (3, 8), (1, 2), (3, 4), (1, 1)];

/// The size to decode a layer at: its footprint in output pixels
/// (`placement.size × |scale| × k`, `k = min(out.w / canvas.w, out.h / canvas.h)`),
/// as a fraction of the display size (the larger of the two axes), capped
/// at 1 and rounded up to a step of 1, ¾, ½, ⅜, ¼, ⅛; each side ≥ 1.
pub fn decode_size(placement: &Placement, canvas: SizeU, output: SizeU, display: SizeU) -> SizeU {
    let k = if canvas.w == 0 || canvas.h == 0 { 1.0 } else { (output.w as f64 / canvas.w as f64).min(output.h as f64 / canvas.h as f64) };
    let fw = placement.size.x.abs() as f64 * placement.scale.x.abs() as f64 * k;
    let fh = placement.size.y.abs() as f64 * placement.scale.y.abs() as f64 * k;
    if display.w == 0 || display.h == 0 {
        return SizeU::new((fw.ceil() as u32).max(1), (fh.ceil() as u32).max(1));
    }
    let frac = (fw / display.w as f64).max(fh / display.h as f64);
    let (n, d) = if frac.is_finite() { STEPS.into_iter().find(|&(n, d)| n as f64 / d as f64 >= frac - 1e-6).unwrap_or((1, 1)) } else { (1, 1) };
    let side = |v: u32| ((v as u64 * n as u64).div_ceil(d as u64) as u32).max(1);
    SizeU::new(side(display.w), side(display.h))
}

/// One distinct frame a scene needs.
struct Req {
    /// As computed from the scene.
    want: FrameKey,
    /// `want` clamped to where the stream really ends.
    key: FrameKey,
    media: Arc<MediaSource>,
    layer: LayerId,
    result: Option<LayerInput>,
    decode: Duration,
    pinned: bool,
    hit: bool,
    /// Export: retries used, the failure sequence number already counted,
    /// and the end of the backoff before the next retry.
    retries: usize,
    seen_failures: u64,
    retry_at: Option<Instant>,
}

/// The scene's layer tree with each media layer pointing at its `Req`.
enum Slot {
    Input(LayerInput),
    Req { req: usize, layer: LayerId },
    Transition { from: Vec<Slot>, to: Vec<Slot> },
}

struct Registry {
    sessions: Vec<Session>,
    /// Threads of sessions told to quit, joined on drop.
    retired: Vec<JoinHandle<()>>,
    next_id: u64,
}

/// Wait granularity for re-asserting requests (a safety net: a request is
/// re-sent if its session was retired or its target replaced meanwhile).
const REASSERT: Duration = Duration::from_millis(50);

pub struct Resolver {
    config: ResolverConfig,
    env: Arc<Env>,
    registry: Mutex<Registry>,
    /// The media each asset was last seen as: a change (relink) invalidates it.
    known: Mutex<HashMap<AssetId, Arc<MediaSource>>>,
    last_allocs: AtomicU64,
}

impl Resolver {
    pub fn new(decoders: Arc<dyn Decoders>, config: ResolverConfig) -> Self {
        let env = Arc::new(Env {
            cache: Arc::new(FrameCache::new(config.cache_bytes)),
            decoders,
            pool: FramePool::new(config.pool_free_bytes),
            latest: Arc::new(AtomicU64::new(0)),
            counters: Arc::new(Counters::default()),
            idle_close: config.idle_close,
            forward_window: config.forward_window,
        });
        Resolver {
            config,
            env,
            registry: Mutex::new(Registry { sessions: vec![], retired: vec![], next_id: 0 }),
            known: Mutex::new(HashMap::new()),
            last_allocs: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> &ResolverConfig {
        &self.config
    }

    pub fn cache(&self) -> &FrameCache {
        &self.env.cache
    }

    /// The pool decoded frames come from.
    pub fn pool(&self) -> &FramePool {
        &self.env.pool
    }

    /// The newest generation seen.
    pub fn generation(&self) -> u64 {
        self.env.latest.load(Ordering::Acquire)
    }

    /// Makes `generation` the newest (if it is newer): waiters and session
    /// work of older generations are abandoned. Cheap; any thread.
    pub fn supersede(&self, generation: u64) {
        let prev = self.env.latest.fetch_max(generation, Ordering::AcqRel);
        if generation > prev {
            self.env.cache.notify();
        }
    }

    pub fn stats(&self) -> ResolverStats {
        let c = &self.env.counters;
        let sessions = self.registry.lock().sessions.iter().filter(|s| !s.shared.state.lock().exited).count();
        ResolverStats {
            streams_opened: c.opens.load(Ordering::Relaxed),
            frames_decoded: c.frames.load(Ordering::Relaxed),
            stills_decoded: c.stills.load(Ordering::Relaxed),
            sessions,
            cache_frames: self.env.cache.len(),
            cache_bytes: self.env.cache.bytes(),
            pool_allocations: self.env.pool.allocations(),
        }
    }

    /// Forgets `media`: cached frames, sessions, end-of-stream and failure
    /// records. Call after relinking or replacing a file.
    pub fn invalidate_media(&self, media: AssetId) {
        self.known.lock().remove(&media);
        self.forget(media);
    }

    /// Forgets `media` in the cache (a new epoch: what its sessions still
    /// deliver is refused), then retires its sessions. In this order, every
    /// session of the old epoch exists when the retiring looks for them.
    fn forget(&self, media: AssetId) {
        self.env.cache.remove_media(media);
        let mut reg = self.registry.lock();
        let ids: Vec<u64> = reg.sessions.iter().filter(|s| s.key.media == media).map(|s| s.id).collect();
        for id in ids {
            retire(&mut reg, id);
        }
    }

    /// Real inputs for `scene`, parallel to its layers. Offline media →
    /// `Missing(Offline)`, a failed decode → `Missing(DecodeFailed)` (in
    /// export after its retries), not ready when the mode stops waiting →
    /// `Missing(NotReady)`. A media layer that draws nothing whatever its
    /// input (see [`draws_nothing`]) is not decoded: it gets a 1×1
    /// transparent frame. Fills
    /// `resolve`, `decode` (per media layer: the time its session spent
    /// producing the frame, reported once), cache hits/misses and
    /// `frame_allocs` (pool buffers allocated since the previous call).
    pub fn prepare(&self, scene: &FrameScene, source: &dyn SceneSource, mode: Mode, perf: &mut FramePerf) -> RenderInputs {
        let started = Instant::now();
        let generation = match mode {
            Mode::Scrub { generation } => {
                self.supersede(generation);
                generation
            }
            _ => self.generation(),
        };
        let reading = !matches!(mode, Mode::Scrub { .. });
        let export = mode == Mode::Export;
        let (slots, mut reqs) = self.collect_scene(scene, source);
        let mut pending = vec![];
        let fail_seq = {
            let mut b = self.env.cache.lock();
            for (i, r) in reqs.iter_mut().enumerate() {
                if !export && b.failed_recently(r.want.media, self.config.retry_failed_after) {
                    r.result = Some(LayerInput::Missing(MissingReason::DecodeFailed));
                    continue;
                }
                r.key = b.clamp_eof(r.want);
                match b.get(&r.key) {
                    Some(f) => {
                        r.decode = b.take_decode(&r.key);
                        r.result = Some(LayerInput::Cpu(f));
                        r.hit = true;
                    }
                    None => {
                        b.pin(r.key);
                        r.pinned = true;
                        pending.push(i);
                    }
                }
            }
            b.fail_seq()
        };
        if reading || !pending.is_empty() {
            let hits: Vec<usize> = if reading { (0..reqs.len()).filter(|&i| reqs[i].hit).collect() } else { vec![] };
            self.assign(&reqs, &pending, &hits, generation, false, reading);
        }
        if !pending.is_empty() {
            self.wait(&mut reqs, pending, mode, generation, fail_seq);
        }
        let dropped = {
            let mut b = self.env.cache.lock();
            let mut d = vec![];
            for r in reqs.iter_mut().filter(|r| r.pinned) {
                d.extend(b.unpin(r.key));
                r.pinned = false;
            }
            d
        };
        drop(dropped);

        let mut reported = vec![false; reqs.len()];
        let layers = build(slots, &reqs, &mut reported, perf);
        let allocs = self.env.pool.allocations();
        let prev = self.last_allocs.swap(allocs, Ordering::Relaxed);
        perf.frame_allocs += allocs.saturating_sub(prev) as u32;
        perf.resolve += started.elapsed();
        RenderInputs { layers }
    }

    /// Opens sessions and decodes ahead for a scene that will be needed
    /// soon (playback: the scene one lookahead ahead, so the next clip's
    /// stream is open before the cut). Never waits, never takes a session
    /// another layer is using, and leaves frames a session can reach by
    /// reading forward to its read-ahead.
    pub fn prefetch(&self, scene: &FrameScene, source: &dyn SceneSource) {
        let (_, mut reqs) = self.collect_scene(scene, source);
        let misses: Vec<usize> = {
            let b = self.env.cache.lock();
            (0..reqs.len())
                .filter(|&i| {
                    let r = &mut reqs[i];
                    r.key = b.clamp_eof(r.want);
                    !b.failed_recently(r.want.media, self.config.retry_failed_after) && !b.contains(&r.key)
                })
                .collect()
        };
        if !misses.is_empty() {
            self.assign(&reqs, &misses, &[], self.generation(), true, false);
        }
    }

    fn collect_scene(&self, scene: &FrameScene, source: &dyn SceneSource) -> (Vec<Slot>, Vec<Req>) {
        let mut medias = HashMap::new();
        let mut reqs = vec![];
        let mut index = HashMap::new();
        let slots = collect(&scene.layers, scene, source, &mut medias, &mut reqs, &mut index);
        let changed: Vec<AssetId> = {
            let mut known = self.known.lock();
            let mut changed = vec![];
            for (id, m) in medias.iter().filter_map(|(id, m)| m.as_ref().map(|m| (*id, m))) {
                match known.get(&id) {
                    Some(k) if k.same_decode(m) => {}
                    Some(_) => {
                        changed.push(id);
                        known.insert(id, m.clone());
                    }
                    None => {
                        known.insert(id, m.clone());
                    }
                }
            }
            changed
        };
        for id in changed {
            tracing::debug!(media = %id, "media changed: dropping its frames and sessions");
            self.forget(id);
        }
        (slots, reqs)
    }

    fn wait(&self, reqs: &mut [Req], mut pending: Vec<usize>, mode: Mode, generation: u64, fail_seq: u64) {
        let reading = !matches!(mode, Mode::Scrub { .. });
        let export = mode == Mode::Export;
        let backoff = &self.config.export_retry_backoff;
        for &i in &pending {
            reqs[i].seen_failures = fail_seq;
        }
        let mut asserted = Instant::now();
        loop {
            let mut again = false;
            let mut dropped = vec![];
            {
                let mut b = self.env.cache.lock();
                loop {
                    let now = Instant::now();
                    pending.retain(|&i| {
                        let r = &mut reqs[i];
                        if export {
                            // Per frame: its session failed producing it since that was last counted.
                            if b.key_failed_since(&r.key, r.seen_failures) {
                                r.seen_failures = b.fail_seq();
                                let allowed = if b.gave_up(r.key.media) { 0 } else { backoff.len() };
                                if r.retries >= allowed {
                                    b.give_up(r.key.media);
                                    tracing::warn!(path = %r.media.path.display(), frame = r.key.frame, retries = r.retries, "export: decode failed, giving up");
                                    r.result = Some(LayerInput::Missing(MissingReason::DecodeFailed));
                                    return false;
                                }
                                let delay = backoff[r.retries];
                                r.retries += 1;
                                r.retry_at = Some(now + delay);
                                tracing::info!(path = %r.media.path.display(), frame = r.key.frame, retry = r.retries, ?delay, "export: decode failed, retrying");
                            }
                        } else if b.failed_since(r.want.media, fail_seq) {
                            r.result = Some(LayerInput::Missing(MissingReason::DecodeFailed));
                            return false;
                        }
                        let k = b.clamp_eof(r.want);
                        if k != r.key {
                            // The stream ended early: the last frame it has stands in.
                            dropped.extend(b.unpin(r.key));
                            b.pin(k);
                            r.key = k;
                            again = true;
                        }
                        match b.get(&r.key) {
                            Some(f) => {
                                r.decode = b.take_decode(&r.key);
                                r.result = Some(LayerInput::Cpu(f));
                                false
                            }
                            None => true,
                        }
                    });
                    if pending.is_empty() {
                        return;
                    }
                    let superseded = self.env.latest.load(Ordering::Acquire) > generation;
                    let late = matches!(mode, Mode::Deadline(d) if now >= d);
                    if superseded || late {
                        for &i in &pending {
                            reqs[i].result = Some(LayerInput::Missing(MissingReason::NotReady));
                        }
                        return;
                    }
                    // Backoffs that ended: re-assert those requests now.
                    let mut next_retry: Option<Instant> = None;
                    for &i in &pending {
                        match reqs[i].retry_at {
                            Some(t) if t <= now => {
                                reqs[i].retry_at = None;
                                again = true;
                            }
                            Some(t) => next_retry = Some(next_retry.map_or(t, |n| n.min(t))),
                            None => {}
                        }
                    }
                    if again || now >= asserted + REASSERT {
                        break;
                    }
                    let mut until = asserted + REASSERT;
                    if let Some(t) = next_retry {
                        until = until.min(t);
                    }
                    if let Mode::Deadline(d) = mode {
                        until = until.min(d);
                    }
                    self.env.cache.wait_until(&mut b, until);
                }
            }
            drop(dropped);
            // Requests in a backoff wait; their sessions stay without a target.
            let ready: Vec<usize> = pending.iter().copied().filter(|&i| reqs[i].retry_at.is_none()).collect();
            if !ready.is_empty() {
                self.assign(reqs, &ready, &[], generation, false, reading);
            }
            asserted = Instant::now();
        }
    }

    fn readahead_frames(&self, media: &MediaSource, size: SizeU) -> i64 {
        if media.rate.num == 0 || media.rate.den == 0 {
            return 1;
        }
        let by_time = media.rate.time_to_frame(self.config.readahead);
        let frame_bytes = (size.w as usize * size.h as usize * 4).max(1);
        let by_bytes = (self.config.cache_bytes / 16 / frame_bytes) as i64;
        by_time.min(by_bytes).max(1)
    }

    /// Points sessions at frames. `fetch`: frames to produce (a session is
    /// chosen or created for each); `hits`: cached frames whose session
    /// should keep reading ahead (playback). `background`: prefetch.
    fn assign(&self, reqs: &[Req], fetch: &[usize], hits: &[usize], generation: u64, background: bool, read_ahead: bool) {
        let now = Instant::now();
        let mut reg = self.registry.lock();
        reap(&mut reg);
        let mut claimed = HashSet::new();
        for &i in fetch {
            let r = &reqs[i];
            let Some(idx) = self.choose(&mut reg, r, &claimed, background, now) else { continue };
            let s = &reg.sessions[idx];
            claimed.insert(s.id);
            let ahead = read_ahead.then(|| r.key.frame + 1 + self.readahead_frames(&r.media, r.key.size));
            {
                let mut st = s.shared.state.lock();
                if background && st.target.is_some_and(|w| !w.background) {
                    continue;
                }
                st.target = Some(Work { frame: r.key.frame, generation, background });
                match ahead {
                    Some(a) => {
                        st.ahead_until = a;
                        st.ahead_gen = generation;
                    }
                    None if !background => st.ahead_until = i64::MIN,
                    None => {}
                }
                st.owner = Some(r.layer);
                st.last_claim = now;
                st.last_active = now;
            }
            s.shared.cv.notify_all();
        }
        for &i in hits {
            let r = &reqs[i];
            let f = r.key.frame;
            let n = self.readahead_frames(&r.media, r.key.size);
            // The session that is (or will be) just ahead of this frame.
            let near = reg
                .sessions
                .iter()
                .filter(|s| s.key.media == r.key.media && s.key.size == r.key.size && !claimed.contains(&s.id))
                .filter_map(|s| {
                    let st = s.shared.state.lock();
                    let p = st.position?;
                    (!st.exited && f - p >= -(n + 1) && f - p < st.window).then_some((s, st.owner != Some(r.layer), (f - p).abs()))
                })
                .min_by_key(|&(_, other, dist)| (other, dist))
                .map(|(s, _, _)| s);
            let Some(s) = near else { continue };
            claimed.insert(s.id);
            {
                let mut st = s.shared.state.lock();
                let current = if st.ahead_gen >= generation { st.ahead_until } else { i64::MIN };
                st.ahead_until = current.max(f + 1 + n);
                st.ahead_gen = generation;
                st.owner = Some(r.layer);
                st.last_claim = now;
                st.last_active = now;
            }
            s.shared.cv.notify_all();
        }
    }

    /// The session to produce `r`, by preference: one that reaches it by
    /// reading forward; the layer's own session (reopened); one no layer
    /// has used lately; a new one (within the caps); the least recently
    /// claimed one. Prefetch takes only the first free options.
    fn choose(&self, reg: &mut Registry, r: &Req, claimed: &HashSet<u64>, background: bool, now: Instant) -> Option<usize> {
        struct Cand {
            idx: usize,
            pos: Option<i64>,
            window: i64,
            owner: Option<LayerId>,
            last_claim: Instant,
            busy: bool,
        }
        let key = SessionKey { media: r.key.media, size: r.key.size };
        let mut of_key = 0;
        let mut cands = vec![];
        for (idx, s) in reg.sessions.iter().enumerate().filter(|(_, s)| s.key == key) {
            let st = s.shared.state.lock();
            if st.exited || st.quit {
                continue;
            }
            of_key += 1;
            if claimed.contains(&s.id) {
                continue;
            }
            cands.push(Cand { idx, pos: st.position, window: st.window, owner: st.owner, last_claim: st.last_claim, busy: st.target.is_some_and(|w| !w.background) });
        }
        if r.media.kind == SourceKind::Image {
            // Every request is frame 0: one session per size is enough.
            return match cands.first() {
                Some(c) => Some(c.idx),
                None if of_key > 0 => None,
                None => Some(self.create(reg, key, r.media.clone())),
            };
        }
        let f = r.key.frame;
        let cheap = cands.iter().filter(|c| c.pos.is_some_and(|p| p <= f && f - p < c.window)).min_by_key(|c| (c.owner != Some(r.layer), f - c.pos.unwrap_or(f)));
        if let Some(c) = cheap {
            return (!background).then_some(c.idx);
        }
        if let Some(c) = cands.iter().find(|c| c.owner == Some(r.layer)) {
            // Prefetch never moves a layer's own session: it is busy with the
            // layer's present, and its read-ahead covers what follows.
            return (!background).then_some(c.idx);
        }
        let idle = cands.iter().filter(|c| now.duration_since(c.last_claim) >= self.config.claim_hold && !(background && c.busy)).min_by_key(|c| c.last_claim);
        if let Some(c) = idle {
            return Some(c.idx);
        }
        if of_key < self.config.max_sessions_per_media {
            let live = reg.sessions.iter().filter(|s| !s.shared.state.lock().exited).count();
            if live >= self.config.max_sessions && !self.retire_lru(reg, claimed, now) && background {
                return None;
            }
            return Some(self.create(reg, key, r.media.clone()));
        }
        if background {
            return None;
        }
        match cands.iter().min_by_key(|c| c.last_claim) {
            Some(c) => Some(c.idx),
            // Every session of this media is needed by this very scene.
            None => Some(self.create(reg, key, r.media.clone())),
        }
    }

    fn create(&self, reg: &mut Registry, key: SessionKey, media: Arc<MediaSource>) -> usize {
        reg.next_id += 1;
        reg.sessions.push(Session::spawn(reg.next_id, key, media, self.env.clone()));
        reg.sessions.len() - 1
    }

    /// Closes the least recently claimed session no layer uses now.
    fn retire_lru(&self, reg: &mut Registry, claimed: &HashSet<u64>, now: Instant) -> bool {
        let victim = reg
            .sessions
            .iter()
            .filter(|s| !claimed.contains(&s.id))
            .filter_map(|s| {
                let st = s.shared.state.lock();
                (!st.exited && now.duration_since(st.last_claim) >= self.config.claim_hold).then_some((s.id, st.last_claim))
            })
            .min_by_key(|&(_, t)| t);
        match victim {
            Some((id, _)) => {
                retire(reg, id);
                true
            }
            None => false,
        }
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut reg = self.registry.lock();
            for s in &reg.sessions {
                s.quit();
            }
            let mut h: Vec<JoinHandle<()>> = reg.sessions.drain(..).filter_map(|mut s| s.thread.take()).collect();
            h.append(&mut reg.retired);
            h
        };
        // Each thread stops after its current read and kills its process.
        for h in handles {
            let _ = h.join();
        }
    }
}

fn retire(reg: &mut Registry, id: u64) {
    if let Some(i) = reg.sessions.iter().position(|s| s.id == id) {
        let mut s = reg.sessions.remove(i);
        s.quit();
        if let Some(h) = s.thread.take() {
            reg.retired.push(h);
        }
    }
}

/// Drops sessions whose threads have exited.
fn reap(reg: &mut Registry) {
    let mut i = 0;
    while i < reg.sessions.len() {
        if reg.sessions[i].shared.state.lock().exited {
            let mut s = reg.sessions.remove(i);
            if let Some(h) = s.thread.take() {
                reg.retired.push(h);
            }
        } else {
            i += 1;
        }
    }
    reg.retired.retain(|h| !h.is_finished());
}

/// Whether `l` draws nothing whatever its input, by the scene contract
/// (`kadr_scene`: an empty crop, a non-positive `placement.size` side, a
/// transform without inverse — e.g. a zero scale — or an opacity that
/// sanitises to 0). The evaluator culls such layers at the top level only;
/// inside a transition's `from`/`to` they reach the resolver.
pub fn draws_nothing(l: &Layer) -> bool {
    let size = l.placement.size;
    !(l.opacity.is_finite() && l.opacity > 0.0) || l.crop.is_empty() || !(size.x > 0.0 && size.y > 0.0) || l.placement.to_canvas().inverse().is_none()
}

/// The input of a media layer that draws nothing: one transparent pixel, so
/// the renderer's input check passes, nothing is decoded and export does
/// not count the layer as missing.
static BLANK: LazyLock<Arc<CpuFrame>> =
    LazyLock::new(|| Arc::new(CpuFrame::from_rgba8(1, 1, ColorInfo { alpha: AlphaMode::Straight, ..ColorInfo::WORKING_SDR }, vec![0; 4])));

fn collect(
    layers: &[Layer],
    scene: &FrameScene,
    source: &dyn SceneSource,
    medias: &mut HashMap<AssetId, Option<Arc<MediaSource>>>,
    reqs: &mut Vec<Req>,
    index: &mut HashMap<FrameKey, usize>,
) -> Vec<Slot> {
    let mut out = Vec::with_capacity(layers.len());
    for l in layers {
        out.push(match &l.content {
            LayerContent::Solid(_) => Slot::Input(LayerInput::None),
            LayerContent::Transition(t) => {
                let from = collect(&t.from, scene, source, medias, reqs, index);
                let to = collect(&t.to, scene, source, medias, reqs, index);
                Slot::Transition { from, to }
            }
            LayerContent::Media { .. } if draws_nothing(l) => Slot::Input(LayerInput::Cpu(BLANK.clone())),
            LayerContent::Media { media, source_time } => {
                let m = medias.entry(media.media).or_insert_with(|| source.media(media.media).filter(|m| m.online).map(Arc::new)).clone();
                match m {
                    None => Slot::Input(LayerInput::Missing(MissingReason::Offline)),
                    Some(m) => {
                        let size = decode_size(&l.placement, scene.canvas, scene.output.size, m.display_size);
                        let want = FrameKey { media: media.media, size, frame: m.frame_at(*source_time) };
                        let req = *index.entry(want).or_insert_with(|| {
                            reqs.push(Req {
                                want,
                                key: want,
                                media: m,
                                layer: l.id,
                                result: None,
                                decode: Duration::ZERO,
                                pinned: false,
                                hit: false,
                                retries: 0,
                                seen_failures: 0,
                                retry_at: None,
                            });
                            reqs.len() - 1
                        });
                        Slot::Req { req, layer: l.id }
                    }
                }
            }
        });
    }
    out
}

fn build(slots: Vec<Slot>, reqs: &[Req], reported: &mut [bool], perf: &mut FramePerf) -> Vec<LayerInput> {
    slots
        .into_iter()
        .map(|s| match s {
            Slot::Input(i) => i,
            Slot::Transition { from, to } => LayerInput::Transition { from: build(from, reqs, reported, perf), to: build(to, reqs, reported, perf) },
            Slot::Req { req, layer } => {
                let r = &reqs[req];
                if r.hit {
                    perf.cache_hits += 1;
                } else {
                    perf.cache_misses += 1;
                }
                let time = if reported[req] { Duration::ZERO } else { r.decode };
                reported[req] = true;
                perf.decode.push(LayerTiming { layer: layer.0 as u64, time });
                r.result.clone().unwrap_or(LayerInput::Missing(MissingReason::NotReady))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_scene::Vec2;

    fn placement(w: f32, h: f32, scale: f32) -> Placement {
        Placement { size: Vec2::new(w, h), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(0.0, 0.0), scale: Vec2::new(scale, scale), rotation: 0.3 }
    }

    #[test]
    fn decode_size_is_the_footprint_quantised_up_and_capped() {
        let canvas = SizeU::new(1920, 1080);
        let disp = SizeU::new(3840, 2160);
        // Full canvas at full output: 1920 of 3840 = ½.
        assert_eq!(decode_size(&placement(1920.0, 1080.0, 1.0), canvas, SizeU::new(1920, 1080), disp), SizeU::new(1920, 1080));
        // Half-size preview: ¼.
        assert_eq!(decode_size(&placement(1920.0, 1080.0, 1.0), canvas, SizeU::new(960, 540), disp), SizeU::new(960, 540));
        // Slightly more than ½ rounds up to ¾.
        assert_eq!(decode_size(&placement(1920.0, 1080.0, 1.01), canvas, SizeU::new(1920, 1080), disp), SizeU::new(2880, 1620));
        // Zoomed in beyond the source: capped at the display size.
        assert_eq!(decode_size(&placement(1920.0, 1080.0, 3.0), canvas, SizeU::new(1920, 1080), disp), disp);
        // Tiny picture-in-picture: ⅛ at least, sides ≥ 1; negative scale (flip) counts by magnitude.
        assert_eq!(decode_size(&placement(1920.0, 1080.0, -0.01), canvas, SizeU::new(1920, 1080), disp), SizeU::new(480, 270));
        assert_eq!(decode_size(&placement(4.0, 4.0, 0.01), SizeU::new(4, 4), SizeU::new(4, 4), SizeU::new(5, 3)), SizeU::new(1, 1));
        // ⅜ of an odd size rounds up.
        assert_eq!(decode_size(&placement(100.0, 50.0, 0.3), SizeU::new(100, 50), SizeU::new(100, 50), SizeU::new(101, 51)), SizeU::new(38, 20));
    }

    #[test]
    fn decode_size_uses_the_larger_axis_of_a_non_uniform_scale() {
        let p = Placement { scale: Vec2::new(0.2, 0.6), ..placement(1920.0, 1080.0, 1.0) };
        assert_eq!(decode_size(&p, SizeU::new(1920, 1080), SizeU::new(1920, 1080), SizeU::new(1920, 1080)), SizeU::new(1440, 810));
    }
}
