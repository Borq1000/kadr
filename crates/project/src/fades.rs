//! Clip-bound fades (keyframes spec §1, §3.3): durations measured from the
//! clip's visible edges, not keyframes.

use kadr_core::Time;
use serde::{Deserialize, Serialize};

/// Fade-in and fade-out durations at a clip's visible edges (zero = none).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeFades {
    pub fade_in: Time,
    pub fade_out: Time,
}

/// Which end of the clip a fade belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeEdge {
    In,
    Out,
}

/// Keeps both fades in `[0, dur]` and `fade_in + fade_out ≤ dur`; when they
/// no longer fit, the fade on `first` (the edge that moved) gives way
/// first, the other only once the first reached zero (keyframes spec §4).
pub fn clamp_fades(fade_in: &mut Time, fade_out: &mut Time, dur: Time, first: FadeEdge) {
    let dur = dur.max(Time::ZERO);
    *fade_in = (*fade_in).max(Time::ZERO).min(dur);
    *fade_out = (*fade_out).max(Time::ZERO).min(dur);
    let over = *fade_in + *fade_out - dur;
    if over > Time::ZERO {
        let (a, b) = match first {
            FadeEdge::In => (fade_in, fade_out),
            FadeEdge::Out => (fade_out, fade_in),
        };
        let cut = over.min(*a);
        *a -= cut;
        *b -= over - cut;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_fades_default_to_zero_and_serialise_as_flicks() {
        let f = EdgeFades { fade_in: Time::from_secs(1), fade_out: Time::ZERO };
        assert_eq!(serde_json::to_value(f).unwrap(), serde_json::json!({"fade_in": 705_600_000, "fade_out": 0}));
        assert_eq!(EdgeFades::default(), EdgeFades { fade_in: Time::ZERO, fade_out: Time::ZERO });
    }

    fn s(x: i64) -> Time {
        Time::from_secs(x)
    }

    fn clamped(fi: i64, fo: i64, dur: i64, first: FadeEdge) -> (i64, i64) {
        let (mut a, mut b) = (s(fi), s(fo));
        clamp_fades(&mut a, &mut b, s(dur), first);
        (a.as_millis() / 1000, b.as_millis() / 1000)
    }

    #[test]
    fn fades_that_fit_are_untouched() {
        assert_eq!(clamped(3, 4, 10, FadeEdge::In), (3, 4));
    }

    #[test]
    fn the_edited_edge_shrinks_first() {
        assert_eq!(clamped(3, 4, 5, FadeEdge::In), (1, 4));
        assert_eq!(clamped(3, 4, 5, FadeEdge::Out), (3, 2));
        assert_eq!(clamped(3, 4, 2, FadeEdge::In), (0, 2), "each ≤ duration, then the edited one gives way");
        assert_eq!(clamped(3, 4, 0, FadeEdge::Out), (0, 0));
    }
}
