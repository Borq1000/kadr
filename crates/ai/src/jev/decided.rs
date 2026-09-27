//! A typed Jev decision plus the gate that says what the editor may do
//! with it on its own.

use crate::providers::jev::{JevAnswer, JevQuestion};
use kadr_project::{DecisionKind, Gate, StoredDecision};
use std::collections::BTreeMap;

/// Values that mean "the model does not know" for every template.
pub const ESCAPE_VALUES: &[&str] = &["REVIEW", "UNSURE"];

#[derive(Clone, Debug)]
pub struct Decided {
    pub stored: StoredDecision,
    pub from_cache: bool,
    /// Raw answers by the item's own question ids (empty when cached).
    pub answers: BTreeMap<String, JevAnswer>,
}

/// Starting thresholds before calibration (research §7.4). They are on the
/// top probability and its lead over the runner-up, never on the API's
/// `confidence`, whose scale depends on the number of options.
pub fn decide_gate(kind: DecisionKind, value: &str, p_max: f32, margin: f32, extra_ok: bool) -> Gate {
    if ESCAPE_VALUES.contains(&value) {
        return Gate::Review;
    }
    let (auto, suggest) = match (kind, value) {
        // Hiding footage needs the local defect detector to agree.
        (DecisionKind::ShotUsability, "DISCARD") => (p_max >= 0.95 && extra_ok, p_max >= 0.80),
        (DecisionKind::ShotUsability, _) => (p_max >= 0.90 && margin >= 0.5, p_max >= 0.70),
        (DecisionKind::CameraPick, _) => (p_max >= 0.75 && margin >= 0.3, p_max >= 0.50),
    };
    if auto {
        Gate::AutoApply
    } else if suggest {
        Gate::Suggest
    } else {
        Gate::Review
    }
}

/// The options a question can legally answer with.
pub fn options_of(q: &JevQuestion) -> Vec<String> {
    match q {
        JevQuestion::Noul { .. } => vec!["YES".into(), "NO".into()],
        JevQuestion::Choice { criteria, .. } => criteria.keys().cloned().collect(),
        JevQuestion::Score { criteria, .. } => (0..criteria.len()).map(|i| i.to_string()).collect(),
    }
}

/// Probability per option; Noul becomes YES/NO. Options the question does
/// not define are dropped, so API drift can't inject values.
pub fn probs_of(q: &JevQuestion, a: &JevAnswer) -> BTreeMap<String, f32> {
    let opts = options_of(q);
    let mut p: BTreeMap<String, f32> = match (q, a.noul) {
        (JevQuestion::Noul { .. }, Some(y)) => BTreeMap::from([("YES".to_string(), y as f32), ("NO".to_string(), 1.0 - y as f32)]),
        _ => a.probabilities.iter().filter(|(k, _)| opts.contains(k)).map(|(k, v)| (k.clone(), *v as f32)).collect(),
    };
    // Choice/Score answers sometimes list only non-zero options.
    for o in opts {
        p.entry(o).or_insert(0.0);
    }
    p
}

/// (argmax, p_max, margin over the runner-up).
pub fn summarize(probs: &BTreeMap<String, f32>) -> Option<(String, f32, f32)> {
    let mut v: Vec<_> = probs.iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(a.1).then(a.0.cmp(b.0)));
    let (k, p1) = v.first()?;
    if **p1 <= 0.0 {
        return None;
    }
    let p2 = v.get(1).map_or(0.0, |x| *x.1);
    Some(((*k).clone(), **p1, **p1 - p2))
}
