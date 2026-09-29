//! Benchmark results: rows printed as a markdown table and saved as JSON.

use kadr_core::perf::Stats;
use serde_json::json;
use std::path::PathBuf;

pub struct Row {
    pub scenario: String,
    pub case: String,
    pub metric: String,
    pub value: f64,
    pub unit: &'static str,
}

pub struct Report {
    pub title: String,
    pub rows: Vec<Row>,
}

impl Report {
    pub fn new(title: &str) -> Self {
        Report { title: title.to_string(), rows: vec![] }
    }

    pub fn push(&mut self, scenario: &str, case: &str, metric: &str, value: f64, unit: &'static str) {
        self.rows.push(Row { scenario: scenario.into(), case: case.into(), metric: metric.into(), value, unit });
    }

    /// count, p50, p90 and max (ms) of a duration set.
    pub fn stats(&mut self, scenario: &str, case: &str, s: &Stats) {
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        self.push(scenario, case, "count", s.count as f64, "n");
        self.push(scenario, case, "p50", ms(s.p50), "ms");
        self.push(scenario, case, "p90", ms(s.p90), "ms");
        self.push(scenario, case, "max", ms(s.max), "ms");
    }
}

pub fn markdown(r: &Report) -> String {
    let mut s = String::from("| scenario | case | metric | value | unit |\n|---|---|---|---|---|\n");
    for row in &r.rows {
        s.push_str(&format!("| {} | {} | {} | {:.2} | {} |\n", row.scenario, row.case, row.metric, row.value, row.unit));
    }
    s
}

/// Writes `<exe dir>/bench/<title>-<unix seconds>.json`.
pub fn save(r: &Report) -> std::io::Result<PathBuf> {
    let dir = std::env::current_exe()?.parent().map(|d| d.join("bench")).unwrap_or_else(|| PathBuf::from("bench"));
    std::fs::create_dir_all(&dir)?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("{}-{secs}.json", r.title));
    let rows: Vec<_> = r.rows.iter().map(|x| json!({"scenario": x.scenario, "case": x.case, "metric": x.metric, "value": x.value, "unit": x.unit})).collect();
    std::fs::write(&path, serde_json::to_vec_pretty(&json!({"title": r.title, "rows": rows}))?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::perf::Stats;
    use std::time::Duration;

    #[test]
    fn stats_become_rows_and_rows_a_markdown_table() {
        let mut r = Report::new("t");
        r.push("decode", "h264_1080p30 full", "fps", 123.456, "fps");
        r.stats("seek", "h264_1080p30 half", &Stats::of(vec![Duration::from_millis(10), Duration::from_millis(30)]));
        assert_eq!(r.rows.len(), 5, "1 value + count, p50, p90, max");
        let md = markdown(&r);
        assert!(md.starts_with("| scenario | case | metric | value | unit |"), "{md}");
        assert!(md.contains("| decode | h264_1080p30 full | fps | 123.46 | fps |"), "{md}");
        assert!(md.contains("| seek | h264_1080p30 half | p90 | 30.00 | ms |"), "{md}");
    }
}
