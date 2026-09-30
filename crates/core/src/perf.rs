//! Frame-level performance telemetry (render spec §10): what producing one
//! displayed or encoded frame cost, stage by stage. Collected by the
//! playback layer; read by the DEV overlay, MCP `get_perf` and benchmarks.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// Decode wait attributed to one layer. `layer` is an opaque key (e.g. the
/// low 64 bits of a clip id); 0 when there is a single source.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayerTiming {
    pub layer: u64,
    pub time: Duration,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FramePerf {
    pub total: Duration,
    pub decode: Vec<LayerTiming>,
    pub evaluate: Duration,
    pub resolve: Duration,
    pub upload: Duration,
    pub composite: Duration,
    pub effects: Duration,
    pub present: Duration,
    pub cache_hits: u32,
    pub cache_misses: u32,
    /// Produced but never shown (late, or for a request that was superseded).
    pub dropped: bool,
    /// Request → first frame shown, for a seek or scrub.
    pub seek_latency: Option<Duration>,
    /// Buffers holding a whole frame allocated for this frame.
    pub frame_allocs: u32,
    /// Whole-frame memory copies made for this frame, and their bytes.
    pub frame_copies: u32,
    pub bytes_copied: u64,
}

impl FramePerf {
    pub fn decode_total(&self) -> Duration {
        self.decode.iter().map(|d| d.time).sum()
    }
}

/// Nearest-rank percentiles of a set of durations.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub count: usize,
    pub p50: Duration,
    pub p90: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Stats {
    pub fn of(mut v: Vec<Duration>) -> Stats {
        if v.is_empty() {
            return Stats::default();
        }
        v.sort_unstable();
        let n = v.len();
        let rank = |p: f64| v[((p * n as f64).ceil() as usize).clamp(1, n) - 1];
        Stats { count: n, p50: rank(0.50), p90: rank(0.90), p99: rank(0.99), max: v[n - 1] }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerfSummary {
    pub frames: usize,
    pub dropped: usize,
    /// Shown frames only.
    pub total: Stats,
    /// All frames, dropped included: decoding them was paid for.
    pub decode: Stats,
    pub composite: Stats,
    pub present: Stats,
    pub seek: Stats,
    pub frame_allocs_per_frame: f64,
    pub frame_copies_per_frame: f64,
    pub bytes_copied_per_frame: f64,
}

/// The last `capacity` frames; safe to push from any thread.
pub struct PerfRing {
    capacity: usize,
    frames: Mutex<VecDeque<FramePerf>>,
}

impl PerfRing {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        PerfRing { capacity, frames: Mutex::new(VecDeque::with_capacity(capacity)) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<FramePerf>> {
        self.frames.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, f: FramePerf) {
        let mut q = self.lock();
        if q.len() == self.capacity {
            q.pop_front();
        }
        q.push_back(f);
    }

    pub fn snapshot(&self) -> Vec<FramePerf> {
        self.lock().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    pub fn summary(&self) -> PerfSummary {
        let frames = self.snapshot();
        let n = frames.len();
        let per = |sum: f64| if n == 0 { 0.0 } else { sum / n as f64 };
        let shown = || frames.iter().filter(|f| !f.dropped);
        PerfSummary {
            frames: n,
            dropped: frames.iter().filter(|f| f.dropped).count(),
            total: Stats::of(shown().map(|f| f.total).collect()),
            decode: Stats::of(frames.iter().map(FramePerf::decode_total).collect()),
            composite: Stats::of(shown().map(|f| f.composite).collect()),
            present: Stats::of(shown().map(|f| f.present).collect()),
            seek: Stats::of(frames.iter().filter_map(|f| f.seek_latency).collect()),
            frame_allocs_per_frame: per(frames.iter().map(|f| f.frame_allocs as f64).sum()),
            frame_copies_per_frame: per(frames.iter().map(|f| f.frame_copies as f64).sum()),
            bytes_copied_per_frame: per(frames.iter().map(|f| f.bytes_copied as f64).sum()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn stats_use_nearest_rank_percentiles() {
        let s = Stats::of((1..=100).map(ms).collect());
        assert_eq!((s.count, s.p50, s.p90, s.p99, s.max), (100, ms(50), ms(90), ms(99), ms(100)));
        assert_eq!(Stats::of(vec![]), Stats::default());
        let one = Stats::of(vec![ms(7)]);
        assert_eq!((one.p50, one.p99, one.max), (ms(7), ms(7), ms(7)));
    }

    #[test]
    fn ring_keeps_the_newest_frames() {
        let ring = PerfRing::new(3);
        for i in 1..=5 {
            ring.push(FramePerf { total: ms(i), ..Default::default() });
        }
        let totals: Vec<Duration> = ring.snapshot().iter().map(|f| f.total).collect();
        assert_eq!(totals, vec![ms(3), ms(4), ms(5)]);
        ring.clear();
        assert!(ring.snapshot().is_empty());
    }

    #[test]
    fn summary_separates_dropped_frames_and_averages_counters() {
        let ring = PerfRing::new(10);
        ring.push(FramePerf {
            total: ms(10),
            decode: vec![LayerTiming { layer: 1, time: ms(3) }, LayerTiming { layer: 2, time: ms(1) }],
            present: ms(2),
            frame_allocs: 1,
            frame_copies: 2,
            bytes_copied: 100,
            ..Default::default()
        });
        ring.push(FramePerf { decode: vec![LayerTiming { layer: 1, time: ms(6) }], dropped: true, frame_allocs: 1, ..Default::default() });
        ring.push(FramePerf { total: ms(30), seek_latency: Some(ms(30)), ..Default::default() });
        let s = ring.summary();
        assert_eq!((s.frames, s.dropped), (3, 1));
        assert_eq!(s.total.count, 2, "dropped frames are not in frame-time stats");
        assert_eq!(s.decode.count, 3, "decode work counts even when the frame is dropped");
        assert_eq!(s.decode.max, ms(6));
        assert_eq!((s.seek.count, s.seek.p50), (1, ms(30)));
        assert!((s.frame_allocs_per_frame - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.frame_copies_per_frame - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.bytes_copied_per_frame - 100.0 / 3.0).abs() < 1e-9);
    }
}
