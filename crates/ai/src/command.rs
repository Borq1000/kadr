//! Edit Command Language — the only thing an AI (local or LLM) can produce.
//!
//! A closed, versioned JSON schema. There is no "free text that gets
//! executed", no filesystem, no shell: a command either maps onto a
//! deterministic `kadr_timeline::EditCommand` after validation, or it is
//! rejected with a reason the user can see.

use kadr_core::{ClipId, Time, TimeRange};
use kadr_project::{Marker, Project, TrackKind, Transform, Transition, TransitionKind};
use kadr_timeline::{ClipProperty, EditCommand, TrimEdge};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiCommand {
    SplitClip {
        at_ms: i64,
        #[serde(default)]
        clip_id: Option<String>,
        #[serde(default)]
        reason: String,
    },
    DeleteRange {
        #[serde(default)]
        sequence_id: Option<String>,
        start_ms: i64,
        end_ms: i64,
        #[serde(default = "yes")]
        ripple: bool,
        #[serde(default)]
        reason: String,
    },
    DeleteClip {
        clip_id: String,
        #[serde(default)]
        ripple: bool,
        #[serde(default)]
        reason: String,
    },
    TrimClip {
        clip_id: String,
        edge: Edge,
        to_ms: i64,
        #[serde(default)]
        ripple: bool,
        #[serde(default)]
        reason: String,
    },
    MoveClip {
        clip_id: String,
        to_ms: i64,
        #[serde(default)]
        reason: String,
    },
    SetTransform {
        clip_id: String,
        #[serde(default)]
        scale: Option<f64>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        #[serde(default)]
        rotation_deg: Option<f64>,
        #[serde(default)]
        opacity: Option<f64>,
        #[serde(default)]
        reason: String,
    },
    ChangeSpeed {
        clip_id: String,
        speed: f64,
        #[serde(default)]
        reason: String,
    },
    SetAudioGain {
        clip_id: String,
        gain_db: f64,
        #[serde(default)]
        reason: String,
    },
    AddMarker {
        at_ms: i64,
        name: String,
        #[serde(default)]
        reason: String,
    },
    AddTransition {
        at_ms: i64,
        kind: TransitionKind,
        duration_ms: i64,
        #[serde(default)]
        reason: String,
    },
    /// Show `angle` (its label, e.g. "CAM2") over a range of a multicam clip.
    SelectCamera {
        start_ms: i64,
        end_ms: i64,
        angle: String,
        #[serde(default)]
        reason: String,
    },
    /// Reserved: captions (V0.3).
    AddCaption {
        start_ms: i64,
        end_ms: i64,
        text: String,
    },
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Start,
    End,
}

impl AiCommand {
    pub fn reason(&self) -> &str {
        match self {
            AiCommand::SplitClip { reason, .. }
            | AiCommand::DeleteRange { reason, .. }
            | AiCommand::DeleteClip { reason, .. }
            | AiCommand::TrimClip { reason, .. }
            | AiCommand::MoveClip { reason, .. }
            | AiCommand::SetTransform { reason, .. }
            | AiCommand::ChangeSpeed { reason, .. }
            | AiCommand::SetAudioGain { reason, .. }
            | AiCommand::AddMarker { reason, .. }
            | AiCommand::AddTransition { reason, .. }
            | AiCommand::SelectCamera { reason, .. } => reason,
            AiCommand::AddCaption { .. } => "",
        }
    }
}

/// What the current AI source may do. Destructive edits can be disabled
/// independently (e.g. "AI may only add markers").
#[derive(Clone, Debug)]
pub struct Permissions {
    pub allow_destructive: bool,
    /// Upper bound on commands in one AI action.
    pub max_commands: usize,
}

impl Default for Permissions {
    fn default() -> Self {
        Permissions { allow_destructive: true, max_commands: 500 }
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    #[error("command {index}: malformed JSON: {msg}")]
    Schema { index: usize, msg: String },
    #[error("command {index}: {msg}")]
    Range { index: usize, msg: String },
    #[error("command {index}: unknown clip {id}")]
    UnknownClip { index: usize, id: String },
    #[error("command {index}: not permitted: {msg}")]
    Permission { index: usize, msg: String },
    #[error("command {index}: {what} is not supported yet")]
    Unsupported { index: usize, what: &'static str },
    #[error("too many commands ({0})")]
    TooMany(usize),
    #[error("sequence id does not match the active sequence")]
    WrongSequence,
}

/// Parses a JSON array (or `{"commands": [...]}`) strictly.
pub fn parse_commands(json: &str) -> Result<Vec<AiCommand>, ValidationError> {
    let v: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| ValidationError::Schema { index: 0, msg: e.to_string() })?;
    let arr = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(mut o) => match o.remove("commands") {
            Some(serde_json::Value::Array(a)) => a,
            _ => return Err(ValidationError::Schema { index: 0, msg: "expected an array of commands".into() }),
        },
        _ => return Err(ValidationError::Schema { index: 0, msg: "expected an array of commands".into() }),
    };
    arr.into_iter()
        .enumerate()
        .map(|(i, c)| serde_json::from_value(c).map_err(|e| ValidationError::Schema { index: i, msg: e.to_string() }))
        .collect()
}

/// Validates against the current project state and translates into engine
/// commands. All-or-nothing: one invalid command rejects the whole action.
pub fn validate(cmds: &[AiCommand], project: &Project, perms: &Permissions) -> Result<Vec<EditCommand>, ValidationError> {
    if cmds.len() > perms.max_commands {
        return Err(ValidationError::TooMany(cmds.len()));
    }
    let seq = project.sequence();
    let dur = seq.duration();
    let ms = Time::from_millis;
    let in_seq = |index: usize, t: i64| -> Result<Time, ValidationError> {
        let tt = ms(t);
        if t < 0 || tt > dur {
            return Err(ValidationError::Range { index, msg: format!("time {t} ms outside sequence (0..{} ms)", dur.as_millis()) });
        }
        Ok(tt)
    };
    let clip = |index: usize, id: &str| -> Result<ClipId, ValidationError> {
        ClipId::parse(id)
            .filter(|c| seq.clip(*c).is_some())
            .ok_or_else(|| ValidationError::UnknownClip { index, id: id.to_string() })
    };
    let destructive = |index: usize| -> Result<(), ValidationError> {
        if perms.allow_destructive { Ok(()) } else { Err(ValidationError::Permission { index, msg: "destructive edits are disabled for AI".into() }) }
    };

    let mut out = Vec::with_capacity(cmds.len());
    for (i, c) in cmds.iter().enumerate() {
        let cmd = match c {
            AiCommand::SplitClip { at_ms, clip_id, .. } => {
                let at = in_seq(i, *at_ms)?;
                let clips = clip_id.as_deref().map(|id| clip(i, id)).transpose()?.map(|c| vec![c]);
                EditCommand::Split { at, clips }
            }
            AiCommand::DeleteRange { sequence_id, start_ms, end_ms, ripple, .. } => {
                destructive(i)?;
                if let Some(sid) = sequence_id.as_deref().filter(|s| !s.is_empty()) {
                    if sid != seq.id.to_string() {
                        return Err(ValidationError::WrongSequence);
                    }
                }
                let (s, e) = (in_seq(i, *start_ms)?, in_seq(i, *end_ms)?);
                if e <= s {
                    return Err(ValidationError::Range { index: i, msg: "end must be after start".into() });
                }
                EditCommand::DeleteRange { range: TimeRange::new(s, e), ripple: *ripple }
            }
            AiCommand::DeleteClip { clip_id, ripple, .. } => {
                destructive(i)?;
                EditCommand::DeleteClips { clips: vec![clip(i, clip_id)?], ripple: *ripple }
            }
            AiCommand::TrimClip { clip_id, edge, to_ms, ripple, .. } => {
                destructive(i)?;
                let edge = match edge {
                    Edge::Start => TrimEdge::Start,
                    Edge::End => TrimEdge::End,
                };
                if *to_ms < 0 {
                    return Err(ValidationError::Range { index: i, msg: "negative time".into() });
                }
                EditCommand::TrimClip { clip: clip(i, clip_id)?, edge, to: ms(*to_ms), ripple: *ripple }
            }
            AiCommand::MoveClip { clip_id, to_ms, .. } => {
                let id = clip(i, clip_id)?;
                if *to_ms < 0 {
                    return Err(ValidationError::Range { index: i, msg: "negative time".into() });
                }
                let cur = seq.clip(id).unwrap().timeline_in;
                EditCommand::MoveClips { clips: vec![id], delta: ms(*to_ms) - cur, track_delta: 0 }
            }
            AiCommand::SetTransform { clip_id, scale, x, y, rotation_deg, opacity, .. } => {
                let id = clip(i, clip_id)?;
                let mut t: Transform = seq.clip(id).unwrap().transform.clone();
                if let Some(v) = scale {
                    check(i, "scale", *v, 0.01, 16.0)?;
                    t.scale = *v;
                }
                if let Some(v) = opacity {
                    check(i, "opacity", *v, 0.0, 1.0)?;
                    t.opacity = *v;
                }
                if let Some(v) = rotation_deg {
                    check(i, "rotation", *v, -3600.0, 3600.0)?;
                    t.rotation_deg = *v;
                }
                if let Some(v) = x {
                    check(i, "x", *v, -20_000.0, 20_000.0)?;
                    t.x = *v;
                }
                if let Some(v) = y {
                    check(i, "y", *v, -20_000.0, 20_000.0)?;
                    t.y = *v;
                }
                EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Transform(t) }
            }
            AiCommand::ChangeSpeed { clip_id, speed, .. } => {
                check(i, "speed", *speed, 0.05, 20.0)?;
                EditCommand::SetClipProperty { clip: clip(i, clip_id)?, prop: ClipProperty::Speed(*speed) }
            }
            AiCommand::SetAudioGain { clip_id, gain_db, .. } => {
                check(i, "gain_db", *gain_db, -60.0, 24.0)?;
                let id = clip(i, clip_id)?;
                let (ti, _) = seq.locate_clip(id).unwrap();
                if seq.tracks[ti].kind != TrackKind::Audio {
                    return Err(ValidationError::Range { index: i, msg: "gain applies to audio clips".into() });
                }
                let mut a = seq.clip(id).unwrap().audio.clone();
                a.gain_db = *gain_db;
                EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Audio(a) }
            }
            AiCommand::AddMarker { at_ms, name, .. } => {
                let at = in_seq(i, *at_ms)?;
                let name: String = name.chars().take(200).collect();
                EditCommand::AddMarker(Marker::new(at, name))
            }
            AiCommand::AddTransition { at_ms, kind, duration_ms, .. } => {
                let at = in_seq(i, *at_ms)?;
                if !(40..=10_000).contains(duration_ms) {
                    return Err(ValidationError::Range { index: i, msg: "transition duration 40..10000 ms".into() });
                }
                let track = seq.tracks.iter().find(|t| t.kind == TrackKind::Video).map(|t| t.id).ok_or(
                    ValidationError::Range { index: i, msg: "no video track".into() },
                )?;
                EditCommand::AddTransition(Transition {
                    id: kadr_core::TransitionId::new(),
                    kind: *kind,
                    track,
                    at,
                    duration: ms(*duration_ms),
                })
            }
            AiCommand::SelectCamera { start_ms, end_ms, angle, .. } => {
                // Milliseconds can't name flick-exact positions: a bound within
                // half a frame of a clip edge means that edge (edges need not
                // sit on frames), anything else lands on a frame. Otherwise a
                // bound misses the clip it names or cuts off a sliver.
                let rate = seq.frame_rate;
                let half = rate.frame_duration().div_ratio(1, 2);
                let snap = |t: Time| {
                    seq.tracks
                        .iter()
                        .flat_map(|t| &t.clips)
                        .flat_map(|c| [c.timeline_in, c.timeline_out])
                        .filter(|edge| (*edge - t).abs() < half)
                        .min_by_key(|edge| (*edge - t).abs())
                        .unwrap_or_else(|| rate.snap(t))
                };
                let (s, e) = (snap(in_seq(i, *start_ms)?), snap(in_seq(i, *end_ms)?));
                if e <= s {
                    return Err(ValidationError::Range { index: i, msg: "end must be after start".into() });
                }
                // The multicam clip on the topmost video track at the start.
                let sel = seq
                    .tracks
                    .iter()
                    .rev()
                    .filter(|t| t.kind == TrackKind::Video)
                    .find_map(|t| t.clip_at(s).and_then(|c| c.multicam.clone()))
                    .ok_or(ValidationError::Range { index: i, msg: "no multicam clip at start_ms".into() })?;
                let group = project
                    .multicam_groups
                    .iter()
                    .find(|g| g.id == sel.group)
                    .ok_or(ValidationError::Range { index: i, msg: "multicam group missing".into() })?;
                let idx = group
                    .angles
                    .iter()
                    .position(|a| a.label.eq_ignore_ascii_case(angle.trim()))
                    .ok_or(ValidationError::Range { index: i, msg: format!("unknown angle {angle:?}") })?;
                EditCommand::SetAngleRange { range: TimeRange::new(s, e), angle: idx as u32 }
            }
            AiCommand::AddCaption { .. } => return Err(ValidationError::Unsupported { index: i, what: "captions" }),
        };
        out.push(cmd);
    }
    Ok(out)
}

fn check(index: usize, name: &str, v: f64, lo: f64, hi: f64) -> Result<(), ValidationError> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(ValidationError::Range { index, msg: format!("{name}={v} outside {lo}..{hi}") })
    }
}

/// JSON schema description embedded in LLM prompts.
pub fn schema_for_prompt() -> &'static str {
    r#"Respond with ONLY a JSON object: {"summary": string, "confidence": 0..1, "commands": [Command...]}
Command is one of (times are sequence milliseconds, integers):
 {"type":"split_clip","at_ms":int,"clip_id"?:string,"reason":string}
 {"type":"delete_range","start_ms":int,"end_ms":int,"ripple":bool,"reason":string}
 {"type":"delete_clip","clip_id":string,"ripple":bool,"reason":string}
 {"type":"trim_clip","clip_id":string,"edge":"start"|"end","to_ms":int,"ripple":bool,"reason":string}
 {"type":"move_clip","clip_id":string,"to_ms":int,"reason":string}
 {"type":"set_transform","clip_id":string,"scale"?:num,"x"?:num,"y"?:num,"rotation_deg"?:num,"opacity"?:num,"reason":string}
 {"type":"change_speed","clip_id":string,"speed":num,"reason":string}
 {"type":"set_audio_gain","clip_id":string,"gain_db":num,"reason":string}
 {"type":"add_marker","at_ms":int,"name":string,"reason":string}
 {"type":"add_transition","at_ms":int,"kind":"cross_dissolve"|"dip_to_black"|"wipe","duration_ms":int,"reason":string}
When deleting several ranges with ripple, list them from the LATEST to the EARLIEST.
If the request cannot be done with these commands, return an empty commands list and explain in summary."#
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{MediaInfo, MediaKind, MulticamId, Time, TimeRange};
    use kadr_project::{Clip, MediaAsset, MulticamAngle, MulticamGroup, MulticamSelection, SyncMethod};

    fn multicam_project() -> Project {
        let mut p = Project::new("mc");
        let info = MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: None, audio: None, timecode: None };
        let (a, b) = (MediaAsset::new("a.mp4", info.clone()), MediaAsset::new("b.mp4", info));
        let angle = |asset, label: &str| MulticamAngle { asset, label: label.into(), description: String::new(), sync_offset: Time::ZERO, sync_method: SyncMethod::Manual };
        let group = MulticamGroup { id: MulticamId::new(), name: "g".into(), angles: vec![angle(a.id, "CAM1"), angle(b.id, "CAM2")], master_audio: None };
        let mut c = Clip::new(a.id, "CAM1", TimeRange::new(Time::ZERO, Time::from_secs(20)), Time::ZERO);
        c.multicam = Some(MulticamSelection { group: group.id, angle: 0 });
        p.sequence_mut().tracks[0].clips.push(c);
        p.assets.extend([a, b]);
        p.multicam_groups.push(group);
        p
    }

    /// The multicam clip placed at `start` for `dur` on a 29.97 fps sequence.
    fn ntsc_multicam_project(start: Time, dur: Time) -> Project {
        let mut p = multicam_project();
        let seq = p.sequence_mut();
        seq.frame_rate = kadr_core::FrameRate::FPS_29_97;
        let c = &mut seq.tracks[0].clips[0];
        (c.source_out, c.timeline_in, c.timeline_out) = (dur, start, start + dur);
        p
    }

    fn select(start_ms: i64, end_ms: i64, angle: &str) -> AiCommand {
        AiCommand::SelectCamera { start_ms, end_ms, angle: angle.into(), reason: String::new() }
    }

    #[test]
    fn select_camera_maps_to_an_angle_range() {
        let p = multicam_project();
        let cmds = validate(&[select(2_000, 6_000, "CAM2")], &p, &Permissions::default()).unwrap();
        assert_eq!(cmds, vec![EditCommand::SetAngleRange { range: TimeRange::new(Time::from_secs(2), Time::from_secs(6)), angle: 1 }]);
    }

    #[test]
    fn select_camera_snaps_millisecond_bounds_to_the_clip_frames() {
        // At 29.97 fps frame boundaries are not whole milliseconds: a clip
        // starting on frame 91 (3.0364 s) is addressed as 3033 ms, which
        // lies before the clip; its end (frame 481) as 16049 ms, just short.
        let rate = kadr_core::FrameRate::FPS_29_97;
        let (start, end) = (rate.frame_to_time(91), rate.frame_to_time(481));
        let p = ntsc_multicam_project(start, end - start);
        let cmds = validate(&[select(start.as_millis(), end.as_millis(), "CAM2")], &p, &Permissions::default()).unwrap();
        assert_eq!(cmds, vec![EditCommand::SetAngleRange { range: TimeRange::new(start, end), angle: 1 }]);
    }

    #[test]
    fn select_camera_ending_at_an_off_frame_clip_end_leaves_no_sliver() {
        // A multicam clip spans the angles' overlap, which need not be a
        // whole number of frames: this one ends 13.016 s after frame 91.
        let p = ntsc_multicam_project(kadr_core::FrameRate::FPS_29_97.frame_to_time(91), Time::from_millis(13_016));
        let clip_end = p.sequence().tracks[0].clips[0].timeline_range().end;
        let cmds = validate(&[select(4_000, clip_end.as_millis(), "CAM2")], &p, &Permissions::default()).unwrap();
        let EditCommand::SetAngleRange { range, .. } = &cmds[0] else { panic!() };
        assert_eq!(range.end, clip_end, "the end names the clip edge, not a frame 3 ms before it");
    }

    #[test]
    fn select_camera_rejects_unknown_angles_and_plain_clips() {
        let p = multicam_project();
        assert!(validate(&[select(2_000, 6_000, "CAM9")], &p, &Permissions::default()).is_err());
        assert!(validate(&[select(30_000, 32_000, "CAM2")], &p, &Permissions::default()).is_err(), "no multicam clip there");
    }
}
