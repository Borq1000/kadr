//! JSON snapshots of editor state for MCP (pure: no window needed).

use kadr_core::{ClipId, Time};
use kadr_project::{Sequence, TrackKind};
use serde_json::{json, Value};

/// The characters the same physical keys produce on the `to` layout.
pub fn layout_text(text: &str, to: &str) -> String {
    let (from, dst) = if to == "ru" { (crate::keys::EN, crate::keys::RU) } else { (crate::keys::RU, crate::keys::EN) };
    text.chars()
        .map(|c| from.chars().position(|f| f == c.to_lowercase().next().unwrap_or(c)).and_then(|i| dst.chars().nth(i)).unwrap_or(c))
        .collect()
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
}
