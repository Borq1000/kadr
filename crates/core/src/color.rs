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

    /// Untagged video stored in `pix_fmt` (FFmpeg name): [`ColorInfo::guess_video`],
    /// except that RGB-family formats (see [`is_rgb_pixel_format`]) are
    /// R'G'B' (`Matrix::Rgb`) and full range; alpha from the pixel format.
    pub fn guess_video_format(width: u32, height: u32, pix_fmt: &str) -> ColorInfo {
        Self::from_ffprobe(width, height, pix_fmt, None, None, None, None)
    }

    /// From ffprobe's `pix_fmt`, `color_primaries`, `color_transfer`,
    /// `color_space` and `color_range`; each unknown or missing field falls
    /// back to [`ColorInfo::guess_video`]. An RGB-family `pix_fmt` without a
    /// known `color_space` is `Matrix::Rgb` and `Range::Full` (FFmpeg treats
    /// RGB as full range). Alpha comes from the pixel format.
    pub fn from_ffprobe(
        width: u32,
        height: u32,
        pix_fmt: &str,
        primaries: Option<&str>,
        transfer: Option<&str>,
        space: Option<&str>,
        range: Option<&str>,
    ) -> ColorInfo {
        let g = Self::guess_video(width, height);
        let tagged_matrix = match space {
            Some("bt709") => Some(Matrix::Bt709),
            Some("bt470bg" | "smpte170m") => Some(Matrix::Bt601),
            Some("bt2020nc") => Some(Matrix::Bt2020Ncl),
            Some("gbr") => Some(Matrix::Rgb),
            _ => None,
        };
        let untagged_rgb = tagged_matrix.is_none() && is_rgb_pixel_format(pix_fmt);
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
            matrix: match tagged_matrix {
                Some(m) => m,
                None if untagged_rgb => Matrix::Rgb,
                None => g.matrix,
            },
            range: match range {
                _ if untagged_rgb => Range::Full,
                Some("pc") => Range::Full,
                Some("tv") => Range::Limited,
                _ => g.range,
            },
            alpha: alpha_of_pixel_format(pix_fmt),
        }
    }

    /// Whether the SDR pipeline processes this correctly today.
    pub fn is_supported_sdr(&self) -> bool {
        matches!(self.primaries, Primaries::Bt709 | Primaries::Bt601_625 | Primaries::Bt601_525)
            && matches!(self.transfer, Transfer::Bt709 | Transfer::Srgb)
    }
}

/// Whether `pix_fmt` (FFmpeg name) stores R'G'B' rather than Y'CbCr:
/// rgb*, bgr*, argb, abgr, 0rgb, 0bgr, gbr* (planar), x2rgb10*, x2bgr10*
/// and pal8.
pub fn is_rgb_pixel_format(pix_fmt: &str) -> bool {
    const RGB_PREFIX: [&str; 10] = ["rgb", "bgr", "argb", "abgr", "0rgb", "0bgr", "gbr", "x2rgb10", "x2bgr10", "pal8"];
    RGB_PREFIX.iter().any(|p| pix_fmt.starts_with(p))
}

/// Whether frames in `pix_fmt` (FFmpeg name) carry an alpha channel:
/// `Opaque` only for formats known to have none; anything else — alpha
/// formats, `pal8`, unknown or empty names — is `Straight`. Fail-safe
/// (Ruling R13): a missed opaque format costs an extra decode, never a
/// wrong frame.
pub fn alpha_of_pixel_format(pix_fmt: &str) -> AlphaMode {
    const OPAQUE: [&str; 20] = [
        "nv12", "nv16", "nv21", "nv24", "nv42", "yuyv422", "uyvy422", "yvyu422", "uyyvyy411", "vuyx", "monow", "monob", "rgb24", "bgr24", "rgb8", "bgr8", "0rgb",
        "rgb0", "0bgr", "bgr0",
    ];
    const OPAQUE_PREFIX: [&str; 26] = [
        "nv20", "p010", "p012", "p016", "p210", "p216", "p410", "p416", "y210", "y212", "xv30", "xv36", "gray", "xyz12", "rgb48", "bgr48", "rgb565", "bgr565",
        "rgb555", "bgr555", "rgb444", "bgr444", "rgb4", "bgr4", "x2rgb10", "x2bgr10",
    ];
    let f = pix_fmt;
    let opaque = (f.starts_with("yuv") && !f.starts_with("yuva"))
        // gbrp* (planar RGB, float included); gbrap* has alpha and does not match.
        || f.starts_with("gbrp")
        || OPAQUE.contains(&f)
        || OPAQUE_PREFIX.iter().any(|p| f.starts_with(p));
    if opaque { AlphaMode::Opaque } else { AlphaMode::Straight }
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
        let c = ColorInfo::from_ffprobe(720, 480, "yuv420p", Some("bt709"), Some("bt709"), Some("bt709"), Some("pc"));
        assert_eq!((c.primaries, c.transfer, c.matrix, c.range), (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709, Range::Full));
        let hdr = ColorInfo::from_ffprobe(3840, 2160, "yuv420p", Some("bt2020"), Some("smpte2084"), Some("bt2020nc"), Some("tv"));
        assert_eq!((hdr.primaries, hdr.transfer, hdr.matrix), (Primaries::Bt2020, Transfer::Pq, Matrix::Bt2020Ncl));
        assert!(!hdr.is_supported_sdr());
        let sd = ColorInfo::from_ffprobe(1920, 1080, "yuv420p", Some("smpte170m"), Some("smpte170m"), Some("smpte170m"), None);
        assert_eq!((sd.primaries, sd.transfer, sd.matrix, sd.range), (Primaries::Bt601_525, Transfer::Bt709, Matrix::Bt601, Range::Limited));
        let pal = ColorInfo::from_ffprobe(1920, 1080, "yuv420p", Some("bt470bg"), None, Some("bt470bg"), None);
        assert_eq!((pal.primaries, pal.matrix), (Primaries::Bt601_625, Matrix::Bt601));
        assert_eq!(ColorInfo::from_ffprobe(1920, 1080, "yuv420p", None, Some("iec61966-2-1"), Some("gbr"), None).matrix, Matrix::Rgb);
    }

    #[test]
    fn unknown_or_missing_tags_fall_back_per_field() {
        let c = ColorInfo::from_ffprobe(1920, 1080, "yuv420p", Some("unknown"), None, Some("reserved"), Some("unknown"));
        assert_eq!(c, ColorInfo::guess_video(1920, 1080));
    }

    #[test]
    fn untagged_rgb_formats_are_rgb_full_range() {
        let argb = ColorInfo::from_ffprobe(1920, 1080, "argb", None, None, None, None);
        assert_eq!((argb.matrix, argb.range, argb.alpha), (Matrix::Rgb, Range::Full, AlphaMode::Straight));
        assert_eq!((argb.primaries, argb.transfer), (Primaries::Bt709, Transfer::Bt709), "primaries and transfer from the guess");
        let rgb24 = ColorInfo::guess_video_format(720, 480, "rgb24");
        assert_eq!((rgb24.matrix, rgb24.range, rgb24.alpha, rgb24.primaries), (Matrix::Rgb, Range::Full, AlphaMode::Opaque, Primaries::Bt601_525));
        for f in ["rgba", "bgr24", "bgra", "abgr", "0rgb", "bgr0", "rgb48le", "gbrp10le", "gbrap", "x2rgb10le", "x2bgr10le", "pal8"] {
            let c = ColorInfo::guess_video_format(1920, 1080, f);
            assert_eq!((c.matrix, c.range), (Matrix::Rgb, Range::Full), "{f}");
        }
        let tagged_yuv = ColorInfo::from_ffprobe(1920, 1080, "yuv420p", Some("bt709"), Some("bt709"), Some("bt709"), Some("tv"));
        assert_eq!((tagged_yuv.matrix, tagged_yuv.range, tagged_yuv.alpha), (Matrix::Bt709, Range::Limited, AlphaMode::Opaque));
        assert_eq!(ColorInfo::guess_video_format(1920, 1080, "yuv420p"), ColorInfo::guess_video(1920, 1080), "untagged YUV unaffected");
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
        for f in ["yuva444p10le", "yuva420p", "rgba", "argb", "bgra", "abgr", "gbrap", "gbrap12le", "gbrap10le", "ya8", "ya16be", "pal8", "rgba64le", "bgra64be", "vuya", "ayuv64le", "uyva"] {
            assert_eq!(alpha_of_pixel_format(f), AlphaMode::Straight, "{f}");
        }
        for f in ["yuv420p", "yuvj420p", "yuv444p10le", "rgb24", "bgr0", "nv12", "gray", "gbrp10le", "p010le", "yuyv422"] {
            assert_eq!(alpha_of_pixel_format(f), AlphaMode::Opaque, "{f}");
        }
    }

    #[test]
    fn unknown_or_missing_pixel_formats_may_have_alpha() {
        // Fail-safe (Ruling R13): a miss costs an extra decode, never a wrong frame.
        for f in ["weird_new_fmt", ""] {
            assert_eq!(alpha_of_pixel_format(f), AlphaMode::Straight, "{f:?}");
        }
    }
}
