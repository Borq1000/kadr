//! Jev decision service — typed decisions made from *locally extracted
//! features* (never raw video).

pub mod decided;
pub mod prefs;
pub mod service;
pub mod templates;

pub use decided::{decide_gate, Decided};
pub use service::{apply_hysteresis, JevDecisionService, JevEstimate, JevItem};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Usability {
    Keep,
    Discard,
    Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SegmentClass {
    Establishing,
    MainAction,
    CloseUp,
    Reaction,
    BRoll,
    Transition,
    LowValue,
    Discard,
}

/// Features sent to Jev for a shot — numbers and short labels only.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShotFeatures {
    pub duration_ms: i64,
    pub blur: f32,
    pub shake: f32,
    pub faces: u32,
    pub largest_face_area: f32,
    pub speech_ratio: f32,
    pub uniqueness: f32,
    pub brightness: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decision<T> {
    pub value: T,
    pub confidence: f32,
}

/// Routing tiers Jev may *recommend*. The gateway still enforces budget
/// and privacy rules: a recommendation never triggers a paid call by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RouteRecommendation {
    Local,
    EconomyModel,
    SmartModel,
    DirectorModel,
}
