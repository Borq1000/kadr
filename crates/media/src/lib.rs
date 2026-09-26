//! Media engine abstraction. Business logic talks to [`MediaBackend`]; the
//! current implementation drives the FFmpeg CLI ([`ffmpeg::FfmpegCli`]), and
//! an in-process libav backend can replace it without touching callers.

pub mod export;
pub mod ffmpeg;

use kadr_core::{CancelToken, FrameRate, MediaInfo, Time};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub use export::{ExportAudio, ExportPlan, ExportSettings, ExportTransition, ExportTransitionKind, ExportVideo};

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
        data.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        RgbaFrame { width, height, data }
    }
}

/// Output size fitting `(w, h)` inside `max_w × max_h`, even dimensions.
pub fn fit_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (max_w & !1, max_h & !1);
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    let ow = ((w as f64 * s).round() as u32).max(2) & !1;
    let oh = ((h as f64 * s).round() as u32).max(2) & !1;
    (ow, oh)
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
    /// Clip look, applied with the same filter as export (WYSIWYG).
    pub look: export::VideoLook,
    /// Sequence-pixel → output-pixel factor for positional transforms.
    pub px_scale: f64,
}

pub type Progress<'a> = &'a (dyn Fn(f32) + Sync);

pub trait MediaBackend: Send + Sync {
    fn name(&self) -> &str;
    fn probe(&self, path: &Path) -> Result<MediaInfo>;
    /// Exact frame at source time `at`, scaled to fit `max_w × max_h`.
    fn decode_frame(&self, path: &Path, at: Time, max_w: u32, max_h: u32) -> Result<RgbaFrame>;
    fn open_stream(&self, req: &StreamRequest) -> Result<Box<dyn VideoStream>>;
    /// Fast, keyframe-accurate thumbnails at the given source times.
    fn thumbnails(&self, path: &Path, times: &[Time], height: u32, cancel: &CancelToken) -> Result<Vec<RgbaFrame>>;
    /// Decodes the audio to interleaved s16le PCM at `rate`/`channels` into `out`.
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
    fn export(&self, plan: &ExportPlan, progress: Progress, cancel: &CancelToken) -> Result<()>;
}
