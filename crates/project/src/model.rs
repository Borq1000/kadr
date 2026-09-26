//! Top-level project entities.

use crate::ai_log::{AIAction, EditOperation, EditorPreferenceEvent};
use crate::analysis::{AnalysisResult, Transcript};
use crate::sequence::Sequence;
use kadr_core::{AssetId, BinId, FrameRate, MediaInfo, MediaKind, MulticamId, ProjectId, SequenceId, Time};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped on any incompatible change; `io::load` migrates older files.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format_version: u32,
    pub id: ProjectId,
    pub name: String,
    /// Unix milliseconds.
    pub created_ms: i64,
    pub modified_ms: i64,
    #[serde(default)]
    pub bins: Vec<Bin>,
    pub assets: Vec<MediaAsset>,
    pub sequences: Vec<Sequence>,
    pub active_sequence: SequenceId,
    #[serde(default)]
    pub multicam_groups: Vec<MulticamGroup>,
    #[serde(default)]
    pub transcripts: Vec<Transcript>,
    #[serde(default)]
    pub analysis: Vec<AnalysisResult>,
    #[serde(default)]
    pub history_log: Vec<EditOperation>,
    #[serde(default)]
    pub ai_actions: Vec<AIAction>,
    #[serde(default)]
    pub preference_events: Vec<EditorPreferenceEvent>,
    /// Jev decisions (cache + audit trail + human overrides).
    #[serde(default)]
    pub jev_decisions: Vec<crate::decisions::StoredDecision>,
    /// Lifetime external-AI spend for this project, USD.
    #[serde(default)]
    pub ai_cost_usd: f64,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let seq = Sequence::new("Sequence 1", FrameRate::FPS_30, 1920, 1080);
        let now = now_ms();
        Project {
            format_version: FORMAT_VERSION,
            id: ProjectId::new(),
            name: name.into(),
            created_ms: now,
            modified_ms: now,
            bins: vec![],
            assets: vec![],
            active_sequence: seq.id,
            sequences: vec![seq],
            multicam_groups: vec![],
            transcripts: vec![],
            analysis: vec![],
            history_log: vec![],
            ai_actions: vec![],
            preference_events: vec![],
            jev_decisions: vec![],
            ai_cost_usd: 0.0,
        }
    }

    pub fn asset(&self, id: AssetId) -> Option<&MediaAsset> {
        self.assets.iter().find(|a| a.id == id)
    }
    pub fn asset_mut(&mut self, id: AssetId) -> Option<&mut MediaAsset> {
        self.assets.iter_mut().find(|a| a.id == id)
    }

    pub fn sequence(&self) -> &Sequence {
        self.sequences
            .iter()
            .find(|s| s.id == self.active_sequence)
            .unwrap_or(&self.sequences[0])
    }
    pub fn sequence_mut(&mut self) -> &mut Sequence {
        let id = self.active_sequence;
        let idx = self.sequences.iter().position(|s| s.id == id).unwrap_or(0);
        &mut self.sequences[idx]
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bin {
    pub id: BinId,
    pub name: String,
    #[serde(default)]
    pub parent: Option<BinId>,
}

/// A source file referenced (never embedded, never modified) by the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: AssetId,
    pub name: String,
    /// Absolute path at import time.
    pub path: PathBuf,
    /// Path relative to the project file, used to relink moved projects.
    #[serde(default)]
    pub relative_path: Option<PathBuf>,
    pub info: MediaInfo,
    #[serde(default)]
    pub bin: Option<BinId>,
    /// Source modification time at import (unix ms) — detects changed media.
    #[serde(default)]
    pub source_mtime_ms: i64,
}

impl MediaAsset {
    pub fn new(path: impl Into<PathBuf>, info: MediaInfo) -> Self {
        let path = path.into();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        MediaAsset {
            id: AssetId::new(),
            name,
            path,
            relative_path: None,
            info,
            bin: None,
            source_mtime_ms: 0,
        }
    }
    pub fn kind(&self) -> MediaKind {
        self.info.kind
    }
    pub fn duration(&self) -> Time {
        self.info.duration
    }
    pub fn has_video(&self) -> bool {
        self.info.video.is_some() && self.info.kind != MediaKind::Audio
    }
    pub fn has_audio(&self) -> bool {
        self.info.audio.is_some()
    }
    pub fn exists(&self) -> bool {
        Path::new(&self.path).exists()
    }
}

/// Several synchronised angles of the same event. All angles are kept; the
/// timeline stores which angle is active per range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamGroup {
    pub id: MulticamId,
    pub name: String,
    pub angles: Vec<MulticamAngle>,
    /// Dedicated external audio recording, if any.
    #[serde(default)]
    pub master_audio: Option<AssetId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamAngle {
    pub asset: AssetId,
    /// "CAM_A", "CAM1"…
    pub label: String,
    /// What the angle shows ("wide stage", "singer close-up"); Jev reads it.
    #[serde(default)]
    pub description: String,
    /// Group time 0 corresponds to this source time.
    pub sync_offset: Time,
    #[serde(default)]
    pub sync_method: SyncMethod,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMethod {
    #[default]
    Manual,
    Waveform,
    Timecode,
}
