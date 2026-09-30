use super::{run_capture, FfmpegCli};
use crate::{MediaError, Result};
use kadr_core::color::AlphaMode;
use kadr_core::{AudioInfo, FrameRate, MediaInfo, MediaKind, Time, VideoInfo};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<Stream>,
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Format {
    #[serde(default)]
    format_name: String,
    duration: Option<String>,
    size: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    duration: Option<String>,
    nb_frames: Option<String>,
    #[serde(default)]
    disposition: std::collections::HashMap<String, i32>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
    sample_aspect_ratio: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
}

/// "8:9" → (8, 9); "0:1", "N/A" or garbage → None.
fn parse_ratio(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once(':')?;
    let (a, b) = (a.parse().ok()?, b.parse().ok()?);
    (a > 0 && b > 0).then_some((a, b))
}

const IMAGE_CODECS: &[&str] = &["png", "mjpeg", "bmp", "webp", "tiff", "gif", "jpegls", "targa"];
/// Default length of a still image dropped on the timeline.
pub const IMAGE_DEFAULT_DURATION: Time = Time::from_secs(5);

pub(super) fn probe(ff: &FfmpegCli, path: &Path) -> Result<MediaInfo> {
    let mut cmd = ff.ffprobe_cmd();
    cmd.args(["-print_format", "json", "-show_format", "-show_streams"]).arg(path);
    let out = run_capture(cmd, "ffprobe")?;
    parse(&out).ok_or_else(|| MediaError::Unsupported(path.display().to_string()))
}

fn secs(s: &Option<String>) -> Option<Time> {
    s.as_deref().and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0).map(Time::from_secs_f64)
}

pub(crate) fn parse(json: &[u8]) -> Option<MediaInfo> {
    let p: Probe = serde_json::from_slice(json).ok()?;
    let format = p.format?;
    // Ignore embedded cover art: it is a "video" stream with attached_pic.
    let v = p
        .streams
        .iter()
        .find(|s| s.codec_type == "video" && s.disposition.get("attached_pic").copied().unwrap_or(0) == 0);
    let a = p.streams.iter().find(|s| s.codec_type == "audio");
    if v.is_none() && a.is_none() {
        return None;
    }
    let is_image = v.is_some_and(|v| {
        IMAGE_CODECS.contains(&v.codec_name.as_str())
            && (format.format_name.contains("image2") || format.format_name.ends_with("_pipe") || v.nb_frames.as_deref() == Some("1"))
    }) && a.is_none();

    let kind = if is_image {
        MediaKind::Image
    } else if v.is_some() {
        MediaKind::Video
    } else {
        MediaKind::Audio
    };

    let video = v.map(|v| {
        let r = v.r_frame_rate.as_deref().and_then(FrameRate::parse);
        let avg = v.avg_frame_rate.as_deref().and_then(FrameRate::parse);
        let rotation = v
            .side_data_list
            .iter()
            .find_map(|sd| sd.get("rotation").and_then(|r| r.as_i64()))
            .or_else(|| v.tags.get("rotate").and_then(|r| r.parse().ok()))
            .unwrap_or(0) as i32;
        VideoInfo {
            width: v.width.unwrap_or(0),
            height: v.height.unwrap_or(0),
            // avg is what plays; r is the timebase-level max. Prefer avg.
            frame_rate: avg.or(r),
            variable_frame_rate: match (r, avg) {
                (Some(r), Some(a)) => (r.as_f64() - a.as_f64()).abs() > 0.01,
                _ => false,
            },
            codec: v.codec_name.clone(),
            pixel_format: v.pix_fmt.clone().unwrap_or_default(),
            rotation,
            sar: v.sample_aspect_ratio.as_deref().and_then(parse_ratio).unwrap_or((1, 1)),
            color: Some(if is_image {
                kadr_core::ColorInfo::IMAGE_SRGB
            } else {
                let c = kadr_core::ColorInfo::from_ffprobe(
                    v.width.unwrap_or(0),
                    v.height.unwrap_or(0),
                    v.pix_fmt.as_deref().unwrap_or(""),
                    v.color_primaries.as_deref(),
                    v.color_transfer.as_deref(),
                    v.color_space.as_deref(),
                    v.color_range.as_deref(),
                );
                // VP8/VP9 in WebM keep alpha in a side stream: the pixel
                // format says yuv420p, the `alpha_mode` tag says otherwise.
                if v.tags.get("alpha_mode").map(String::as_str) == Some("1") {
                    kadr_core::ColorInfo { alpha: AlphaMode::Straight, ..c }
                } else {
                    c
                }
            }),
        }
    });

    let audio = a.map(|a| AudioInfo {
        sample_rate: a.sample_rate.as_deref().and_then(|s| s.parse().ok()).unwrap_or(48_000),
        channels: a.channels.unwrap_or(2),
        codec: a.codec_name.clone(),
        channel_layout: a.channel_layout.clone().unwrap_or_default(),
    });

    let duration = if is_image {
        IMAGE_DEFAULT_DURATION
    } else {
        secs(&format.duration).or_else(|| v.and_then(|v| secs(&v.duration))).or_else(|| a.and_then(|a| secs(&a.duration)))?
    };

    let timecode = v.and_then(|v| v.tags.get("timecode").cloned()).or_else(|| format.tags.get("timecode").cloned());

    Some(MediaInfo {
        kind,
        duration,
        container: format.format_name,
        size_bytes: format.size.and_then(|s| s.parse().ok()).unwrap_or(0),
        video,
        audio,
        timecode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_with_audio_and_ignores_cover_art() {
        let json = br#"{
          "streams": [
            {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"pix_fmt":"yuv420p",
             "r_frame_rate":"30000/1001","avg_frame_rate":"30000/1001","tags":{"timecode":"01:00:00;00"}},
            {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"channel_layout":"stereo"},
            {"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}
          ],
          "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"20.020000","size":"123"}
        }"#;
        let m = parse(json).unwrap();
        assert_eq!(m.kind, MediaKind::Video);
        assert_eq!(m.video.as_ref().unwrap().frame_rate, Some(FrameRate::FPS_29_97));
        assert_eq!(m.duration, Time::from_millis(20_020));
        assert_eq!(m.timecode.as_deref(), Some("01:00:00;00"));
        assert_eq!(m.audio.unwrap().channels, 2);
    }

    #[test]
    fn detects_images_and_vfr() {
        let img = br#"{"streams":[{"codec_type":"video","codec_name":"png","width":10,"height":10}],
                      "format":{"format_name":"png_pipe"}}"#;
        let m = parse(img).unwrap();
        assert_eq!(m.kind, MediaKind::Image);
        assert_eq!(m.duration, IMAGE_DEFAULT_DURATION);

        let vfr = br#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1080,"height":1920,
                      "r_frame_rate":"60/1","avg_frame_rate":"2997/100",
                      "side_data_list":[{"rotation":-90}]}],
                      "format":{"format_name":"mov","duration":"3.0"}}"#;
        let m = parse(vfr).unwrap();
        let v = m.video.unwrap();
        assert!(v.variable_frame_rate);
        assert_eq!(v.rotation, -90);
    }

    #[test]
    fn reads_colour_tags_and_sample_aspect_ratio() {
        let json = br#"{"streams":[{"codec_type":"video","codec_name":"h264","width":720,"height":480,
                        "sample_aspect_ratio":"8:9","color_range":"tv","color_space":"smpte170m",
                        "color_transfer":"smpte170m","color_primaries":"smpte170m"}],
                        "format":{"format_name":"mov","duration":"1.0"}}"#;
        let v = parse(json).unwrap().video.unwrap();
        assert_eq!(v.sar, (8, 9));
        assert_eq!(v.display_size(), (640, 480));
        let c = v.color.unwrap();
        assert_eq!((c.matrix, c.range), (kadr_core::color::Matrix::Bt601, kadr_core::color::Range::Limited));

        let untagged = br#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"sample_aspect_ratio":"0:1"}],
                            "format":{"format_name":"mov","duration":"1.0"}}"#;
        let v = parse(untagged).unwrap().video.unwrap();
        assert_eq!(v.sar, (1, 1));
        assert_eq!(v.color.unwrap().matrix, kadr_core::color::Matrix::Bt709);

        let png = br#"{"streams":[{"codec_type":"video","codec_name":"png","width":10,"height":10}],"format":{"format_name":"png_pipe"}}"#;
        assert_eq!(parse(png).unwrap().video.unwrap().color, Some(kadr_core::ColorInfo::IMAGE_SRGB));
    }

    #[test]
    fn video_alpha_comes_from_pixel_format_or_alpha_mode_tag() {
        use kadr_core::color::AlphaMode;
        let alpha = |stream: &str| {
            let json = format!(r#"{{"streams":[{stream}],"format":{{"format_name":"mov","duration":"1.0"}}}}"#);
            parse(json.as_bytes()).unwrap().video.unwrap().color.unwrap().alpha
        };
        let prores = r#"{"codec_type":"video","codec_name":"prores","width":1920,"height":1080,"pix_fmt":"yuva444p10le"}"#;
        assert_eq!(alpha(prores), AlphaMode::Straight, "ProRes 4444");
        let webm = r#"{"codec_type":"video","codec_name":"vp9","width":1920,"height":1080,"pix_fmt":"yuv420p","tags":{"alpha_mode":"1"}}"#;
        assert_eq!(alpha(webm), AlphaMode::Straight, "VP9 with an alpha side stream");
        let plain = r#"{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"pix_fmt":"yuv420p"}"#;
        assert_eq!(alpha(plain), AlphaMode::Opaque);
    }

    #[test]
    fn untagged_rgb_video_is_rgb_full_range() {
        use kadr_core::color::{AlphaMode, Matrix, Range};
        let colour = |stream: &str| {
            let json = format!(r#"{{"streams":[{stream}],"format":{{"format_name":"mov","duration":"1.0"}}}}"#);
            let c = parse(json.as_bytes()).unwrap().video.unwrap().color.unwrap();
            (c.matrix, c.range, c.alpha)
        };
        let qtrle = r#"{"codec_type":"video","codec_name":"qtrle","width":1920,"height":1080,"pix_fmt":"argb"}"#;
        assert_eq!(colour(qtrle), (Matrix::Rgb, Range::Full, AlphaMode::Straight));
        let rgb24 = r#"{"codec_type":"video","codec_name":"qtrle","width":1920,"height":1080,"pix_fmt":"rgb24"}"#;
        assert_eq!(colour(rgb24), (Matrix::Rgb, Range::Full, AlphaMode::Opaque));
        let tagged_yuv = r#"{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"pix_fmt":"yuv420p","color_space":"bt709","color_range":"tv"}"#;
        assert_eq!(colour(tagged_yuv), (Matrix::Bt709, Range::Limited, AlphaMode::Opaque));
    }
}
