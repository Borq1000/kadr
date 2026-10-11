//! Integer hashes with saved semantics (effects spec §3: «детерминированный
//! целочисленный хэш (PCG32), одинаковый на CPU и будущем GPU»): u64
//! wrapping operations only, so a shader reproduces them bit for bit. A
//! project's handheld shake (`kadr_effects::Noise1D`) and grain
//! (`kadr_scene::Grain`) are made of them: never change one.

/// Odd 64-bit golden-ratio constant (splitmix64's increment).
pub const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// murmur3's `fmix64` finaliser: a bijection on `u64` that avalanches every
/// input bit into every output bit (0 is its fixed point).
pub fn fmix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    x ^ (x >> 33)
}

/// The hash of index `i` on stream `seed`:
///
/// 1. `stream = fmix64(seed + GOLDEN mod 2⁶⁴)` ([`pcg32_stream`]) — all 64
///    seed bits avalanched (the offset keeps seed 0 off `fmix64`'s fixed point);
/// 2. `state = fmix64(stream ^ (i · GOLDEN mod 2⁶⁴))`;
/// 3. the PCG32 XSH-RR output of `state`:
///    `rotr32(((state >> 18) ^ state) >> 27 truncated to 32 bits, state >> 59)`.
///
/// Different seeds give unrelated sequences, never shifted copies.
pub fn pcg32_hash(seed: u64, i: u64) -> u32 {
    pcg32_at(pcg32_stream(seed), i)
}

/// Step 1 of [`pcg32_hash`], once per seed (a renderer hashing every pixel of a layer).
pub fn pcg32_stream(seed: u64) -> u64 {
    fmix64(seed.wrapping_add(GOLDEN))
}

/// Steps 2 and 3 of [`pcg32_hash`] on a [`pcg32_stream`].
pub fn pcg32_at(stream: u64, i: u64) -> u32 {
    let state = fmix64(stream ^ i.wrapping_mul(GOLDEN));
    let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
    xorshifted.rotate_right((state >> 59) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Saved semantics: Handheld's shake and the grain are made of these numbers.
    #[test]
    fn the_hash_is_pinned() {
        assert_eq!(pcg32_hash(7, 0), 1_630_685_630);
        assert_eq!(pcg32_hash(7, 1), 1_910_406_292);
        assert_eq!(pcg32_hash(7, u64::MAX), 246_282_542);
        assert_eq!(pcg32_hash(42, 3), 3_782_942_457);
        assert_eq!(pcg32_hash(7, 1 << 32), 4_255_874_242);
        assert_eq!((fmix64(0), fmix64(1)), (0, 12_994_781_566_227_106_604));
    }

    /// A renderer takes the stream once per layer and hashes every pixel's index on it.
    #[test]
    fn a_stream_then_an_index_is_the_hash() {
        for seed in [0u64, 7, u64::MAX] {
            let s = pcg32_stream(seed);
            for i in [0u64, 1, 99, 1 << 32, u64::MAX] {
                assert_eq!(pcg32_at(s, i), pcg32_hash(seed, i), "seed {seed} index {i}");
            }
        }
    }
}
