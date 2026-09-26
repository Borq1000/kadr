//! Technical description of a media file, produced by the media backend's
//! probe and stored in the project so reopening needs no re-probe.

use crate::time::{FrameRate, Time};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Average rate; `None` if the container does not declare one.
    pub frame_rate: Option<FrameRate>,
    /// True when r_frame_rate and avg_frame_rate disagree (variable rate).
    pub variable_frame_rate: bool,
    pub codec: String,
    pub pixel_format: String,
    /// Display rotation in degrees from stream side data (phones).
    #[serde(default)]
    pub rotation: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u32,
    pub codec: String,
    #[serde(default)]
    pub channel_layout: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub duration: Time,
    pub container: String,
    pub size_bytes: u64,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    /// Embedded start timecode (e.g. "01:00:00:00") — used for multicam sync.
    #[serde(default)]
    pub timecode: Option<String>,
}

impl MediaInfo {
    pub fn resolution_label(&self) -> String {
        match &self.video {
            Some(v) => format!("{}×{}", v.width, v.height),
            None => String::new(),
        }
    }
    pub fn fps_label(&self) -> String {
        match self.video.as_ref().and_then(|v| v.frame_rate) {
            Some(fr) if self.kind == MediaKind::Video => format!("{fr} fps"),
            _ => String::new(),
        }
    }
    pub fn audio_label(&self) -> String {
        match &self.audio {
            Some(a) => {
                let ch = match a.channels {
                    1 => "mono".to_string(),
                    2 => "stereo".to_string(),
                    n => format!("{n}ch"),
                };
                format!("{} kHz {ch}", a.sample_rate as f64 / 1000.0)
            }
            None => String::new(),
        }
    }
}
