//! Personalisation from the editor's own corrections. Jev is never trained
//! on our data; instead similar past decisions travel in `state` as
//! precedents (research §4.3, §7.5), and a plain frequency prior computed
//! here keeps Jev's tendency to over-sharpen small histories in check.

use kadr_project::{CorrectionKind, DecisionKind, EditorPreferenceEvent, StoredDecision};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// One past decision as Jev sees it in `editor_history`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Precedent {
    pub context: serde_json::Value,
    pub ai_suggested: String,
    pub editor_chose: String,
    #[serde(skip)]
    pub at_ms: i64,
}

/// Taste drifts: the recency part of a correction's weight halves monthly.
const HALF_LIFE_DAYS: f64 = 30.0;

fn kind_of(k: CorrectionKind) -> Option<DecisionKind> {
    match k {
        CorrectionKind::CameraChanged => Some(DecisionKind::CameraPick),
        CorrectionKind::SegmentRestored | CorrectionKind::SegmentDeleted => Some(DecisionKind::ShotUsability),
        _ => None,
    }
}

fn leaves(v: &serde_json::Value, path: String, out: &mut Vec<(String, String)>) {
    match v {
        serde_json::Value::Object(m) => m.iter().for_each(|(k, x)| leaves(x, format!("{path}.{k}"), out)),
        serde_json::Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| leaves(x, format!("{path}[{i}]"), out)),
        x => out.push((path, x.to_string())),
    }
}

/// Number of `a`'s leaf values (same path, same value) found in `b`.
fn similarity(a: &serde_json::Value, b: &HashSet<(String, String)>) -> usize {
    let mut la = vec![];
    leaves(a, String::new(), &mut la);
    la.iter().filter(|x| b.contains(*x)).count()
}

/// The `k` most relevant past decisions of `kind` for `current`: human
/// overrides of Jev decisions and explicit confirmations, ranked by
/// context similarity with recency decay, returned oldest first (newest
/// next to `current` in the state).
pub fn precedents(
    events: &[EditorPreferenceEvent],
    decisions: &[StoredDecision],
    kind: DecisionKind,
    current: &serde_json::Value,
    k: usize,
    now_ms: i64,
) -> Vec<Precedent> {
    let mut all: Vec<Precedent> = events
        .iter()
        .filter(|e| e.decision.is_some() && kind_of(e.kind) == Some(kind))
        .map(|e| Precedent { context: e.context.clone(), ai_suggested: e.ai_choice.clone(), editor_chose: e.human_choice.clone(), at_ms: e.at_ms })
        .collect();
    all.extend(decisions.iter().filter(|d| d.kind == kind && d.human.as_deref() == Some(d.value.as_str())).map(|d| Precedent {
        context: d.features.clone(),
        ai_suggested: d.value.clone(),
        editor_chose: d.value.clone(),
        at_ms: d.at_ms,
    }));
    let mut cur = vec![];
    leaves(current, String::new(), &mut cur);
    let cur: HashSet<(String, String)> = cur.into_iter().collect();
    let score = |p: &Precedent| {
        let age_days = (now_ms - p.at_ms).max(0) as f64 / 86_400_000.0;
        // Relevance leads; age can at most halve it (an old solo still says
        // more about a solo than yesterday's verse).
        similarity(&p.context, &cur) as f64 * (0.5 + 0.5 * 0.5f64.powf(age_days / HALF_LIFE_DAYS))
    };
    // Scored once each: the history can be long and this runs per interval.
    let mut ranked: Vec<(f64, Precedent)> = all.into_iter().map(|p| (score(&p), p)).collect();
    ranked.sort_by(|(sa, a), (sb, b)| sb.total_cmp(sa).then(b.at_ms.cmp(&a.at_ms)));
    let mut top: Vec<Precedent> = ranked.into_iter().take(k).map(|(_, p)| p).collect();
    top.sort_by_key(|p| p.at_ms);
    top
}

/// P(option) from past picks with Dirichlet(α = 1) smoothing.
/// Jev's `editor_pick` distribution blended with the editor's past picks;
/// `None` when Jev itself can't tell (an escape value on top, or the angles
/// hold under half the mass). Precedents sharpen a pick, never invent one.
pub fn personal_pick(p_pick: &BTreeMap<String, f32>, labels: &[String], history: &[Precedent]) -> Option<BTreeMap<String, f32>> {
    let (top, ..) = crate::jev::decided::summarize(p_pick)?;
    let p_jev: BTreeMap<String, f32> = p_pick.iter().filter(|(k, _)| labels.contains(k)).map(|(k, v)| (k.clone(), *v)).collect();
    if crate::jev::decided::ESCAPE_VALUES.contains(&top.as_str()) || p_jev.values().sum::<f32>() < 0.5 {
        return None;
    }
    Some(blend(&p_jev, &frequency_prior(history, labels), blend_weight(history.len())))
}

pub fn frequency_prior(precedents: &[Precedent], options: &[String]) -> BTreeMap<String, f32> {
    let n = precedents.iter().filter(|p| options.contains(&p.editor_chose)).count() as f32;
    let denom = n + options.len() as f32;
    options
        .iter()
        .map(|o| (o.clone(), (precedents.iter().filter(|p| &p.editor_chose == o).count() as f32 + 1.0) / denom))
        .collect()
}

/// How much to trust Jev's in-context guess (vs. the plain frequency of the
/// editor's past picks) given `n_precedents` similar past decisions.
pub fn blend_weight(n_precedents: usize) -> f32 {
    // Few precedents: frequencies are noise, Jev's common sense leads.
    // Many: Jev tends to over-sharpen (3:2 history → 0.76), frequencies lead.
    (4.0 / (4.0 + n_precedents as f32)).clamp(0.3, 1.0)
}

/// `w · p_jev + (1 − w) · p_freq`, renormalised over the union of options.
pub fn blend(p_jev: &BTreeMap<String, f32>, p_freq: &BTreeMap<String, f32>, w: f32) -> BTreeMap<String, f32> {
    let w = w.clamp(0.0, 1.0);
    let mut out: BTreeMap<String, f32> = p_jev.keys().chain(p_freq.keys()).map(|k| (k.clone(), 0.0)).collect();
    for (k, v) in out.iter_mut() {
        *v = w * p_jev.get(k).copied().unwrap_or(0.0) + (1.0 - w) * p_freq.get(k).copied().unwrap_or(0.0);
    }
    let sum: f32 = out.values().sum();
    if sum > 0.0 {
        out.values_mut().for_each(|v| *v /= sum);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{CorrectionKind, DecisionKind, EditorPreferenceEvent};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn event(section: &str, ai: &str, human: &str, at_ms: i64) -> EditorPreferenceEvent {
        EditorPreferenceEvent {
            at_ms,
            action: None,
            kind: CorrectionKind::CameraChanged,
            context: json!({"music_section": section, "singer_singing": false}),
            features: json!({}),
            ai_choice: ai.into(),
            ai_confidence: 0.9,
            human_choice: human.into(),
            decision: Some(kadr_core::DecisionId::new()),
        }
    }

    fn p(c: &str) -> Precedent {
        Precedent { context: json!({}), ai_suggested: "CAM3".into(), editor_chose: c.into(), at_ms: 0 }
    }

    #[test]
    fn ranking_a_long_history_is_fast() {
        // One camera pass over a 10-minute clip (150 intervals) with 200 past
        // corrections, each context four angles wide: this ran on the UI thread.
        let ctx = |i: usize| {
            json!({"context": {"audio_activity": "loud"}, "cameras": (0..4).map(|a| json!({"label": format!("CAM{a}"), "shows": "wide stage", "sharpness": if (i + a) % 2 == 0 { "sharp" } else { "soft" }, "exposure": "normal", "loudness": "loud"})).collect::<Vec<_>>()})
        };
        let events: Vec<EditorPreferenceEvent> = (0..200)
            .map(|i| EditorPreferenceEvent {
                at_ms: i as i64,
                action: None,
                kind: CorrectionKind::CameraChanged,
                context: ctx(i),
                features: json!({}),
                ai_choice: "CAM1".into(),
                ai_confidence: 0.5,
                human_choice: "CAM2".into(),
                decision: Some(kadr_core::DecisionId::new()),
            })
            .collect();
        let t = std::time::Instant::now();
        for i in 0..150 {
            assert_eq!(precedents(&events, &[], DecisionKind::CameraPick, &ctx(i), 16, 0).len(), 16);
        }
        assert!(t.elapsed() < std::time::Duration::from_millis(1500), "{:?}", t.elapsed());
    }

    #[test]
    fn precedents_never_turn_unsure_into_a_pick() {
        let labels: Vec<String> = vec!["CAM1".into(), "CAM2".into()];
        let unsure = BTreeMap::from([("UNSURE".to_string(), 0.9), ("CAM2".to_string(), 0.1), ("CAM1".to_string(), 0.0)]);
        assert_eq!(personal_pick(&unsure, &labels, &[p("CAM2"), p("CAM2")]), None);
        let thin = BTreeMap::from([("CAM9".to_string(), 0.35), ("CAM2".to_string(), 0.3), ("UNSURE".to_string(), 0.25), ("CAM1".to_string(), 0.1)]);
        assert_eq!(personal_pick(&thin, &labels, &[p("CAM2")]), None, "angles hold under half the mass");
        let sure = BTreeMap::from([("CAM2".to_string(), 0.8), ("CAM1".to_string(), 0.1), ("UNSURE".to_string(), 0.1)]);
        let b = personal_pick(&sure, &labels, &[p("CAM2"), p("CAM2")]).unwrap();
        assert!(b["CAM2"] > b["CAM1"] && !b.contains_key("UNSURE"));
    }

    #[test]
    fn prior_is_smoothed_frequency() {
        let f = frequency_prior(&[p("CAM1"), p("CAM1"), p("CAM3")], &["CAM1".into(), "CAM2".into(), "CAM3".into()]);
        assert!((f["CAM1"] - 0.5).abs() < 1e-6);
        assert!((f["CAM2"] - 1.0 / 6.0).abs() < 1e-6);
        assert!((f.values().sum::<f32>() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn precedents_prefer_similar_context_and_stay_chronological() {
        let now = 40 * 86_400_000;
        let mut events: Vec<_> = (0..20).map(|i| event("verse", "CAM2", "CAM1", now - i * 1000)).collect();
        events.extend((0..3).map(|i| event("guitar solo", "CAM3", "CAM1", 1000 + i)));
        // Unrelated to Jev (no decision): never a precedent.
        let mut plain = event("guitar solo", "x", "y", now);
        plain.decision = None;
        events.push(plain);
        let got = precedents(&events, &[], DecisionKind::CameraPick, &json!({"music_section": "guitar solo", "singer_singing": false}), 3, now);
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|x| x.context["music_section"] == "guitar solo"), "{got:?}");
        assert!(got.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
        // Shot events don't leak into camera decisions.
        assert!(precedents(&events, &[], DecisionKind::ShotUsability, &json!({}), 3, now).is_empty());
    }

    #[test]
    fn blend_endpoints_and_normalisation() {
        let j = BTreeMap::from([("A".to_string(), 1.0f32), ("B".to_string(), 0.0)]);
        let f = BTreeMap::from([("A".to_string(), 0.25f32), ("B".to_string(), 0.75)]);
        assert_eq!(blend(&j, &f, 1.0)["A"], 1.0);
        assert_eq!(blend(&j, &f, 0.0)["B"], 0.75);
        let m = blend(&j, &f, 0.5);
        assert!((m.values().sum::<f32>() - 1.0).abs() < 1e-6 && (m["A"] - 0.625).abs() < 1e-6);
    }

    #[test]
    fn blend_weight_is_a_probability_that_never_grows_with_evidence() {
        let w: Vec<f32> = (0..200).map(blend_weight).collect();
        assert!(w.iter().all(|x| (0.0..=1.0).contains(x)));
        assert!(w.windows(2).all(|p| p[1] <= p[0] + 1e-6));
    }
}
