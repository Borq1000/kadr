//! JSON snapshots of editor state for MCP (pure: no window needed).

use kadr_core::{ClipId, Time};
use kadr_mcp_bridge::BridgeError;
use kadr_project::{Project, Sequence, TrackKind};
use serde_json::{json, Value};
use std::path::PathBuf;

/// What the same key gives with Shift, on the US and the Russian (ЙЦУКЕН)
/// layout, in the order of `keys::EN` / `keys::RU`.
const EN_SHIFT: &str = "QWERTYUIOP{}ASDFGHJKL:\"ZXCVBNM<>";
const RU_SHIFT: &str = "ЙЦУКЕНГШЩЗХЪФЫВАПРОЛДЖЭЯЧСМИТЬБЮ";
/// Keys outside the letter block: `` ` ``/ё, `/`/`.` and the Shift digits
/// that differ (US on the left, Russian on the right).
const EN_OTHER: &str = "`~/?@#$^&";
const RU_OTHER: &str = "ёЁ.,\"№;:?";

/// The characters the same physical keys (with the same Shift state)
/// produce on the `to` layout.
pub fn layout_text(text: &str, to: &str) -> String {
    let en = [crate::keys::EN, EN_SHIFT, EN_OTHER].concat();
    let ru = [crate::keys::RU, RU_SHIFT, RU_OTHER].concat();
    let (from, dst) = if to == "ru" { (en, ru) } else { (ru, en) };
    let dst: Vec<char> = dst.chars().collect();
    text.chars().map(|c| from.chars().position(|f| f == c).map_or(c, |i| dst[i])).collect()
}

/// What `get_frame` decodes for a timeline time.
#[derive(Debug, PartialEq)]
pub struct FrameSource {
    pub path: PathBuf,
    /// Time in the source file.
    pub source: Time,
    /// Displayed size (rotation applied), when probed.
    pub size: Option<(u32, u32)>,
}

/// The video frame shown at `t`, or why there is none.
pub fn frame_source(project: &Project, t: Time) -> Result<FrameSource, BridgeError> {
    let seq = project.sequence();
    let end = seq.duration();
    if t < Time::ZERO {
        return Err(BridgeError::new("bad_params", "at_ms must not be negative"));
    }
    if end == Time::ZERO {
        return Err(BridgeError::new("not_found", "the timeline is empty"));
    }
    if t >= end {
        return Err(BridgeError::new("not_found", format!("at_ms {} is past the end of the sequence ({} ms)", t.as_millis(), end.as_millis())));
    }
    let Some(v) = kadr_timeline::composition::video_at(seq, t) else {
        let audio = seq.tracks.iter().any(|tr| tr.kind == TrackKind::Audio && tr.clip_at(t).is_some());
        let why = if audio { "only audio" } else { "a gap" };
        return Err(BridgeError::new("not_found", format!("no video at {} ms: {why} there", t.as_millis())));
    };
    let asset = project.asset(v.asset).ok_or_else(|| BridgeError::new("not_found", "the clip's media is missing from the project"))?;
    let size = asset.info.video.as_ref().map(|i| if i.rotation.rem_euclid(180) == 90 { (i.height, i.width) } else { (i.width, i.height) });
    Ok(FrameSource { path: asset.path.clone(), source: v.source_start, size })
}

/// Output size for a frame of `src` size: at most `max_w` wide (and
/// `max_h` tall when given), aspect kept, never upscaled, even, never zero.
pub fn frame_size(src: Option<(u32, u32)>, max_w: u32, max_h: Option<u32>) -> (u32, u32) {
    let even = |x: f64| ((x.round() as u32) & !1).max(2);
    let Some((w, h)) = src.filter(|(w, h)| *w > 0 && *h > 0) else {
        return (even(max_w as f64), even(max_h.unwrap_or(max_w.saturating_mul(4)) as f64));
    };
    let mut scale = (max_w as f64 / w as f64).min(1.0);
    if let Some(mh) = max_h {
        scale = scale.min(mh as f64 / h as f64);
    }
    (even(w as f64 * scale), even(h as f64 * scale))
}

pub fn timeline_json(seq: &Sequence, selection: &[ClipId], playhead: Time) -> Value {
    let ms = |t: Time| t.as_millis();
    json!({
        "name": seq.name,
        "format": {"fps": seq.frame_rate.to_string(), "width": seq.width, "height": seq.height},
        "duration_ms": ms(seq.duration()),
        "playhead_ms": ms(playhead),
        "in_out": seq.in_out.map(|r| json!([ms(r.start), ms(r.end)])),
        "tracks": seq.tracks.iter().map(|t| json!({
            "id": t.id.to_string(),
            "name": t.name,
            "kind": if t.kind == TrackKind::Video { "video" } else { "audio" },
            "muted": t.muted, "locked": t.locked,
            "clips": t.clips.iter().map(|c| json!({
                "id": c.id.to_string(), "name": c.name, "asset": c.asset.to_string(),
                "start_ms": ms(c.timeline_in), "end_ms": ms(c.timeline_out),
                "source_in_ms": ms(c.source_in), "enabled": c.enabled,
                "link": c.link.map(|l| l.to_string()),
                "multicam": c.multicam.as_ref().map(|m| json!({"group": m.group.to_string(), "angle": m.angle})),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "markers": seq.markers.iter().map(|m| json!({"name": m.name, "at_ms": ms(m.time)})).collect::<Vec<_>>(),
        "selection": selection.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{LinkId, MediaInfo, MediaKind, Time, TimeRange};
    use kadr_project::{Clip, Project};

    #[test]
    fn timeline_json_lists_tracks_clips_links_and_selection() {
        let mut p = Project::new("t");
        let asset = kadr_project::MediaAsset::new("a.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(10), container: "mp4".into(), size_bytes: 0, video: None, audio: None, timecode: None });
        let link = LinkId::new();
        let mut v = Clip::new(asset.id, "a", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::from_secs(1));
        v.link = Some(link);
        let id = v.id;
        p.sequence_mut().tracks[0].clips.push(v);
        let j = timeline_json(p.sequence(), &[id], Time::from_millis(1500));
        assert_eq!(j["playhead_ms"], 1500);
        let clip = &j["tracks"][0]["clips"][0];
        assert_eq!((clip["start_ms"].as_i64(), clip["end_ms"].as_i64()), (Some(1000), Some(5000)));
        assert!(clip["link"].is_string());
        assert_eq!(j["selection"][0], id.to_string());
        assert_eq!(j["tracks"][0]["kind"], "video");
    }

    #[test]
    fn layout_text_maps_physical_keys_both_ways() {
        assert_eq!(layout_text("ug", "ru"), "гп");
        assert_eq!(layout_text("гп", "en"), "ug");
        assert_eq!(layout_text("1 ,.", "ru"), "1 бю");
    }

    #[test]
    fn layout_text_keeps_letter_case() {
        assert_eq!(layout_text("Ug", "ru"), "Гп");
        assert_eq!(layout_text("ГП", "en"), "UG");
    }

    #[test]
    fn layout_text_knows_what_shift_gives_on_symbol_keys() {
        assert_eq!(layout_text("ХЪЖЭБЮ", "en"), "{}:\"<>");
        assert_eq!(layout_text("{}:\"<>", "ru"), "ХЪЖЭБЮ");
        assert_eq!(layout_text("ёЁ", "en"), "`~");
        assert_eq!(layout_text("`~", "ru"), "ёЁ");
        assert_eq!(layout_text(".,", "en"), "/?", "on the Russian layout . and , sit on the / key");
        assert_eq!(layout_text("/?", "ru"), ".,");
        assert_eq!(layout_text("\"№;:?", "en"), "@#$^&");
    }

    fn asset(kind: MediaKind, w: u32, h: u32, rotation: i32) -> kadr_project::MediaAsset {
        let video = (kind == MediaKind::Video).then(|| kadr_core::VideoInfo { width: w, height: h, frame_rate: None, variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation });
        kadr_project::MediaAsset::new("a.mp4", MediaInfo { kind, duration: Time::from_secs(10), container: "mp4".into(), size_bytes: 0, video, audio: None, timecode: None })
    }

    /// A project with `asset` placed on `track` from `start` to `end` seconds.
    fn placed(a: kadr_project::MediaAsset, track: usize, start: i64, end: i64) -> Project {
        let mut p = Project::new("t");
        let c = Clip::new(a.id, "a", TimeRange::new(Time::ZERO, Time::from_secs(end - start)), Time::from_secs(start));
        p.sequence_mut().tracks[track].clips.push(c);
        p.assets.push(a);
        p
    }

    #[test]
    fn timeline_json_of_an_empty_project() {
        let j = timeline_json(Project::new("t").sequence(), &[], Time::ZERO);
        assert_eq!(j["duration_ms"], 0);
        assert!(j["tracks"].as_array().unwrap().iter().all(|t| t["clips"].as_array().unwrap().is_empty()));
        assert!(j["in_out"].is_null());
    }

    #[test]
    fn frame_source_explains_why_there_is_no_frame() {
        let msg = |p: &Project, ms| frame_source(p, Time::from_millis(ms)).unwrap_err().message;
        assert!(msg(&Project::new("t"), 0).contains("empty"), "{}", msg(&Project::new("t"), 0));
        let video = placed(asset(MediaKind::Video, 1920, 1080, 0), 0, 1, 4);
        assert!(msg(&video, 5000).contains("past the end"), "{}", msg(&video, 5000));
        assert!(msg(&video, 500).contains("no video"), "{}", msg(&video, 500));
        let audio = placed(asset(MediaKind::Audio, 0, 0, 0), 1, 0, 4);
        assert!(msg(&audio, 1000).contains("only audio"), "{}", msg(&audio, 1000));
        assert_eq!(frame_source(&video, Time::from_millis(-1)).unwrap_err().code, "bad_params");
    }

    #[test]
    fn frame_source_finds_the_clip_and_its_displayed_size() {
        let p = placed(asset(MediaKind::Video, 1920, 1080, 0), 0, 1, 4);
        let f = frame_source(&p, Time::from_millis(1500)).unwrap();
        assert_eq!((f.source, f.size), (Time::from_millis(500), Some((1920, 1080))));
        let phone = placed(asset(MediaKind::Video, 1920, 1080, 90), 0, 0, 4);
        assert_eq!(frame_source(&phone, Time::from_millis(1)).unwrap().size, Some((1080, 1920)), "rotated streams display swapped");
    }

    #[test]
    fn frame_size_limits_width_keeps_aspect_and_never_upscales() {
        assert_eq!(frame_size(Some((1920, 1080)), 960, None), (960, 540));
        assert_eq!(frame_size(Some((1080, 1920)), 960, None), (960, 1706), "portrait is limited by width, not squeezed into a square");
        assert_eq!(frame_size(Some((640, 360)), 960, None), (640, 360));
        assert_eq!(frame_size(Some((1920, 1080)), 960, Some(270)), (480, 270));
        assert_eq!(frame_size(None, 960, None), (960, 3840), "unknown size: a generous box");
        assert_eq!(frame_size(Some((1920, 1080)), 1, None), (2, 2), "never zero");
    }
}
