//! Deterministic, local media analysis (no network, no API).

pub mod audio;
pub mod silence;
pub mod video;

pub use audio::{analyze_pcm, AudioOverview, OVERVIEW_VERSION};
pub use silence::{auto_threshold_db, detect_silence, SilenceParams, SILENCE_VERSION};
