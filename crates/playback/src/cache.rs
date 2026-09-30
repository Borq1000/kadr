//! Decoded source frames, LRU by bytes (render spec §9), plus what the
//! decoder sessions learned about each media (where its stream really ends,
//! whether it fails). One mutex and one condvar guard all of it, so a
//! waiter re-checks "is my frame here, did it fail, was I superseded" under
//! the same lock every producer notifies under — no lost wakeups.
//!
//! Evicted frames drop their `Arc`; the buffer returns to its `FramePool`
//! as soon as nobody else (renderer, display) holds it.
//!
//! Forgetting a media (relink, replaced file) starts a new *epoch* for it.
//! A decoder session writes with the epoch it started in, and what it
//! produces or learns after its media was forgotten — a frame whose read was
//! in flight, an end of stream, a failure — is refused, so the old file
//! never speaks for the new one.

use kadr_core::{AssetId, CpuFrame};
use kadr_scene::SizeU;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One decoded source frame: which media, at which decode size, which
/// source frame index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub media: AssetId,
    pub size: SizeU,
    pub frame: i64,
}

struct Entry {
    frame: Arc<CpuFrame>,
    /// Time the session spent producing it; reported once, with the first
    /// frame it is delivered for.
    decode: Duration,
    reported: bool,
    tick: u64,
}

pub(crate) struct Board {
    entries: HashMap<FrameKey, Entry>,
    /// Oldest first.
    lru: BTreeMap<u64, FrameKey>,
    tick: u64,
    bytes: usize,
    max_bytes: usize,
    /// Keys someone is waiting for: never evicted, so a frame cannot vanish
    /// between its insertion and the waiter waking up.
    pins: HashMap<FrameKey, u32>,
    /// Per media: frames at or after this index do not exist (the stream
    /// ended earlier than its duration said).
    eof: HashMap<AssetId, i64>,
    /// Per media: the last decode failure (sequence number, when).
    failures: HashMap<AssetId, (u64, Instant)>,
    /// Per frame a session was producing when it failed: that failure's
    /// sequence number (export counts its retries per frame by it).
    key_failures: HashMap<FrameKey, u64>,
    /// Media an export gave up on after retrying: its later requests are
    /// tried once, without retries, until a frame of it decodes again.
    gave_up: HashSet<AssetId>,
    fail_seq: u64,
    /// Per media: how often it was forgotten; plus how often everything was.
    epochs: HashMap<AssetId, u64>,
    clears: u64,
}

impl Board {
    pub(crate) fn get(&mut self, key: &FrameKey) -> Option<Arc<CpuFrame>> {
        let tick = self.tick + 1;
        let e = self.entries.get_mut(key)?;
        self.tick = tick;
        self.lru.remove(&e.tick);
        e.tick = tick;
        self.lru.insert(tick, *key);
        Some(e.frame.clone())
    }

    pub(crate) fn contains(&self, key: &FrameKey) -> bool {
        self.entries.contains_key(key)
    }

    /// The decode time behind `key`, the first time it is asked for.
    pub(crate) fn take_decode(&mut self, key: &FrameKey) -> Duration {
        match self.entries.get_mut(key) {
            Some(e) if !e.reported => {
                e.reported = true;
                e.decode
            }
            _ => Duration::ZERO,
        }
    }

    /// Inserts unless present; returns frames to drop outside the lock
    /// (evicted ones, or `frame` itself when the key was already there).
    pub(crate) fn insert(&mut self, key: FrameKey, frame: Arc<CpuFrame>, decode: Duration) -> Vec<Arc<CpuFrame>> {
        if self.entries.contains_key(&key) {
            return vec![frame];
        }
        if self.eof.get(&key.media).is_some_and(|&end| key.frame >= end) {
            // A frame beyond a recorded end: that record was wrong (a glitch), forget it.
            self.eof.remove(&key.media);
        }
        // It decodes again.
        self.gave_up.remove(&key.media);
        self.tick += 1;
        self.bytes += frame.byte_len();
        self.lru.insert(self.tick, key);
        self.entries.insert(key, Entry { frame, decode, reported: false, tick: self.tick });
        self.evict(Some(key))
    }

    fn evict(&mut self, keep: Option<FrameKey>) -> Vec<Arc<CpuFrame>> {
        let mut out = vec![];
        if self.bytes <= self.max_bytes {
            return out;
        }
        let victims: Vec<(u64, FrameKey)> = {
            let mut over = self.bytes - self.max_bytes;
            let mut v = vec![];
            for (&tick, key) in &self.lru {
                if over == 0 {
                    break;
                }
                if Some(*key) == keep || self.pins.contains_key(key) {
                    continue;
                }
                let len = self.entries[key].frame.byte_len();
                over = over.saturating_sub(len);
                v.push((tick, *key));
            }
            v
        };
        for (tick, key) in victims {
            self.lru.remove(&tick);
            if let Some(e) = self.entries.remove(&key) {
                self.bytes -= e.frame.byte_len();
                out.push(e.frame);
            }
        }
        out
    }

    pub(crate) fn pin(&mut self, key: FrameKey) {
        *self.pins.entry(key).or_default() += 1;
    }

    pub(crate) fn unpin(&mut self, key: FrameKey) -> Vec<Arc<CpuFrame>> {
        if let Some(n) = self.pins.get_mut(&key) {
            *n -= 1;
            if *n == 0 {
                self.pins.remove(&key);
            }
        }
        self.evict(None)
    }

    /// `want` with its frame clamped to the last frame the stream really has.
    pub(crate) fn clamp_eof(&self, want: FrameKey) -> FrameKey {
        match self.eof.get(&want.media) {
            Some(&end) if want.frame >= end => FrameKey { frame: (end - 1).max(0), ..want },
            _ => want,
        }
    }

    pub(crate) fn beyond_eof(&self, media: AssetId, frame: i64) -> bool {
        self.eof.get(&media).is_some_and(|&end| frame >= end)
    }

    pub(crate) fn record_eof(&mut self, media: AssetId, end: i64) {
        let e = self.eof.entry(media).or_insert(end);
        *e = (*e).min(end);
    }

    pub(crate) fn record_failure(&mut self, media: AssetId) {
        self.fail_seq += 1;
        self.failures.insert(media, (self.fail_seq, Instant::now()));
    }

    /// A failure of `media` (as [`Board::record_failure`]) while producing
    /// the frames in `keys` (all of `media`).
    pub(crate) fn record_failure_at(&mut self, media: AssetId, keys: &[FrameKey]) {
        self.record_failure(media);
        for k in keys {
            self.key_failures.insert(*k, self.fail_seq);
        }
    }

    /// Producing `key` failed after sequence number `seq` was current.
    pub(crate) fn key_failed_since(&self, key: &FrameKey, seq: u64) -> bool {
        self.key_failures.get(key).is_some_and(|&s| s > seq)
    }

    pub(crate) fn give_up(&mut self, media: AssetId) {
        self.gave_up.insert(media);
    }

    pub(crate) fn gave_up(&self, media: AssetId) -> bool {
        self.gave_up.contains(&media)
    }

    pub(crate) fn fail_seq(&self) -> u64 {
        self.fail_seq
    }

    /// The current epoch of `media`: it changes whenever the media is
    /// forgotten ([`FrameCache::remove_media`], [`FrameCache::clear`]).
    pub(crate) fn epoch(&self, media: AssetId) -> u64 {
        self.clears + self.epochs.get(&media).copied().unwrap_or(0)
    }

    /// Failed within the last `window` (requests then answer at once instead of retrying).
    pub(crate) fn failed_recently(&self, media: AssetId, window: Duration) -> bool {
        self.failures.get(&media).is_some_and(|(_, at)| at.elapsed() < window)
    }

    /// Failed after sequence number `seq` was current.
    pub(crate) fn failed_since(&self, media: AssetId, seq: u64) -> bool {
        self.failures.get(&media).is_some_and(|&(s, _)| s > seq)
    }

    fn remove_where(&mut self, keep: impl Fn(&FrameKey) -> bool) -> Vec<Arc<CpuFrame>> {
        let gone: Vec<FrameKey> = self.entries.keys().filter(|k| !keep(k)).copied().collect();
        let mut out = vec![];
        for k in gone {
            if let Some(e) = self.entries.remove(&k) {
                self.lru.remove(&e.tick);
                self.bytes -= e.frame.byte_len();
                out.push(e.frame);
            }
        }
        out
    }
}

/// Decoded frames shared by the resolver and its decoder sessions.
pub struct FrameCache {
    board: Mutex<Board>,
    cv: Condvar,
}

impl FrameCache {
    /// A cache holding at most `max_bytes` of frames (a single frame larger
    /// than that is still kept while it is the newest).
    pub fn new(max_bytes: usize) -> Self {
        FrameCache {
            board: Mutex::new(Board {
                entries: HashMap::new(),
                lru: BTreeMap::new(),
                tick: 0,
                bytes: 0,
                max_bytes,
                pins: HashMap::new(),
                eof: HashMap::new(),
                failures: HashMap::new(),
                key_failures: HashMap::new(),
                gave_up: HashSet::new(),
                fail_seq: 0,
                epochs: HashMap::new(),
                clears: 0,
            }),
            cv: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Board> {
        self.board.lock()
    }

    /// Waits on the shared condvar until notified or `until`.
    pub(crate) fn wait_until(&self, guard: &mut MutexGuard<'_, Board>, until: Instant) {
        self.cv.wait_until(guard, until);
    }

    /// Wakes every waiter (a frame arrived, a stream ended or failed, a
    /// generation was superseded). Taking the lock first orders this after
    /// any waiter's check, so the wakeup cannot be lost.
    pub(crate) fn notify(&self) {
        drop(self.board.lock());
        self.cv.notify_all();
    }

    /// Runs `f` under the lock, drops what it returns outside the lock and
    /// wakes waiters.
    pub(crate) fn update<R>(&self, f: impl FnOnce(&mut Board) -> (R, Vec<Arc<CpuFrame>>)) -> R {
        let (r, dropped) = {
            let mut b = self.board.lock();
            f(&mut b)
        };
        self.cv.notify_all();
        drop(dropped);
        r
    }

    pub fn get(&self, key: &FrameKey) -> Option<Arc<CpuFrame>> {
        self.board.lock().get(key)
    }

    pub fn contains(&self, key: &FrameKey) -> bool {
        self.board.lock().contains(key)
    }

    /// Adds a frame (kept as is if the key is already cached) and wakes waiters.
    pub fn insert(&self, key: FrameKey, frame: Arc<CpuFrame>, decode: Duration) {
        self.update(|b| ((), b.insert(key, frame, decode)));
    }

    /// [`FrameCache::insert`] by a producer that started in `epoch` of the
    /// key's media: refused (false) if the media was forgotten since.
    pub(crate) fn insert_from(&self, epoch: u64, key: FrameKey, frame: Arc<CpuFrame>, decode: Duration) -> bool {
        self.update(|b| if b.epoch(key.media) == epoch { (true, b.insert(key, frame, decode)) } else { (false, vec![frame]) })
    }

    /// Waits until `key` is cached, or until `deadline` (`None` = forever).
    pub fn wait_for(&self, key: &FrameKey, deadline: Option<Instant>) -> Option<Arc<CpuFrame>> {
        let mut b = self.board.lock();
        b.pin(*key);
        let found = loop {
            if let Some(f) = b.get(key) {
                break Some(f);
            }
            match deadline {
                Some(d) if Instant::now() >= d => break None,
                Some(d) => {
                    self.cv.wait_until(&mut b, d);
                }
                None => self.cv.wait(&mut b),
            }
        };
        let dropped = b.unpin(*key);
        drop(b);
        drop(dropped);
        found
    }

    /// Bytes of frames held.
    pub fn bytes(&self) -> usize {
        self.board.lock().bytes
    }

    pub fn len(&self) -> usize {
        self.board.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn max_bytes(&self) -> usize {
        self.board.lock().max_bytes
    }

    /// Forgets everything about `media` (relinked or replaced file),
    /// including what decoding in flight for it would still deliver.
    pub fn remove_media(&self, media: AssetId) {
        self.update(|b| {
            *b.epochs.entry(media).or_default() += 1;
            b.eof.remove(&media);
            b.failures.remove(&media);
            b.key_failures.retain(|k, _| k.media != media);
            b.gave_up.remove(&media);
            ((), b.remove_where(|k| k.media != media))
        });
    }

    /// Forgets everything, including what decoding in flight would still deliver.
    pub fn clear(&self) {
        self.update(|b| {
            b.clears += 1;
            b.eof.clear();
            b.failures.clear();
            b.key_failures.clear();
            b.gave_up.clear();
            ((), b.remove_where(|_| false))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{ColorInfo, FramePool};

    fn frame(pool: &FramePool) -> Arc<CpuFrame> {
        Arc::new(CpuFrame::rgba8(pool, 4, 4, ColorInfo::WORKING_SDR)) // 64 bytes
    }

    fn key(media: AssetId, frame: i64) -> FrameKey {
        FrameKey { media, size: SizeU::new(4, 4), frame }
    }

    #[test]
    fn evicts_least_recently_used_by_bytes_and_returns_buffers_to_the_pool() {
        let pool = FramePool::new(1 << 20);
        let cache = FrameCache::new(64 * 3);
        let m = AssetId::new();
        for i in 0..3 {
            cache.insert(key(m, i), frame(&pool), Duration::ZERO);
        }
        assert!(cache.get(&key(m, 0)).is_some(), "touch 0: now 1 is the oldest");
        cache.insert(key(m, 3), frame(&pool), Duration::ZERO);
        assert!(!cache.contains(&key(m, 1)));
        assert!(cache.contains(&key(m, 0)) && cache.contains(&key(m, 2)) && cache.contains(&key(m, 3)));
        assert_eq!(cache.bytes(), 64 * 3);
        assert_eq!(pool.free_bytes(), 64, "the evicted buffer went back to the pool");
        let _again = frame(&pool);
        assert_eq!(pool.allocations(), 4, "and is reused");
    }

    #[test]
    fn pinned_keys_survive_eviction_until_unpinned() {
        let pool = FramePool::new(1 << 20);
        let cache = FrameCache::new(64);
        let m = AssetId::new();
        cache.lock().pin(key(m, 0));
        cache.insert(key(m, 0), frame(&pool), Duration::ZERO);
        cache.insert(key(m, 1), frame(&pool), Duration::ZERO);
        assert!(cache.contains(&key(m, 0)), "pinned");
        let dropped = cache.lock().unpin(key(m, 0));
        drop(dropped);
        assert!(!cache.contains(&key(m, 0)), "evicted once unpinned: over budget");
    }

    #[test]
    fn decode_time_is_reported_once() {
        let pool = FramePool::new(1 << 20);
        let cache = FrameCache::new(1 << 20);
        let m = AssetId::new();
        cache.insert(key(m, 0), frame(&pool), Duration::from_millis(7));
        let mut b = cache.lock();
        assert_eq!(b.take_decode(&key(m, 0)), Duration::from_millis(7));
        assert_eq!(b.take_decode(&key(m, 0)), Duration::ZERO);
    }

    #[test]
    fn eof_clamps_and_a_later_frame_beyond_it_clears_it() {
        let pool = FramePool::new(1 << 20);
        let cache = FrameCache::new(1 << 20);
        let m = AssetId::new();
        cache.update(|b| {
            b.record_eof(m, 90);
            b.record_eof(m, 95);
            ((), vec![])
        });
        assert_eq!(cache.lock().clamp_eof(key(m, 120)).frame, 89);
        assert_eq!(cache.lock().clamp_eof(key(m, 50)).frame, 50);
        cache.insert(key(m, 91), frame(&pool), Duration::ZERO);
        assert!(!cache.lock().beyond_eof(m, 120));
    }

    #[test]
    fn a_forgotten_media_refuses_frames_of_its_old_epoch() {
        let pool = FramePool::new(1 << 20);
        let cache = FrameCache::new(1 << 20);
        let (m, other) = (AssetId::new(), AssetId::new());
        let e = cache.lock().epoch(m);
        let e_other = cache.lock().epoch(other);
        assert!(cache.insert_from(e, key(m, 0), frame(&pool), Duration::ZERO));
        cache.remove_media(m);
        assert!(!cache.contains(&key(m, 0)));
        assert!(!cache.insert_from(e, key(m, 1), frame(&pool), Duration::ZERO), "in flight before the relink");
        assert!(!cache.contains(&key(m, 1)));
        let e = cache.lock().epoch(m);
        assert!(cache.insert_from(e, key(m, 1), frame(&pool), Duration::ZERO));
        assert!(cache.insert_from(e_other, key(other, 0), frame(&pool), Duration::ZERO), "other media unaffected");
        cache.clear();
        assert!(!cache.insert_from(e_other, key(other, 1), frame(&pool), Duration::ZERO), "clear forgets everything");
    }

    #[test]
    fn wait_for_wakes_on_insert_and_times_out() {
        let pool = FramePool::new(1 << 20);
        let cache = Arc::new(FrameCache::new(1 << 20));
        let m = AssetId::new();
        assert!(cache.wait_for(&key(m, 5), Some(Instant::now() + Duration::from_millis(20))).is_none());
        let c = cache.clone();
        let f = frame(&pool);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            c.insert(key(m, 5), f, Duration::ZERO);
        });
        assert!(cache.wait_for(&key(m, 5), None).is_some());
        t.join().unwrap();
    }
}
