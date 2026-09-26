//! Timeline composition plan: *what* is visible/audible *when*. Pure data,
//! consumed identically by the preview player and by export, so what you see
//! is what you render.

use kadr_core::{AssetId, ClipId, Time, TimeRange};
use kadr_project::{AudioProps, Clip, ColorAdjust, Sequence, TrackKind, Transform, Transition};

#[derive(Clone, Debug, PartialEq)]
pub struct VideoSource {
    pub clip: ClipId,
    pub asset: AssetId,
    /// Source time shown at the segment start.
    pub source_start: Time,
    /// Source seconds per timeline second.
    pub speed: f64,
    pub transform: Transform,
    pub color: ColorAdjust,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoSegment {
    pub range: TimeRange,
    /// `None` = gap (black).
    pub source: Option<VideoSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSegment {
    pub clip: ClipId,
    pub asset: AssetId,
    /// Portion of the clip inside the requested range.
    pub range: TimeRange,
    pub source_start: Time,
    pub speed: f64,
    pub props: AudioProps,
    /// Whole clip range (for fades).
    pub clip_range: TimeRange,
}

fn video_source(c: &Clip, at: Time) -> VideoSource {
    VideoSource {
        clip: c.id,
        asset: c.asset,
        source_start: c.source_time_at(at),
        speed: c.speed(),
        transform: c.transform.clone(),
        color: c.color.clone(),
    }
}

/// Topmost enabled video clip at `t` (the last video track wins).
pub fn video_at(seq: &Sequence, t: Time) -> Option<VideoSource> {
    seq.tracks
        .iter()
        .rev()
        .filter(|tr| tr.kind == TrackKind::Video && !tr.muted)
        .find_map(|tr| tr.clip_at(t).filter(|c| c.enabled))
        .map(|c| video_source(c, t))
}

/// Flattens the video tracks over `range` into non-overlapping segments,
/// including gap segments, covering `range` exactly.
pub fn video_segments(seq: &Sequence, range: TimeRange) -> Vec<VideoSegment> {
    let tracks: Vec<_> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.muted).collect();
    let mut cuts = vec![range.start, range.end];
    for t in &tracks {
        for c in t.clips.iter().filter(|c| c.enabled && c.timeline_range().overlaps(&range)) {
            cuts.extend([c.timeline_in, c.timeline_out].into_iter().filter(|x| range.contains(*x)));
        }
    }
    cuts.sort();
    cuts.dedup();

    let mut out: Vec<VideoSegment> = vec![];
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let top = tracks.iter().rev().find_map(|t| t.clip_at(a).filter(|c| c.enabled));
        match (top, out.last_mut()) {
            // Extend the previous segment if it's the same clip (or both gaps).
            (Some(c), Some(prev)) if prev.source.as_ref().is_some_and(|s| s.clip == c.id) => prev.range.end = b,
            (None, Some(prev)) if prev.source.is_none() => prev.range.end = b,
            (top, _) => out.push(VideoSegment { range: TimeRange::new(a, b), source: top.map(|c| video_source(c, a)) }),
        }
    }
    out
}

/// For each segment from [`video_segments`], the transition played on the cut
/// into it: one centred on that cut whose track holds both visible clips.
/// A transition hidden under an upper track's clip doesn't render.
pub fn transitions_into<'a>(seq: &'a Sequence, segs: &[VideoSegment]) -> Vec<Option<&'a Transition>> {
    let on_track = |tr: &Transition, seg: &VideoSegment| {
        seg.source.as_ref().is_some_and(|s| seq.tracks.iter().any(|t| t.id == tr.track && t.clips.iter().any(|c| c.id == s.clip)))
    };
    segs.iter()
        .enumerate()
        .map(|(i, seg)| {
            let prev = segs.get(i.checked_sub(1)?)?;
            seq.transitions.iter().find(|tr| tr.at == seg.range.start && on_track(tr, prev) && on_track(tr, seg))
        })
        .collect()
}

/// All audible audio clip portions overlapping `range`.
pub fn audio_segments(seq: &Sequence, range: TimeRange) -> Vec<AudioSegment> {
    let mut out = vec![];
    for t in seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio && seq.track_audible(t)) {
        for c in t.clips.iter().filter(|c| c.enabled) {
            if let Some(r) = c.timeline_range().intersect(&range) {
                out.push(AudioSegment {
                    clip: c.id,
                    asset: c.asset,
                    range: r,
                    source_start: c.source_time_at(r.start),
                    speed: c.speed(),
                    props: c.audio.clone(),
                    clip_range: c.timeline_range(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::FrameRate;
    use kadr_project::Track;

    fn s(x: i64) -> Time {
        Time::from_secs(x)
    }

    #[test]
    fn upper_track_wins_and_gaps_are_explicit() {
        let mut seq = Sequence::new("s", FrameRate::FPS_25, 1920, 1080);
        let a = AssetId::new();
        let b = AssetId::new();
        seq.tracks[0].clips.push(Clip::new(a, "a", TimeRange::new(s(0), s(10)), s(0)));
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.clips.push(Clip::new(b, "b", TimeRange::new(s(100), s(103)), s(4)));
        seq.tracks.insert(1, v2);

        let segs = video_segments(&seq, TimeRange::new(s(0), s(12)));
        let summary: Vec<_> = segs
            .iter()
            .map(|g| (g.range.start.as_millis() / 1000, g.range.end.as_millis() / 1000, g.source.as_ref().map(|x| x.asset)))
            .collect();
        assert_eq!(summary, vec![(0, 4, Some(a)), (4, 7, Some(b)), (7, 10, Some(a)), (10, 12, None)]);
        // After the overlay, V1 resumes at the correct source time.
        assert_eq!(segs[2].source.as_ref().unwrap().source_start, s(7));
    }

    #[test]
    fn transitions_attach_to_the_cut_they_are_centred_on() {
        let mut seq = Sequence::new("s", FrameRate::FPS_25, 1920, 1080);
        let a = AssetId::new();
        let track = seq.tracks[0].id;
        seq.tracks[0].clips.push(Clip::new(a, "a", TimeRange::new(s(0), s(5)), s(0)));
        seq.tracks[0].clips.push(Clip::new(a, "b", TimeRange::new(s(10), s(15)), s(5)));
        let dissolve = |at| Transition {
            id: kadr_core::TransitionId::new(),
            kind: kadr_project::TransitionKind::CrossDissolve,
            track,
            at,
            duration: s(1),
        };
        seq.transitions = vec![dissolve(s(5)), dissolve(s(7))];

        let segs = video_segments(&seq, TimeRange::new(s(0), s(12)));
        let found: Vec<_> = transitions_into(&seq, &segs).iter().map(|t| t.map(|t| t.at)).collect();
        // The one at 7 s sits on no cut, so it is ignored.
        assert_eq!(found, vec![None, Some(s(5)), None]);
    }
}
