//! M0 baseline: seek latency and sequential decode throughput through the
//! scaling stream (`open_stream`), allocations and frame buffer cost. The
//! M0 legacy export row is gone with the legacy export; its numbers stay in
//! `docs/perf/2026-09-30-m0-baseline.md`.

use crate::media::{self, TestClip};
use crate::report::Report;
use crate::alloc;
use kadr_core::perf::Stats;
use kadr_core::{FrameRate, Time};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::{MediaBackend, StreamRequest};
use std::time::Instant;

pub fn run(quick: bool) -> Result<Report, String> {
    let ff = FfmpegCli::locate().map_err(|e| e.to_string())?;
    let dir = media::bench_dir();
    let clips = media::ensure(&dir).map_err(|e| e.to_string())?;
    let (seeks, frames, reps) = if quick { (5, 60, 5) } else { (15, 150, 30) };
    let mut r = Report::new("m0-baseline");
    for clip in &clips {
        for (label, w, h) in [("full", clip.width, clip.height), ("half", clip.width / 2, clip.height / 2)] {
            let case = format!("{} {label}", clip.name);
            r.stats("seek", &case, &seek_latency(&ff, clip, w, h, seeks)?);
            let (fps, allocs) = decode_throughput(&ff, clip, w, h, frames)?;
            r.push("decode", &case, "fps", fps, "fps");
            r.push("decode", &case, "large_allocs_per_frame", allocs, "n");
        }
    }
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let (alloc_ms, copy_ms) = frame_memory(w, h, reps);
        let case = format!("{w}x{h} rgba");
        r.push("frame_buffer", &case, "alloc_and_first_touch", alloc_ms, "ms");
        r.push("frame_buffer", &case, "copy", copy_ms, "ms");
    }
    Ok(r)
}

fn request(clip: &TestClip, start: Time, w: u32, h: u32) -> StreamRequest {
    StreamRequest { path: clip.path.clone(), start, width: w, height: h, rate: FrameRate::FPS_30, speed: 1.0 }
}

/// Deterministic pseudo-random times in [0, secs − 2 s).
pub fn seek_times(secs: u32, n: usize) -> Vec<Time> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let span = (secs.saturating_sub(2) as u64).max(1) * 1000;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Time::from_millis((x % span) as i64)
        })
        .collect()
}

/// Request → first decoded frame for random positions (legacy: a new FFmpeg process each time).
fn seek_latency(ff: &FfmpegCli, clip: &TestClip, w: u32, h: u32, n: usize) -> Result<Stats, String> {
    let mut v = vec![];
    for t in seek_times(clip.secs, n) {
        let started = Instant::now();
        let mut s = ff.open_stream(&request(clip, t, w, h)).map_err(|e| e.to_string())?;
        s.next_frame().map_err(|e| e.to_string())?.ok_or("no frame after seek")?;
        v.push(started.elapsed());
    }
    Ok(Stats::of(v))
}

/// Frames per second of sequential decoding, and frame-sized allocations per frame.
fn decode_throughput(ff: &FfmpegCli, clip: &TestClip, w: u32, h: u32, frames: usize) -> Result<(f64, f64), String> {
    let mut s = ff.open_stream(&request(clip, Time::ZERO, w, h)).map_err(|e| e.to_string())?;
    s.next_frame().map_err(|e| e.to_string())?; // process start is measured by `seek`
    let allocs = alloc::large_allocs();
    let started = Instant::now();
    let mut n = 0usize;
    while n < frames {
        match s.next_frame().map_err(|e| e.to_string())? {
            Some(_) => n += 1,
            None => break,
        }
    }
    let secs = started.elapsed().as_secs_f64().max(1e-9);
    Ok((n as f64 / secs, (alloc::large_allocs() - allocs) as f64 / n.max(1) as f64))
}

/// Cost of a fresh frame buffer (allocation + first touch of every page) and of a full-frame copy.
fn frame_memory(w: u32, h: u32, reps: usize) -> (f64, f64) {
    let n = (w * h * 4) as usize;
    let started = Instant::now();
    for _ in 0..reps {
        let mut v = vec![0u8; n];
        for i in (0..n).step_by(4096) {
            v[i] = 1;
        }
        std::hint::black_box(v);
    }
    let alloc_ms = started.elapsed().as_secs_f64() * 1000.0 / reps as f64;
    let src = vec![7u8; n];
    let mut dst = vec![0u8; n];
    let started = Instant::now();
    for _ in 0..reps {
        dst.copy_from_slice(std::hint::black_box(&src));
        std::hint::black_box(&mut dst);
    }
    (alloc_ms, started.elapsed().as_secs_f64() * 1000.0 / reps as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_times_are_deterministic_and_inside_the_clip() {
        let a = seek_times(60, 15);
        assert_eq!(a, seek_times(60, 15));
        assert_eq!(a.len(), 15);
        assert!(a.iter().all(|t| *t >= Time::ZERO && *t < Time::from_secs(58)), "{a:?}");
        assert!(a.windows(2).any(|w| w[0] != w[1]));
    }
}
