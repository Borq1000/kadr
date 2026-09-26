//! Transcripts and analysis summaries. Bulky data (peaks, PCM, embeddings)
//! lives in the cache; the project keeps only compact, user-relevant results.

use kadr_core::{AssetId, Time, TimeRange, TranscriptId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub id: TranscriptId,
    pub asset: AssetId,
    pub language: String,
    /// Which engine produced it, e.g. "whisper.cpp/large-v3".
    pub engine: String,
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    /// Source-time range within the asset.
    pub range: TimeRange,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
    pub confidence: f32,
    #[serde(default)]
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub range: TimeRange,
    pub text: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub asset: AssetId,
    /// Algorithm version; results with an older version are recomputed.
    pub algo_version: u32,
    pub data: AnalysisData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisData {
    Silence {
        threshold_db: f32,
        min_duration: Time,
        /// Source-time ranges.
        ranges: Vec<TimeRange>,
    },
    Loudness {
        integrated_lufs: f32,
        peak_dbfs: f32,
        clipping_ranges: Vec<TimeRange>,
    },
    SceneCuts {
        cuts: Vec<Time>,
    },
}
