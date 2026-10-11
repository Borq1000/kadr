//! `Noise1D` (effects spec §3): smooth value noise for natural motion
//! (Handheld). Lattice values come from one PCG32 (XSH-RR) output step
//! over an integer hash — integer-exact, so a GPU shader reproduces them
//! bit for bit.

/// Value noise in `[−amplitude, amplitude]`: a lattice value every
/// `1 / frequency` seconds, smoothstep-blended in between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Noise1D {
    pub seed: u64,
    pub frequency: f64,
    pub amplitude: f64,
}

impl Noise1D {
    /// The noise at `t` seconds (0 for a non-finite `t`).
    pub fn at(&self, t: f64) -> f64 {
        let x = t * self.frequency;
        if !x.is_finite() {
            return 0.0;
        }
        let floor = x.floor();
        let f = x - floor;
        let i = floor as i64;
        let (a, b) = (lattice(self.seed, i), lattice(self.seed, i.wrapping_add(1)));
        let s = f * f * (3.0 - 2.0 * f);
        self.amplitude * (a + (b - a) * s)
    }
}

const PCG_MUL: u64 = 6_364_136_223_846_793_005;

/// PCG32 (XSH-RR) output of one LCG step from `i` on stream `seed`.
pub fn pcg32_hash(seed: u64, i: u64) -> u32 {
    let inc = (seed << 1) | 1;
    let state = i.wrapping_add(inc).wrapping_mul(PCG_MUL).wrapping_add(inc);
    let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
    xorshifted.rotate_right((state >> 59) as u32)
}

/// The lattice value of point `i`, in `[−1, 1]`.
fn lattice(seed: u64, i: i64) -> f64 {
    pcg32_hash(seed, i as u64) as f64 / u32::MAX as f64 * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lattice is integer-exact: a GPU port must give these numbers.
    #[test]
    fn lattice_values_are_pinned() {
        assert_eq!(pcg32_hash(7, 0), 751_650_196);
        assert_eq!(pcg32_hash(7, 1), 1_919_132_652);
        assert_eq!(pcg32_hash(7, u64::MAX), 3_682_281_251);
        assert_eq!(pcg32_hash(42, 3), 1_655_746_530);
        let n = Noise1D { seed: 7, frequency: 1.0, amplitude: 1.0 };
        assert!((n.at(0.0) - (-0.6499856020440313)).abs() < 1e-12, "a lattice point is its value");
        assert!((n.at(0.5) - (-0.3781599102956615)).abs() < 1e-12);
        assert!((Noise1D { frequency: 0.7, ..n }.at(1.0) - (-0.22376291738258747)).abs() < 1e-12);
    }

    #[test]
    fn noise_is_bounded_smooth_and_seeded() {
        let n = Noise1D { seed: 99, frequency: 0.7, amplitude: 0.006 };
        let mut prev = n.at(0.0);
        for k in 1..3000 {
            let v = n.at(k as f64 / 300.0);
            assert!(v.abs() <= 0.006 + 1e-15, "bounded at {k}");
            assert!((v - prev).abs() < 0.0001, "smooth at {k}: {prev} → {v}");
            prev = v;
        }
        assert_eq!(n.at(3.21), n.at(3.21), "deterministic");
        assert_ne!(n.at(3.21), Noise1D { seed: 100, ..n }.at(3.21));
        assert_eq!(n.at(f64::NAN), 0.0);
    }
}
