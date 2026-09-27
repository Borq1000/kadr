//! Audio playback and the playback master clock.
//!
//! ```text
//!  PCM cache files ──► feeder thread (mix, gain, pan, fades, resample)
//!                        │ rtrb SPSC ring (lock-free)
//!                        ▼
//!                  cpal callback (real-time: no alloc, no locks, no I/O)
//!                        │ frames played → AtomicU64
//!                        ▼
//!                  position() = start + played / rate   ← video syncs to this
//! ```
//! If no output device exists, the clock falls back to wall time so video
//! playback still works.

mod mixer;

pub use mixer::MixSource;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use kadr_core::Time;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// Sample format of the PCM cache written by the media layer.
pub const PCM_RATE: u32 = 48_000;
pub const PCM_CHANNELS: u32 = 2;

struct Shared {
    consumer: Mutex<Option<rtrb::Consumer<f32>>>,
    played_frames: AtomicU64,
    playing: AtomicBool,
    out_channels: usize,
    start_flicks: AtomicI64,
    wall_start: Mutex<Option<Instant>>,
}

impl Shared {
    fn new(out_channels: usize) -> Self {
        Shared {
            consumer: Mutex::new(None),
            played_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            out_channels,
            start_flicks: AtomicI64::new(0),
            wall_start: Mutex::new(None),
        }
    }
}

/// Thread-safe view of the playback clock (the cpal stream itself is not
/// `Send`, so the video thread gets this instead).
#[derive(Clone)]
pub struct AudioClock {
    shared: Arc<Shared>,
    rate: u32,
    has_device: bool,
}

impl AudioClock {
    pub fn now(&self) -> Option<Time> {
        let sh = &self.shared;
        if !sh.playing.load(Ordering::Acquire) {
            return None;
        }
        let start = Time(sh.start_flicks.load(Ordering::Acquire));
        if self.has_device {
            let frames = sh.played_frames.load(Ordering::Acquire) as i64;
            Some(start + Time::from_samples(frames, self.rate))
        } else {
            sh.wall_start.lock().unwrap().map(|w| start + Time::from_secs_f64(w.elapsed().as_secs_f64()))
        }
    }
}

pub struct AudioEngine {
    _stream: Option<cpal::Stream>,
    shared: Arc<Shared>,
    rate: u32,
    feeder: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    pub device_name: String,
}

impl AudioEngine {
    pub fn new() -> Self {
        match Self::open_device() {
            Ok((stream, shared, rate, name)) => {
                tracing::info!(device = %name, rate, channels = shared.out_channels, "audio output ready");
                AudioEngine { _stream: Some(stream), shared, rate, feeder: None, device_name: name }
            }
            Err(e) => {
                tracing::warn!(error = %e, "no audio output; using wall clock");
                Self::silent()
            }
        }
    }

    /// No output device: playback runs on the wall clock, silently.
    pub fn silent() -> Self {
        AudioEngine { _stream: None, shared: Arc::new(Shared::new(2)), rate: PCM_RATE, feeder: None, device_name: "No audio device".into() }
    }

    fn open_device() -> Result<(cpal::Stream, Arc<Shared>, u32, String), String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no default output device")?;
        let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "Audio output".into());
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared::new(supported.channels() as usize));
        let rate = supported.sample_rate();
        let config = cpal::StreamConfig { channels: supported.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let err = |e| tracing::error!(error = %e, "audio stream error");
        let sh = shared.clone();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream::<f32, _, _>(config, move |out, _| fill(&sh, out, |v| v), err, None),
            cpal::SampleFormat::I16 => {
                device.build_output_stream::<i16, _, _>(config, move |out, _| fill(&sh, out, |v| (v * 32767.0) as i16), err, None)
            }
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, shared, rate, name))
    }

    pub fn has_device(&self) -> bool {
        self._stream.is_some()
    }

    /// Starts playback of `sources` from timeline time `start`.
    pub fn play(&mut self, start: Time, sources: Vec<MixSource>) {
        self.stop();
        // ~250 ms of audio buffered ahead.
        let cap = (self.rate as usize / 4) * PCM_CHANNELS as usize;
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(cap);
        *self.shared.consumer.lock().unwrap() = Some(consumer);
        self.shared.played_frames.store(0, Ordering::Release);
        self.shared.start_flicks.store(start.flicks(), Ordering::Release);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = mixer::spawn_feeder(producer, start, sources, self.rate, stop.clone());
        self.feeder = Some((stop, handle));
        *self.shared.wall_start.lock().unwrap() = Some(Instant::now());
        self.shared.playing.store(true, Ordering::Release);
    }

    pub fn stop(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
        if let Some((stop, h)) = self.feeder.take() {
            stop.store(true, Ordering::Release);
            let _ = h.join();
        }
        *self.shared.consumer.lock().unwrap() = None;
        *self.shared.wall_start.lock().unwrap() = None;
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    /// Current playback position (master clock).
    pub fn position(&self) -> Option<Time> {
        self.clock().now()
    }

    pub fn clock(&self) -> AudioClock {
        AudioClock { shared: self.shared.clone(), rate: self.rate, has_device: self.has_device() }
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Real-time callback: copies stereo frames from the ring into the device
/// buffer. `try_lock` never blocks; on contention we output silence once.
fn fill<T: Copy + Default>(sh: &Shared, out: &mut [T], conv: impl Fn(f32) -> T) {
    let ch = sh.out_channels.max(1);
    let mut guard = match sh.consumer.try_lock() {
        Ok(g) => g,
        Err(_) => {
            out.fill(T::default());
            return;
        }
    };
    let Some(cons) = guard.as_mut().filter(|_| sh.playing.load(Ordering::Acquire)) else {
        out.fill(T::default());
        return;
    };
    let mut frames = 0u64;
    for frame in out.chunks_exact_mut(ch) {
        if cons.slots() < 2 {
            frame.fill(T::default());
            continue; // underrun: silence, clock does not advance
        }
        let l = cons.pop().unwrap_or(0.0);
        let r = cons.pop().unwrap_or(0.0);
        match ch {
            1 => frame[0] = conv((l + r) * 0.5),
            _ => {
                frame[0] = conv(l);
                frame[1] = conv(r);
                for x in &mut frame[2..] {
                    *x = T::default();
                }
            }
        }
        frames += 1;
    }
    sh.played_frames.fetch_add(frames, Ordering::AcqRel);
}
