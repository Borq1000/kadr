//! Multicam time mapping. A group has one timeline ("group time"); each
//! angle shows `source = group + sync_offset`. A multicam edit is a chain of
//! ordinary clips carrying `Clip.multicam`, so preview, export and
//! transitions need no special cases.

use crate::commands::EditError;
use kadr_core::{AssetId, Time, TimeRange};
use kadr_project::{Clip, MediaAsset, MulticamGroup, MulticamSelection};

/// Group-time range where `angle` has media.
pub fn angle_range(group: &MulticamGroup, angle: u32, assets: &[MediaAsset]) -> Option<TimeRange> {
    let a = group.angles.get(angle as usize)?;
    let dur = assets.iter().find(|x| x.id == a.asset)?.info.duration;
    Some(TimeRange::new(Time::ZERO - a.sync_offset, dur - a.sync_offset))
}

/// Group time covered by *every* angle, so any switch inside it is valid.
/// Empty (zero-length) when the angles don't overlap.
pub fn group_span(group: &MulticamGroup, assets: &[MediaAsset]) -> TimeRange {
    let mut span: Option<TimeRange> = None;
    for i in 0..group.angles.len() as u32 {
        let Some(r) = angle_range(group, i, assets) else { continue };
        span = Some(match span {
            None => r,
            Some(s) => TimeRange::new(s.start.max(r.start), s.end.min(r.end).max(s.start.max(r.start))),
        });
    }
    span.unwrap_or(TimeRange::new(Time::ZERO, Time::ZERO))
}

/// Group time shown by a multicam clip at timeline time `t`.
pub fn group_time_at(group: &MulticamGroup, clip: &Clip, t: Time) -> Option<Time> {
    let sel = clip.multicam.as_ref().filter(|m| m.group == group.id)?;
    let a = group.angles.get(sel.angle as usize)?;
    Some(clip.source_time_at(t) - a.sync_offset)
}

/// Asset and source range that show `clip`'s group-time span on `angle`.
pub fn angle_source(group: &MulticamGroup, angle: u32, clip: &Clip, assets: &[MediaAsset]) -> Result<(AssetId, TimeRange), EditError> {
    let a = group.angles.get(angle as usize).ok_or(EditError::InvalidRange)?;
    let g0 = group_time_at(group, clip, clip.timeline_in).ok_or(EditError::InvalidRange)?;
    let len = clip.source_out - clip.source_in;
    let src = TimeRange::new(g0 + a.sync_offset, g0 + a.sync_offset + len);
    let dur = assets.iter().find(|x| x.id == a.asset).ok_or(EditError::InvalidRange)?.info.duration;
    if src.start < Time::ZERO || src.end > dur {
        return Err(EditError::InvalidRange);
    }
    Ok((a.asset, src))
}

/// A clip for the whole common span of the group on `angle`, placed at `at`.
pub fn group_clip(group: &MulticamGroup, angle: u32, assets: &[MediaAsset], at: Time) -> Option<Clip> {
    let span = group_span(group, assets);
    let a = group.angles.get(angle as usize)?;
    if span.is_empty() {
        return None;
    }
    let src = TimeRange::new(span.start + a.sync_offset, span.end + a.sync_offset);
    let mut c = Clip::new(a.asset, a.label.clone(), src, at);
    c.multicam = Some(MulticamSelection { group: group.id, angle });
    Some(c)
}
