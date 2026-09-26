//! Locally executed "AI" operations — deterministic, offline, $0.

use crate::command::AiCommand;
use crate::plan::{Plan, PlanItem};
use crate::tier::Tier;
use kadr_core::{ActionId, AssetId, Time, TimeRange};
use kadr_i18n::{duration, t, tf, tn};
use kadr_project::{AnalysisData, Project, TrackKind};
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub enum LocalOpError {
    /// Some audio has not been analysed yet (analysis jobs still running).
    AnalysisPending(Vec<AssetId>),
    NothingToAnalyse,
}

/// Merges overlapping/adjacent ranges.
pub fn union(mut v: Vec<TimeRange>) -> Vec<TimeRange> {
    v.sort_by_key(|r| r.start);
    let mut out: Vec<TimeRange> = vec![];
    for r in v.into_iter().filter(|r| !r.is_empty()) {
        match out.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// `a \ b` for sorted, disjoint range lists.
pub fn subtract(a: &[TimeRange], b: &[TimeRange]) -> Vec<TimeRange> {
    let mut out = vec![];
    for r in a {
        let mut cur = r.start;
        for x in b.iter().filter(|x| x.overlaps(r)) {
            if x.start > cur {
                out.push(TimeRange::new(cur, x.start));
            }
            cur = cur.max(x.end);
        }
        if cur < r.end {
            out.push(TimeRange::new(cur, r.end));
        }
    }
    out
}

/// Timeline ranges where every audible audio clip is silent.
pub fn timeline_silences(project: &Project) -> Result<Vec<TimeRange>, LocalOpError> {
    let seq = project.sequence();
    let silence: HashMap<AssetId, &Vec<TimeRange>> = project
        .analysis
        .iter()
        .filter_map(|a| match &a.data {
            AnalysisData::Silence { ranges, .. } => Some((a.asset, ranges)),
            _ => None,
        })
        .collect();

    let mut covered = vec![];
    let mut loud = vec![];
    let mut pending = vec![];
    for t in seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio && seq.track_audible(t)) {
        for c in t.clips.iter().filter(|c| c.enabled) {
            let Some(ranges) = silence.get(&c.asset) else {
                if !pending.contains(&c.asset) {
                    pending.push(c.asset);
                }
                continue;
            };
            let clip_range = c.timeline_range();
            covered.push(clip_range);
            let src = c.source_range();
            let silent_tl: Vec<TimeRange> = ranges
                .iter()
                .filter_map(|r| r.intersect(&src))
                .map(|r| TimeRange::new(c.timeline_time_of(r.start), c.timeline_time_of(r.end)))
                .filter_map(|r| r.intersect(&clip_range))
                .collect();
            loud.extend(subtract(&[clip_range], &union(silent_tl)));
        }
    }
    if !pending.is_empty() {
        return Err(LocalOpError::AnalysisPending(pending));
    }
    if covered.is_empty() {
        return Err(LocalOpError::NothingToAnalyse);
    }
    Ok(subtract(&union(covered), &union(loud)))
}

/// "Удали паузы длиннее N секунд" — plan built entirely from local analysis.
pub fn plan_remove_pauses(project: &Project, prompt: &str, min: Time, padding: Time) -> Result<Plan, LocalOpError> {
    let seq_dur = project.sequence().duration();
    let fr = project.sequence().frame_rate;
    let all = timeline_silences(project)?;
    let long: Vec<TimeRange> = all.iter().copied().filter(|r| r.duration() >= min).collect();
    let short = all.len() - long.len();

    let mut items: Vec<PlanItem> = long
        .iter()
        .filter_map(|r| {
            // Keep a little air around speech/music, except at the very edges.
            let s = if r.start == Time::ZERO { r.start } else { r.start + padding };
            let e = if r.end >= seq_dur { r.end } else { r.end - padding };
            // Quantize exactly like the engine will, so Review shows real numbers.
            let (s, e) = (fr.snap(s), fr.snap(e));
            (s < e).then_some((s, e))
        })
        .map(|(s, e)| PlanItem {
            label: format!("{} – {} ({})", tc(s), tc(e), duration((e - s).as_secs_f64())),
            start: s,
            end: e,
            enabled: true,
            command: AiCommand::DeleteRange {
                sequence_id: Some(project.sequence().id.to_string()),
                start_ms: s.as_millis(),
                end_ms: e.as_millis(),
                ripple: true,
                reason: "long silence".into(),
            },
        })
        .collect();
    // Ripple deletes must run from the end backwards.
    items.sort_by_key(|i| std::cmp::Reverse(i.start));

    let removed: Time = items.iter().map(|i| i.end - i.start).fold(Time::ZERO, |a, b| a + b);
    let n = items.len();
    let d = |t: Time| duration(t.as_secs_f64());
    let mut findings = vec![tn("ai.pauses.found", n as i64, &[("min", &d(min))])];
    if short > 0 {
        findings.push(tn("ai.pauses.kept_short", short as i64, &[]));
    }
    if n > 0 {
        findings.push(tf("ai.pauses.total", &[("d", &d(removed))]));
    }
    let proposal = if n == 0 { t("ai.pauses.nothing") } else { tn("ai.pauses.proposal", n as i64, &[("pad", &d(padding))]) };
    Ok(Plan {
        id: ActionId::new(),
        prompt: prompt.to_string(),
        title: t("ai.pauses.title"),
        findings,
        proposal,
        items,
        tier: Tier::Local,
        provider: "Local".into(),
        model: "silence-detector v1".into(),
        cost_usd: 0.0,
        confidence: 0.95,
        before_duration: Some(seq_dur),
        after_duration: Some(seq_dur - removed),
    })
}

fn tc(t: Time) -> String {
    let ms = t.as_millis().max(0);
    format!("{:02}:{:02}.{:01}", ms / 60_000, (ms / 1000) % 60, (ms % 1000) / 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(a: i64, b: i64) -> TimeRange {
        TimeRange::new(Time::from_secs(a), Time::from_secs(b))
    }

    #[test]
    fn interval_algebra() {
        assert_eq!(union(vec![r(5, 7), r(0, 2), r(1, 3)]), vec![r(0, 3), r(5, 7)]);
        assert_eq!(subtract(&[r(0, 10)], &[r(2, 3), r(5, 12)]), vec![r(0, 2), r(3, 5)]);
    }

}
