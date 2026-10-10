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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_fades_default_to_zero_and_serialise_as_flicks() {
        let f = EdgeFades { fade_in: Time::from_secs(1), fade_out: Time::ZERO };
        assert_eq!(serde_json::to_value(f).unwrap(), serde_json::json!({"fade_in": 705_600_000, "fade_out": 0}));
        assert_eq!(EdgeFades::default(), EdgeFades { fade_in: Time::ZERO, fade_out: Time::ZERO });
    }
}
