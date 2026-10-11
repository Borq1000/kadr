//! `Noise1D` (effects spec §3): smooth value noise for natural motion
//! (Handheld). Lattice values come from an integer hash of (seed, lattice
//! index) ending in one PCG32 (XSH-RR) output step — integer-exact, so a
//! GPU shader reproduces them bit for bit.

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

/// Odd 64-bit golden-ratio constant (splitmix64's increment).
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// murmur3's `fmix64` finaliser: a bijection on `u64` that avalanches every
/// input bit into every output bit.
fn fmix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    x ^ (x >> 33)
}

/// The hash of lattice point `i` on stream `seed` — saved project
/// semantics, so the algorithm is fixed (u64 wrapping operations only; a GPU
/// port reproduces it bit for bit):
///
/// 1. `stream = fmix64(seed + 0x9E3779B97F4A7C15 mod 2⁶⁴)` — all 64 seed
///    bits, every one avalanched (the offset keeps seed 0 off `fmix64`'s
///    fixed point 0);
/// 2. `state = fmix64(stream ^ (i · 0x9E3779B97F4A7C15 mod 2⁶⁴))`;
/// 3. the PCG32 XSH-RR output of `state`:
///    `rotr32(((state >> 18) ^ state) >> 27 truncated to 32 bits, state >> 59)`.
///
/// Different seeds therefore give unrelated sequences (never shifted copies
/// of one another).
pub fn pcg32_hash(seed: u64, i: u64) -> u32 {
    let state = fmix64(fmix64(seed.wrapping_add(GOLDEN)) ^ i.wrapping_mul(GOLDEN));
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
        assert_eq!(pcg32_hash(7, 0), 1_630_685_630);
        assert_eq!(pcg32_hash(7, 1), 1_910_406_292);
        assert_eq!(pcg32_hash(7, u64::MAX), 246_282_542);
        assert_eq!(pcg32_hash(42, 3), 3_782_942_457);
        let n = Noise1D { seed: 7, frequency: 1.0, amplitude: 1.0 };
        assert!((n.at(0.0) - (-0.24065283016316896)).abs() < 1e-12, "a lattice point is its value");
        assert!((n.at(0.5) - (-0.17552528837125875)).abs() < 1e-12);
        assert!((Noise1D { frequency: 0.7, ..n }.at(1.0) - (-0.13853284463345375)).abs() < 1e-12);
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

    /// The seed selects an independent stream: no seed's sequence is a shifted
    /// copy of another's (the first version was one global sequence read at
    /// a seed-dependent offset).
    #[test]
    fn seeds_are_independent_streams_not_shifted_copies() {
        let seeds = [0u64, 1, 2, 99, 100, 7, 1 << 63, (1 << 63) ^ 1, u64::MAX, 7 ^ (1 << 63)];
        for &a in &seeds {
            for &b in &seeds {
                if a == b {
                    continue;
                }
                for k in -64i64..=64 {
                    let same = (0..256u64).filter(|&i| pcg32_hash(a, 1000 + i) == pcg32_hash(b, (1000 + i as i64 + k) as u64)).count();
                    assert!(same < 4, "seed {a} is seed {b} shifted by {k} ({same} of 256 equal)");
                }
            }
        }
        // the same at the noise level: seed 100 at t is not seed 99 at t + 2/f
        let (n99, n100) = (Noise1D { seed: 99, frequency: 0.7, amplitude: 1.0 }, Noise1D { seed: 100, frequency: 0.7, amplitude: 1.0 });
        for k in 0..50 {
            let t = k as f64 * 0.37;
            assert!((n100.at(t) - n99.at(t + 2.0 / 0.7)).abs() > 1e-9, "shifted copy at t = {t}");
            assert!((n100.at(t) - n99.at(t)).abs() > 1e-9, "same value at t = {t}");
        }
    }
}
