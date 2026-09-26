//! AI layer: an optional, cost- and privacy-controlled assistant on top of
//! the deterministic edit engine. The editor is fully functional without it.
//!
//! Invariants:
//! - AI output is only ever [`command::AiCommand`]s, validated before use.
//! - Every outbound request passes [`privacy::gate`] and [`cost::check_budget`].
//! - API keys live in the OS credential store and are never logged.

pub mod assistant;
pub mod command;
pub mod context;
pub mod cost;
pub mod credentials;
pub mod intent;
pub mod jev;
pub mod local_ops;
pub mod plan;
pub mod privacy;
pub mod providers;
pub mod settings;
pub mod tier;

pub use assistant::{Assistant, CloudOffer, Reply};
pub use context::EditorState;
pub use plan::{Plan, PlanItem};
pub use settings::AiSettings;
pub use tier::Tier;
