//! Views of preview telemetry (render spec §10): JSON for MCP `get_perf`
//! and the rows of the DEV overlay.

use kadr_core::perf::{FramePerf, PerfSummary, Stats};
use kadr_i18n::{t, tf};
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

/// Share of layer frames served from the cache, in percent; `None` before any.
pub fn cache_hit_pct(frames: &[FramePerf]) -> Option<f64> {
    let (hits, misses) = frames.iter().fold((0u64, 0u64), |(h, m), f| (h + f.cache_hits as u64, m + f.cache_misses as u64));
    (hits + misses > 0).then(|| hits as f64 * 100.0 / (hits + misses) as f64)
}

fn ms1(d: Duration) -> String {
    tf("dev.ms", &[("v", &format!("{:.1}", ms(d)))])
}

/// The DEV overlay's rows (label, value): frames shown in the last second,
/// composite / decode p50, player → screen p50, drops (window and since the
/// reset), cache hits, renderer.
pub fn dev_rows(s: &PerfSummary, frames: &[FramePerf], fps: usize, present: Option<Duration>, renderer: &str) -> Vec<(String, String)> {
    let dash = || "—".to_string();
    let stat = |st: &Stats| if st.count == 0 { dash() } else { ms1(st.p50) };
    vec![
        (t("dev.fps"), fps.to_string()),
        (t("dev.render"), stat(&s.composite)),
        (t("dev.decode"), stat(&s.decode)),
        (t("dev.present"), present.map_or_else(dash, ms1)),
        (
            t("dev.dropped"),
            tf("dev.dropped_value", &[("n", &s.dropped.to_string()), ("of", &s.frames.to_string()), ("total", &s.total_dropped.to_string()), ("total_of", &s.total_frames.to_string())]),
        ),
        (t("dev.cache"), cache_hit_pct(frames).map_or_else(dash, |p| format!("{p:.0} %"))),
        (t("dev.renderer"), renderer.to_string()),
    ]
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
    fn dev_rows_show_medians_drops_and_cache_hits() {
        let ring = PerfRing::new(10);
        ring.push(FramePerf { composite: Duration::from_micros(4_250), cache_hits: 3, cache_misses: 1, decode: vec![kadr_core::perf::LayerTiming { layer: 1, time: Duration::from_millis(9) }], ..Default::default() });
        ring.push(FramePerf { dropped: true, cache_misses: 0, ..Default::default() });
        let rows = dev_rows(&ring.summary(), &ring.snapshot(), 24, Some(Duration::from_millis(3)), "cpu");
        let value = |label: &str| rows.iter().find(|r| r.0 == t(label)).map(|r| r.1.clone()).unwrap();
        assert_eq!(value("dev.fps"), "24");
        // Units come from the catalog of the current language.
        assert_eq!(value("dev.render"), tf("dev.ms", &[("v", "4.2")]));
        assert_eq!(value("dev.present"), tf("dev.ms", &[("v", "3.0")]));
        assert_eq!(value("dev.cache"), "75 %");
        assert!(value("dev.dropped").starts_with("1/2"), "{}", value("dev.dropped"));
        assert_eq!(value("dev.renderer"), "cpu");
        let empty = dev_rows(&PerfRing::new(4).summary(), &[], 0, None, "legacy");
        assert!(empty.iter().filter(|r| r.1 == "—").count() >= 3, "no data yet: dashes, not zeros: {empty:?}");
        assert_eq!(cache_hit_pct(&[]), None);
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
