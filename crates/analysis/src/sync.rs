//! Multicam sync: where does angle B sit relative to angle A? From embedded
//! start timecodes when both have them, otherwise by correlating the 10 ms
//! loudness envelopes every audio proxy already has.

use kadr_core::{FrameRate, Time};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncResult {
    /// B relative to A: A's envelope at `k + offset` matches B's at `k`
    /// (positive = B started recording later).
    pub offset: Time,
    /// 0..1: how clearly the best match beats every other; < 0.15 is noise.
    pub confidence: f32,
}

const FLOOR_DB: f32 = -60.0;
const COARSE: usize = 10;
/// Enough material to lock onto without O(n²) on a 90-minute concert.
const MAX_COMPARE: usize = 60_000;

/// Pearson correlation of `a[k + lag]` with `b[k]` over their overlap.
fn corr(a: &[f32], b: &[f32], lag: i64, min_overlap: usize) -> f32 {
    let k0 = (-lag).max(0) as usize;
    let k1 = (b.len() as i64).min(a.len() as i64 - lag).max(0) as usize;
    if k1 <= k0 || k1 - k0 < min_overlap {
        return 0.0;
    }
    let n = (k1 - k0) as f64;
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for k in k0..k1 {
        let x = a[(k as i64 + lag) as usize] as f64;
        let y = b[k] as f64;
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
    }
    let va = saa - sa * sa / n;
    let vb = sbb - sb * sb / n;
    if va < 1e-6 || vb < 1e-6 {
        return 0.0;
    }
    ((sab - sa * sb / n) / (va * vb).sqrt()) as f32
}

fn prepare(v: &[f32]) -> Vec<f32> {
    v.iter().map(|d| d.max(FLOOR_DB) - FLOOR_DB).collect()
}

fn decimate(v: &[f32]) -> Vec<f32> {
    v.chunks(COARSE).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect()
}

/// Best lag in `lags` and its correlation.
fn best(a: &[f32], b: &[f32], lags: impl Iterator<Item = i64>, min_overlap: usize) -> Vec<(i64, f32)> {
    lags.map(|l| (l, corr(a, b, l, min_overlap))).collect()
}

/// Cross-correlates 10 ms dB envelopes within ±`max_offset`.
pub fn align_envelopes(a_db: &[f32], b_db: &[f32], window: Time, max_offset: Time) -> SyncResult {
    let none = SyncResult { offset: Time::ZERO, confidence: 0.0 };
    let (a, b) = (prepare(a_db), prepare(b_db));
    // Compare a slice from B's middle: the start is often silence or tuning.
    let bs = b.len().saturating_sub(MAX_COMPARE) / 2;
    let b = &b[bs..(bs + MAX_COMPARE).min(b.len())];
    if a.is_empty() || b.is_empty() || window <= Time::ZERO {
        return none;
    }
    let max_lag = (max_offset.flicks() / window.flicks()).max(1);
    let (ca, cb) = (decimate(&a), decimate(b));
    let min_overlap = (cb.len() / 4).max(20);
    let bsc = (bs / COARSE) as i64;
    let coarse = best(&ca, &cb, (-max_lag / COARSE as i64 + bsc)..=(max_lag / COARSE as i64 + bsc), min_overlap);
    let Some(&(lc, r1)) = coarse.iter().max_by(|x, y| x.1.total_cmp(&y.1)) else { return none };
    // Runner-up outside ±1 s of the peak: periodic music gives several.
    let r2 = coarse.iter().filter(|(l, _)| (l - lc).abs() > 10).map(|x| x.1).fold(0.0f32, f32::max);
    let fine = best(&a, b, (lc * COARSE as i64 - 15)..=(lc * COARSE as i64 + 15), min_overlap * COARSE);
    let Some(&(lf, _)) = fine.iter().max_by(|x, y| x.1.total_cmp(&y.1)) else { return none };
    let confidence = if r1 <= 0.2 { 0.0 } else { ((r1 - r2.max(0.0)) / r1).clamp(0.0, 1.0) };
    SyncResult { offset: Time::from_flicks((lf - bs as i64) * window.flicks()), confidence }
}

fn tc_frames(tc: &str, nominal: i64, drop: i64) -> Option<i64> {
    let parts: Vec<i64> = tc.split([':', ';', '.']).map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let [h, m, s, f] = parts[..] else { return None };
    if m >= 60 || s >= 60 || f >= nominal {
        return None;
    }
    let minutes = 60 * h + m;
    let dropped = if tc.contains(';') { drop * (minutes - minutes / 10) } else { 0 };
    Some(((minutes * 60) + s) * nominal + f - dropped)
}

/// Offset between two embedded start timecodes (B − A) at `rate`.
pub fn timecode_offset(a_tc: &str, b_tc: &str, rate: FrameRate) -> Option<Time> {
    let nominal = rate.as_f64().round() as i64;
    let drop = if rate.den == 1001 { nominal / 15 } else { 0 };
    let (a, b) = (tc_frames(a_tc, nominal, drop)?, tc_frames(b_tc, nominal, drop)?);
    Some(rate.frame_to_time(b - a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{FrameRate, Time};

    /// Deterministic "concert" envelope: loud/quiet runs of 50–500 ms.
    fn envelope(n: usize, seed: u64) -> Vec<f32> {
        let mut x = seed;
        let mut next = move || {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u32
        };
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let len = 5 + next() as usize % 45;
            let level = -60.0 + (next() % 54) as f32;
            out.extend(std::iter::repeat_n(level, len));
        }
        out.truncate(n);
        out
    }

    #[test]
    fn finds_known_shift() {
        let a = envelope(6000, 7); // 60 s
        let b: Vec<f32> = a[137..].to_vec(); // B started 1.37 s later
        let r = align_envelopes(&a, &b, Time::from_millis(10), Time::from_secs(10));
        assert_eq!(r.offset, Time::from_millis(1370));
        assert!(r.confidence > 0.5, "{}", r.confidence);
    }

    #[test]
    fn finds_negative_shift_with_noise() {
        let a = envelope(6000, 11);
        // B started 2.5 s earlier and hears the room a bit differently.
        let mut b = envelope(250, 99);
        b.extend(a.iter().enumerate().map(|(i, v)| (v + if i % 7 == 0 { 4.0 } else { -2.0 }).min(0.0)));
        let r = align_envelopes(&a, &b, Time::from_millis(10), Time::from_secs(10));
        assert_eq!(r.offset, Time::from_millis(-2500));
    }

    #[test]
    fn flat_envelopes_report_low_confidence() {
        let r = align_envelopes(&vec![-60.0; 3000], &vec![-60.0; 3000], Time::from_millis(10), Time::from_secs(10));
        assert!(r.confidence < 0.15, "{}", r.confidence);
    }

    #[test]
    fn unrelated_envelopes_report_low_confidence() {
        let r = align_envelopes(&envelope(6000, 1), &envelope(6000, 2), Time::from_millis(10), Time::from_secs(10));
        assert!(r.confidence < 0.15, "{}", r.confidence);
    }

    #[test]
    fn timecode_difference() {
        assert_eq!(timecode_offset("01:00:00:00", "01:00:02:12", FrameRate::FPS_25), Some(Time::from_millis(2480)));
        assert_eq!(timecode_offset("10:00:00:00", "09:59:59:00", FrameRate::FPS_25), Some(Time::from_secs(-1)));
        assert_eq!(timecode_offset("junk", "01:00:00:00", FrameRate::FPS_25), None);
    }
}
