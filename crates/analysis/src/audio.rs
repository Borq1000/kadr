//! One streaming pass over interleaved s16le PCM producing waveform peaks
//! and a 10 ms RMS level envelope. Memory is O(duration / window), never
//! O(samples): 90 minutes → ~1 MB peaks + ~2 MB levels.

use std::io::{self, Read};

pub const OVERVIEW_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioOverview {
    pub sample_rate: u32,
    /// Samples (per channel) represented by one peak.
    pub samples_per_peak: u32,
    /// Max |sample| of the mono mix per bucket, 0..=255 (linear).
    pub peaks: Vec<u8>,
    /// Samples per level window (10 ms).
    pub level_window: u32,
    /// RMS level per window, dBFS (−100 for digital silence).
    pub levels_db: Vec<f32>,
    /// Count of samples at or near full scale (clipping indicator).
    pub clipped_samples: u64,
}

impl AudioOverview {
    pub fn duration_secs(&self) -> f64 {
        self.levels_db.len() as f64 * self.level_window as f64 / self.sample_rate as f64
    }

    /// Peak value (0..1) for the bucket range covering `[t0, t1)` seconds —
    /// used to render waveform columns at any zoom level.
    pub fn peak_in(&self, t0: f64, t1: f64) -> f32 {
        let per = self.samples_per_peak as f64 / self.sample_rate as f64;
        let a = (t0 / per).floor().max(0.0) as usize;
        let b = ((t1 / per).ceil() as usize).min(self.peaks.len());
        if a >= b {
            return self.peaks.get(a).map_or(0.0, |&p| p as f32 / 255.0);
        }
        self.peaks[a..b].iter().copied().max().unwrap_or(0) as f32 / 255.0
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.peaks.len() + self.levels_db.len() * 4);
        out.extend_from_slice(b"KAOV");
        for v in [OVERVIEW_VERSION, self.sample_rate, self.samples_per_peak, self.level_window] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.clipped_samples.to_le_bytes());
        out.extend_from_slice(&(self.peaks.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.peaks);
        out.extend_from_slice(&(self.levels_db.len() as u64).to_le_bytes());
        for l in &self.levels_db {
            out.extend_from_slice(&l.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let mut r = b;
        let mut take = |n: usize| -> Option<&[u8]> {
            if r.len() < n {
                return None;
            }
            let (h, t) = r.split_at(n);
            r = t;
            Some(h)
        };
        if take(4)? != b"KAOV" {
            return None;
        }
        let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap());
        let u64_ = |s: &[u8]| u64::from_le_bytes(s.try_into().unwrap());
        if u32_(take(4)?) != OVERVIEW_VERSION {
            return None;
        }
        let sample_rate = u32_(take(4)?);
        let samples_per_peak = u32_(take(4)?);
        let level_window = u32_(take(4)?);
        let clipped_samples = u64_(take(8)?);
        let np = u64_(take(8)?) as usize;
        let peaks = take(np)?.to_vec();
        let nl = u64_(take(8)?) as usize;
        let levels_db = take(nl * 4)?.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        Some(AudioOverview { sample_rate, samples_per_peak, peaks, level_window, levels_db, clipped_samples })
    }
}

/// Streams PCM from `reader`. `on_progress` receives processed seconds.
pub fn analyze_pcm(
    mut reader: impl Read,
    sample_rate: u32,
    channels: u32,
    mut on_progress: impl FnMut(f64) -> bool,
) -> io::Result<AudioOverview> {
    let samples_per_peak = (sample_rate / 187).max(1); // ≈ 5.3 ms buckets
    let level_window = sample_rate / 100; // 10 ms
    let ch = channels.max(1) as usize;
    let mut buf = vec![0u8; 1 << 16];
    let mut carry: Vec<u8> = vec![];
    let frame_bytes = 2 * ch;

    let mut peaks = vec![];
    let mut levels_db = vec![];
    let (mut peak_max, mut peak_n) = (0i32, 0u32);
    let (mut sum_sq, mut lvl_n) = (0f64, 0u32);
    let mut clipped = 0u64;
    let mut total_frames = 0u64;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let whole = carry.len() / frame_bytes * frame_bytes;
        for frame in carry[..whole].chunks_exact(frame_bytes) {
            let mut mix = 0i32;
            for c in 0..ch {
                let s = i16::from_le_bytes([frame[2 * c], frame[2 * c + 1]]) as i32;
                if s.abs() >= 32_700 {
                    clipped += 1;
                }
                mix += s;
            }
            let m = mix / ch as i32;
            peak_max = peak_max.max(m.abs());
            peak_n += 1;
            if peak_n == samples_per_peak {
                peaks.push(((peak_max as f32 / 32768.0).sqrt() * 255.0).round().min(255.0) as u8);
                peak_max = 0;
                peak_n = 0;
            }
            let f = m as f64 / 32768.0;
            sum_sq += f * f;
            lvl_n += 1;
            if lvl_n == level_window {
                levels_db.push(rms_db(sum_sq, lvl_n));
                sum_sq = 0.0;
                lvl_n = 0;
            }
        }
        total_frames += (whole / frame_bytes) as u64;
        carry.drain(..whole);
        if !on_progress(total_frames as f64 / sample_rate as f64) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
    }
    if peak_n > 0 {
        peaks.push(((peak_max as f32 / 32768.0).sqrt() * 255.0).round().min(255.0) as u8);
    }
    if lvl_n > 0 {
        levels_db.push(rms_db(sum_sq, lvl_n));
    }
    Ok(AudioOverview { sample_rate, samples_per_peak, peaks, level_window, levels_db, clipped_samples: clipped })
}

fn rms_db(sum_sq: f64, n: u32) -> f32 {
    let rms = (sum_sq / n.max(1) as f64).sqrt();
    if rms <= 1e-5 { -100.0 } else { (20.0 * rms.log10()) as f32 }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Stereo s16le: `(seconds, amplitude)` blocks of a 440 Hz sine.
    pub fn synth(blocks: &[(f64, f64)], rate: u32) -> Vec<u8> {
        let mut out = vec![];
        let mut i = 0u64;
        for &(secs, amp) in blocks {
            for _ in 0..(secs * rate as f64) as u64 {
                let v = (amp * (i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 32767.0) as i16;
                out.extend_from_slice(&v.to_le_bytes());
                out.extend_from_slice(&v.to_le_bytes());
                i += 1;
            }
        }
        out
    }

    #[test]
    fn levels_and_peaks() {
        let pcm = synth(&[(1.0, 0.5), (1.0, 0.0)], 48_000);
        let ov = analyze_pcm(&pcm[..], 48_000, 2, |_| true).unwrap();
        assert_eq!(ov.levels_db.len(), 200);
        // 0.5 amplitude sine → RMS 0.3536 → −9.03 dBFS.
        assert!((ov.levels_db[50] + 9.03).abs() < 0.2, "{}", ov.levels_db[50]);
        assert_eq!(ov.levels_db[150], -100.0);
        assert!(ov.peak_in(0.0, 1.0) > 0.6);
        assert_eq!(ov.peak_in(1.1, 2.0), 0.0);
        assert!((ov.duration_secs() - 2.0).abs() < 1e-9);
        let rt = AudioOverview::from_bytes(&ov.to_bytes()).unwrap();
        assert_eq!(rt, ov);
    }
}
