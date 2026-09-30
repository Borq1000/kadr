//! Easing curves shared by keyframe animation and, later, effect recipes.
//! The formulas are part of saved project semantics (keyframes spec §3.1):
//! never change one — add a new variant instead.

/// A curve `[0, 1] → [0, 1]`, `apply(0) = 0`, `apply(1) = 1`, monotonic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Cubic in-out ("Smooth"): `u < ½ ? 4u³ : 1 − (2 − 2u)³ / 2`.
    EaseInOut,
    Linear,
    /// Cubic in: `u³`.
    EaseIn,
    /// Cubic out: `1 − (1 − u)³`.
    EaseOut,
    /// Holds the start value until the next keyframe.
    Hold,
}

impl Curve {
    pub fn apply(self, u: f64) -> f64 {
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        match self {
            Curve::Linear => u,
            Curve::EaseInOut => {
                if u < 0.5 {
                    4.0 * u * u * u
                } else {
                    1.0 - (2.0 - 2.0 * u).powi(3) / 2.0
                }
            }
            Curve::EaseIn => u * u * u,
            Curve::EaseOut => 1.0 - (1.0 - u).powi(3),
            Curve::Hold => {
                if u < 1.0 {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PTS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    fn table(c: Curve) -> Vec<f64> {
        PTS.iter().map(|u| c.apply(*u)).collect()
    }

    #[test]
    fn control_points_are_fixed_semantics() {
        assert_eq!(table(Curve::Linear), vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(table(Curve::EaseInOut), vec![0.0, 0.0625, 0.5, 0.9375, 1.0]);
        assert_eq!(table(Curve::EaseIn), vec![0.0, 0.015625, 0.125, 0.421875, 1.0]);
        assert_eq!(table(Curve::EaseOut), vec![0.0, 0.578125, 0.875, 0.984375, 1.0]);
        assert_eq!(table(Curve::Hold), vec![0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn every_curve_is_monotonic_and_clamped() {
        for c in [Curve::EaseInOut, Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::Hold] {
            let mut prev = c.apply(-1.0);
            assert_eq!(prev, 0.0, "{c:?} below 0");
            for i in 0..=1000 {
                let v = c.apply(i as f64 / 1000.0);
                assert!(v >= prev, "{c:?} not monotonic at {i}");
                prev = v;
            }
            assert_eq!(c.apply(2.0), 1.0, "{c:?} above 1");
            assert_eq!(c.apply(f64::NAN), 0.0, "{c:?} NaN");
        }
    }
}
