//! Test doubles for the playback pipeline (used by this crate's tests, the
//! bench and the app's tests): a fake decoder whose frames say which media
//! and frame they are, and a scene source built from a closure.

use crate::decoders::Decoders;
use crate::source::{MediaSource, SceneSource};
use kadr_core::{AssetId, ColorInfo, FrameRate, Time};
use kadr_media::{MediaError, SourceStream};
use kadr_scene::{BlendMode, FrameScene, Layer, LayerContent, LayerId, MediaRef, OutputSpec, Placement, SizeU, SourceKind, Vec2};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The colour every pixel of fake frame `frame` of media `tag` has: R = tag,
/// G/B = frame index (low/high byte), opaque.
pub fn fake_pixel(tag: u8, frame: i64) -> [u8; 4] {
    [tag, frame as u8, (frame >> 8) as u8, 255]
}

/// Inverse of [`fake_pixel`]: (tag, frame index).
pub fn read_fake_pixel(px: &[u8]) -> (u8, i64) {
    (px[0], px[1] as i64 | (px[2] as i64) << 8)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FakeMedia {
    pub tag: u8,
    /// Frames the stream really has (`None` = endless); may be fewer than the duration says.
    pub frames: Option<i64>,
    /// Opening (and still decoding) fails.
    pub fail: bool,
}

#[derive(Default)]
struct Counts {
    opens: AtomicU64,
    frames: AtomicU64,
    stills: AtomicU64,
    live: AtomicI64,
}

/// [`Decoders`] producing [`fake_pixel`] frames, with configurable open and
/// per-frame delays (sleeps, to simulate slow decoding) and counters.
pub struct FakeDecoders {
    media: Mutex<HashMap<PathBuf, FakeMedia>>,
    delays: Mutex<(Duration, Duration)>,
    counts: Arc<Counts>,
}

impl FakeDecoders {
    pub fn new(open_delay: Duration, frame_delay: Duration) -> Self {
        FakeDecoders { media: Mutex::new(HashMap::new()), delays: Mutex::new((open_delay, frame_delay)), counts: Arc::new(Counts::default()) }
    }

    pub fn add(&self, path: impl Into<PathBuf>, media: FakeMedia) {
        self.media.lock().insert(path.into(), media);
    }

    pub fn set_delays(&self, open: Duration, frame: Duration) {
        *self.delays.lock() = (open, frame);
    }

    /// Streams opened so far.
    pub fn opens(&self) -> u64 {
        self.counts.opens.load(Ordering::SeqCst)
    }

    /// Frames read from streams so far.
    pub fn frames(&self) -> u64 {
        self.counts.frames.load(Ordering::SeqCst)
    }

    pub fn stills(&self) -> u64 {
        self.counts.stills.load(Ordering::SeqCst)
    }

    /// Streams open right now (not yet dropped).
    pub fn live_streams(&self) -> i64 {
        self.counts.live.load(Ordering::SeqCst)
    }

    fn get(&self, path: &Path) -> Result<FakeMedia, MediaError> {
        match self.media.lock().get(path) {
            Some(m) if !m.fail => Ok(*m),
            Some(_) => Err(MediaError::Unsupported(format!("{}: fake failure", path.display()))),
            None => Err(MediaError::Unsupported(format!("{}: unknown fake media", path.display()))),
        }
    }
}

impl Decoders for FakeDecoders {
    fn open(&self, media: &MediaSource, start_frame: i64, _size: SizeU) -> Result<Box<dyn SourceStream>, MediaError> {
        let (open, frame) = *self.delays.lock();
        std::thread::sleep(open);
        let m = self.get(&media.path)?;
        self.counts.opens.fetch_add(1, Ordering::SeqCst);
        self.counts.live.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeStream { tag: m.tag, next: start_frame, end: m.frames, delay: frame, counts: self.counts.clone() }))
    }

    fn still(&self, media: &MediaSource, _size: SizeU, out: &mut [u8]) -> Result<(), MediaError> {
        let (open, _) = *self.delays.lock();
        std::thread::sleep(open);
        let m = self.get(&media.path)?;
        self.counts.stills.fetch_add(1, Ordering::SeqCst);
        fill(out, fake_pixel(m.tag, 0));
        Ok(())
    }
}

fn fill(out: &mut [u8], px: [u8; 4]) {
    for p in out.as_chunks_mut::<4>().0 {
        *p = px;
    }
}

struct FakeStream {
    tag: u8,
    next: i64,
    end: Option<i64>,
    delay: Duration,
    counts: Arc<Counts>,
}

impl SourceStream for FakeStream {
    fn read_into(&mut self, buf: &mut [u8]) -> Result<bool, MediaError> {
        std::thread::sleep(self.delay);
        if self.end.is_some_and(|e| self.next >= e) {
            return Ok(false);
        }
        fill(buf, fake_pixel(self.tag, self.next));
        self.next += 1;
        self.counts.frames.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.counts.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A video [`MediaSource`] for tests: `path`, `rate`, `duration`, display
/// size, opaque Rec.709.
pub fn video_source(path: &str, rate: FrameRate, duration: Time, display: SizeU) -> MediaSource {
    MediaSource { path: path.into(), kind: SourceKind::Video, rate, duration, display_size: display, color: ColorInfo::guess_video(1920, 1080), online: true }
}

/// Content of `display` size fitted (contain) into the canvas, centred.
pub fn fit_placement(display: SizeU, canvas: SizeU) -> Placement {
    let s = (canvas.w as f32 / display.w.max(1) as f32).min(canvas.h as f32 / display.h.max(1) as f32);
    let size = Vec2::new(display.w as f32 * s, display.h as f32 * s);
    Placement { size, anchor: Vec2::new(0.5, 0.5), position: Vec2::new(canvas.w as f32 / 2.0, canvas.h as f32 / 2.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 }
}

/// A media layer showing `media` at `source_time`, fitted into the canvas.
pub fn media_layer(id: u128, asset: AssetId, media: &MediaSource, source_time: Time, canvas: SizeU) -> Layer {
    let placement = fit_placement(media.display_size, canvas);
    Layer {
        id: LayerId(id),
        content: LayerContent::Media {
            media: MediaRef { media: asset, stream: 0, kind: media.kind, display_size: media.display_size, color: media.color },
            source_time,
        },
        crop: placement.full_crop(),
        placement,
        opacity: 1.0,
        blend: BlendMode::Normal,
        effects: vec![],
    }
}

/// A [`SceneSource`] from a closure and a media table.
pub struct FnSceneSource<F> {
    pub media: HashMap<AssetId, MediaSource>,
    pub rate: FrameRate,
    pub duration: Time,
    pub canvas: SizeU,
    pub scene: F,
}

impl<F: Fn(Time, &OutputSpec) -> FrameScene + Send + Sync> SceneSource for FnSceneSource<F> {
    fn scene_at(&self, t: Time, out: &OutputSpec) -> FrameScene {
        (self.scene)(t, out)
    }
    fn media(&self, id: AssetId) -> Option<MediaSource> {
        self.media.get(&id).cloned()
    }
    fn duration(&self) -> Time {
        self.duration
    }
    fn frame_rate(&self) -> FrameRate {
        self.rate
    }
    fn canvas(&self) -> SizeU {
        self.canvas
    }
}
