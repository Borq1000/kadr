//! Audible clip portions over a time range (video is the scene evaluator's
//! business, see `scene`).

use kadr_core::{AssetId, ClipId, Time, TimeRange};
use kadr_project::{AudioProps, Sequence, TrackKind};

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
