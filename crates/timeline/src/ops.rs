//! Track-level primitives. Every function preserves the track invariant:
//! clips sorted by `timeline_in` and non-overlapping.

use kadr_core::{ClipId, LinkId, Time, TimeRange};
use kadr_project::{Clip, Track};
use std::collections::HashMap;

/// When one linked clip is split, all pieces on the right must share a new
/// link id so the right-hand V and A stay linked to each other but not to
/// the left-hand pieces. The map makes that consistent across tracks.
#[derive(Default)]
pub struct LinkRemap(HashMap<LinkId, LinkId>);

impl LinkRemap {
    pub fn right_link(&mut self, link: Option<LinkId>) -> Option<LinkId> {
        link.map(|l| *self.0.entry(l).or_insert_with(LinkId::new))
    }
}

pub fn insert_sorted(track: &mut Track, clip: Clip) {
    let idx = track.clips.partition_point(|c| c.timeline_in < clip.timeline_in);
    track.clips.insert(idx, clip);
}

/// Moves the start edge to `t`, keeping the source frame under `timeline_out`.
pub fn trim_start(clip: &mut Clip, t: Time) {
    clip.source_in = clip.source_time_at(t);
    clip.timeline_in = t;
}

pub fn trim_end(clip: &mut Clip, t: Time) {
    clip.source_out = clip.source_time_at(t);
    clip.timeline_out = t;
}

/// Splits at `t` (must be strictly inside). Left keeps the original id.
pub fn split_clip(clip: &Clip, t: Time, links: &mut LinkRemap) -> (Clip, Clip) {
    debug_assert!(clip.timeline_in < t && t < clip.timeline_out);
    let mut left = clip.clone();
    let mut right = clip.clone();
    trim_end(&mut left, t);
    trim_start(&mut right, t);
    right.id = ClipId::new();
    right.link = links.right_link(clip.link);
    // Fades belong to the outer edges.
    left.audio.fade_out = Time::ZERO;
    right.audio.fade_in = Time::ZERO;
    (left, right)
}

/// Splits whatever clip spans `t` on this track. Returns the new right id.
pub fn split_track_at(track: &mut Track, t: Time, links: &mut LinkRemap) -> Option<ClipId> {
    let idx = track.clips.iter().position(|c| c.timeline_in < t && t < c.timeline_out)?;
    let (l, r) = split_clip(&track.clips[idx], t, links);
    let rid = r.id;
    track.clips[idx] = l;
    track.clips.insert(idx + 1, r);
    Some(rid)
}

/// Removes everything inside `range`, trimming or splitting clips that
/// straddle its edges. Returns ids of clips removed entirely.
pub fn clear_range(track: &mut Track, range: TimeRange, links: &mut LinkRemap) -> Vec<ClipId> {
    let mut removed = vec![];
    let mut out = Vec::with_capacity(track.clips.len() + 1);
    for clip in track.clips.drain(..) {
        if !clip.timeline_range().overlaps(&range) {
            out.push(clip);
            continue;
        }
        let starts_before = clip.timeline_in < range.start;
        let ends_after = clip.timeline_out > range.end;
        match (starts_before, ends_after) {
            (false, false) => removed.push(clip.id),
            (true, false) => {
                let mut c = clip;
                trim_end(&mut c, range.start);
                out.push(c);
            }
            (false, true) => {
                let mut c = clip;
                trim_start(&mut c, range.end);
                out.push(c);
            }
            (true, true) => {
                let (left, rest) = split_clip(&clip, range.start, links);
                let mut right = rest;
                trim_start(&mut right, range.end);
                out.push(left);
                out.push(right);
            }
        }
    }
    track.clips = out;
    removed
}

/// Shifts every clip starting at or after `from` by `delta`.
pub fn shift_from(track: &mut Track, from: Time, delta: Time) {
    for c in track.clips.iter_mut().filter(|c| c.timeline_in >= from) {
        let t = c.timeline_in + delta;
        c.move_to(t);
    }
}

/// Removes `range` and closes the gap.
pub fn ripple_remove_range(track: &mut Track, range: TimeRange, links: &mut LinkRemap) {
    clear_range(track, range, links);
    shift_from(track, range.end, -range.duration());
}

/// Opens a gap of `dur` at `at`, splitting a clip that spans it.
pub fn ripple_open_gap(track: &mut Track, at: Time, dur: Time, links: &mut LinkRemap) {
    split_track_at(track, at, links);
    shift_from(track, at, dur);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::AssetId;
    use kadr_project::TrackKind;

    fn s(x: i64) -> Time {
        Time::from_secs(x)
    }

    fn track_with(ranges: &[(i64, i64)]) -> Track {
        let mut t = Track::new(TrackKind::Video, "V1");
        let asset = AssetId::new();
        for &(a, b) in ranges {
            t.clips.push(Clip::new(asset, "c", TimeRange::new(s(100 + a), s(100 + b)), s(a)));
        }
        t
    }

    fn spans(t: &Track) -> Vec<(i64, i64)> {
        t.clips.iter().map(|c| (c.timeline_in.as_millis() / 1000, c.timeline_out.as_millis() / 1000)).collect()
    }

    #[test]
    fn clear_range_trims_splits_and_removes() {
        let mut t = track_with(&[(0, 10), (10, 20), (25, 40)]);
        let removed = clear_range(&mut t, TimeRange::new(s(5), s(30)), &mut LinkRemap::default());
        assert_eq!(spans(&t), vec![(0, 5), (30, 40)]);
        assert_eq!(removed.len(), 1);
        // The trimmed tail keeps its source mapping.
        assert_eq!(t.clips[1].source_in, s(100 + 30));
        assert!(t.is_sorted_and_disjoint());
    }

    #[test]
    fn clear_range_inside_one_clip_splits_it() {
        let mut t = track_with(&[(0, 60)]);
        clear_range(&mut t, TimeRange::new(s(10), s(20)), &mut LinkRemap::default());
        assert_eq!(spans(&t), vec![(0, 10), (20, 60)]);
        assert_eq!(t.clips[1].source_in, s(120));
        assert_ne!(t.clips[0].id, t.clips[1].id);
    }

    #[test]
    fn ripple_remove_closes_gap() {
        let mut t = track_with(&[(0, 10), (10, 20), (20, 30)]);
        ripple_remove_range(&mut t, TimeRange::new(s(5), s(15)), &mut LinkRemap::default());
        assert_eq!(spans(&t), vec![(0, 5), (5, 10), (10, 20)]);
        assert!(t.is_sorted_and_disjoint());
    }

    #[test]
    fn split_keeps_source_continuity() {
        let mut t = track_with(&[(0, 10)]);
        split_track_at(&mut t, s(4), &mut LinkRemap::default()).unwrap();
        assert_eq!(t.clips[0].source_out, t.clips[1].source_in);
        assert_eq!(t.clips[1].source_in, s(104));
        // Splitting exactly on a boundary is a no-op.
        assert!(split_track_at(&mut t, s(4), &mut LinkRemap::default()).is_none());
    }

    #[test]
    fn split_respects_speed() {
        let mut t = track_with(&[(0, 10)]);
        // Make it 2x: 20 s of source in 10 s of timeline.
        t.clips[0].source_out = s(120);
        split_track_at(&mut t, s(5), &mut LinkRemap::default()).unwrap();
        assert_eq!(t.clips[0].source_out, s(110));
        assert!((t.clips[1].speed() - 2.0).abs() < 1e-9);
    }
}
