//! Edit commands. Each command is a pure transformation of a `Sequence`;
//! undo is handled generically by the engine (see `engine.rs`), so commands
//! only need to be correct forwards and to fail *before* partial mutation
//! is observable (the engine restores the snapshot on error anyway).

use crate::ops::{self, LinkRemap};
use kadr_core::{AssetId, ClipId, EffectId, MarkerId, MediaKind, Time, TimeRange, TrackId, TransitionId};
use kadr_effects::{ParamDef, ParamKind};
use kadr_project::{
    AnimError, AppliedEffect, AudioProps, Clip, ColorAdjust, EdgeFades, Ease, MediaAsset, Marker, ParamValue, Sequence,
    SourceTime, TextDoc, Track, TrackKey, TrackKind, Transform, Transition, TEXT_ASSET,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum EditError {
    #[error("clip not found")]
    ClipNotFound,
    #[error("track not found")]
    TrackNotFound,
    #[error("track {0} is locked")]
    TrackLocked(String),
    #[error("invalid range")]
    InvalidRange,
    #[error("operation would overlap another clip")]
    Overlap,
    #[error("clip kind does not match track kind")]
    KindMismatch,
    #[error("nothing to do")]
    NoOp,
    #[error("track is not empty")]
    TrackNotEmpty,
    #[error("a sequence needs at least one video and one audio track")]
    LastTrack,
    #[error("a keyframe already exists at that time")]
    KeyframeCollision,
    #[error("effect not found")]
    EffectNotFound,
    #[error("unknown effect {0}")]
    UnknownRecipe(String),
    #[error("the effect does not fit this clip")]
    EffectTarget,
    #[error("invalid effect parameter: {0}")]
    InvalidParam(String),
    #[error("the clips do not meet at a cut")]
    NotACut,
    #[error("transition not found")]
    TransitionNotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InsertMode {
    /// Pushes later clips right (ripple insert).
    Insert,
    /// Replaces whatever is underneath.
    Overwrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrimEdge {
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackFlag {
    Mute,
    Solo,
    Lock,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipProperty {
    Transform(Transform),
    Color(ColorAdjust),
    Audio(AudioProps),
    Enabled(bool),
    Name(String),
    /// Link group (None = unlink from partners).
    Link(Option<kadr_core::LinkId>),
    /// Playback speed factor (2.0 = twice as fast). Changes the clip's
    /// timeline length; fails if the new length would overlap the next clip.
    Speed(f64),
    /// Video fade-in / fade-out durations (each >= 0, sum <= clip duration).
    VideoFade(EdgeFades),
    /// A text clip's document (titles spec §7), normalised: not a text
    /// clip → `KindMismatch`, a non-finite number → `InvalidRange`, the
    /// same document → `NoOp`. The clip's name follows the text while it
    /// is still the automatic one (the old text's label).
    Text(Box<TextDoc>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EditCommand {
    InsertClip { track: TrackId, clip: Clip, mode: InsertMode },
    /// A text clip (titles spec §7, refinement 11): on `track` (a video
    /// track, unlocked, free over the clip's frame-snapped range), or —
    /// `None` — on the topmost video track when it is not the bottom one
    /// (V1 is for footage: a title placed first must not be pushed away,
    /// cut or followed by the footage placed after it, TX-R11), unlocked,
    /// shown (not muted) and free there, else on a new video track above
    /// all. One undo entry. The document is normalised and must be finite
    /// (`InvalidRange`); the clip's asset becomes `TEXT_ASSET`.
    AddText { clip: Clip, track: Option<TrackId> },
    /// Moves clips (and their linked partners) by `delta`; clips named in
    /// `clips` additionally move `track_delta` tracks within their kind.
    MoveClips { clips: Vec<ClipId>, delta: Time, track_delta: i32 },
    TrimClip { clip: ClipId, edge: TrimEdge, to: Time, ripple: bool },
    /// Splits the given clips (or every unlocked track when `None`) at `at`.
    Split { at: Time, clips: Option<Vec<ClipId>> },
    DeleteClips { clips: Vec<ClipId>, ripple: bool },
    /// Removes a time range on every unlocked track (sync-preserving).
    DeleteRange { range: TimeRange, ripple: bool },
    SetClipProperty { clip: ClipId, prop: ClipProperty },
    /// Puts a transition on the cut where `from` ends and `to` starts (D3,
    /// effects spec §7): both on one unlocked video track; a cut recipe at
    /// its current version, valid parameters (missing ones get defaults), a
    /// duration snapped to whole frames and at least two of them (the
    /// evaluator clamps it to both clips). The cut's transition, if any, is
    /// replaced in place (same id); the same transition again is a `NoOp`.
    SetTransition { from: ClipId, to: ClipId, recipe: String, version: u16, params: BTreeMap<String, ParamValue>, duration: Time },
    /// Sets the given parameters (validated, merged into the stored ones;
    /// none = unchanged) and/or the duration of a transition (whole frames,
    /// at least two, as `SetTransition`). One the registry cannot run (a
    /// newer Kadr's) cannot be changed, only removed.
    UpdateTransition { id: TransitionId, params: BTreeMap<String, ParamValue>, duration: Option<Time> },
    RemoveTransition(TransitionId),
    AddMarker(Marker),
    RemoveMarker(MarkerId),
    SetTrackFlag { track: TrackId, flag: TrackFlag, value: bool },
    AddTrack { kind: TrackKind },
    RenameTrack { track: TrackId, name: String },
    /// Only empty tracks can be removed (never silently deletes clips).
    RemoveTrack { track: TrackId },
    /// Replaces the marker with the same id (rename / recolour / move).
    UpdateMarker(Marker),
    /// Shows another angle of the clip's multicam group over the same
    /// timeline span. With `at` inside the clip it first cuts there and
    /// switches only the part after the cut (a live cut).
    SwitchAngle { clip: ClipId, angle: u32, at: Option<Time> },
    /// Shows `angle` over `range` on the topmost video track that has a
    /// multicam clip at `range.start`, cutting at both edges (AI camera picks).
    SetAngleRange { range: TimeRange, angle: u32 },
    /// Inserts or updates a keyframe (`ease: None` -> keep / inherit, keyframes spec §3.1).
    /// An effect parameter's keyframe edits need a number of an effect this
    /// build runs, keys within its range (`track_def`).
    SetKeyframe { clip: ClipId, param: TrackKey, at: SourceTime, value: f64, ease: Option<Ease> },
    /// Removes a keyframe; if it was the track's last, the static value
    /// becomes `keep` (the value at the playhead) or the removed value.
    RemoveKeyframe { clip: ClipId, param: TrackKey, at: SourceTime, keep: Option<f64> },
    /// Moves keyframes (all or nothing); a move onto another keyframe of the
    /// same track is rejected.
    MoveKeyframes { clip: ClipId, moves: Vec<(TrackKey, SourceTime, SourceTime)> },
    SetKeyframeEase { clip: ClipId, param: TrackKey, at: SourceTime, ease: Ease },
    /// Drops the listed tracks and sets their static values.
    ClearAnimation { clip: ClipId, statics: Vec<(TrackKey, f64)> },
    /// Applies an effect (effects spec §7), validated against the registry:
    /// a known recipe at its current version, a clip it fits, valid
    /// parameters (missing ones get defaults); a new effect covers the whole
    /// clip (progress (0, 1)). A recipe the clip has is replaced in place —
    /// same id, position and progress range (ruling E2-R6), the new
    /// parameters, enabled, its `fx:` tracks dropped when the parameters
    /// change (spec §2); more instances of it (a lenient load) go too
    /// (`Clip::put_effect`). The same parameters again are a `NoOp`.
    AddEffect { clip: ClipId, effect: AppliedEffect },
    /// Removes an effect and its `fx:` tracks.
    RemoveEffect { clip: ClipId, id: EffectId },
    /// Sets the given parameters (validated, merged into the stored ones)
    /// and/or `enabled`. A recipe from a newer Kadr cannot be changed.
    UpdateEffect { clip: ClipId, id: EffectId, params: BTreeMap<String, ParamValue>, enabled: Option<bool> },
    /// Moves an effect to position `to` (clamped) of the clip's list.
    MoveEffect { clip: ClipId, id: EffectId, to: usize },
    /// Applied atomically; one undo step (e.g. an AI edit).
    Batch { label: String, commands: Vec<EditCommand> },
}

/// Read-only project data some commands need (source durations).
pub struct EditContext<'a> {
    pub assets: &'a [MediaAsset],
    pub multicam: &'a [kadr_project::MulticamGroup],
}

impl EditContext<'_> {
    /// Source length limit for trimming; stills can be extended freely.
    fn source_limit(&self, id: AssetId) -> Time {
        match self.assets.iter().find(|a| a.id == id) {
            Some(a) if a.info.kind == MediaKind::Image => Time::MAX,
            Some(a) => a.info.duration,
            None => Time::MAX,
        }
    }
}

impl EditCommand {
    /// History label: an i18n key (`cmd.*`) for built-in commands, or the
    /// caller-provided text for batches. The UI translates keys on display.
    pub fn label(&self) -> String {
        let k = match self {
            EditCommand::InsertClip { mode: InsertMode::Insert, .. } => "cmd.insert",
            EditCommand::InsertClip { .. } => "cmd.overwrite",
            EditCommand::AddText { .. } => "cmd.add_text",
            EditCommand::MoveClips { .. } => "cmd.move",
            EditCommand::TrimClip { ripple: true, .. } => "cmd.ripple_trim",
            EditCommand::TrimClip { .. } => "cmd.trim",
            EditCommand::Split { .. } => "cmd.split",
            EditCommand::DeleteClips { ripple: true, .. } => "cmd.ripple_delete",
            EditCommand::DeleteClips { .. } => "cmd.delete",
            EditCommand::DeleteRange { .. } => "cmd.delete_range",
            EditCommand::SetClipProperty { .. } => "cmd.property",
            EditCommand::SetTransition { .. } => "cmd.set_transition",
            EditCommand::UpdateTransition { .. } => "cmd.update_transition",
            EditCommand::RemoveTransition(_) => "cmd.remove_transition",
            EditCommand::AddMarker(_) => "cmd.add_marker",
            EditCommand::RemoveMarker(_) => "cmd.remove_marker",
            EditCommand::SetTrackFlag { flag: TrackFlag::Mute, .. } => "cmd.toggle_mute",
            EditCommand::SetTrackFlag { flag: TrackFlag::Solo, .. } => "cmd.toggle_solo",
            EditCommand::SetTrackFlag { flag: TrackFlag::Lock, .. } => "cmd.toggle_lock",
            EditCommand::AddTrack { kind: TrackKind::Video } => "cmd.add_video_track",
            EditCommand::AddTrack { kind: TrackKind::Audio } => "cmd.add_audio_track",
            EditCommand::RenameTrack { .. } => "cmd.rename_track",
            EditCommand::RemoveTrack { .. } => "cmd.remove_track",
            EditCommand::UpdateMarker(_) => "cmd.edit_marker",
            EditCommand::SwitchAngle { at: Some(_), .. } => "cmd.cut_to_angle",
            EditCommand::SwitchAngle { .. } => "cmd.switch_angle",
            EditCommand::SetAngleRange { .. } => "cmd.switch_angle",
            EditCommand::SetKeyframe { .. } => "cmd.keyframe",
            EditCommand::RemoveKeyframe { .. } => "cmd.remove_keyframe",
            EditCommand::MoveKeyframes { .. } => "cmd.move_keyframes",
            EditCommand::SetKeyframeEase { .. } => "cmd.keyframe_ease",
            EditCommand::ClearAnimation { .. } => "cmd.clear_animation",
            EditCommand::AddEffect { .. } => "cmd.add_effect",
            EditCommand::RemoveEffect { .. } => "cmd.remove_effect",
            EditCommand::UpdateEffect { .. } => "cmd.update_effect",
            EditCommand::MoveEffect { .. } => "cmd.move_effect",
            EditCommand::Batch { label, .. } => return label.clone(),
        };
        k.to_string()
    }

    /// i18n key describing an engine error.
    pub fn error_key(e: &EditError) -> &'static str {
        match e {
            EditError::ClipNotFound => "err.edit.clip_not_found",
            EditError::TrackNotFound => "err.edit.track_not_found",
            EditError::TrackLocked(_) => "err.edit.track_locked",
            EditError::InvalidRange => "err.edit.invalid_range",
            EditError::Overlap => "err.edit.overlap",
            EditError::KindMismatch => "err.edit.kind_mismatch",
            EditError::NoOp => "err.edit.noop",
            EditError::TrackNotEmpty => "err.edit.track_not_empty",
            EditError::LastTrack => "err.edit.last_track",
            EditError::KeyframeCollision => "err.edit.keyframe_collision",
            EditError::EffectNotFound => "err.edit.effect_not_found",
            EditError::UnknownRecipe(_) => "err.edit.unknown_recipe",
            EditError::EffectTarget => "err.edit.effect_target",
            EditError::InvalidParam(_) => "err.edit.invalid_param",
            EditError::NotACut => "err.edit.not_a_cut",
            EditError::TransitionNotFound => "err.edit.transition_not_found",
        }
    }

    /// Applies the command, then keeps every transition on its cut (D2):
    /// what a command does to clips it does to their transitions, in the
    /// same undo entry (`Sequence::reconcile_transitions`). Inside a
    /// `Batch` transitions only follow each command
    /// (`Sequence::reanchor_transitions`); what is dead is dropped once, at
    /// its end.
    pub fn apply(&self, seq: &mut Sequence, ctx: &EditContext) -> Result<(), EditError> {
        self.apply_one(seq, ctx)?;
        for t in seq.reconcile_transitions() {
            tracing::debug!(id = %t.id, recipe = %t.recipe, "a transition's clips no longer meet: removed with the edit");
        }
        Ok(())
    }

    fn apply_one(&self, seq: &mut Sequence, ctx: &EditContext) -> Result<(), EditError> {
        let fr = seq.frame_rate;
        match self {
            EditCommand::InsertClip { track, clip, mode } => {
                let t = unlocked_mut(seq, *track)?;
                if clip.text.is_some() && t.kind != TrackKind::Video {
                    return Err(EditError::KindMismatch);
                }
                let text;
                let clip = if clip.text.is_some() {
                    text = text_clip(clip)?;
                    &text
                } else {
                    clip
                };
                let expected = if ctx.assets.iter().any(|a| a.id == clip.asset && a.info.kind == MediaKind::Audio) {
                    TrackKind::Audio
                } else {
                    t.kind
                };
                if expected != t.kind {
                    return Err(EditError::KindMismatch);
                }
                let mut clip = clip.clone();
                clip.move_to(fr.snap(clip.timeline_in).max(Time::ZERO));
                if clip.duration() <= Time::ZERO {
                    return Err(EditError::InvalidRange);
                }
                let mut links = LinkRemap::default();
                match mode {
                    InsertMode::Insert => ops::ripple_open_gap(t, clip.timeline_in, clip.duration(), &mut links),
                    InsertMode::Overwrite => {
                        ops::clear_range(t, clip.timeline_range(), &mut links);
                    }
                }
                ops::insert_sorted(t, clip);
                Ok(())
            }

            EditCommand::AddText { clip, track } => add_text(seq, clip, *track),

            EditCommand::MoveClips { clips, delta, track_delta } => move_clips(seq, clips, fr.snap(*delta), *track_delta),

            EditCommand::TrimClip { clip, edge, to, ripple } => trim(seq, ctx, *clip, *edge, fr.snap(*to), *ripple),

            EditCommand::Split { at, clips } => {
                let at = fr.snap(*at);
                let targets: Vec<ClipId> = match clips {
                    Some(ids) => with_links(seq, ids),
                    None => seq
                        .tracks
                        .iter()
                        .filter(|t| !t.locked)
                        .filter_map(|t| t.clip_at(at).map(|c| c.id))
                        .collect(),
                };
                let mut links = LinkRemap::default();
                let mut any = false;
                for id in targets {
                    let (ti, ci) = seq.locate_clip(id).ok_or(EditError::ClipNotFound)?;
                    let track = &mut seq.tracks[ti];
                    if track.locked {
                        continue;
                    }
                    let c = &track.clips[ci];
                    if c.timeline_in < at && at < c.timeline_out {
                        let (l, r) = ops::split_clip(c, at, &mut links);
                        track.clips[ci] = l;
                        track.clips.insert(ci + 1, r);
                        any = true;
                    }
                }
                if any { Ok(()) } else { Err(EditError::NoOp) }
            }

            EditCommand::DeleteClips { clips, ripple } => {
                let ids: HashSet<ClipId> = with_links(seq, clips).into_iter().collect();
                if ids.is_empty() {
                    return Err(EditError::NoOp);
                }
                for t in &seq.tracks {
                    if t.locked && t.clips.iter().any(|c| ids.contains(&c.id)) {
                        return Err(EditError::TrackLocked(t.name.clone()));
                    }
                }
                for t in &mut seq.tracks {
                    let mut removed: Vec<TimeRange> =
                        t.clips.iter().filter(|c| ids.contains(&c.id)).map(|c| c.timeline_range()).collect();
                    t.clips.retain(|c| !ids.contains(&c.id));
                    if *ripple {
                        // Close gaps right-to-left so earlier shifts don't move later ranges.
                        removed.sort_by_key(|r| std::cmp::Reverse(r.start));
                        for r in removed {
                            ops::shift_from(t, r.end, -r.duration());
                        }
                    }
                }
                Ok(())
            }

            EditCommand::DeleteRange { range, ripple } => {
                let range = TimeRange::new(fr.snap(range.start), fr.snap(range.end));
                if range.is_empty() {
                    return Err(EditError::InvalidRange);
                }
                let mut links = LinkRemap::default();
                for t in seq.tracks.iter_mut().filter(|t| !t.locked) {
                    if *ripple {
                        ops::ripple_remove_range(t, range, &mut links);
                    } else {
                        ops::clear_range(t, range, &mut links);
                    }
                }
                if *ripple {
                    seq.markers.retain(|m| !range.contains(m.time) || m.time == range.start);
                    for m in seq.markers.iter_mut().filter(|m| m.time >= range.end) {
                        m.time -= range.duration();
                    }
                }
                Ok(())
            }

            EditCommand::SetClipProperty { clip, prop } => {
                let (ti, ci) = seq.locate_clip(*clip).ok_or(EditError::ClipNotFound)?;
                if seq.tracks[ti].locked {
                    return Err(EditError::TrackLocked(seq.tracks[ti].name.clone()));
                }
                let next_in = seq.tracks[ti].clips.get(ci + 1).map(|c| c.timeline_in);
                let c = &mut seq.tracks[ti].clips[ci];
                match prop {
                    ClipProperty::Transform(v) => c.transform = v.clone(),
                    ClipProperty::Color(v) => c.color = v.clone(),
                    ClipProperty::Audio(v) => c.audio = v.clone(),
                    ClipProperty::Enabled(v) => c.enabled = *v,
                    ClipProperty::Name(v) => c.name = v.clone(),
                    ClipProperty::Link(v) => c.link = *v,
                    ClipProperty::Text(doc) => {
                        let Some(cur) = c.text.as_deref() else { return Err(EditError::KindMismatch) };
                        let mut doc = doc.clone();
                        doc.normalise();
                        if !doc.is_finite() {
                            return Err(EditError::InvalidRange);
                        }
                        if *cur == *doc {
                            return Err(EditError::NoOp);
                        }
                        if c.name == cur.label() {
                            c.name = doc.label();
                        }
                        c.text = Some(doc);
                    }
                    ClipProperty::VideoFade(f) => {
                        if f.fade_in < Time::ZERO || f.fade_out < Time::ZERO || f.fade_in > c.duration() - f.fade_out {
                            return Err(EditError::InvalidRange);
                        }
                        c.video_fade = *f;
                    }
                    ClipProperty::Speed(speed) => {
                        if !(speed.is_finite() && (0.05..=20.0).contains(speed)) {
                            return Err(EditError::InvalidRange);
                        }
                        let out = c.timeline_in + duration_at_speed(c.source_out - c.source_in, *speed, fr);
                        if next_in.is_some_and(|n| out > n) {
                            return Err(EditError::Overlap);
                        }
                        c.timeline_out = out;
                        c.clamp_fades(kadr_project::FadeEdge::Out);
                    }
                }
                Ok(())
            }

            EditCommand::SetTransition { from, to, recipe, version, params, duration } => {
                let def = kadr_effects::recipe(recipe).filter(|d| d.version == *version).ok_or_else(|| EditError::UnknownRecipe(recipe.clone()))?;
                if def.target != kadr_effects::Target::Cut {
                    return Err(EditError::EffectTarget);
                }
                kadr_effects::validate(def.params, params).map_err(EditError::InvalidParam)?;
                // Whole frames (E3-R6), so the length shown is the one stored.
                let duration = fr.snap(*duration);
                if duration < fr.frame_to_time(2) {
                    return Err(EditError::InvalidRange);
                }
                let (ti, ci) = seq.locate_clip(*from).ok_or(EditError::ClipNotFound)?;
                let track = &seq.tracks[ti];
                if track.kind != TrackKind::Video {
                    return Err(EditError::KindMismatch);
                }
                if track.locked {
                    return Err(EditError::TrackLocked(track.name.clone()));
                }
                if !track.clips.get(ci + 1).is_some_and(|n| n.id == *to && n.timeline_in == track.clips[ci].timeline_out) {
                    return Err(EditError::NotACut);
                }
                let mut values = kadr_effects::transition_params(def);
                values.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
                match seq.transitions.iter_mut().find(|t| t.to == *to) {
                    Some(t) => {
                        let next = Transition { id: t.id, ..Transition::new(def.id, def.version, values, *from, *to, duration) };
                        if *t == next {
                            return Err(EditError::NoOp);
                        }
                        *t = next;
                    }
                    None => seq.transitions.push(Transition::new(def.id, def.version, values, *from, *to, duration)),
                }
                Ok(())
            }
            EditCommand::UpdateTransition { id, params, duration } => {
                let i = seq.transitions.iter().position(|t| t.id == *id).ok_or(EditError::TransitionNotFound)?;
                transition_track_unlocked(seq, &seq.transitions[i])?;
                let tr = &seq.transitions[i];
                // A recipe this build cannot run cannot be validated, so it is not changed (spec §7).
                let def = kadr_effects::transition_definition(tr).ok_or_else(|| EditError::UnknownRecipe(tr.recipe.clone()))?;
                kadr_effects::validate(def.params, params).map_err(EditError::InvalidParam)?;
                let duration = duration.map(|d| fr.snap(d));
                if duration.is_some_and(|d| d < fr.frame_to_time(2)) {
                    return Err(EditError::InvalidRange);
                }
                let mut next = tr.clone();
                next.params.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
                if let Some(d) = duration {
                    next.duration = d;
                }
                if next == *tr {
                    return Err(EditError::NoOp);
                }
                seq.transitions[i] = next;
                Ok(())
            }
            EditCommand::RemoveTransition(id) => {
                let Some(i) = seq.transitions.iter().position(|t| t.id == *id) else { return Err(EditError::NoOp) };
                transition_track_unlocked(seq, &seq.transitions[i])?;
                seq.transitions.remove(i);
                Ok(())
            }
            EditCommand::AddMarker(m) => {
                let mut m = m.clone();
                m.time = fr.snap(m.time).max(Time::ZERO);
                let idx = seq.markers.partition_point(|x| x.time <= m.time);
                seq.markers.insert(idx, m);
                Ok(())
            }
            EditCommand::RemoveMarker(id) => {
                let n = seq.markers.len();
                seq.markers.retain(|m| m.id != *id);
                if seq.markers.len() == n { Err(EditError::NoOp) } else { Ok(()) }
            }
            EditCommand::SetTrackFlag { track, flag, value } => {
                let t = seq.track_mut(*track).ok_or(EditError::TrackNotFound)?;
                match flag {
                    TrackFlag::Mute => t.muted = *value,
                    TrackFlag::Solo => t.solo = *value,
                    TrackFlag::Lock => t.locked = *value,
                }
                Ok(())
            }
            EditCommand::AddTrack { kind } => {
                let n = seq.tracks_of(*kind).count() + 1;
                let name = match kind {
                    TrackKind::Video => format!("V{n}"),
                    TrackKind::Audio => format!("A{n}"),
                };
                let track = Track::new(*kind, name);
                match kind {
                    // Video tracks stack above existing video tracks.
                    TrackKind::Video => {
                        let idx = seq.tracks.iter().rposition(|t| t.kind == TrackKind::Video).map_or(0, |i| i + 1);
                        seq.tracks.insert(idx, track);
                    }
                    TrackKind::Audio => seq.tracks.push(track),
                }
                Ok(())
            }
            EditCommand::RenameTrack { track, name } => {
                let t = seq.track_mut(*track).ok_or(EditError::TrackNotFound)?;
                let name = name.trim();
                if name.is_empty() || t.name == name {
                    return Err(EditError::NoOp);
                }
                t.name = name.chars().take(40).collect();
                Ok(())
            }
            EditCommand::RemoveTrack { track } => {
                let idx = seq.tracks.iter().position(|t| t.id == *track).ok_or(EditError::TrackNotFound)?;
                let t = &seq.tracks[idx];
                if !t.clips.is_empty() {
                    return Err(EditError::TrackNotEmpty);
                }
                if seq.tracks_of(t.kind).count() <= 1 {
                    return Err(EditError::LastTrack);
                }
                seq.tracks.remove(idx);
                Ok(())
            }
            EditCommand::UpdateMarker(m) => {
                let slot = seq.markers.iter_mut().find(|x| x.id == m.id).ok_or(EditError::NoOp)?;
                let mut m = m.clone();
                m.time = fr.snap(m.time).max(Time::ZERO);
                if *slot == m {
                    return Err(EditError::NoOp);
                }
                *slot = m;
                seq.markers.sort_by_key(|x| x.time);
                Ok(())
            }
            EditCommand::SwitchAngle { clip, angle, at } => {
                let mut target = *clip;
                if let Some(t) = *at {
                    // Split cuts on a frame; locate the right half by that same
                    // frame (a playing playhead sits between frames).
                    let t = fr.snap(t);
                    let (ti, _) = seq.locate_clip(*clip).ok_or(EditError::ClipNotFound)?;
                    let c = seq.clip(*clip).ok_or(EditError::ClipNotFound)?;
                    if t >= c.timeline_out {
                        return Err(EditError::NoOp);
                    }
                    if t > c.timeline_in {
                        EditCommand::Split { at: t, clips: Some(vec![*clip]) }.apply_one(seq, ctx)?;
                        target = seq.tracks[ti].clip_at(t).ok_or(EditError::ClipNotFound)?.id;
                    }
                }
                let (ti, ci) = seq.locate_clip(target).ok_or(EditError::ClipNotFound)?;
                if seq.tracks[ti].locked {
                    return Err(EditError::TrackLocked(seq.tracks[ti].name.clone()));
                }
                let c = &seq.tracks[ti].clips[ci];
                let sel = c.multicam.clone().ok_or(EditError::InvalidRange)?;
                if sel.angle == *angle {
                    return Err(EditError::NoOp);
                }
                let group = ctx.multicam.iter().find(|g| g.id == sel.group).ok_or(EditError::InvalidRange)?;
                let (asset, src) = crate::multicam::angle_source(group, *angle, c, ctx.assets)?;
                let label = group.angles[*angle as usize].label.clone();
                let c = &mut seq.tracks[ti].clips[ci];
                // Keyframes and effect Moments are source-bound: follow the
                // sync-offset difference (SetAngleRange and the AI's
                // select_camera come through here too).
                let shift = src.start - c.source_in;
                c.shift_source_bound(shift);
                c.asset = asset;
                c.source_in = src.start;
                c.source_out = src.end;
                c.name = label;
                c.multicam = Some(kadr_project::MulticamSelection { group: sel.group, angle: *angle });
                Ok(())
            }
            EditCommand::SetAngleRange { range, angle } => {
                if range.is_empty() {
                    return Err(EditError::InvalidRange);
                }
                let ti = seq
                    .tracks
                    .iter()
                    .rposition(|t| t.kind == TrackKind::Video && t.clip_at(range.start).is_some_and(|c| c.multicam.is_some()))
                    .ok_or(EditError::ClipNotFound)?;
                // Cut at both edges (each cut only if it falls inside a clip).
                for at in [range.start, range.end] {
                    if let Some(c) = seq.tracks[ti].clip_at(at).filter(|c| c.timeline_in < at && c.multicam.is_some()) {
                        let id = c.id;
                        EditCommand::Split { at, clips: Some(vec![id]) }.apply_one(seq, ctx)?;
                    }
                }
                let inside: Vec<ClipId> = seq.tracks[ti]
                    .clips
                    .iter()
                    .filter(|c| c.multicam.is_some() && c.timeline_in >= range.start && c.timeline_out <= range.end)
                    .map(|c| c.id)
                    .collect();
                let mut changed = false;
                for id in inside {
                    match (EditCommand::SwitchAngle { clip: id, angle: *angle, at: None }).apply_one(seq, ctx) {
                        Ok(()) => changed = true,
                        Err(EditError::NoOp) => {}
                        Err(e) => return Err(e),
                    }
                }
                if changed { Ok(()) } else { Err(EditError::NoOp) }
            }
            EditCommand::SetKeyframe { clip, param, at, value, ease } => {
                if !value.is_finite() {
                    return Err(EditError::InvalidRange);
                }
                let c = anim_clip(seq, *clip)?;
                if let Some(d) = track_def(c, param)? {
                    d.check(&ParamValue::Number(*value)).map_err(EditError::InvalidParam)?;
                }
                c.animation.set_key(param, *at, *value, ease.clone());
                Ok(())
            }
            EditCommand::RemoveKeyframe { clip, param, at, keep } => {
                let c = anim_clip(seq, *clip)?;
                let d = track_def(c, param)?;
                let k = c.animation.remove_key(param, *at).ok_or(EditError::NoOp)?;
                if !c.animation.is_animated(param) {
                    c.write_static(param, in_range(d, keep.filter(|v| v.is_finite()).unwrap_or(k.value)));
                }
                Ok(())
            }
            EditCommand::MoveKeyframes { clip, moves } => {
                let c = anim_clip(seq, *clip)?;
                if moves.is_empty() {
                    return Err(EditError::NoOp);
                }
                for (p, _, _) in moves {
                    track_def(c, p)?;
                }
                // Work on a copy and assign only on success: a failed move must not
                // leave half the keys moved, also inside a `Batch` (which swallows `NoOp`).
                // Every moving keyframe is removed first, so keyframes can swap past each other.
                let mut anim = c.animation.clone();
                let mut taken = vec![];
                for (p, from, to) in moves {
                    let k = anim.remove_key(p, *from).ok_or(EditError::NoOp)?;
                    taken.push((p.clone(), *to, k));
                }
                for (p, to, k) in taken {
                    if anim.track(&p).iter().any(|x| x.at == to) {
                        return Err(EditError::KeyframeCollision);
                    }
                    anim.set_key(p, to, k.value, Some(k.ease));
                }
                c.animation = anim;
                Ok(())
            }
            EditCommand::SetKeyframeEase { clip, param, at, ease } => {
                let c = anim_clip(seq, *clip)?;
                track_def(c, param)?;
                c.animation.set_ease(param, *at, ease.clone()).map_err(anim_err)
            }
            EditCommand::ClearAnimation { clip, statics } => {
                let c = anim_clip(seq, *clip)?;
                if statics.is_empty() {
                    return Err(EditError::NoOp);
                }
                if statics.iter().any(|(_, v)| !v.is_finite()) {
                    return Err(EditError::InvalidRange);
                }
                let defs = statics.iter().map(|(p, _)| track_def(c, p)).collect::<Result<Vec<_>, _>>()?;
                for ((p, v), d) in statics.iter().zip(defs) {
                    c.animation.clear(p);
                    c.write_static(p, in_range(d, *v));
                }
                Ok(())
            }

            EditCommand::AddEffect { clip, effect } => {
                let def = kadr_effects::recipe(&effect.recipe)
                    .filter(|d| d.version == effect.version)
                    .ok_or_else(|| EditError::UnknownRecipe(effect.recipe.clone()))?;
                let (ti, ci) = seq.locate_clip(*clip).ok_or(EditError::ClipNotFound)?;
                let t = &mut seq.tracks[ti];
                if t.locked {
                    return Err(EditError::TrackLocked(t.name.clone()));
                }
                if !kadr_effects::fits(def.target, t.kind) {
                    return Err(EditError::EffectTarget);
                }
                kadr_effects::validate(def.params, &effect.params).map_err(EditError::InvalidParam)?;
                let c = &mut t.clips[ci];
                // Stored as validated: this build's fields only (`extra` is
                // for a newer Kadr's file), applied whole and enabled.
                let mut e = AppliedEffect { extra: BTreeMap::new(), ..effect.clone() };
                // The moment of applying is unknown here: the clip's middle (refinement 4).
                let middle = c.timeline_in + Time(c.duration().flicks() / 2);
                kadr_effects::complete(def, &mut e.params, c, middle);
                e.enabled = true;
                e.progress = (0.0, 1.0);
                if c.put_effect(e) { Ok(()) } else { Err(EditError::NoOp) }
            }
            EditCommand::RemoveEffect { clip, id } => {
                let c = effect_clip(seq, *clip)?;
                let i = c.effects.iter().position(|e| e.id == *id).ok_or(EditError::EffectNotFound)?;
                c.effects.remove(i);
                c.animation.remove_effect(*id);
                Ok(())
            }
            EditCommand::UpdateEffect { clip, id, params, enabled } => {
                let c = effect_clip(seq, *clip)?;
                let e = c.effect_mut(*id).ok_or(EditError::EffectNotFound)?;
                // A recipe this build cannot run cannot be validated, so it is not changed (spec §7).
                let def = kadr_effects::definition(e).ok_or_else(|| EditError::UnknownRecipe(e.recipe.clone()))?;
                kadr_effects::validate(def.params, params).map_err(EditError::InvalidParam)?;
                // Merged into a copy: on "no change" the effect is left untouched (R11).
                let mut next = e.clone();
                next.params.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
                if let Some(on) = enabled {
                    next.enabled = *on;
                }
                if next == *e {
                    return Err(EditError::NoOp);
                }
                *e = next;
                Ok(())
            }
            EditCommand::MoveEffect { clip, id, to } => {
                let c = effect_clip(seq, *clip)?;
                let from = c.effects.iter().position(|e| e.id == *id).ok_or(EditError::EffectNotFound)?;
                let to = (*to).min(c.effects.len() - 1);
                if from == to {
                    return Err(EditError::NoOp);
                }
                let e = c.effects.remove(from);
                c.effects.insert(to, e);
                Ok(())
            }

            EditCommand::Batch { commands, .. } => {
                let mut applied = 0;
                for c in commands {
                    match c.apply_one(seq, ctx) {
                        Ok(()) => {
                            applied += 1;
                            // Transitions follow each step without being
                            // dropped: a later step can remove the clip one
                            // was anchored to, or bring its cut back (E3-R6).
                            seq.reanchor_transitions();
                        }
                        // A no-op inside a batch (e.g. range already empty) is fine.
                        Err(EditError::NoOp) => {}
                        Err(e) => return Err(e),
                    }
                }
                if applied == 0 { Err(EditError::NoOp) } else { Ok(()) }
            }
        }
    }
}

/// The unlocked video clip `id` for an animation edit.
fn anim_clip(seq: &mut Sequence, id: ClipId) -> Result<&mut Clip, EditError> {
    let (ti, ci) = seq.locate_clip(id).ok_or(EditError::ClipNotFound)?;
    let t = &mut seq.tracks[ti];
    if t.locked {
        return Err(EditError::TrackLocked(t.name.clone()));
    }
    if t.kind != TrackKind::Video {
        return Err(EditError::KindMismatch);
    }
    Ok(&mut t.clips[ci])
}

/// The unlocked clip `id` for an effect edit (any track kind: audio effects are E4).
fn effect_clip(seq: &mut Sequence, id: ClipId) -> Result<&mut Clip, EditError> {
    let (ti, ci) = seq.locate_clip(id).ok_or(EditError::ClipNotFound)?;
    let t = &mut seq.tracks[ti];
    if t.locked {
        return Err(EditError::TrackLocked(t.name.clone()));
    }
    Ok(&mut t.clips[ci])
}

/// Whether keyframe commands may edit track `k` of `c` (ruling R4), with
/// the parameter's definition for an effect parameter (`None`: a clip
/// property, always editable). An effect parameter needs its effect on the
/// clip and a recipe this build runs — one from a newer Kadr keeps its
/// tracks as they are (spec §2, review focus 5) — and must be a `Number`
/// of that recipe holding a number (only numbers animate, spec §2), so its
/// static value is never written over something else. Callers that build
/// commands for every track of a moment (`Animation::keys_at`) can filter
/// with it.
pub fn track_def(c: &Clip, k: &TrackKey) -> Result<Option<&'static ParamDef>, EditError> {
    let Some(f) = k.as_fx() else { return Ok(None) };
    let e = c.effect(f.effect).ok_or(EditError::EffectNotFound)?;
    let def = kadr_effects::definition(e).ok_or_else(|| EditError::UnknownRecipe(e.recipe.clone()))?;
    let d = def.param(&f.key).ok_or_else(|| EditError::InvalidParam(format!("{} has no parameter {:?}", def.id, f.key)))?;
    if !d.animatable() {
        return Err(EditError::InvalidParam(format!("{} is a {}: only numbers can be animated", f.key, d.kind.type_name())));
    }
    if c.static_value(k).is_none() {
        return Err(EditError::InvalidParam(format!("{} holds no number this version can read", f.key)));
    }
    Ok(Some(d))
}

/// The tracks a gesture on the keyframe moment `at` edits (ruling R8):
/// those with a keyframe there that keyframe commands may change
/// (`track_def`). A read-only track — an effect from a newer Kadr, an
/// orphan `fx:` track — keeps its keyframe where it is; empty when the
/// moment has only such keys (the gesture then does nothing).
pub fn editable_keys_at(c: &Clip, at: SourceTime) -> Vec<TrackKey> {
    c.animation.keys_at(at).into_iter().filter(|k| track_def(c, k).is_ok()).collect()
}

/// A static value for a track: an effect number is kept in its range — the
/// value the picture showed (the evaluator clamps a track the same way).
fn in_range(d: Option<&ParamDef>, v: f64) -> f64 {
    match d.map(|d| d.kind) {
        Some(ParamKind::Number { min, max, .. }) => v.clamp(min, max),
        _ => v,
    }
}

fn anim_err(e: AnimError) -> EditError {
    match e {
        AnimError::NoKeyframe => EditError::NoOp,
        AnimError::Collision => EditError::KeyframeCollision,
    }
}

fn unlocked_mut(seq: &mut Sequence, id: TrackId) -> Result<&mut Track, EditError> {
    let t = seq.track_mut(id).ok_or(EditError::TrackNotFound)?;
    if t.locked {
        return Err(EditError::TrackLocked(t.name.clone()));
    }
    Ok(t)
}

/// A transition is changed only on an unlocked track (a lock protects its
/// cuts too); one whose clips are gone can always go.
fn transition_track_unlocked(seq: &Sequence, tr: &Transition) -> Result<(), EditError> {
    match seq.locate_clip(tr.to).map(|(ti, _)| &seq.tracks[ti]) {
        Some(t) if t.locked => Err(EditError::TrackLocked(t.name.clone())),
        _ => Ok(()),
    }
}

/// Expands a clip list with every linked partner, deduplicated, stable order.
pub fn with_links(seq: &Sequence, ids: &[ClipId]) -> Vec<ClipId> {
    let mut out: Vec<ClipId> = vec![];
    for &id in ids {
        for x in std::iter::once(id).chain(seq.linked_clips(id)) {
            if !out.contains(&x) {
                out.push(x);
            }
        }
    }
    out
}

/// Unlink and delete only the `kind` part (video or audio) of the selection
/// and its link partners, leaving the other part in place. `None` when the
/// selection has no clip of that kind.
pub fn delete_part(seq: &Sequence, ids: &[ClipId], kind: TrackKind) -> Option<Vec<EditCommand>> {
    let of_kind = |id: &ClipId| seq.locate_clip(*id).is_some_and(|(ti, _)| seq.tracks[ti].kind == kind);
    let targets: Vec<ClipId> = with_links(seq, ids).into_iter().filter(of_kind).collect();
    if targets.is_empty() {
        return None;
    }
    // Detach first: DeleteClips takes link partners along. The partners of
    // a pair lose their link too, so nothing points at a deleted clip.
    let mut out: Vec<EditCommand> = with_links(seq, &targets)
        .into_iter()
        .filter(|id| seq.clip(*id).is_some_and(|c| c.link.is_some()))
        .map(|clip| EditCommand::SetClipProperty { clip, prop: ClipProperty::Link(None) })
        .collect();
    out.push(EditCommand::DeleteClips { clips: targets, ripple: false });
    Some(out)
}

/// One new link group for the selection and everything already linked to
/// it, so they move, trim and delete together. `None` for fewer than two clips.
pub fn link_selection(seq: &Sequence, ids: &[ClipId]) -> Option<Vec<EditCommand>> {
    let all = with_links(seq, ids);
    if all.len() < 2 {
        return None;
    }
    let link = kadr_core::LinkId::new();
    Some(all.into_iter().map(|clip| EditCommand::SetClipProperty { clip, prop: ClipProperty::Link(Some(link)) }).collect())
}

fn move_clips(seq: &mut Sequence, ids: &[ClipId], delta: Time, track_delta: i32) -> Result<(), EditError> {
    let primary: HashSet<ClipId> = ids.iter().copied().collect();
    let all = with_links(seq, ids);
    if all.is_empty() {
        return Err(EditError::ClipNotFound);
    }
    // Plan destinations first (no mutation yet).
    let mut plan: Vec<(usize, Clip)> = vec![];
    let mut min_in = Time::MAX;
    for &id in &all {
        let (ti, ci) = seq.locate_clip(id).ok_or(EditError::ClipNotFound)?;
        let src_track = &seq.tracks[ti];
        if src_track.locked {
            return Err(EditError::TrackLocked(src_track.name.clone()));
        }
        let dest = if primary.contains(&id) && track_delta != 0 {
            let same_kind: Vec<usize> =
                seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == src_track.kind).map(|(i, _)| i).collect();
            let pos = same_kind.iter().position(|&i| i == ti).unwrap() as i32;
            let np = (pos + track_delta).clamp(0, same_kind.len() as i32 - 1);
            same_kind[np as usize]
        } else {
            ti
        };
        if seq.tracks[dest].locked {
            return Err(EditError::TrackLocked(seq.tracks[dest].name.clone()));
        }
        let c = src_track.clips[ci].clone();
        min_in = min_in.min(c.timeline_in);
        plan.push((dest, c));
    }
    // Never move anything before zero; the whole group stays in sync.
    let delta = delta.max(-min_in);
    if delta == Time::ZERO && plan.iter().all(|(d, c)| seq.locate_clip(c.id).map(|(t, _)| t) == Some(*d)) {
        return Err(EditError::NoOp);
    }
    let moving: HashSet<ClipId> = all.iter().copied().collect();
    for t in &mut seq.tracks {
        t.clips.retain(|c| !moving.contains(&c.id));
    }
    let mut links = LinkRemap::default();
    for (dest, mut c) in plan {
        c.move_to(c.timeline_in + delta);
        let t = &mut seq.tracks[dest];
        ops::clear_range(t, c.timeline_range(), &mut links);
        ops::insert_sorted(t, c);
    }
    Ok(())
}

fn trim(seq: &mut Sequence, ctx: &EditContext, id: ClipId, edge: TrimEdge, to: Time, ripple: bool) -> Result<(), EditError> {
    let min_len = seq.frame_rate.frame_duration();
    let group = with_links(seq, &[id]);
    let (ti0, ci0) = seq.locate_clip(id).ok_or(EditError::ClipNotFound)?;
    let anchor = &seq.tracks[ti0].clips[ci0];
    let old_edge = match edge {
        TrimEdge::Start => anchor.timeline_in,
        TrimEdge::End => anchor.timeline_out,
    };
    let mut delta = to - old_edge;

    // Clamp delta so every clip in the link group stays valid.
    for &cid in &group {
        let (ti, ci) = seq.locate_clip(cid).ok_or(EditError::ClipNotFound)?;
        let t = &seq.tracks[ti];
        if t.locked {
            return Err(EditError::TrackLocked(t.name.clone()));
        }
        let c = &t.clips[ci];
        let limit = ctx.source_limit(c.asset);
        match edge {
            TrimEdge::Start => {
                // Can't shrink below one frame.
                delta = delta.min(c.duration() - min_len);
                // Can't extend before source start — a text clip has no
                // source to run out of (titles spec §2.1).
                if c.text.is_none() {
                    let earliest = c.timeline_time_of(Time::ZERO);
                    delta = delta.max(earliest - c.timeline_in);
                }
                if !ripple {
                    let prev_out = ci.checked_sub(1).map_or(Time::ZERO, |p| t.clips[p].timeline_out);
                    delta = delta.max(prev_out - c.timeline_in);
                }
            }
            TrimEdge::End => {
                delta = delta.max(min_len - c.duration());
                if limit != Time::MAX {
                    let latest = c.timeline_time_of(limit);
                    delta = delta.min(latest - c.timeline_out);
                }
                if !ripple {
                    if let Some(n) = t.clips.get(ci + 1) {
                        delta = delta.min(n.timeline_in - c.timeline_out);
                    }
                }
            }
        }
    }
    if delta == Time::ZERO {
        return Err(EditError::NoOp);
    }
    for &cid in &group {
        let (ti, ci) = seq.locate_clip(cid).unwrap();
        let t = &mut seq.tracks[ti];
        let c = &mut t.clips[ci];
        match edge {
            TrimEdge::Start => {
                let old_in = c.timeline_in;
                ops::trim_start(c, old_in + delta);
                if ripple {
                    // Clip keeps its position; it and everything after shifts.
                    let shift = -delta;
                    let from = c.timeline_in;
                    c.move_to(old_in);
                    for later in t.clips.iter_mut().skip(ci + 1).filter(|x| x.timeline_in >= from) {
                        let n = later.timeline_in + shift;
                        later.move_to(n);
                    }
                }
            }
            TrimEdge::End => {
                let old_out = c.timeline_out;
                ops::trim_end(c, old_out + delta);
                if ripple {
                    for later in t.clips.iter_mut().skip(ci + 1) {
                        let n = later.timeline_in + delta;
                        later.move_to(n);
                    }
                }
            }
        }
    }
    Ok(())
}

/// A text clip as the commands store it: the nil asset, a normalised and
/// finite document (`InvalidRange` otherwise). `KindMismatch` for media.
fn text_clip(clip: &Clip) -> Result<Clip, EditError> {
    let mut clip = clip.clone();
    let doc = clip.text.as_deref_mut().ok_or(EditError::KindMismatch)?;
    doc.normalise();
    if !doc.is_finite() {
        return Err(EditError::InvalidRange);
    }
    clip.asset = TEXT_ASSET;
    Ok(clip)
}

fn add_text(seq: &mut Sequence, clip: &Clip, track: Option<TrackId>) -> Result<(), EditError> {
    let fr = seq.frame_rate;
    let mut clip = text_clip(clip)?;
    clip.move_to(fr.snap(clip.timeline_in).max(Time::ZERO));
    let len = fr.snap(clip.duration()).max(fr.frame_duration());
    (clip.timeline_out, clip.source_out) = (clip.timeline_in + len, clip.source_in + len);
    let range = clip.timeline_range();
    let free = |t: &Track| t.clips.iter().all(|c| !c.timeline_range().overlaps(&range));
    let ti = match track {
        Some(id) => {
            let ti = seq.tracks.iter().position(|t| t.id == id).ok_or(EditError::TrackNotFound)?;
            let t = &seq.tracks[ti];
            if t.kind != TrackKind::Video {
                return Err(EditError::KindMismatch);
            }
            if t.locked {
                return Err(EditError::TrackLocked(t.name.clone()));
            }
            if !free(t) {
                return Err(EditError::Overlap);
            }
            ti
        }
        None => {
            // Video tracks are stored V1 (bottom) first: the topmost is the
            // last. The bottom one is the footage's (TX-R11); a locked or
            // hidden (muted) one is not used either.
            let bottom = seq.tracks.iter().position(|t| t.kind == TrackKind::Video);
            let top = seq.tracks.iter().rposition(|t| t.kind == TrackKind::Video);
            match top.filter(|&i| Some(i) != bottom && !seq.tracks[i].locked && !seq.tracks[i].muted && free(&seq.tracks[i])) {
                Some(ti) => ti,
                None => {
                    let n = seq.tracks_of(TrackKind::Video).count() + 1;
                    let at = top.map_or(0, |i| i + 1);
                    seq.tracks.insert(at, Track::new(TrackKind::Video, format!("V{n}")));
                    at
                }
            }
        }
    };
    ops::insert_sorted(&mut seq.tracks[ti], clip);
    Ok(())
}

/// The length `ClipProperty::Speed` gives a clip showing `source` of its
/// media at `speed`: whole sequence frames, at least one.
pub fn duration_at_speed(source: Time, speed: f64, fr: kadr_core::FrameRate) -> Time {
    fr.snap(Time::from_secs_f64(source.as_secs_f64() / speed)).max(fr.frame_duration())
}
