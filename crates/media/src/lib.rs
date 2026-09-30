//! Media engine abstraction. Business logic talks to [`MediaBackend`]; the
//! current implementation drives the FFmpeg CLI ([`ffmpeg::FfmpegCli`]), and
//! an in-process libav backend can replace it without touching callers.

pub mod encode;
pub mod export;
pub mod ffmpeg;
pub mod stats;

use kadr_core::{CancelToken, ColorInfo, FrameRate, MediaInfo, Time};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub use encode::{EncodeJob, FrameEncoder};
pub use export::{ExportAudio, ExportSettings};

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("FFmpeg not found (looked for {0}). Install FFmpeg or set KADR_FFMPEG_DIR.")]
    BackendMissing(String),
    #[error("cannot start {tool}: {source}")]
    Spawn { tool: String, source: std::io::Error },
    #[error("{tool} failed ({status}): {stderr}")]
    ToolFailed { tool: String, status: String, stderr: String },
    #[error("unsupported or unreadable media: {0}")]
    Unsupported(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, MediaError>;

/// Tightly packed RGBA8 image (straight alpha, alpha = 255 for video).
#[derive(Clone)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for RgbaFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RgbaFrame({}x{})", self.width, self.height)
    }
}

impl RgbaFrame {
    pub fn black(width: u32, height: u32) -> Self {
        let mut data = vec![0u8; (width * height * 4) as usize];
        stats::note_frame_alloc();
        data.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
        RgbaFrame { width, height, data }
    }
}

/// Sequential decoder for playback: yields frames at a fixed output rate.
pub trait VideoStream: Send {
    /// Next frame, `Ok(None)` at end of stream.
    fn next_frame(&mut self) -> Result<Option<RgbaFrame>>;
}

/// Parameters for a streaming decode.
#[derive(Clone, Debug)]
pub struct StreamRequest {
    pub path: PathBuf,
    pub start: Time,
    /// Output frame size (the stream letterboxes into it).
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    /// Source seconds per output second.
    pub speed: f64,
}

/// A decode of a source's own frames (render spec §4.2, §9): no
/// composition, no letterbox — the resolver's input.
#[derive(Clone, Debug)]
pub struct SourceRequest {
    pub path: PathBuf,
    /// Constant rate the stream is normalised to (the source's own rate).
    pub rate: FrameRate,
    /// Frame `n` read from the stream is source frame `start_frame + n`;
    /// source frame `k` is the one shown at `rate.frame_to_time(k)` (a frame
    /// stamped up to a quarter frame later still counts, for container
    /// timestamp rounding). Must not be negative.
    pub start_frame: i64,
    /// Exact output size, upright (rotation metadata applied) with the
    /// sample aspect ratio absorbed.
    pub width: u32,
    pub height: u32,
    /// The source's colour: YUV → R'G'B' uses exactly this matrix and range.
    pub color: ColorInfo,
    /// Hardware decoding (slower on the CLI path, see the M3 plan).
    pub hwaccel: bool,
}

/// Sequential source frames as RGBA8 (full-range non-linear R'G'B'; alpha
/// straight, or 255 for opaque sources).
pub trait SourceStream: Send {
    /// Reads the next frame into `buf` (`width × height × 4` bytes);
    /// `Ok(false)` at the end of the stream.
    fn read_into(&mut self, buf: &mut [u8]) -> Result<bool>;
}

pub type Progress<'a> = &'a (dyn Fn(f32) + Sync);

pub trait MediaBackend: Send + Sync {
    fn name(&self) -> &str;
    fn probe(&self, path: &Path) -> Result<MediaInfo>;
    /// Exact frame at source time `at`, scaled to fit `max_w × max_h`.
    fn decode_frame(&self, path: &Path, at: Time, max_w: u32, max_h: u32) -> Result<RgbaFrame>;
    fn open_stream(&self, req: &StreamRequest) -> Result<Box<dyn VideoStream>>;
    /// Source frames from `req.start_frame` on (see [`SourceRequest`]).
    fn open_source(&self, req: &SourceRequest) -> Result<Box<dyn SourceStream>>;
    /// A still image scaled to exactly `width × height`, RGBA8 straight
    /// alpha, into `out` (`width × height × 4` bytes).
    fn decode_still(&self, path: &Path, width: u32, height: u32, out: &mut [u8]) -> Result<()>;
    /// Fast, keyframe-accurate thumbnails at the given source times.
    fn thumbnails(&self, path: &Path, times: &[Time], height: u32, cancel: &CancelToken) -> Result<Vec<RgbaFrame>>;
    /// Decodes the audio to interleaved s16le PCM at `rate`/`channels` into `out`.
    #[allow(clippy::too_many_arguments)]
    fn extract_pcm(
        &self,
        path: &Path,
        out: &Path,
        rate: u32,
        channels: u32,
        duration: Time,
        progress: Progress,
        cancel: &CancelToken,
    ) -> Result<()>;
    /// Starts encoding rendered RGBA frames plus the job's audio into
    /// `job.output` (see [`EncodeJob`]).
    fn start_encode(&self, job: &EncodeJob) -> Result<Box<dyn FrameEncoder>>;
}
