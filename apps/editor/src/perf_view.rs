//! JSON view of preview telemetry for MCP `get_perf` (render spec §10).

use kadr_core::perf::{PerfSummary, Stats};
use serde_json::{json, Value};
use std::time::Duration;

/// Milliseconds with microsecond precision.
fn ms(d: Duration) -> f64 {
    d.as_micros() as f64 / 1000.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn stats(s: &Stats) -> Value {
    json!({"count": s.count, "p50": ms(s.p50), "p90": ms(s.p90), "p99": ms(s.p99), "max": ms(s.max)})
}

/// Share of frames dropped since the reset, in percent with 2 decimals.
fn dropped_pct(s: &PerfSummary) -> f64 {
    if s.total_frames == 0 {
        return 0.0;
    }
    (s.total_dropped as f64 / s.total_frames as f64 * 10_000.0).round() / 100.0
}

pub fn summary_json(s: &PerfSummary) -> Value {
    json!({
        "frames": s.frames,
        "dropped": s.dropped,
        "total_frames": s.total_frames,
        "total_dropped": s.total_dropped,
        "dropped_pct": dropped_pct(s),
        "total_ms": stats(&s.total),
        "decode_ms": stats(&s.decode),
        "composite_ms": stats(&s.composite),
        "present_ms": stats(&s.present),
        "seek_ms": stats(&s.seek),
        "frame_allocs_per_frame": round3(s.frame_allocs_per_frame),
        "frame_copies_per_frame": round3(s.frame_copies_per_frame),
        "mb_copied_per_frame": round3(s.bytes_copied_per_frame / 1_048_576.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::perf::{FramePerf, PerfRing};
    use std::time::Duration;

    #[test]
    fn summary_is_reported_in_milliseconds_with_per_frame_counters() {
        let ring = PerfRing::new(10);
        ring.push(FramePerf { total: Duration::from_micros(12_345), present: Duration::from_millis(2), frame_copies: 1, bytes_copied: 2 * 1_048_576, ..Default::default() });
        ring.push(FramePerf { dropped: true, ..Default::default() });
        let v = summary_json(&ring.summary());
        assert_eq!(v["frames"], 2);
        assert_eq!(v["dropped"], 1);
        assert_eq!((v["total_frames"].clone(), v["total_dropped"].clone()), (json!(2), json!(1)));
        assert_eq!(v["dropped_pct"], 50.0);
        assert_eq!(v["total_ms"]["p50"], 12.345);
        assert_eq!(v["present_ms"]["max"], 2.0);
        assert_eq!(v["frame_copies_per_frame"], 0.5);
        assert_eq!(v["mb_copied_per_frame"], 1.0);
        assert_eq!(v["seek_ms"]["count"], 0);
    }

    #[test]
    fn totals_outlive_the_window_and_dropped_pct_is_of_total_frames() {
        let ring = PerfRing::new(4);
        for i in 0..300 {
            ring.push(FramePerf { dropped: i < 2, ..Default::default() });
        }
        let v = summary_json(&ring.take_summary());
        assert_eq!((v["frames"].clone(), v["dropped"].clone()), (json!(4), json!(0)));
        assert_eq!((v["total_frames"].clone(), v["total_dropped"].clone()), (json!(300), json!(2)));
        assert_eq!(v["dropped_pct"], 0.67);
        assert_eq!(summary_json(&ring.summary())["dropped_pct"], 0.0, "no frames: 0, not NaN");
    }
}
