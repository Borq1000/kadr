//! Export on the render pipeline (M5 plan, task 2): every output frame goes
//! through the scene source (evaluator), a [`Resolver`] of its own in
//! [`Mode::Export`] and the [`CpuRenderer`] — what the preview shows is what
//! the file gets — and is piped as raw RGBA into a [`FrameEncoder`].
//!
//! ```text
//!  scene_at(t) ─► Resolver (Export: waits for every layer) ─► CpuRenderer ─► buffer A ─┐
//!  scene_at(t + 1 s) ─► prefetch (next clip's stream opens before its cut)             │
//!                                   writer thread: write_frame(buffer B) ◄─ channel ◄──┘
//!                                   (buffers go back to the renderer for reuse)
//! ```
//!
//! Two output buffers: frame `n + 1` renders while the writer thread pipes
//! frame `n`; no frame-sized allocation per frame once warm. Cancel is
//! checked every frame (and interrupts a wait for a slow decoder); it, a
//! render or encoder error and missing media (with [`MissingPolicy::Fail`])
//! abort the encoder, which deletes its partial output.

use crate::decoders::Decoders;
use crate::resolver::{Mode, Resolver, ResolverConfig, ResolverStats};
use crate::source::SceneSource;
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use kadr_core::perf::{FramePerf, Stats};
use kadr_core::{AssetId, CancelToken, ColorInfo, Time};
use kadr_media::{EncodeJob, FrameEncoder, MediaBackend, MediaError};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderError, RenderTarget, Renderer};
use kadr_scene::{Layer, LayerContent, OutputSpec, RenderQuality, SizeU};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How far ahead of the frame being rendered the scene is prefetched.
const PREFETCH_AHEAD: Time = Time::from_secs(1);
/// Progress is reported at most once per this much progress…
const PROGRESS_STEP: f32 = 0.01;
/// …or after this long, whichever comes first.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// How often a wait for decoded frames looks at the cancel token.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Starts encoders. Implemented for every [`MediaBackend`] (its
/// `start_encode`); tests implement it with an encoder that records frames.
pub trait EncoderFactory: Send + Sync {
    fn start(&self, job: &EncodeJob) -> Result<Box<dyn FrameEncoder>, MediaError>;
}

impl<B: MediaBackend + ?Sized> EncoderFactory for B {
    fn start(&self, job: &EncodeJob) -> Result<Box<dyn FrameEncoder>, MediaError> {
        self.start_encode(job)
    }
}

/// What to do when a layer's media is offline or fails to decode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MissingPolicy {
    /// Stop the export with [`ExportError::MissingMedia`] (never silently
    /// export the MISSING placeholder).
    #[default]
    Fail,
    /// Draw the MISSING placeholder, as the preview does (the user opted in).
    DrawPlaceholder,
}

/// One export: the scene source, the encode job (its size, rate and frame
/// count define the frames rendered) and the missing-media policy.
pub struct ExportRequest {
    pub source: Arc<dyn SceneSource>,
    /// `job.frames` frames are rendered (the caller computes
    /// `job.rate.time_to_frame_round(job.total)`); frame `n` is the scene
    /// at `job.rate.frame_to_time(n)`, rendered at `job.width × job.height`.
    pub job: EncodeJob,
    pub missing: MissingPolicy,
    /// The export's own resolver (default: [`ExportRequest::resolver_config`]).
    pub resolver: ResolverConfig,
}

impl ExportRequest {
    /// Missing media fails the export; resolver sized for export.
    pub fn new(source: Arc<dyn SceneSource>, job: EncodeJob) -> Self {
        ExportRequest { source, job, missing: MissingPolicy::Fail, resolver: Self::resolver_config() }
    }

    pub fn with_missing(mut self, missing: MissingPolicy) -> Self {
        self.missing = missing;
        self
    }

    /// Frames are needed once, in order: a smaller cache than the preview's
    /// (it holds the read-ahead and transition overlaps; once full, evicted
    /// buffers are reused instead of allocating).
    pub fn resolver_config() -> ResolverConfig {
        ResolverConfig { cache_bytes: 512 << 20, pool_free_bytes: 256 << 20, ..ResolverConfig::default() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportStats {
    /// Frames rendered and written.
    pub frames: i64,
    /// Whole export, encoder start to finish.
    pub elapsed: Duration,
    /// `frames / elapsed`.
    pub fps: f64,
    /// Rendering (`CpuRenderer::render`) per frame.
    pub render: Stats,
    pub render_total: Duration,
    /// Top-level layers drawn over all frames, and how many of those took the
    /// renderer's fast path (a row copy instead of per-pixel sampling).
    pub layers_drawn: u64,
    pub fast_paths: u64,
    /// Time spent in `Resolver::prepare`, waiting for decoded frames.
    pub decode_wait: Duration,
    /// Time the renderer waited for a free output buffer (encoder back-pressure).
    pub encoder_wait: Duration,
    /// Time the writer thread spent in `FrameEncoder::write_frame`.
    pub write_total: Duration,
    /// `FrameEncoder::finish` (flushing the encoder, muxing).
    pub finish: Duration,
    /// The resolver at the end (streams opened, frames decoded, …).
    pub resolver: ResolverStats,
    /// Frame-sized buffers allocated in all: decoded frames, transition
    /// buffers, output buffers.
    pub allocations: u64,
    /// Of those, the ones allocated while rendering the second half of the
    /// frames: 0 once the pipeline is warm (buffers are reused).
    pub late_allocations: u64,
}

#[derive(Debug)]
pub enum ExportError {
    Cancelled,
    /// A layer's media is offline or failed to decode ([`MissingPolicy::Fail`]).
    MissingMedia {
        media: AssetId,
        /// `None` when the scene source does not know the media.
        path: Option<PathBuf>,
        reason: MissingReason,
        /// Timeline time and output frame index where it is needed first.
        time: Time,
        frame: i64,
    },
    /// Starting, feeding or finishing the encoder failed.
    Encode(MediaError),
    Render(RenderError),
    /// The job cannot be exported (empty size, negative frame count).
    InvalidJob(String),
    /// A bug: a layer not ready in export mode, a renderer panic, …
    Internal(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::Cancelled => write!(f, "export cancelled"),
            ExportError::MissingMedia { media, path, reason, time, frame } => {
                let what = match reason {
                    MissingReason::Offline => "media offline",
                    MissingReason::DecodeFailed => "media cannot be decoded",
                    MissingReason::NotReady => "media not ready",
                };
                match path {
                    Some(p) => write!(f, "{what}: {}", p.display())?,
                    None => write!(f, "{what}: unknown media {media}")?,
                }
                write!(f, " (needed at {}, frame {frame})", clock(*time))
            }
            ExportError::Encode(e) => write!(f, "encoder: {e}"),
            ExportError::Render(e) => write!(f, "render: {e}"),
            ExportError::InvalidJob(m) => write!(f, "invalid export: {m}"),
            ExportError::Internal(m) => write!(f, "export failed (internal): {m}"),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Encode(e) => Some(e),
            ExportError::Render(e) => Some(e),
            _ => None,
        }
    }
}

/// `h:mm:ss.mmm` (or `m:ss.mmm` under an hour).
fn clock(t: Time) -> String {
    let ms = t.as_micros().max(0) / 1000;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{ms:03}") } else { format!("{m}:{s:02}.{ms:03}") }
}

/// Renders every frame of `req` and encodes it (see the module docs).
/// `progress` gets frames written / frames in `[0, 1]`, at most about every
/// 1 % or 100 ms, from the calling thread or the writer thread; 1.0 once the
/// output is complete. Blocks until done, failed or cancelled; on any error
/// the encoder is aborted (no partial output is left).
pub fn export<E: EncoderFactory + ?Sized>(
    req: ExportRequest,
    decoders: Arc<dyn Decoders>,
    encoder: &E,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &CancelToken,
) -> Result<ExportStats, ExportError> {
    let job = &req.job;
    if job.width == 0 || job.height == 0 {
        return Err(ExportError::InvalidJob(format!("frame size {}×{}", job.width, job.height)));
    }
    if job.frames < 0 {
        return Err(ExportError::InvalidJob(format!("negative frame count {}", job.frames)));
    }
    if job.rate.num == 0 || job.rate.den == 0 {
        return Err(ExportError::InvalidJob(format!("frame rate {}/{}", job.rate.num, job.rate.den)));
    }
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    let started = Instant::now();
    let enc = encoder.start(job).map_err(ExportError::Encode)?;
    let resolver = Resolver::new(decoders, req.resolver.clone());
    let mut runner = Runner {
        source: &*req.source,
        job,
        missing: req.missing,
        out: OutputSpec { size: SizeU::new(job.width, job.height), quality: RenderQuality::Export, color: ColorInfo::WORKING_SDR },
        resolver: &resolver,
        renderer: CpuRenderer::new(),
        cancel,
        stats: ExportStats::default(),
        render_times: Vec::with_capacity(job.frames.clamp(0, 1 << 20) as usize),
        out_buffers: 0,
    };
    tracing::info!(output = %job.output.display(), frames = job.frames, width = job.width, height = job.height, rate = %job.rate, "export start");

    let frame_len = job.width as usize * job.height as usize * 4;
    let (outcome, written) = std::thread::scope(|s| {
        // Filled frames to the writer (one queued while one is written) and
        // written buffers back.
        let (to_writer, from_renderer) = bounded::<Vec<u8>>(1);
        let (to_renderer, returned) = bounded::<Vec<u8>>(2);
        let frames = job.frames;
        let writer = std::thread::Builder::new()
            .name("kadr-export-writer".into())
            .spawn_scoped(s, move || write_frames(enc, from_renderer, to_renderer, frames, progress))
            // `enc` was dropped with the closure (its Drop deletes the partial output).
            .map_err(|e| ExportError::Internal(format!("cannot start the writer thread: {e}")))?;
        // Cancel interrupts a wait for decoded frames: the watcher supersedes
        // the resolver's generation, the wait returns `NotReady`.
        let (done, watch) = bounded::<()>(0);
        let resolver = &resolver;
        let watcher = std::thread::Builder::new().name("kadr-export-cancel".into()).spawn_scoped(s, move || loop {
            match watch.recv_timeout(CANCEL_POLL) {
                Err(RecvTimeoutError::Timeout) if cancel.is_cancelled() => return resolver.supersede(u64::MAX),
                Err(RecvTimeoutError::Timeout) => {}
                _ => return,
            }
        });
        if let Err(e) = watcher {
            tracing::warn!(error = %e, "export: no cancel watcher; cancel is checked between frames only");
        }
        let outcome = runner.run(frame_len, &to_writer, &returned);
        drop(done);
        drop(to_writer);
        let written = writer.join().map_err(|_| ExportError::Internal("the writer thread panicked".into()))?;
        Ok::<_, ExportError>((outcome, written))
    })?;
    runner.stats.write_total = written.write_total;
    let enc = written.encoder;

    let failure = match (outcome, written.error) {
        // The writer failed first: its error is the cause (the renderer only saw it go).
        (Err(ExportError::Encode(_)) | Ok(()), Some(e)) => Some(ExportError::Encode(e)),
        (Err(e), _) => Some(e),
        (Ok(()), None) if written.frames != job.frames => Some(ExportError::Internal(format!("{} of {} frames written", written.frames, job.frames))),
        (Ok(()), None) => None,
    };
    if let Some(e) = failure {
        tracing::warn!(output = %job.output.display(), frames = runner.stats.frames, error = %e, "export stopped");
        enc.abort();
        return Err(e);
    }
    let f0 = Instant::now();
    enc.finish().map_err(ExportError::Encode)?;
    progress(1.0);

    let mut stats = runner.stats;
    stats.finish = f0.elapsed();
    stats.elapsed = started.elapsed();
    stats.fps = if stats.elapsed.is_zero() { 0.0 } else { stats.frames as f64 / stats.elapsed.as_secs_f64() };
    stats.render = Stats::of(runner.render_times);
    stats.resolver = resolver.stats();
    stats.allocations = stats.resolver.pool_allocations + runner.renderer.pool_allocations() + runner.out_buffers;
    stats.late_allocations = stats.allocations.saturating_sub(stats.late_allocations);
    tracing::info!(
        output = %job.output.display(),
        frames = stats.frames,
        fps = stats.fps,
        render_p50_ms = stats.render.p50.as_secs_f64() * 1e3,
        decode_wait_ms = stats.decode_wait.as_millis() as u64,
        encoder_wait_ms = stats.encoder_wait.as_millis() as u64,
        "export finished"
    );
    Ok(stats)
}

struct Runner<'a> {
    source: &'a dyn SceneSource,
    job: &'a EncodeJob,
    missing: MissingPolicy,
    out: OutputSpec,
    resolver: &'a Resolver,
    renderer: CpuRenderer,
    cancel: &'a CancelToken,
    /// `late_allocations` holds the allocation count at the half-way frame
    /// until the end, when it becomes the difference.
    stats: ExportStats,
    render_times: Vec<Duration>,
    out_buffers: u64,
}

impl Runner<'_> {
    fn allocations(&self) -> u64 {
        self.resolver.pool().allocations() + self.renderer.pool_allocations() + self.out_buffers
    }

    /// Renders every frame into the writer's channel. Buffers: up to two,
    /// allocated for the first two frames, then taken back from the writer.
    fn run(&mut self, frame_len: usize, to_writer: &Sender<Vec<u8>>, returned: &Receiver<Vec<u8>>) -> Result<(), ExportError> {
        let rate = self.job.rate;
        let n_frames = self.job.frames;
        let (w, h) = (self.job.width, self.job.height);
        let writer_gone = || ExportError::Encode(MediaError::Io(std::io::Error::other("the encoder stopped")));
        for n in 0..n_frames {
            if self.cancel.is_cancelled() {
                return Err(ExportError::Cancelled);
            }
            if n == n_frames / 2 {
                self.stats.late_allocations = self.allocations();
            }
            let t = rate.frame_to_time(n);
            let scene = self.source.scene_at(t, &self.out);
            let ahead = t + PREFETCH_AHEAD;
            if ahead < rate.frame_to_time(n_frames) {
                self.resolver.prefetch(&self.source.scene_at(ahead, &self.out), self.source);
            }

            let mut perf = FramePerf::default();
            let inputs = self.resolver.prepare(&scene, self.source, Mode::Export, &mut perf);
            self.stats.decode_wait += perf.resolve;
            if let Some(e) = self.check_missing(&scene.layers, &inputs.layers, t, n) {
                return Err(e);
            }

            let w0 = Instant::now();
            let mut buf = if self.out_buffers < 2 {
                self.out_buffers += 1;
                vec![0u8; frame_len]
            } else {
                returned.recv().map_err(|_| writer_gone())?
            };
            self.stats.encoder_wait += w0.elapsed();

            let r0 = Instant::now();
            let frame = PreparedFrame { scene: &scene, inputs: &inputs };
            let mut target = RenderTarget::Cpu(CpuTarget::packed(w, h, &mut buf));
            let renderer = &mut self.renderer;
            match std::panic::catch_unwind(AssertUnwindSafe(|| renderer.render(&frame, &mut target))) {
                Ok(Ok(rs)) => {
                    self.stats.layers_drawn += u64::from(rs.layers_drawn);
                    self.stats.fast_paths += u64::from(rs.fast_paths);
                }
                Ok(Err(e)) => return Err(ExportError::Render(e)),
                Err(_) => return Err(ExportError::Internal(format!("the renderer panicked at frame {n}"))),
            }
            let spent = r0.elapsed();
            self.render_times.push(spent);
            self.stats.render_total += spent;
            // Inputs (decoded frames) go back to the cache before the writer may block us.
            drop(inputs);

            let s0 = Instant::now();
            to_writer.send(buf).map_err(|_| writer_gone())?;
            self.stats.encoder_wait += s0.elapsed();
            self.stats.frames = n + 1;
        }
        if n_frames == 0 {
            self.stats.late_allocations = self.allocations();
        }
        Ok(())
    }

    /// `NotReady` never happens in export mode unless cancel interrupted a
    /// wait; offline or undecodable media fails the export unless the
    /// policy draws the placeholder.
    fn check_missing(&self, layers: &[Layer], inputs: &[LayerInput], t: Time, n: i64) -> Option<ExportError> {
        let mut first = None;
        let mut not_ready = false;
        find_missing(layers, inputs, &mut first, &mut not_ready);
        if not_ready {
            return Some(if self.cancel.is_cancelled() {
                ExportError::Cancelled
            } else {
                ExportError::Internal(format!("a layer was not ready at frame {n} in export mode"))
            });
        }
        match (first, self.missing) {
            (Some((media, reason)), MissingPolicy::Fail) => {
                let path = self.source.media(media).map(|m| m.path);
                Some(ExportError::MissingMedia { media, path, reason, time: t, frame: n })
            }
            _ => None,
        }
    }
}

/// The first offline or undecodable media layer (depth first, bottom to
/// top), and whether any layer is `NotReady`. `inputs` is parallel to `layers`.
fn find_missing(layers: &[Layer], inputs: &[LayerInput], first: &mut Option<(AssetId, MissingReason)>, not_ready: &mut bool) {
    for (layer, input) in layers.iter().zip(inputs) {
        match (&layer.content, input) {
            (_, LayerInput::Missing(MissingReason::NotReady)) => *not_ready = true,
            (LayerContent::Media { media, .. }, LayerInput::Missing(reason)) => {
                if first.is_none() {
                    *first = Some((media.media, *reason));
                }
            }
            (LayerContent::Transition(t), LayerInput::Transition { from, to }) => {
                find_missing(&t.from, from, first, not_ready);
                find_missing(&t.to, to, first, not_ready);
            }
            _ => {}
        }
    }
}

struct Written {
    encoder: Box<dyn FrameEncoder>,
    frames: i64,
    error: Option<MediaError>,
    write_total: Duration,
}

/// The writer thread: frames from the renderer into the encoder, buffers
/// back; stops at the first encoder error (dropping its receiver, so the
/// renderer's next send fails) or when the renderer hangs up.
fn write_frames(mut enc: Box<dyn FrameEncoder>, frames: Receiver<Vec<u8>>, back: Sender<Vec<u8>>, total: i64, progress: &(dyn Fn(f32) + Sync)) -> Written {
    let mut write_total = Duration::ZERO;
    let mut reported = (0.0f32, Instant::now());
    let mut written = 0i64;
    for buf in frames.iter() {
        let t0 = Instant::now();
        let r = enc.write_frame(&buf);
        write_total += t0.elapsed();
        if let Err(error) = r {
            return Written { encoder: enc, frames: written, error: Some(error), write_total };
        }
        written += 1;
        // Never blocks: at most two buffers exist and the channel holds two.
        let _ = back.try_send(buf);
        if written < total {
            let p = written as f32 / total as f32;
            if p - reported.0 >= PROGRESS_STEP || reported.1.elapsed() >= PROGRESS_INTERVAL {
                progress(p);
                reported = (p, Instant::now());
            }
        }
    }
    Written { encoder: enc, frames: written, error: None, write_total }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(Time::from_millis(1240)), "0:01.240");
        assert_eq!(clock(Time::from_secs(3 * 3600 + 62)), "3:01:02.000");
    }
}
