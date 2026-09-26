//! Per-frame picture statistics from a small grey proxy stream, and shots
//! derived from them. Feeds Jev with *words* ("sharp", "dark"), never pixels.

use kadr_core::{Time, TimeRange};

pub const VIDEO_VERSION: u32 = 1;
pub const ANALYSIS_W: u32 = 160;
pub const ANALYSIS_H: u32 = 90;
pub const ANALYSIS_FPS: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStats {
    /// Mean luma, 0..1.
    pub luma: f32,
    /// Variance of the 4-neighbour Laplacian / 255² (focus measure).
    pub sharpness: f32,
    /// Mean |Δ| against the previous sample, 0..1 (cuts, shake, action).
    pub motion: f32,
    pub black: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoOverview {
    pub fps: u32,
    pub frames: Vec<FrameStats>,
}

/// A run of frames between cuts, in source time, with robust statistics.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub range: TimeRange,
    /// Medians over the shot (the cut frame itself excluded from motion).
    pub sharpness: f32,
    pub luma: f32,
    pub motion: f32,
    /// Mostly black frames.
    pub black: bool,
}

pub fn rgba_to_gray(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .map(|p| (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round().min(255.0) as u8)
        .collect()
}

pub fn analyze_gray_frame(gray: &[u8], w: u32, h: u32, prev: Option<&[u8]>) -> FrameStats {
    let (w, h) = (w as usize, h as usize);
    let n = gray.len().max(1) as f32;
    let luma = gray.iter().map(|&v| v as f32).sum::<f32>() / n / 255.0;
    let (mut s, mut s2, mut m) = (0f64, 0f64, 0usize);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let lap = 4.0 * gray[i] as f64 - gray[i - 1] as f64 - gray[i + 1] as f64 - gray[i - w] as f64 - gray[i + w] as f64;
            s += lap;
            s2 += lap * lap;
            m += 1;
        }
    }
    let sharpness = if m == 0 { 0.0 } else { ((s2 / m as f64 - (s / m as f64).powi(2)) / (255.0 * 255.0)) as f32 };
    let motion = prev
        .filter(|p| p.len() == gray.len())
        .map_or(0.0, |p| p.iter().zip(gray).map(|(a, b)| a.abs_diff(*b) as f32).sum::<f32>() / n / 255.0);
    FrameStats { luma, sharpness, motion, black: luma < 0.04 && sharpness < 0.001 }
}

// Bucket words are what Jev reads; they match the level wording of the
// shot-usability template so the model doesn't mis-map them (research §5b).
pub fn bucket_sharpness(s: f32) -> &'static str {
    if s >= 0.01 {
        "sharp"
    } else if s >= 0.002 {
        "soft"
    } else {
        "very blurry"
    }
}

pub fn bucket_exposure(l: f32) -> &'static str {
    if l < 0.04 {
        "black"
    } else if l < 0.2 {
        "dark"
    } else if l > 0.85 {
        "blown out"
    } else {
        "normal"
    }
}

pub fn bucket_shake(m: f32) -> &'static str {
    if m < 0.03 {
        "none"
    } else if m < 0.08 {
        "slight"
    } else {
        "heavy"
    }
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Splits at hard cuts: a motion spike above 0.35 and 3× the local median.
/// Shots shorter than `min_len` (flash frames, fades) merge into the previous.
pub fn detect_shots(ov: &VideoOverview, min_len: Time) -> Vec<Shot> {
    let f = &ov.frames;
    if f.is_empty() || ov.fps == 0 {
        return vec![];
    }
    let at = |i: usize| Time::from_secs_f64(i as f64 / ov.fps as f64);
    let min_frames = ((min_len.as_secs_f64() * ov.fps as f64).ceil() as usize).max(1);
    let mut starts = vec![0usize];
    for i in 1..f.len() {
        let lo = i.saturating_sub(8);
        let hi = (i + 9).min(f.len());
        let local = median((lo..hi).filter(|&k| k != i).map(|k| f[k].motion).collect());
        if f[i].motion > 0.35 && f[i].motion > 3.0 * local && i - starts.last().unwrap() >= min_frames {
            starts.push(i);
        }
    }
    // A too-short tail joins the last shot as well.
    if starts.len() > 1 && f.len() - starts.last().unwrap() < min_frames {
        starts.pop();
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &a)| {
            let b = starts.get(k + 1).copied().unwrap_or(f.len());
            let fr = &f[a..b];
            Shot {
                range: TimeRange::new(at(a), at(b)),
                sharpness: median(fr.iter().map(|x| x.sharpness).collect()),
                luma: median(fr.iter().map(|x| x.luma).collect()),
                motion: median(fr.iter().skip(1).map(|x| x.motion).collect()),
                black: fr.iter().filter(|x| x.black).count() * 5 >= fr.len() * 4,
            }
        })
        .collect()
}

impl VideoOverview {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(20 + self.frames.len() * 13);
        out.extend_from_slice(b"KAVO");
        out.extend_from_slice(&VIDEO_VERSION.to_le_bytes());
        out.extend_from_slice(&self.fps.to_le_bytes());
        out.extend_from_slice(&(self.frames.len() as u64).to_le_bytes());
        for f in &self.frames {
            for v in [f.luma, f.sharpness, f.motion] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.push(f.black as u8);
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let rest = b.strip_prefix(b"KAVO")?;
        let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap());
        if rest.len() < 16 || u32_(&rest[0..4]) != VIDEO_VERSION {
            return None;
        }
        let fps = u32_(&rest[4..8]);
        let n = u64::from_le_bytes(rest[8..16].try_into().unwrap()) as usize;
        let body = &rest[16..];
        if body.len() != n.checked_mul(13)? {
            return None;
        }
        let f32_ = |s: &[u8]| f32::from_le_bytes(s.try_into().unwrap());
        let frames = body
            .chunks_exact(13)
            .map(|c| FrameStats { luma: f32_(&c[0..4]), sharpness: f32_(&c[4..8]), motion: f32_(&c[8..12]), black: c[12] != 0 })
            .collect();
        Some(VideoOverview { fps, frames })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;

    fn checker(w: u32, h: u32, cell: u32, shift: u32) -> Vec<u8> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w + shift, i / w);
                if (x / cell + y / cell) % 2 == 0 { 230 } else { 20 }
            })
            .collect()
    }

    fn flat(w: u32, h: u32, v: u8) -> Vec<u8> {
        vec![v; (w * h) as usize]
    }

    #[test]
    fn sharp_beats_flat_and_black_is_black() {
        let s = analyze_gray_frame(&checker(160, 90, 4, 0), 160, 90, None);
        let f = analyze_gray_frame(&flat(160, 90, 120), 160, 90, None);
        let b = analyze_gray_frame(&flat(160, 90, 3), 160, 90, None);
        assert!(s.sharpness > 10.0 * f.sharpness.max(1e-6));
        assert!(b.black && !s.black && !f.black);
        assert_eq!(bucket_sharpness(s.sharpness), "sharp");
        assert_eq!(bucket_sharpness(f.sharpness), "very blurry");
        assert_eq!(bucket_exposure(b.luma), "black");
        assert_eq!(bucket_exposure(f.luma), "normal");
    }

    #[test]
    fn motion_measures_change() {
        let a = checker(160, 90, 8, 0);
        assert!(analyze_gray_frame(&checker(160, 90, 8, 4), 160, 90, Some(&a)).motion > 0.2);
        assert!(analyze_gray_frame(&a, 160, 90, Some(&a)).motion < 0.01);
    }

    #[test]
    fn rgba_to_gray_uses_luma_weights() {
        assert_eq!(rgba_to_gray(&[255, 255, 255, 255, 0, 0, 0, 255, 0, 255, 0, 255]), vec![255, 0, 150]);
    }

    #[test]
    fn cuts_split_shots() {
        let calm = FrameStats { luma: 0.5, sharpness: 0.05, motion: 0.02, black: false };
        let mut frames = vec![calm; 40];
        frames[20].motion = 0.8; // hard cut at 5 s (4 fps)
        let shots = detect_shots(&VideoOverview { fps: 4, frames }, Time::from_secs(1));
        assert_eq!(shots.len(), 2);
        assert_eq!(shots[1].range.start, Time::from_secs(5));
        assert_eq!(shots[1].range.end, Time::from_secs(10));
        assert_eq!(bucket_shake(shots[0].motion), "none");
    }

    #[test]
    fn short_shots_merge_into_the_previous_one() {
        let calm = FrameStats { luma: 0.5, sharpness: 0.05, motion: 0.02, black: false };
        let mut frames = vec![calm; 40];
        frames[20].motion = 0.8;
        frames[22].motion = 0.8; // 0.5 s flash frame
        let shots = detect_shots(&VideoOverview { fps: 4, frames }, Time::from_secs(1));
        assert_eq!(shots.iter().map(|s| s.range.start).collect::<Vec<_>>(), vec![Time::ZERO, Time::from_secs(5)]);
    }

    #[test]
    fn overview_roundtrip() {
        let ov = VideoOverview { fps: 4, frames: vec![FrameStats { luma: 0.25, sharpness: 0.01, motion: 0.5, black: true }; 3] };
        assert_eq!(VideoOverview::from_bytes(&ov.to_bytes()), Some(ov));
        assert_eq!(VideoOverview::from_bytes(b"junk"), None);
    }
}
