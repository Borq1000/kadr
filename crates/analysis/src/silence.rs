//! Deterministic silence detection over a dB level envelope.

use kadr_core::{Time, TimeRange};

pub const SILENCE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceParams {
    /// Windows below this RMS level are silent. `None` = derive from the
    /// material's own noise floor (see [`auto_threshold_db`]).
    pub threshold_db: Option<f32>,
    /// Only report silences at least this long.
    pub min_duration: Time,
    /// Audio kept on each side of a silence so cuts don't clip breaths or
    /// word tails. The reported range is shrunk by this amount per side.
    pub padding: Time,
    /// Non-silent blips shorter than this inside a silence (a click, a
    /// cough) do not break it.
    pub max_blip: Time,
}

impl Default for SilenceParams {
    fn default() -> Self {
        SilenceParams {
            threshold_db: None,
            min_duration: Time::from_secs(2),
            padding: Time::from_millis(150),
            max_blip: Time::from_millis(80),
        }
    }
}

/// Picks a silence threshold from the level distribution.
///
/// Recordings differ wildly: a lavalier in a studio has a −70 dB floor, a
/// concert hall −35 dB. A fixed threshold either misses pauses in noisy
/// rooms or eats quiet speech in clean ones. This estimates the noise floor
/// as the 10th percentile of non-digital-silence windows and puts the
/// threshold 8 dB above it, clamped to a sane range.
pub fn auto_threshold_db(levels_db: &[f32]) -> f32 {
    let mut v: Vec<f32> = levels_db.iter().copied().filter(|&l| l > -99.0).collect();
    if v.is_empty() {
        return -50.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let floor = v[v.len() / 10];
    let loud = v[v.len() * 9 / 10];
    // Keep the threshold well below typical programme level.
    (floor + 8.0).min(loud - 12.0).clamp(-60.0, -25.0)
}

/// Returns silent ranges (in the envelope's time base, i.e. source time).
pub fn detect_silence(levels_db: &[f32], window: Time, p: &SilenceParams) -> (f32, Vec<TimeRange>) {
    let threshold = p.threshold_db.unwrap_or_else(|| auto_threshold_db(levels_db));
    let blip = (p.max_blip.flicks() / window.flicks().max(1)) as usize;

    // Raw runs of silent windows.
    let mut runs: Vec<(usize, usize)> = vec![];
    let mut start = None;
    for (i, &l) in levels_db.iter().enumerate() {
        match (l < threshold, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, levels_db.len()));
    }
    // Merge runs separated by short blips.
    let mut merged: Vec<(usize, usize)> = vec![];
    for r in runs {
        match merged.last_mut() {
            Some(last) if r.0 - last.1 <= blip => last.1 = r.1,
            _ => merged.push(r),
        }
    }
    let at = |i: usize| Time(window.flicks() * i as i64);
    let ranges = merged
        .into_iter()
        .map(|(a, b)| (at(a), at(b)))
        .filter(|(a, b)| *b - *a >= p.min_duration)
        .filter_map(|(a, b)| {
            // Don't pad at the very start/end of the media: nothing to protect.
            let s = if a == Time::ZERO { a } else { a + p.padding };
            let e = if b == at(levels_db.len()) { b } else { b - p.padding };
            (s < e).then(|| TimeRange::new(s, e))
        })
        .collect();
    (threshold, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{analyze_pcm, tests::synth};

    fn detect(blocks: &[(f64, f64)], p: SilenceParams) -> Vec<(i64, i64)> {
        let pcm = synth(blocks, 48_000);
        let ov = analyze_pcm(&pcm[..], 48_000, 2, |_| true).unwrap();
        let (_, r) = detect_silence(&ov.levels_db, Time::from_millis(10), &p);
        r.iter().map(|r| (r.start.as_millis(), r.end.as_millis())).collect()
    }

    #[test]
    fn finds_long_pauses_only_with_padding() {
        let p = SilenceParams { threshold_db: Some(-40.0), ..Default::default() };
        let r = detect(&[(3.0, 0.5), (2.5, 0.0), (1.0, 0.5), (1.0, 0.0), (2.0, 0.5)], p);
        // 3.0–5.5 s silence (2.5 s ≥ 2 s) padded by 150 ms; the 1 s pause is ignored.
        assert_eq!(r, vec![(3_150, 5_350)]);
    }

    #[test]
    fn blips_do_not_split_a_pause() {
        let p = SilenceParams { threshold_db: Some(-40.0), padding: Time::ZERO, ..Default::default() };
        let r = detect(&[(1.0, 0.5), (1.2, 0.0), (0.05, 0.5), (1.2, 0.0), (1.0, 0.5)], p);
        assert_eq!(r, vec![(1_000, 3_450)]);
    }

    #[test]
    fn auto_threshold_adapts_to_noise_floor() {
        // Noisy room: floor at 0.01 amplitude (−43 dBFS RMS) plus speech.
        let pcm = synth(&[(4.0, 0.3), (3.0, 0.01), (4.0, 0.3)], 48_000);
        let ov = analyze_pcm(&pcm[..], 48_000, 2, |_| true).unwrap();
        let th = auto_threshold_db(&ov.levels_db);
        assert!(th > -43.0 && th < -20.0, "threshold {th}");
        let (_, r) = detect_silence(&ov.levels_db, Time::from_millis(10), &SilenceParams::default());
        assert_eq!(r.len(), 1);
    }
}
