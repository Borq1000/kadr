//! Compact, text-only project context for LLM prompts. Contains timeline
//! structure and analysis summaries — never media, frames or audio.

use crate::local_ops::timeline_silences;
use kadr_core::{format_duration, ClipId, Time, TimeRange};
use kadr_project::Project;
use serde_json::json;

#[derive(Clone, Debug, Default)]
pub struct EditorState {
    pub playhead: Time,
    pub selection: Vec<ClipId>,
    pub in_out: Option<TimeRange>,
}

const MAX_CLIPS: usize = 300;

pub fn project_context(project: &Project, st: &EditorState) -> serde_json::Value {
    let seq = project.sequence();
    let mut n = 0;
    let tracks: Vec<_> = seq
        .tracks
        .iter()
        .map(|t| {
            let clips: Vec<_> = t
                .clips
                .iter()
                .take(MAX_CLIPS.saturating_sub(n))
                .map(|c| {
                    json!({
                        "id": c.id.to_string(),
                        "name": c.name,
                        "start_ms": c.timeline_in.as_millis(),
                        "end_ms": c.timeline_out.as_millis(),
                        "source_in_ms": c.source_in.as_millis(),
                        "speed": (c.speed() * 1000.0).round() / 1000.0,
                        "selected": st.selection.contains(&c.id),
                    })
                })
                .collect();
            n += clips.len();
            json!({"name": t.name, "kind": t.kind, "muted": t.muted, "locked": t.locked, "clips": clips})
        })
        .collect();
    let silences: Vec<_> = timeline_silences(project)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.duration() >= Time::from_millis(700))
        .take(200)
        .map(|r| json!([r.start.as_millis(), r.end.as_millis()]))
        .collect();
    json!({
        "sequence": {
            "id": seq.id.to_string(),
            "name": seq.name,
            "duration_ms": seq.duration().as_millis(),
            "duration": format_duration(seq.duration()),
            "fps": seq.frame_rate.to_string(),
            "resolution": format!("{}x{}", seq.width, seq.height),
        },
        "playhead_ms": st.playhead.as_millis(),
        "in_out_ms": st.in_out.map(|r| [r.start.as_millis(), r.end.as_millis()]),
        "tracks": tracks,
        "markers": seq.markers.iter().map(|m| json!({"at_ms": m.time.as_millis(), "name": m.name})).collect::<Vec<_>>(),
        "silences_ms": silences,
        "truncated": n >= MAX_CLIPS,
    })
}
