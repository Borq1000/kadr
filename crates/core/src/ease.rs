//! Easing curves shared by keyframe animation and, later, effect recipes.
//! The formulas are part of saved project semantics (keyframes spec §3.1,
//! effects spec §3): never change one — add a new variant instead.

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
    /// cubic-bezier(0.45, 0, 0.2, 1) ("Cinematic"): an unhurried start and
    /// a long, soft landing.
    Cinematic,
    /// Exponential out normalised to end at 1 ("Snap"):
    /// `(1 − 2^(−10u)) / (1 − 2^(−10))`.
    Snap,
    /// Sine in-out ("Gentle"): `(1 − cos πu) / 2`.
    Gentle,
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
            Curve::Cinematic => cubic_bezier((0.45, 0.0), (0.2, 1.0), u),
            Curve::Snap => (1.0 - (-10.0 * u).exp2()) / (1.0 - (-10.0f64).exp2()),
            Curve::Gentle => (1.0 - (std::f64::consts::PI * u).cos()) / 2.0,
        }
    }
}

/// y of the CSS-style cubic Bézier through (0, 0), `p1`, `p2`, (1, 1) at
/// x = `u`. x(t) is monotonic for control x in [0, 1], so 60 bisection
/// steps find t to full f64 precision — deterministically (no Newton step
/// whose result could differ between implementations).
fn cubic_bezier(p1: (f64, f64), p2: (f64, f64), u: f64) -> f64 {
    if u <= 0.0 {
        return 0.0;
    }
    if u >= 1.0 {
        return 1.0;
    }
    let b = |a: f64, c: f64, t: f64| 3.0 * a * (1.0 - t) * (1.0 - t) * t + 3.0 * c * (1.0 - t) * t * t + t * t * t;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if b(p1.0, p2.0, mid) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    b(p1.1, p2.1, (lo + hi) / 2.0)
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
        for c in [Curve::EaseInOut, Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::Hold, Curve::Cinematic, Curve::Snap, Curve::Gentle] {
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

    /// Effects spec §3: the recipe curves are saved semantics too.
    #[test]
    fn recipe_curves_have_fixed_control_points() {
        let close = |c: Curve, u: f64, v: f64| assert!((c.apply(u) - v).abs() < 1e-12, "{c:?}({u}) = {} ≠ {v}", c.apply(u));
        for (u, v) in [(0.25, 0.1876516490062228), (0.5, 0.7622512948816345), (0.75, 0.9586378581607304)] {
            close(Curve::Cinematic, u, v);
        }
        for (u, v) in [(0.25, 0.8240280195662207), (0.5, 0.9696969696969697), (0.75, 0.995447845308414)] {
            close(Curve::Snap, u, v);
        }
        for (u, v) in [(0.25, 0.1464466094067262), (0.5, 0.5), (0.75, 0.8535533905932737)] {
            close(Curve::Gentle, u, v);
        }
        for c in [Curve::Cinematic, Curve::Snap, Curve::Gentle] {
            assert_eq!((c.apply(0.0), c.apply(1.0)), (0.0, 1.0), "{c:?}");
        }
    }
}
