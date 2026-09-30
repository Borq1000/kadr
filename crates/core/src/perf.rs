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
    /// Frames / dropped frames in the window (the last `capacity` frames).
    pub frames: usize,
    pub dropped: usize,
    /// Frames / dropped frames since the last reset; not limited to the window.
    pub total_frames: u64,
    pub total_dropped: u64,
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

/// The one ring size shared by the preview and its readers (~20 s at 30 fps).
pub const PERF_RING_FRAMES: usize = 600;

#[derive(Default)]
struct RingState {
    window: VecDeque<FramePerf>,
    /// Frames pushed / dropped since the last reset; they do not roll off with the window.
    total_frames: u64,
    total_dropped: u64,
}

/// The last `capacity` frames, plus cumulative counters since the last
/// reset; safe to push from any thread.
pub struct PerfRing {
    capacity: usize,
    state: Mutex<RingState>,
}

impl PerfRing {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        PerfRing {
            capacity,
            state: Mutex::new(RingState { window: VecDeque::with_capacity(capacity), ..Default::default() }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, f: FramePerf) {
        let mut st = self.lock();
        st.total_frames += 1;
        st.total_dropped += u64::from(f.dropped);
        if st.window.len() == self.capacity {
            st.window.pop_front();
        }
        st.window.push_back(f);
    }

    pub fn snapshot(&self) -> Vec<FramePerf> {
        self.lock().window.iter().cloned().collect()
    }

    /// Empties the window and zeroes the cumulative counters.
    pub fn clear(&self) {
        let mut st = self.lock();
        st.window.clear();
        st.total_frames = 0;
        st.total_dropped = 0;
    }

    pub fn summary(&self) -> PerfSummary {
        summarize(&self.lock())
    }

    /// `summary()` followed by `clear()` under one lock: a frame pushed
    /// concurrently lands either in this summary or in the next, never both
    /// and never neither.
    pub fn take_summary(&self) -> PerfSummary {
        let mut st = self.lock();
        let s = summarize(&st);
        st.window.clear();
        st.total_frames = 0;
        st.total_dropped = 0;
        s
    }
}

fn summarize(st: &RingState) -> PerfSummary {
    let frames = &st.window;
    let n = frames.len();
    let per = |sum: f64| if n == 0 { 0.0 } else { sum / n as f64 };
    let shown = || frames.iter().filter(|f| !f.dropped);
    PerfSummary {
        frames: n,
        dropped: frames.iter().filter(|f| f.dropped).count(),
        total_frames: st.total_frames,
        total_dropped: st.total_dropped,
        total: Stats::of(shown().map(|f| f.total).collect()),
        decode: Stats::of(frames.iter().map(FramePerf::decode_total).collect()),
        composite: Stats::of(shown().map(|f| f.composite).collect()),
        present: Stats::of(shown().map(|f| f.present).collect()),
        seek: Stats::of(shown().filter_map(|f| f.seek_latency).collect()),
        frame_allocs_per_frame: per(frames.iter().map(|f| f.frame_allocs as f64).sum()),
        frame_copies_per_frame: per(frames.iter().map(|f| f.frame_copies as f64).sum()),
        bytes_copied_per_frame: per(frames.iter().map(|f| f.bytes_copied as f64).sum()),
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

    #[test]
    fn seek_stats_exclude_dropped_frames() {
        let ring = PerfRing::new(10);
        ring.push(FramePerf { total: ms(20), seek_latency: Some(ms(20)), ..Default::default() });
        ring.push(FramePerf { dropped: true, seek_latency: Some(ms(500)), ..Default::default() });
        let s = ring.summary();
        assert_eq!((s.seek.count, s.seek.max), (1, ms(20)));
    }

    #[test]
    fn cumulative_counters_survive_the_window_rolling_over() {
        let ring = PerfRing::new(3);
        for i in 0..10 {
            ring.push(FramePerf { dropped: i % 5 == 0, ..Default::default() });
        }
        let s = ring.summary();
        assert_eq!((s.frames, s.dropped), (3, 0), "window: frames 7..=9, none dropped");
        assert_eq!((s.total_frames, s.total_dropped), (10, 2), "frames 0 and 5 dropped");
        ring.clear();
        let s = ring.summary();
        assert_eq!((s.frames, s.total_frames, s.total_dropped), (0, 0, 0), "clear resets the counters too");
    }

    #[test]
    fn take_summary_reads_then_resets() {
        let ring = PerfRing::new(2);
        for i in 0..5 {
            ring.push(FramePerf { dropped: i == 0, total: ms(i), ..Default::default() });
        }
        let s = ring.take_summary();
        assert_eq!((s.frames, s.total_frames, s.total_dropped), (2, 5, 1));
        assert_eq!(ring.take_summary(), PerfSummary::default(), "nothing carried over");
        ring.push(FramePerf::default());
        assert_eq!(ring.take_summary().total_frames, 1);
    }

    #[test]
    fn take_summary_loses_and_double_counts_no_frame_under_concurrent_pushes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        const PUSHED: u64 = 20_000;
        let ring = Arc::new(PerfRing::new(16));
        let done = Arc::new(AtomicBool::new(false));
        let pusher = {
            let (ring, done) = (ring.clone(), done.clone());
            std::thread::spawn(move || {
                for i in 0..PUSHED {
                    ring.push(FramePerf { dropped: i % 7 == 0, ..Default::default() });
                }
                done.store(true, Ordering::SeqCst);
            })
        };
        let (mut frames, mut dropped) = (0u64, 0u64);
        while !done.load(Ordering::SeqCst) {
            let s = ring.take_summary();
            frames += s.total_frames;
            dropped += s.total_dropped;
            std::thread::yield_now();
        }
        pusher.join().unwrap();
        let s = ring.take_summary();
        frames += s.total_frames;
        dropped += s.total_dropped;
        assert_eq!(frames, PUSHED);
        assert_eq!(dropped, PUSHED.div_ceil(7));
    }
}
