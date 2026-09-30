//! Where decoder sessions get their streams from. A trait so tests (and the
//! bench) can run the whole pipeline on a fake with controlled timing.

use crate::source::MediaSource;
use kadr_media::{MediaBackend, MediaError, SourceRequest, SourceStream};
use kadr_scene::SizeU;
use std::sync::Arc;

/// Opens source streams and decodes stills. Called on decoder-session
/// threads, possibly several at once.
pub trait Decoders: Send + Sync {
    /// Frames of `media` at exactly `size`, from source frame `start_frame`
    /// on (frame `n` read is source frame `start_frame + n`). Dropping the
    /// stream must release the decoder (kill the process).
    fn open(&self, media: &MediaSource, start_frame: i64, size: SizeU) -> Result<Box<dyn SourceStream>, MediaError>;
    /// A still image at exactly `size`, RGBA8 straight alpha, into `out`.
    fn still(&self, media: &MediaSource, size: SizeU, out: &mut [u8]) -> Result<(), MediaError>;
}

/// [`Decoders`] over a [`MediaBackend`] (FFmpeg CLI today). Software
/// decoding by default: on the CLI path `-hwaccel` is slower (M3 plan).
pub struct FfmpegDecoders {
    backend: Arc<dyn MediaBackend>,
    hwaccel: bool,
}

impl FfmpegDecoders {
    pub fn new(backend: Arc<dyn MediaBackend>) -> Self {
        FfmpegDecoders { backend, hwaccel: false }
    }

    pub fn with_hwaccel(mut self, on: bool) -> Self {
        self.hwaccel = on;
        self
    }
}

impl Decoders for FfmpegDecoders {
    fn open(&self, media: &MediaSource, start_frame: i64, size: SizeU) -> Result<Box<dyn SourceStream>, MediaError> {
        self.backend.open_source(&SourceRequest {
            path: media.path.clone(),
            rate: media.rate,
            start_frame,
            width: size.w,
            height: size.h,
            color: media.color,
            hwaccel: self.hwaccel,
        })
    }

    fn still(&self, media: &MediaSource, size: SizeU, out: &mut [u8]) -> Result<(), MediaError> {
        self.backend.decode_still(&media.path, size.w, size.h, out)
    }
}
