//! Edit history, AI action log and human-correction events. These are
//! written from day one so a personal editor profile can be learned later.
//! None of it ever leaves the machine automatically.

use kadr_core::ActionId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSource {
    User,
    Ai,
    Voice,
    Transcript,
}

/// One entry of the persisted edit history (human-readable audit trail;
/// the in-memory undo stack is separate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditOperation {
    pub at_ms: i64,
    pub source: EditSource,
    pub label: String,
    #[serde(default)]
    pub action: Option<ActionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AIActionStatus {
    Proposed,
    Applied,
    Rejected,
    Undone,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AIAction {
    pub id: ActionId,
    pub at_ms: i64,
    pub prompt: String,
    /// "local", "economy", "smart", "director".
    pub tier: String,
    pub provider: String,
    pub model: String,
    pub summary: String,
    /// The validated EditCommand list, as JSON.
    pub commands: serde_json::Value,
    pub status: AIActionStatus,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionKind {
    CameraChanged,
    SegmentRestored,
    SegmentDeleted,
    TransitionChanged,
    ClipTrimmed,
    ClipMoved,
    ActionUndone,
}

/// A human overriding (or confirming) an AI decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorPreferenceEvent {
    pub at_ms: i64,
    pub action: Option<ActionId>,
    pub kind: CorrectionKind,
    /// Timeline context (neighbouring clips, playhead, section…).
    pub context: serde_json::Value,
    /// Features the AI decision was based on.
    pub features: serde_json::Value,
    pub ai_choice: String,
    pub ai_confidence: f32,
    pub human_choice: String,
    /// The Jev decision this event overrides or confirms.
    #[serde(default)]
    pub decision: Option<kadr_core::DecisionId>,
}

pub type EditorCorrection = EditorPreferenceEvent;
