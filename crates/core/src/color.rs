//! Colour metadata carried by every source, frame and output (render spec
//! §6). Only SDR Rec.709/601 is processed today; everything else is at least
//! described, so a frame never loses what it is.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primaries {
    Bt709,
    Bt601_625,
    Bt601_525,
    Bt2020,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    Bt709,
    Srgb,
    Linear,
    Pq,
    Hlg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Matrix {
    Rgb,
    Bt709,
    Bt601,
    Bt2020Ncl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    Limited,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaMode {
    Opaque,
    Straight,
    Premultiplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorInfo {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub matrix: Matrix,
    pub range: Range,
    pub alpha: AlphaMode,
}

impl ColorInfo {
    /// The SDR renderer's working space and output: full-range non-linear
    /// Rec.709 R'G'B' with premultiplied alpha.
    pub const WORKING_SDR: ColorInfo =
        ColorInfo { primaries: Primaries::Bt709, transfer: Transfer::Bt709, matrix: Matrix::Rgb, range: Range::Full, alpha: AlphaMode::Premultiplied };

    /// Still images (PNG, JPEG, …): sRGB, full range, straight alpha.
    pub const IMAGE_SRGB: ColorInfo =
        ColorInfo { primaries: Primaries::Bt709, transfer: Transfer::Srgb, matrix: Matrix::Rgb, range: Range::Full, alpha: AlphaMode::Straight };

    /// Untagged video: ≥ 1280 wide or taller than 576 lines → Rec.709; 576
    /// lines → Rec.601/625; otherwise Rec.601/525 (the mpv/libplacebo
    /// heuristic; cropped widescreen HD such as 1280×536 stays Rec.709).
    /// Limited range, opaque.
    pub fn guess_video(width: u32, height: u32) -> ColorInfo {
        let (primaries, matrix) = if width >= 1280 || height > 576 {
            (Primaries::Bt709, Matrix::Bt709)
        } else if height == 576 {
            (Primaries::Bt601_625, Matrix::Bt601)
        } else {
            (Primaries::Bt601_525, Matrix::Bt601)
        };
        ColorInfo { primaries, transfer: Transfer::Bt709, matrix, range: Range::Limited, alpha: AlphaMode::Opaque }
    }

    /// From ffprobe's `color_primaries`, `color_transfer`, `color_space` and
    /// `color_range`; each unknown or missing field falls back to
    /// [`ColorInfo::guess_video`].
    pub fn from_ffprobe(width: u32, height: u32, primaries: Option<&str>, transfer: Option<&str>, space: Option<&str>, range: Option<&str>) -> ColorInfo {
        let g = Self::guess_video(width, height);
        ColorInfo {
            primaries: match primaries {
                Some("bt709") => Primaries::Bt709,
                Some("bt470bg") => Primaries::Bt601_625,
                Some("smpte170m" | "smpte240m") => Primaries::Bt601_525,
                Some("bt2020") => Primaries::Bt2020,
                _ => g.primaries,
            },
            transfer: match transfer {
                // BT.601 and BT.709 share the same OETF.
                Some("bt709" | "smpte170m" | "bt470bg" | "bt2020-10" | "bt2020-12") => Transfer::Bt709,
                Some("iec61966-2-1") => Transfer::Srgb,
                Some("linear") => Transfer::Linear,
                Some("smpte2084") => Transfer::Pq,
                Some("arib-std-b67") => Transfer::Hlg,
                _ => g.transfer,
            },
            matrix: match space {
                Some("bt709") => Matrix::Bt709,
                Some("bt470bg" | "smpte170m") => Matrix::Bt601,
                Some("bt2020nc") => Matrix::Bt2020Ncl,
                Some("gbr") => Matrix::Rgb,
                _ => g.matrix,
            },
            range: match range {
                Some("pc") => Range::Full,
                Some("tv") => Range::Limited,
                _ => g.range,
            },
            alpha: g.alpha,
        }
    }

    /// Whether the SDR pipeline processes this correctly today.
    pub fn is_supported_sdr(&self) -> bool {
        matches!(self.primaries, Primaries::Bt709 | Primaries::Bt601_625 | Primaries::Bt601_525)
            && matches!(self.transfer, Transfer::Bt709 | Transfer::Srgb)
    }
}

/// Whether frames in `pix_fmt` (FFmpeg name) carry an alpha channel:
/// `Straight` for alpha formats (yuva*, rgba*, argb, bgra*, abgr, gbrap*,
/// ya8/ya16, pal8, …), otherwise `Opaque`.
pub fn alpha_of_pixel_format(pix_fmt: &str) -> AlphaMode {
    const WITH_ALPHA: [&str; 10] = ["yuva", "rgba", "argb", "bgra", "abgr", "gbrap", "ya", "vuya", "ayuv", "pal8"];
    if WITH_ALPHA.iter().any(|p| pix_fmt.starts_with(p)) {
        AlphaMode::Straight
    } else {
        AlphaMode::Opaque
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_video_is_guessed_from_its_size() {
        let hd = ColorInfo::guess_video(1920, 1080);
        assert_eq!((hd.primaries, hd.transfer, hd.matrix, hd.range, hd.alpha), (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709, Range::Limited, AlphaMode::Opaque));
        assert_eq!(ColorInfo::guess_video(1280, 720).matrix, Matrix::Bt709);
        // Cropped widescreen HD stays Rec.709.
        assert_eq!(ColorInfo::guess_video(1280, 536).matrix, Matrix::Bt709);
        assert_eq!(ColorInfo::guess_video(1440, 600).primaries, Primaries::Bt709);
        let pal = ColorInfo::guess_video(720, 576);
        assert_eq!((pal.primaries, pal.matrix), (Primaries::Bt601_625, Matrix::Bt601));
        let ntsc = ColorInfo::guess_video(720, 480);
        assert_eq!((ntsc.primaries, ntsc.matrix), (Primaries::Bt601_525, Matrix::Bt601));
        // Small web video stays SD.
        assert_eq!(ColorInfo::guess_video(640, 360).primaries, Primaries::Bt601_525);
    }

    #[test]
    fn ffprobe_tags_win_over_the_guess() {
        let c = ColorInfo::from_ffprobe(720, 480, Some("bt709"), Some("bt709"), Some("bt709"), Some("pc"));
        assert_eq!((c.primaries, c.transfer, c.matrix, c.range), (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709, Range::Full));
        let hdr = ColorInfo::from_ffprobe(3840, 2160, Some("bt2020"), Some("smpte2084"), Some("bt2020nc"), Some("tv"));
        assert_eq!((hdr.primaries, hdr.transfer, hdr.matrix), (Primaries::Bt2020, Transfer::Pq, Matrix::Bt2020Ncl));
        assert!(!hdr.is_supported_sdr());
        let sd = ColorInfo::from_ffprobe(1920, 1080, Some("smpte170m"), Some("smpte170m"), Some("smpte170m"), None);
        assert_eq!((sd.primaries, sd.transfer, sd.matrix, sd.range), (Primaries::Bt601_525, Transfer::Bt709, Matrix::Bt601, Range::Limited));
        let pal = ColorInfo::from_ffprobe(1920, 1080, Some("bt470bg"), None, Some("bt470bg"), None);
        assert_eq!((pal.primaries, pal.matrix), (Primaries::Bt601_625, Matrix::Bt601));
        assert_eq!(ColorInfo::from_ffprobe(1920, 1080, None, Some("iec61966-2-1"), Some("gbr"), None).matrix, Matrix::Rgb);
    }

    #[test]
    fn unknown_or_missing_tags_fall_back_per_field() {
        let c = ColorInfo::from_ffprobe(1920, 1080, Some("unknown"), None, Some("reserved"), Some("unknown"));
        assert_eq!(c, ColorInfo::guess_video(1920, 1080));
    }

    #[test]
    fn working_space_is_full_range_rec709_premultiplied() {
        let w = ColorInfo::WORKING_SDR;
        assert_eq!((w.primaries, w.transfer, w.matrix, w.range, w.alpha), (Primaries::Bt709, Transfer::Bt709, Matrix::Rgb, Range::Full, AlphaMode::Premultiplied));
        assert!(w.is_supported_sdr());
        assert_eq!(ColorInfo::IMAGE_SRGB.alpha, AlphaMode::Straight);
    }

    #[test]
    fn alpha_comes_from_the_pixel_format() {
        for f in ["yuva444p10le", "yuva420p", "rgba", "argb", "bgra", "abgr", "gbrap", "gbrap12le", "ya8", "ya16be", "pal8", "rgba64le", "bgra64be", "vuya", "ayuv64le"] {
            assert_eq!(alpha_of_pixel_format(f), AlphaMode::Straight, "{f}");
        }
        for f in ["yuv420p", "yuv444p10le", "rgb24", "nv12", "gray", "gbrp10le", "p010le", "yuyv422", ""] {
            assert_eq!(alpha_of_pixel_format(f), AlphaMode::Opaque, "{f}");
        }
    }
}
