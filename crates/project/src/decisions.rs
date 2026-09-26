//! Jev decisions persisted with the project: the same project reproduces the
//! same suggestions without new network calls, and human overrides of them
//! are the training signal for personalisation.

use kadr_core::DecisionId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    ShotUsability,
    CameraPick,
}

/// What the editor may do with a decision on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    AutoApply,
    Suggest,
    Review,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredDecision {
    pub id: DecisionId,
    pub kind: DecisionKind,
    /// blake3 of the canonical {model, prompt version, item state, questions}.
    pub key: String,
    /// What it is about, e.g. "asset:<uuid>:shot:3" or "clip:<uuid>@12000".
    pub subject: String,
    pub value: String,
    pub probs: BTreeMap<String, f32>,
    pub p_max: f32,
    pub margin: f32,
    pub gate: Gate,
    pub model: String,
    pub prompt_version: String,
    pub at_ms: i64,
    /// Bucketed features that were sent (precedents, audits).
    #[serde(default)]
    pub features: serde_json::Value,
    /// Set when the human overrode the decision.
    #[serde(default)]
    pub human: Option<String>,
}

impl StoredDecision {
    /// The value in effect: the human's override wins.
    pub fn effective(&self) -> &str {
        self.human.as_deref().unwrap_or(&self.value)
    }
}
