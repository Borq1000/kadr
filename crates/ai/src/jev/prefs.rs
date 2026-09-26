//! Personalisation from the editor's own corrections. Jev is never trained
//! on our data; instead similar past decisions travel in `state` as
//! precedents (research §4.3, §7.5), and a plain frequency prior computed
//! here keeps Jev's tendency to over-sharpen small histories in check.

use kadr_project::{CorrectionKind, DecisionKind, EditorPreferenceEvent, StoredDecision};
use serde::Serialize;
use std::collections::BTreeMap;

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

/// Number of identical leaf values (same path, same value).
fn similarity(a: &serde_json::Value, b: &serde_json::Value) -> usize {
    let (mut la, mut lb) = (vec![], vec![]);
    leaves(a, String::new(), &mut la);
    leaves(b, String::new(), &mut lb);
    la.iter().filter(|x| lb.contains(x)).count()
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
    let score = |p: &Precedent| {
        let age_days = (now_ms - p.at_ms).max(0) as f64 / 86_400_000.0;
        // Relevance leads; age can at most halve it (an old solo still says
        // more about a solo than yesterday's verse).
        similarity(&p.context, current) as f64 * (0.5 + 0.5 * 0.5f64.powf(age_days / HALF_LIFE_DAYS))
    };
    all.sort_by(|a, b| score(b).total_cmp(&score(a)).then(b.at_ms.cmp(&a.at_ms)));
    all.truncate(k);
    all.sort_by_key(|p| p.at_ms);
    all
}

/// P(option) from past picks with Dirichlet(α = 1) smoothing.
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
