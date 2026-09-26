//! Feeder thread: mixes clip sources from the PCM cache into the ring.

use crate::{PCM_CHANNELS, PCM_RATE};
use kadr_core::Time;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// One audio clip, in timeline terms, reading from a cached PCM file
/// (s16le, 48 kHz, stereo).
#[derive(Clone, Debug)]
pub struct MixSource {
    pub pcm: PathBuf,
    pub timeline_start: Time,
    pub timeline_end: Time,
    /// Source time at `timeline_start`.
    pub source_start: Time,
    pub speed: f64,
    pub gain_db: f64,
    pub pan: f64,
    pub fade_in: Time,
    pub fade_out: Time,
}

const BLOCK: usize = 1024; // device frames per mix block

struct Reader {
    src: MixSource,
    file: Option<File>,
    scratch: Vec<i16>,
    raw: Vec<u8>,
}

impl Reader {
    /// Reads source frames `[first, first+n)` into `scratch` (zeros past EOF).
    fn load(&mut self, first: i64, n: usize) {
        self.scratch.clear();
        self.scratch.resize(n * PCM_CHANNELS as usize, 0);
        let Some(f) = self.file.as_mut() else { return };
        let skip = (-first).max(0) as usize; // frames before source start
        if skip >= n {
            return;
        }
        let byte_pos = (first.max(0) as u64) * 2 * PCM_CHANNELS as u64;
        if f.seek(SeekFrom::Start(byte_pos)).is_err() {
            return;
        }
        let want = (n - skip) * 2 * PCM_CHANNELS as usize;
        self.raw.resize(want, 0);
        let mut got = 0;
        while got < want {
            match f.read(&mut self.raw[got..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => got += k,
            }
        }
        for (i, c) in self.raw[..got - got % 2].chunks_exact(2).enumerate() {
            self.scratch[skip * PCM_CHANNELS as usize + i] = i16::from_le_bytes([c[0], c[1]]);
        }
    }
}

pub(crate) fn spawn_feeder(
    mut producer: rtrb::Producer<f32>,
    start: Time,
    sources: Vec<MixSource>,
    out_rate: u32,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("kadr-audio-feeder".into())
        .spawn(move || {
            let mut readers: Vec<Reader> = sources
                .into_iter()
                .map(|src| {
                    let file = File::open(&src.pcm).map_err(|e| tracing::warn!(pcm = %src.pcm.display(), error = %e, "pcm missing")).ok();
                    Reader { src, file, scratch: vec![], raw: vec![] }
                })
                .collect();
            let mut produced: i64 = 0;
            let mut mix = vec![0f32; BLOCK * 2];
            while !stop.load(Ordering::Acquire) {
                if producer.slots() < BLOCK * 2 {
                    std::thread::sleep(Duration::from_millis(3));
                    continue;
                }
                let t0 = start + Time::from_samples(produced, out_rate);
                let t1 = start + Time::from_samples(produced + BLOCK as i64, out_rate);
                mix.fill(0.0);
                for r in readers.iter_mut().filter(|r| r.src.timeline_start < t1 && r.src.timeline_end > t0) {
                    mix_source(r, t0, out_rate, &mut mix);
                }
                if let Ok(mut chunk) = producer.write_chunk_uninit(BLOCK * 2) {
                    let (a, b) = chunk.as_mut_slices();
                    let mut it = mix.iter().map(|v| v.clamp(-1.0, 1.0));
                    for slot in a.iter_mut().chain(b.iter_mut()) {
                        slot.write(it.next().unwrap_or(0.0));
                    }
                    // SAFETY: every slot in both slices was initialised above.
                    unsafe { chunk.commit_all() };
                }
                produced += BLOCK as i64;
            }
        })
        .expect("spawn feeder")
}

/// Adds one source's contribution for the block starting at `t0`.
fn mix_source(r: &mut Reader, t0: Time, out_rate: u32, mix: &mut [f32]) {
    let s = r.src.clone();
    let speed = if s.speed > 0.0 { s.speed } else { 1.0 };
    // Source position (in PCM frames, fractional) of the block's first frame.
    let rel0 = (t0 - s.timeline_start).as_secs_f64();
    let src0 = s.source_start.as_secs_f64() + rel0 * speed;
    let step = speed * PCM_RATE as f64 / out_rate as f64; // source frames per output frame
    let first = (src0 * PCM_RATE as f64).floor() as i64;
    let need = (BLOCK as f64 * step).ceil() as usize + 2;
    r.load(first, need);
    let frac0 = src0 * PCM_RATE as f64 - first as f64;

    let gain = 10f64.powf(s.gain_db / 20.0) as f32;
    let pan = s.pan.clamp(-1.0, 1.0) as f32;
    let (gl, gr) = ((1.0 - pan).min(1.0) * gain, (1.0 + pan).min(1.0) * gain);
    let dur = (s.timeline_end - s.timeline_start).as_secs_f64();
    let (fi, fo) = (s.fade_in.as_secs_f64(), s.fade_out.as_secs_f64());
    let samples = &r.scratch;

    for i in 0..BLOCK {
        let rel = rel0 + i as f64 / out_rate as f64;
        if rel < 0.0 || rel >= dur {
            continue;
        }
        let mut env = 1.0f64;
        if fi > 0.0 && rel < fi {
            env = rel / fi;
        }
        if fo > 0.0 && rel > dur - fo {
            env = env.min((dur - rel) / fo);
        }
        let pos = frac0 + i as f64 * step;
        let idx = pos.floor() as usize;
        let f = (pos - idx as f64) as f32;
        let at = |k: usize, c: usize| samples.get(k * 2 + c).copied().unwrap_or(0) as f32 / 32768.0;
        // Linear interpolation covers speed changes and 44.1 kHz devices.
        let l = at(idx, 0) * (1.0 - f) + at(idx + 1, 0) * f;
        let rr = at(idx, 1) * (1.0 - f) + at(idx + 1, 1) * f;
        let e = env as f32;
        mix[i * 2] += l * gl * e;
        mix[i * 2 + 1] += rr * gr * e;
    }
}
