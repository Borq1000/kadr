//! Edit commands. Each command is a pure transformation of a `Sequence`;
//! undo is handled generically by the engine (see `engine.rs`), so commands
//! only need to be correct forwards and to fail *before* partial mutation
//! is observable (the engine restores the snapshot on error anyway).

use crate::ops::{self, LinkRemap};
use kadr_core::{AssetId, ClipId, MarkerId, MediaKind, Time, TimeRange, TrackId, TransitionId};
use kadr_project::{
    AudioProps, Clip, ColorAdjust, MediaAsset, Marker, Sequence, Track, TrackKind, Transform,
    Transition,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EditCommand {
    InsertClip { track: TrackId, clip: Clip, mode: InsertMode },
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
    AddTransition(Transition),
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
            EditCommand::MoveClips { .. } => "cmd.move",
            EditCommand::TrimClip { ripple: true, .. } => "cmd.ripple_trim",
            EditCommand::TrimClip { .. } => "cmd.trim",
            EditCommand::Split { .. } => "cmd.split",
            EditCommand::DeleteClips { ripple: true, .. } => "cmd.ripple_delete",
            EditCommand::DeleteClips { .. } => "cmd.delete",
            EditCommand::DeleteRange { .. } => "cmd.delete_range",
            EditCommand::SetClipProperty { .. } => "cmd.property",
            EditCommand::AddTransition(_) => "cmd.add_transition",
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
        }
    }

    pub fn apply(&self, seq: &mut Sequence, ctx: &EditContext) -> Result<(), EditError> {
        let fr = seq.frame_rate;
        match self {
            EditCommand::InsertClip { track, clip, mode } => {
                let t = unlocked_mut(seq, *track)?;
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
                    ClipProperty::Speed(speed) => {
                        if !(speed.is_finite() && (0.05..=20.0).contains(speed)) {
                            return Err(EditError::InvalidRange);
                        }
                        let src = c.source_out - c.source_in;
                        let dur = fr.snap(Time::from_secs_f64(src.as_secs_f64() / speed)).max(fr.frame_duration());
                        let out = c.timeline_in + dur;
                        if next_in.is_some_and(|n| out > n) {
                            return Err(EditError::Overlap);
                        }
                        c.timeline_out = out;
                    }
                }
                Ok(())
            }

            EditCommand::AddTransition(tr) => {
                let track = unlocked_mut(seq, tr.track)?;
                // Snap to the cut the caller names: when exactly one adjacent
                // clip pair has its boundary within half a frame of `at`
                // (MCP clients send ms-rounded times), use that boundary.
                let half = Time(fr.frame_duration().flicks() / 2);
                let mut cuts = track.clips.windows(2).filter(|w| w[0].timeline_out == w[1].timeline_in).map(|w| w[0].timeline_out).filter(|c| (*c - tr.at).abs() <= half);
                let mut tr = tr.clone();
                if let (Some(cut), None) = (cuts.next(), cuts.next()) {
                    tr.at = cut;
                }
                seq.transitions.retain(|t| !(t.track == tr.track && t.at == tr.at));
                seq.transitions.push(tr);
                Ok(())
            }
            EditCommand::RemoveTransition(id) => {
                let n = seq.transitions.len();
                seq.transitions.retain(|t| t.id != *id);
                if seq.transitions.len() == n { Err(EditError::NoOp) } else { Ok(()) }
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
                let id = t.id;
                seq.tracks.remove(idx);
                seq.transitions.retain(|tr| tr.track != id);
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
                        EditCommand::Split { at: t, clips: Some(vec![*clip]) }.apply(seq, ctx)?;
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
                        EditCommand::Split { at, clips: Some(vec![id]) }.apply(seq, ctx)?;
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
                    match (EditCommand::SwitchAngle { clip: id, angle: *angle, at: None }).apply(seq, ctx) {
                        Ok(()) => changed = true,
                        Err(EditError::NoOp) => {}
                        Err(e) => return Err(e),
                    }
                }
                if changed { Ok(()) } else { Err(EditError::NoOp) }
            }
            EditCommand::Batch { commands, .. } => {
                let mut applied = 0;
                for c in commands {
                    match c.apply(seq, ctx) {
                        Ok(()) => applied += 1,
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

fn unlocked_mut(seq: &mut Sequence, id: TrackId) -> Result<&mut Track, EditError> {
    let t = seq.track_mut(id).ok_or(EditError::TrackNotFound)?;
    if t.locked {
        return Err(EditError::TrackLocked(t.name.clone()));
    }
    Ok(t)
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
                // Can't extend before source start.
                let earliest = c.timeline_time_of(Time::ZERO);
                delta = delta.max(earliest - c.timeline_in);
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
