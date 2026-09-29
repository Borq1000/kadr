//! Technical description of a media file, produced by the media backend's
//! probe and stored in the project so reopening needs no re-probe.

use crate::color::{alpha_of_pixel_format, ColorInfo};
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
    /// Sample (pixel) aspect ratio; (1, 1) for square pixels.
    #[serde(default = "square_pixels")]
    pub sar: (u32, u32),
    /// Colour metadata from probe; `None` for projects saved before it was read.
    #[serde(default)]
    pub color: Option<ColorInfo>,
}

fn square_pixels() -> (u32, u32) {
    (1, 1)
}

impl VideoInfo {
    /// Size as displayed: the sample aspect ratio applied to the width, then
    /// swapped for ±90°/270° rotation (phones).
    pub fn display_size(&self) -> (u32, u32) {
        let (n, d) = if self.sar.0 > 0 && self.sar.1 > 0 { self.sar } else { (1, 1) };
        let w = ((self.width as u64 * n as u64 + d as u64 / 2) / d as u64) as u32;
        if self.rotation.rem_euclid(180) == 90 { (self.height, w) } else { (w, self.height) }
    }

    /// Probed colour, or for projects saved before colour tags a guess from
    /// the size with alpha from the pixel format.
    pub fn color_info(&self) -> ColorInfo {
        self.color.unwrap_or_else(|| ColorInfo { alpha: alpha_of_pixel_format(&self.pixel_format), ..ColorInfo::guess_video(self.width, self.height) })
    }
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
    /// Colour of the decoded source (render spec §6).
    pub fn source_color(&self) -> ColorInfo {
        match (self.kind, &self.video) {
            (MediaKind::Image, v) => v.as_ref().and_then(|v| v.color).unwrap_or(ColorInfo::IMAGE_SRGB),
            (_, Some(v)) => v.color_info(),
            (_, None) => ColorInfo::WORKING_SDR,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{AlphaMode, ColorInfo, Matrix};

    fn video(width: u32, height: u32, sar: (u32, u32), rotation: i32) -> VideoInfo {
        VideoInfo { width, height, frame_rate: None, variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation, sar, color: None }
    }

    #[test]
    fn display_size_applies_sar_then_rotation() {
        assert_eq!(video(1920, 1080, (1, 1), 0).display_size(), (1920, 1080));
        assert_eq!(video(720, 480, (8, 9), 0).display_size(), (640, 480), "anamorphic NTSC 4:3");
        assert_eq!(video(1920, 1080, (1, 1), 90).display_size(), (1080, 1920), "phone portrait");
        assert_eq!(video(1920, 1080, (1, 1), -90).display_size(), (1080, 1920));
        assert_eq!(video(1920, 1080, (1, 1), 180).display_size(), (1920, 1080));
        assert_eq!(video(1920, 1080, (0, 1), 0).display_size(), (1920, 1080), "unknown SAR = square");
    }

    #[test]
    fn projects_saved_before_colour_tags_still_load_and_get_a_guess() {
        let old = r#"{"width":720,"height":576,"frame_rate":null,"variable_frame_rate":false,"codec":"mpeg2video","pixel_format":"yuv420p","rotation":0}"#;
        let v: VideoInfo = serde_json::from_str(old).unwrap();
        assert_eq!((v.sar, v.color), ((1, 1), None));
        assert_eq!(v.color_info().matrix, Matrix::Bt601);
        assert_eq!(v.color_info().alpha, AlphaMode::Opaque);
        let prores_4444 = VideoInfo { pixel_format: "yuva444p10le".into(), ..video(1920, 1080, (1, 1), 0) };
        let c = prores_4444.color_info();
        assert_eq!((c.alpha, c.matrix), (AlphaMode::Straight, Matrix::Bt709), "alpha from the pixel format, the rest guessed");
    }

    #[test]
    fn source_colour_of_images_has_straight_alpha() {
        let mut info = MediaInfo { kind: MediaKind::Image, duration: crate::Time::from_secs(5), container: "png_pipe".into(), size_bytes: 0, video: Some(video(10, 10, (1, 1), 0)), audio: None, timecode: None };
        assert_eq!(info.source_color(), ColorInfo::IMAGE_SRGB);
        info.kind = MediaKind::Video;
        assert_eq!(info.source_color().alpha, AlphaMode::Opaque);
    }
}
