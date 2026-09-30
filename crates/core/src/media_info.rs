//! Technical description of a media file, produced by the media backend's
//! probe and stored in the project so reopening needs no re-probe.

use crate::color::ColorInfo;
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
    /// Colour metadata from probe; `None` for projects saved before it was
    /// read, or when the stored value cannot be read (then it is guessed).
    #[serde(default, deserialize_with = "lenient_color")]
    pub color: Option<ColorInfo>,
}

fn square_pixels() -> (u32, u32) {
    (1, 1)
}

/// A stored colour this build cannot read (a value from a newer format, a
/// missing field) becomes `None`, so the colour is guessed again instead of
/// the whole project failing to load.
fn lenient_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<ColorInfo>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Readable(ColorInfo),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(match Stored::deserialize(d)? {
        Stored::Readable(c) => Some(c),
        Stored::Unreadable(_) => None,
    })
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
    /// the size and the pixel format (RGB family, alpha).
    pub fn color_info(&self) -> ColorInfo {
        self.color.unwrap_or_else(|| ColorInfo::guess_video_format(self.width, self.height, &self.pixel_format))
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
    fn stored_colour_names_are_pinned() {
        use crate::color::{Primaries, Range, Transfer};
        let json = r#"{"primaries":"bt709","transfer":"bt709","matrix":"bt709","range":"limited","alpha":"opaque"}"#;
        let c: ColorInfo = serde_json::from_str(json).unwrap();
        assert_eq!(c, ColorInfo { primaries: Primaries::Bt709, transfer: Transfer::Bt709, matrix: Matrix::Bt709, range: Range::Limited, alpha: AlphaMode::Opaque });
        assert_eq!(serde_json::to_string(&c).unwrap(), json);
    }

    #[test]
    fn unreadable_stored_colour_falls_back_to_the_guess() {
        let base = r#""width":1920,"height":1080,"frame_rate":null,"variable_frame_rate":false,"codec":"h264","pixel_format":"yuv420p","rotation":0,"sar":[1,1]"#;
        for colour in [
            r#"{"primaries":"future_thing","transfer":"bt709","matrix":"bt709","range":"limited","alpha":"opaque"}"#,
            r#"{"primaries":"bt709","transfer":"bt709","matrix":"bt709","range":"limited"}"#,
            r#""bt709""#,
        ] {
            let v: VideoInfo = serde_json::from_str(&format!("{{{base},\"color\":{colour}}}")).unwrap_or_else(|e| panic!("{colour}: {e}"));
            assert_eq!(v.color, None, "{colour}");
            assert_eq!(v.color_info(), ColorInfo::guess_video(1920, 1080));
        }
        let v: VideoInfo = serde_json::from_str(&format!("{{{base},\"color\":null}}")).unwrap();
        assert_eq!(v.color, None);
        let tagged = r#"{"primaries":"bt709","transfer":"bt709","matrix":"bt709","range":"full","alpha":"opaque"}"#;
        let v: VideoInfo = serde_json::from_str(&format!("{{{base},\"color\":{tagged}}}")).unwrap();
        assert_eq!(v.color.map(|c| c.range), Some(crate::color::Range::Full), "a readable colour is kept");
    }

    #[test]
    fn legacy_rgb_sources_are_rgb_full_range() {
        use crate::color::Range;
        let colour = |pix_fmt: &str| {
            let c = VideoInfo { pixel_format: pix_fmt.into(), ..video(1920, 1080, (1, 1), 0) }.color_info();
            (c.matrix, c.range, c.alpha)
        };
        assert_eq!(colour("argb"), (Matrix::Rgb, Range::Full, AlphaMode::Straight), "QuickTime Animation");
        assert_eq!(colour("rgb24"), (Matrix::Rgb, Range::Full, AlphaMode::Opaque));
        assert_eq!(colour("yuv420p"), (Matrix::Bt709, Range::Limited, AlphaMode::Opaque));
    }

    #[test]
    fn source_colour_of_images_has_straight_alpha() {
        let mut info = MediaInfo { kind: MediaKind::Image, duration: crate::Time::from_secs(5), container: "png_pipe".into(), size_bytes: 0, video: Some(video(10, 10, (1, 1), 0)), audio: None, timecode: None };
        assert_eq!(info.source_color(), ColorInfo::IMAGE_SRGB);
        info.kind = MediaKind::Video;
        assert_eq!(info.source_color().alpha, AlphaMode::Opaque);
    }
}
