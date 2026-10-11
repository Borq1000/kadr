//! Edit Command Language — the only thing an AI (local or LLM) can produce.
//!
//! A closed, versioned JSON schema. There is no "free text that gets
//! executed", no filesystem, no shell: a command either maps onto a
//! deterministic `kadr_timeline::EditCommand` after validation, or it is
//! rejected with a reason the user can see.

use kadr_core::{ClipId, EffectId, Time, TimeRange};
use kadr_project::{AnimParam, AppliedEffect, Marker, Project, TrackKey, TrackKind, Transform, Transition, TransitionKind};
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
    /// Applies an effect from the library (effects spec §7); one instance
    /// per recipe per clip: a recipe the clip has is replaced.
    AddEffect {
        clip_id: String,
        recipe: String,
        #[serde(default)]
        params: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        reason: String,
    },
    /// Changes the given parameters and/or switches the effect on or off.
    SetEffect {
        clip_id: String,
        effect_id: String,
        #[serde(default)]
        params: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        reason: String,
    },
    RemoveEffect {
        clip_id: String,
        effect_id: String,
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
            | AiCommand::SelectCamera { reason, .. }
            | AiCommand::AddEffect { reason, .. }
            | AiCommand::SetEffect { reason, .. }
            | AiCommand::RemoveEffect { reason, .. } => reason,
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
    /// The property has keyframes, so its static value is ignored (keyframes
    /// spec §3.2): writing it would succeed and change nothing (R24).
    #[error("command {index}: {param} is animated (keyframes); use set_keyframe or clear_animation")]
    Animated { index: usize, param: &'static str },
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
/// An effect command that would change nothing (the clip already has the
/// effect with these settings) is dropped, not rejected: a plan with it
/// still stands (ruling R10).
pub fn validate(cmds: &[AiCommand], project: &Project, perms: &Permissions) -> Result<Vec<EditCommand>, ValidationError> {
    Ok(validate_each(cmds, project, perms, false)?.into_iter().flatten().collect())
}

/// Which of `cmds` [`validate`] keeps (one flag per command): `false` for an
/// effect command that changes nothing. Errors as [`validate`].
pub fn kept(cmds: &[AiCommand], project: &Project, perms: &Permissions) -> Result<Vec<bool>, ValidationError> {
    Ok(validate_each(cmds, project, perms, false)?.iter().map(Option::is_some).collect())
}

/// [`validate`] for a client that sends one op and wants to know why
/// nothing happens (MCP): an effect command that changes nothing is
/// rejected with "no change".
pub fn validate_strict(cmds: &[AiCommand], project: &Project, perms: &Permissions) -> Result<Vec<EditCommand>, ValidationError> {
    Ok(validate_each(cmds, project, perms, true)?.into_iter().flatten().collect())
}

/// One entry per command: its engine command, or `None` when it changes nothing.
fn validate_each(cmds: &[AiCommand], project: &Project, perms: &Permissions, strict: bool) -> Result<Vec<Option<EditCommand>>, ValidationError> {
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
        let c = ClipId::parse(id)
            .filter(|c| seq.clip(*c).is_some())
            .ok_or_else(|| ValidationError::UnknownClip { index, id: id.to_string() })?;
        let (ti, _) = seq.locate_clip(c).unwrap();
        if seq.tracks[ti].locked {
            return Err(ValidationError::Range { index, msg: format!("clip {id} is on the locked track {}", seq.tracks[ti].name) });
        }
        Ok(c)
    };
    // A millisecond bound within half a frame of a clip's first frame names it.
    let half_frame = seq.frame_rate.frame_duration().div_ratio(1, 2);
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
                let c = seq.clip(id).unwrap();
                let given = [(scale, AnimParam::Scale), (x, AnimParam::X), (y, AnimParam::Y), (rotation_deg, AnimParam::Rotation), (opacity, AnimParam::Opacity)];
                if let Some((_, p)) = given.iter().find(|(v, p)| v.is_some() && c.animation.is_animated(*p)) {
                    return Err(ValidationError::Animated { index: i, param: p.key() });
                }
                let mut t: Transform = c.transform.clone();
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
            AiCommand::AddEffect { clip_id, recipe, params, .. } => {
                let id = clip(i, clip_id)?;
                let c = seq.clip(id).unwrap();
                let range = |msg: String| ValidationError::Range { index: i, msg };
                let ids = || kadr_effects::registry().iter().map(|d| d.id).collect::<Vec<_>>().join(", ");
                let def = kadr_effects::recipe(recipe).ok_or_else(|| range(format!("unknown effect {recipe:?} (one of {})", ids())))?;
                let (ti, _) = seq.locate_clip(id).unwrap();
                if !kadr_effects::fits(def.target, seq.tracks[ti].kind) {
                    let on = match def.target {
                        kadr_effects::Target::Clip => "video clips",
                        kadr_effects::Target::AudioClip => "audio clips",
                        kadr_effects::Target::Cut => "cuts between clips",
                    };
                    return Err(range(format!("{recipe} is for {on}")));
                }
                let current = c.effect_of(recipe).map(|e| &e.params);
                let mut values = kadr_effects::json::params_from_json_in(def, c, params, current, half_frame).map_err(range)?;
                kadr_effects::complete(def, &mut values, c, c.timeline_in + Time(c.duration().flicks() / 2));
                let effect = AppliedEffect::new(def.id, def.version, values);
                if !c.clone().put_effect(effect.clone()) {
                    if strict {
                        return Err(range(format!("no change: the clip already has {recipe} with these parameters")));
                    }
                    out.push(None);
                    continue;
                }
                // Replacing an instance drops its keyframes.
                if c.effects.iter().any(|x| x.recipe == *recipe && c.animation.has_effect_tracks(x.id)) {
                    destructive(i)?;
                }
                EditCommand::AddEffect { clip: id, effect }
            }
            AiCommand::SetEffect { clip_id, effect_id, params, enabled, .. } => {
                let id = clip(i, clip_id)?;
                let c = seq.clip(id).unwrap();
                let range = |msg: String| ValidationError::Range { index: i, msg };
                let e = EffectId::parse(effect_id).and_then(|e| c.effect(e)).ok_or_else(|| range(format!("the clip has no effect {effect_id}")))?;
                let def = kadr_effects::definition(e).ok_or_else(|| range(format!("effect {} ({}) was made by a newer Kadr and cannot be changed", e.id, e.recipe)))?;
                if params.is_empty() && enabled.is_none() {
                    return Err(range("give params and/or enabled".into()));
                }
                let values = kadr_effects::json::params_from_json_in(def, c, params, Some(&e.params), half_frame).map_err(range)?;
                // R24: a number with keyframes ignores its stored value.
                if let Some(d) = def.params.iter().find(|d| values.contains_key(d.key) && c.animation.is_animated(TrackKey::fx(e.id, d.key))) {
                    return Err(ValidationError::Animated { index: i, param: d.key });
                }
                let mut probe = e.clone();
                probe.params.extend(values.clone());
                if let Some(on) = enabled {
                    probe.enabled = *on;
                }
                if probe == *e {
                    if strict {
                        return Err(range("no change: the effect already has these settings".into()));
                    }
                    out.push(None);
                    continue;
                }
                EditCommand::UpdateEffect { clip: id, id: e.id, params: values, enabled: *enabled }
            }
            AiCommand::RemoveEffect { clip_id, effect_id, .. } => {
                destructive(i)?;
                let id = clip(i, clip_id)?;
                let e = EffectId::parse(effect_id)
                    .filter(|e| seq.clip(id).unwrap().effect(*e).is_some())
                    .ok_or_else(|| ValidationError::Range { index: i, msg: format!("the clip has no effect {effect_id}") })?;
                EditCommand::RemoveEffect { clip: id, id: e }
            }
            AiCommand::AddCaption { .. } => return Err(ValidationError::Unsupported { index: i, what: "captions" }),
        };
        out.push(Some(cmd));
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
pub fn schema_for_prompt() -> String {
    let effects: Vec<String> = kadr_effects::registry()
        .iter()
        .map(|d| {
            let params: Vec<String> = d
                .params
                .iter()
                .map(|p| match p.kind {
                    kadr_effects::ParamKind::Number { min, max, default } => format!("{} {min}..{max} (default {default})", p.key),
                    kadr_effects::ParamKind::Choice { options, .. } => format!("{} {}", p.key, options.join("|")),
                    kadr_effects::ParamKind::Point => format!("{} {{\"x\":0..1,\"y\":0..1}}", p.key),
                    kadr_effects::ParamKind::Moment => format!("{} {{\"at_ms\":int}} (inside the clip; the effect is fully in at this frame)", p.key),
                })
                .collect();
            format!("   {}: {}", d.id, params.join(", "))
        })
        .collect();
    let head = r#"Respond with ONLY a JSON object: {"summary": string, "confidence": 0..1, "commands": [Command...]}
Command is one of (times are sequence milliseconds, integers):
 {"type":"split_clip","at_ms":int,"clip_id"?:string,"reason":string}
 {"type":"delete_range","start_ms":int,"end_ms":int,"ripple":bool,"reason":string}
 {"type":"delete_clip","clip_id":string,"ripple":bool,"reason":string}
 {"type":"trim_clip","clip_id":string,"edge":"start"|"end","to_ms":int,"ripple":bool,"reason":string}
 {"type":"move_clip","clip_id":string,"to_ms":int,"reason":string}
 {"type":"set_transform","clip_id":string,"scale"?:num,"x"?:num,"y"?:num,"rotation_deg"?:num,"opacity"?:num,"reason":string} (not for a property with keyframes: rejected)
 {"type":"change_speed","clip_id":string,"speed":num,"reason":string}
 {"type":"set_audio_gain","clip_id":string,"gain_db":num,"reason":string}
 {"type":"add_marker","at_ms":int,"name":string,"reason":string}
 {"type":"add_transition","at_ms":int,"kind":"cross_dissolve"|"dip_to_black"|"wipe","duration_ms":int,"reason":string}
 {"type":"add_effect","clip_id":string,"recipe":string,"params"?:{...},"reason":string} (video clips; one per recipe per clip: an existing one is replaced; omitted params get defaults, strength 0.6 = normal)
 {"type":"set_effect","clip_id":string,"effect_id":string,"params"?:{...},"enabled"?:bool,"reason":string} (effect ids are in each clip's "effects"; an effect's "animated" lists its params that have keyframes: set_effect rejects those, leave them out)
 {"type":"remove_effect","clip_id":string,"effect_id":string,"reason":string}
Effects (recipe: params):
"#;
    let tail = r#"
When deleting several ranges with ripple, list them from the LATEST to the EARLIEST.
If the request cannot be done with these commands, return an empty commands list and explain in summary."#;
    format!("{head}{}{tail}", effects.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{MediaInfo, MediaKind, MulticamId, Time, TimeRange};
    use kadr_project::{AppliedEffect, Clip, MediaAsset, MulticamAngle, MulticamGroup, MulticamSelection, ParamValue, SourceTime, SyncMethod, TrackKey};
    use serde_json::json;

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

    /// R24: an animated property ignores its static value (spec §3.2), so
    /// writing it would report success and change nothing.
    #[test]
    fn set_transform_rejects_animated_properties() {
        use kadr_project::{AnimParam, SourceTime};
        let mut p = multicam_project();
        let id = p.sequence().tracks[0].clips[0].id.to_string();
        let set = |field: &str, v: f64| {
            let mut j = serde_json::json!({"type": "set_transform", "clip_id": id});
            j[field] = v.into();
            serde_json::from_value::<AiCommand>(j).unwrap()
        };
        let fields = [("scale", AnimParam::Scale, 2.0), ("x", AnimParam::X, 10.0), ("y", AnimParam::Y, 10.0), ("rotation_deg", AnimParam::Rotation, 45.0), ("opacity", AnimParam::Opacity, 0.5)];
        let perms = Permissions::default();
        for (field, _, v) in fields {
            assert!(validate(&[set(field, v)], &p, &perms).is_ok(), "{field}: static, accepted");
        }
        fn anim(p: &mut Project) -> &mut kadr_project::Animation {
            &mut p.sequence_mut().tracks[0].clips[0].animation
        }
        for (_, param, _) in fields {
            anim(&mut p).set_key(param, SourceTime(Time::from_secs(1)), param.default_value(), None);
        }
        for (field, param, v) in fields {
            let err = validate(&[set(field, v)], &p, &perms).unwrap_err();
            assert_eq!(err, ValidationError::Animated { index: 0, param: param.key() }, "{field}");
            assert_eq!(err.to_string(), format!("command 0: {} is animated (keyframes); use set_keyframe or clear_animation", param.key()));
        }
        // Only the fields given count: animating scale does not block opacity.
        anim(&mut p).clear(AnimParam::Opacity);
        assert!(validate(&[set("opacity", 0.5)], &p, &perms).is_ok());
    }

    #[test]
    fn select_camera_rejects_unknown_angles_and_plain_clips() {
        let p = multicam_project();
        assert!(validate(&[select(2_000, 6_000, "CAM9")], &p, &Permissions::default()).is_err());
        assert!(validate(&[select(30_000, 32_000, "CAM2")], &p, &Permissions::default()).is_err(), "no multicam clip there");
    }

    /// V1 10..20 s (source 30..40 s) and a sound clip on A1.
    fn fx_project() -> (Project, String) {
        let mut p = Project::new("fx");
        let c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10));
        let id = c.id.to_string();
        p.sequence_mut().tracks[0].clips.push(c);
        let a = Clip::new(kadr_core::AssetId::new(), "a", TimeRange::new(Time::ZERO, Time::from_secs(5)), Time::ZERO);
        p.sequence_mut().tracks[1].clips.push(a);
        (p, id)
    }

    fn cmd(j: serde_json::Value) -> AiCommand {
        serde_json::from_value(j).unwrap()
    }

    /// D8: a Look is added by its preset; a preset this build lacks is
    /// refused with the list; the Vignette takes its softness.
    #[test]
    fn a_look_is_added_by_preset_and_an_unknown_preset_is_refused() {
        let (p, id) = fx_project();
        let perms = Permissions::default();
        let ok = validate(&[cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "look", "params": {"preset": "warm"}}))], &p, &perms).unwrap();
        let EditCommand::AddEffect { effect, .. } = &ok[0] else { panic!("{ok:?}") };
        assert_eq!((effect.recipe.as_str(), &effect.params["preset"], &effect.params["strength"]), ("look", &ParamValue::Choice("warm".into()), &ParamValue::Number(0.6)));
        let err = validate(&[cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "look", "params": {"preset": "sepia"}}))], &p, &perms).unwrap_err().to_string();
        assert!(err.contains("preset must be one of cinematic, warm, soft_film, dramatic, vintage, natural"), "{err}");
        let v = validate(&[cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "vignette", "params": {"softness": 0.8}}))], &p, &perms).unwrap();
        assert!(matches!(&v[0], EditCommand::AddEffect { effect, .. } if effect.params["softness"] == ParamValue::Number(0.8) && effect.params["strength"] == ParamValue::Number(0.6)));
    }

    #[test]
    fn add_effect_is_checked_against_the_registry() {
        let (p, id) = fx_project();
        let perms = Permissions::default();
        let ok = validate(&[cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "punch_in", "params": {"strength": 0.9, "moment": {"at_ms": 12_000}}}))], &p, &perms).unwrap();
        let EditCommand::AddEffect { effect, .. } = &ok[0] else { panic!("{ok:?}") };
        assert_eq!(effect.params["strength"], ParamValue::Number(0.9));
        assert_eq!(effect.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(32))));
        assert_eq!(effect.params["point"], ParamValue::Point { x: 0.5, y: 0.5 }, "defaults filled");
        let err = |j: serde_json::Value| validate(&[cmd(j)], &p, &perms).unwrap_err().to_string();
        assert!(err(json!({"type": "add_effect", "clip_id": id, "recipe": "glitch"})).contains("unknown effect \"glitch\""));
        assert!(err(json!({"type": "add_effect", "clip_id": id, "recipe": "handheld", "params": {"strength": 2.0}})).contains("strength"));
        assert!(err(json!({"type": "add_effect", "clip_id": id, "recipe": "handheld", "params": {"speed": 1}})).contains("no parameter"));
        assert!(err(json!({"type": "add_effect", "clip_id": id, "recipe": "punch_in", "params": {"moment": {"at_ms": 25_000}}})).contains("outside the clip"));
        let audio = p.sequence().tracks[1].clips[0].id.to_string();
        assert!(err(json!({"type": "add_effect", "clip_id": audio, "recipe": "handheld"})).contains("video clips"));
        assert!(serde_json::from_value::<AiCommand>(json!({"type": "add_effect", "clip_id": id, "recipe": "handheld", "extra": 1})).is_err(), "unknown fields are rejected");
    }

    /// R24 for effects: a number with keyframes ignores its stored value,
    /// so writing it is rejected with the reason.
    #[test]
    fn set_effect_rejects_animated_params_no_change_and_newer_effects() {
        let (mut p, id) = fx_project();
        let cid = kadr_core::ClipId::parse(&id).unwrap();
        let e = kadr_effects::fresh(kadr_effects::recipe("smooth_zoom").unwrap(), p.sequence().clip(cid).unwrap(), Time::from_secs(12));
        let fid = e.id;
        p.sequence_mut().tracks[0].clips[0].effects.push(e);
        let perms = Permissions::default();
        let set = |params: serde_json::Value| cmd(json!({"type": "set_effect", "clip_id": id, "effect_id": fid.to_string(), "params": params}));
        let ok = validate(&[set(json!({"direction": "out"}))], &p, &perms).unwrap();
        assert!(matches!(&ok[0], EditCommand::UpdateEffect { params, enabled: None, .. } if params["direction"] == ParamValue::Choice("out".into())));
        // R10: a plan tolerates a command that changes nothing (it is dropped); one op asked alone says why.
        assert_eq!(validate(&[set(json!({"direction": "in"}))], &p, &perms).unwrap(), vec![]);
        assert!(validate_strict(&[set(json!({"direction": "in"}))], &p, &perms).unwrap_err().to_string().contains("no change"));
        p.sequence_mut().tracks[0].clips[0].animation.set_key(TrackKey::fx(fid, "strength"), SourceTime(Time::from_secs(31)), 0.2, None);
        assert_eq!(validate(&[set(json!({"strength": 0.9}))], &p, &perms).unwrap_err(), ValidationError::Animated { index: 0, param: "strength" });
        assert!(validate(&[set(json!({"direction": "out"}))], &p, &perms).is_ok(), "only the given params count");
        let future = AppliedEffect::new("glitch_v2", 1, Default::default());
        let gid = future.id.to_string();
        p.sequence_mut().tracks[0].clips[0].effects.push(future);
        let msg = validate(&[cmd(json!({"type": "set_effect", "clip_id": id, "effect_id": gid, "enabled": false}))], &p, &perms).unwrap_err().to_string();
        assert!(msg.contains("newer Kadr"), "{msg}");
    }

    #[test]
    fn remove_effect_is_a_destructive_edit() {
        let (mut p, id) = fx_project();
        let cid = kadr_core::ClipId::parse(&id).unwrap();
        let e = kadr_effects::fresh(kadr_effects::recipe("handheld").unwrap(), p.sequence().clip(cid).unwrap(), Time::ZERO);
        let fid = e.id;
        p.sequence_mut().tracks[0].clips[0].effects.push(e);
        let rm = cmd(json!({"type": "remove_effect", "clip_id": id, "effect_id": fid.to_string()}));
        assert_eq!(validate(std::slice::from_ref(&rm), &p, &Permissions::default()).unwrap(), vec![EditCommand::RemoveEffect { clip: cid, id: fid }]);
        let no = Permissions { allow_destructive: false, max_commands: 10 };
        assert!(matches!(validate(&[rm], &p, &no), Err(ValidationError::Permission { .. })));
        let gone = cmd(json!({"type": "remove_effect", "clip_id": id, "effect_id": kadr_core::EffectId::new().to_string()}));
        assert!(validate(&[gone], &p, &Permissions::default()).unwrap_err().to_string().contains("no effect"));
    }

    #[test]
    fn the_prompt_schema_and_context_know_the_effects() {
        let s = schema_for_prompt();
        for d in kadr_effects::registry() {
            assert!(s.contains(d.id), "schema lacks {}", d.id);
        }
        assert!(s.contains("\"add_effect\"") && s.contains("\"set_effect\"") && s.contains("\"remove_effect\""));
        assert!(s.contains("punch_in: ") && s.contains("fully in at this frame"), "ruling R17: the moment is where the punch is in");
        let (mut p, id) = fx_project();
        let cid = kadr_core::ClipId::parse(&id).unwrap();
        let e = kadr_effects::fresh(kadr_effects::recipe("handheld").unwrap(), p.sequence().clip(cid).unwrap(), Time::ZERO);
        p.sequence_mut().tracks[0].clips[0].effects.push(e);
        let ctx = crate::context::project_context(&p, &Default::default());
        assert_eq!(ctx["tracks"][0]["clips"][0]["effects"][0]["recipe"], "handheld");
        assert!(ctx["tracks"][1]["clips"][0].get("effects").is_none(), "a clip without effects says nothing");
    }

    /// An effect's numbers with keyframes are marked, so the model does not
    /// try to set them statically (set_effect would be rejected).
    #[test]
    fn the_context_marks_animated_effect_params() {
        let (mut p, id) = fx_project();
        let zoom = with_effect(&mut p, "smooth_zoom");
        with_effect(&mut p, "handheld");
        let effects = |p: &Project| crate::context::project_context(p, &Default::default())["tracks"][0]["clips"][0]["effects"].clone();
        assert!(effects(&p)[0].get("animated").is_none(), "no keyframes: nothing marked");
        p.sequence_mut().tracks[0].clips[0].animation.set_key(TrackKey::fx(zoom, "strength"), SourceTime(Time::from_secs(31)), 0.2, None);
        let e = effects(&p);
        assert_eq!(e[0]["animated"], json!(["strength"]));
        assert!(e[1].get("animated").is_none(), "only the effect with the keyframes");
        let set = cmd(json!({"type": "set_effect", "clip_id": id, "effect_id": zoom.to_string(), "params": {"strength": 0.9}}));
        assert!(matches!(validate(&[set], &p, &Permissions::default()), Err(ValidationError::Animated { param: "strength", .. })), "what the mark warns of");
        assert!(schema_for_prompt().contains("\"animated\" lists its params that have keyframes"));
    }

    fn with_effect(p: &mut Project, recipe: &str) -> kadr_core::EffectId {
        let c = &p.sequence().tracks[0].clips[0];
        let e = kadr_effects::fresh(kadr_effects::recipe(recipe).unwrap(), c, Time::from_secs(12));
        let id = e.id;
        p.sequence_mut().tracks[0].clips[0].effects.push(e);
        id
    }

    /// R10: one no-op effect command must not lose the rest of a paid plan.
    #[test]
    fn a_plan_tolerates_no_op_effect_commands_but_one_op_alone_says_why() {
        let (mut p, id) = fx_project();
        with_effect(&mut p, "handheld");
        let perms = Permissions::default();
        let again = cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "handheld"}));
        let marker = cmd(json!({"type": "add_marker", "at_ms": 1_000, "name": "m"}));
        let zoom = cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "smooth_zoom"}));
        let out = validate(&[again.clone(), marker, zoom], &p, &perms).unwrap();
        assert_eq!(out.len(), 2, "the no-op add is dropped: {out:?}");
        assert!(matches!(out[0], EditCommand::AddMarker(_)) && matches!(out[1], EditCommand::AddEffect { .. }));
        let msg = validate_strict(&[again], &p, &perms).unwrap_err().to_string();
        assert!(msg.contains("no change") && msg.contains("handheld"), "{msg}");
        // Still an error in a plan: a real mistake.
        assert!(validate(&[cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "handheld", "params": {"strength": 5}}))], &p, &perms).is_err());
    }

    /// R11(a): what get_state shows is accepted back and is no change.
    #[test]
    fn echoing_an_unchanged_moment_is_no_change() {
        let (mut p, id) = fx_project();
        let fid = with_effect(&mut p, "punch_in");
        p.sequence_mut().tracks[0].clips[0].effects[0].params.insert("moment".into(), ParamValue::Moment(SourceTime(Time::from_secs(32) + Time(1234))));
        let c = &p.sequence().tracks[0].clips[0];
        let echo = kadr_effects::json::value_json(c, &c.effects[0].params["moment"]);
        let set = cmd(json!({"type": "set_effect", "clip_id": id, "effect_id": fid.to_string(), "params": {"moment": echo}}));
        assert_eq!(validate(std::slice::from_ref(&set), &p, &Permissions::default()).unwrap(), vec![]);
        assert!(validate_strict(&[set], &p, &Permissions::default()).unwrap_err().to_string().contains("no change"));
    }

    /// R11(b): a clip that starts between two milliseconds is addressed by the floored one.
    #[test]
    fn a_floored_clip_start_names_the_clips_first_frame() {
        let (mut p, id) = fx_project();
        p.sequence_mut().tracks[0].clips[0].move_to(Time::from_secs(10) + Time(1000));
        let add = cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "punch_in", "params": {"moment": {"at_ms": 10_000}}}));
        let out = validate(&[add], &p, &Permissions::default()).unwrap();
        let EditCommand::AddEffect { effect, .. } = &out[0] else { panic!() };
        assert_eq!(effect.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(30))));
    }

    /// R11(e): replacing an effect drops its keyframes, which is destructive.
    #[test]
    fn replacing_an_animated_effect_needs_the_destructive_permission() {
        let (mut p, id) = fx_project();
        let fid = with_effect(&mut p, "handheld");
        let add = cmd(json!({"type": "add_effect", "clip_id": id, "recipe": "handheld", "params": {"strength": 0.9}}));
        let no = Permissions { allow_destructive: false, max_commands: 10 };
        assert!(validate(std::slice::from_ref(&add), &p, &no).is_ok(), "no keyframes: nothing is lost");
        p.sequence_mut().tracks[0].clips[0].animation.set_key(TrackKey::fx(fid, "strength"), SourceTime(Time::from_secs(31)), 0.2, None);
        assert!(matches!(validate(std::slice::from_ref(&add), &p, &no), Err(ValidationError::Permission { .. })));
        assert!(validate(&[add], &p, &Permissions::default()).is_ok());
    }

    /// R11(f): a locked track fails at validation, with a reason.
    #[test]
    fn a_clip_on_a_locked_track_is_refused_with_a_reason() {
        let (mut p, id) = fx_project();
        p.sequence_mut().tracks[0].locked = true;
        let perms = Permissions::default();
        for j in [
            json!({"type": "add_effect", "clip_id": id, "recipe": "handheld"}),
            json!({"type": "move_clip", "clip_id": id, "to_ms": 12_000}),
            json!({"type": "change_speed", "clip_id": id, "speed": 2.0}),
        ] {
            let msg = validate(&[cmd(j)], &p, &perms).unwrap_err().to_string();
            assert!(msg.contains("locked"), "{msg}");
        }
    }

    /// R11(c): the cloud gets no raw data of an effect this version cannot run.
    #[test]
    fn the_context_tells_the_cloud_nothing_of_unknown_effects() {
        let (mut p, _) = fx_project();
        let mut future = AppliedEffect::new("glitch_v2", 1, Default::default());
        future.params.insert("secret".into(), ParamValue::Unknown(json!({"blob": [1, 2, 3]})));
        p.sequence_mut().tracks[0].clips[0].effects.push(future);
        let ctx = crate::context::project_context(&p, &Default::default());
        let e = &ctx["tracks"][0]["clips"][0]["effects"][0];
        assert_eq!((e["recipe"].as_str(), e["known"].as_bool(), e["enabled"].as_bool()), (Some("glitch_v2"), Some(false), Some(true)));
        assert!(e.get("params").is_none() && !ctx.to_string().contains("blob"), "{e}");
    }
}
