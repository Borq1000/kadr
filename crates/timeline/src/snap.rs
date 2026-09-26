//! Snapping: clip edges, playhead, markers, In/Out.

use kadr_core::{ClipId, Time};
use kadr_project::Sequence;
use std::collections::HashSet;

/// Sorted, deduplicated snap targets, excluding edges of `exclude` clips
/// (the ones being dragged).
pub fn snap_points(seq: &Sequence, exclude: &HashSet<ClipId>, extra: &[Time]) -> Vec<Time> {
    let mut pts: Vec<Time> = vec![Time::ZERO];
    for t in &seq.tracks {
        for c in t.clips.iter().filter(|c| !exclude.contains(&c.id)) {
            pts.push(c.timeline_in);
            pts.push(c.timeline_out);
        }
    }
    pts.extend(seq.markers.iter().map(|m| m.time));
    if let Some(io) = seq.in_out {
        pts.extend([io.start, io.end]);
    }
    pts.extend_from_slice(extra);
    pts.sort();
    pts.dedup();
    pts
}

/// Nearest point within `threshold` of `t`.
pub fn snap(t: Time, points: &[Time], threshold: Time) -> Option<Time> {
    let i = points.partition_point(|&p| p < t);
    [i.checked_sub(1), Some(i)]
        .into_iter()
        .flatten()
        .filter_map(|j| points.get(j).copied())
        .filter(|&p| (p - t).abs() <= threshold)
        .min_by_key(|&p| (p - t).abs())
}

/// For a block moving by `delta`, returns the adjusted delta so that either
/// its start or its end lands on a snap point (whichever is closer).
pub fn snap_move(start: Time, end: Time, delta: Time, points: &[Time], threshold: Time) -> Time {
    let a = snap(start + delta, points, threshold).map(|p| p - start);
    let b = snap(end + delta, points, threshold).map(|p| p - end);
    match (a, b) {
        (Some(x), Some(y)) => {
            if (x - delta).abs() <= (y - delta).abs() { x } else { y }
        }
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => delta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_nearest_within_threshold() {
        let pts = [Time::from_secs(0), Time::from_secs(10), Time::from_secs(20)];
        let th = Time::from_millis(300);
        assert_eq!(snap(Time::from_millis(10_200), &pts, th), Some(Time::from_secs(10)));
        assert_eq!(snap(Time::from_millis(10_400), &pts, th), None);
        assert_eq!(snap(Time::from_millis(19_900), &pts, th), Some(Time::from_secs(20)));
    }

    #[test]
    fn move_snaps_either_edge() {
        let pts = [Time::from_secs(30)];
        let th = Time::from_millis(500);
        // Block 0..5 moved by 24.8 → end at 29.8 snaps to 30.
        let d = snap_move(Time::ZERO, Time::from_secs(5), Time::from_millis(24_800), &pts, th);
        assert_eq!(d, Time::from_secs(25));
    }
}
