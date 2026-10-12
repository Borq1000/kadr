//! Rasters of text layers (titles spec §5.3): drawn by the layer's own
//! picture at the scale the output needs, kept by (key, scale step) in an
//! LRU by bytes. A static text is drawn once; an animated one once per
//! frame of its animation. Drawing runs on the calling thread (the player
//! or the export), synchronously: a text is never "not ready".
//!
//! A picture that panics while drawing is isolated: its (key, step) is
//! remembered as failed (logged once, never drawn again) and the layer gets
//! no raster (`Missing(DecodeFailed)`: it draws nothing; export reports it).

use kadr_core::{CpuFrame, FramePool};
use kadr_scene::{Placement, SizeU, TextPicture};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// The largest raster side: a title magnified past it is sampled up from this.
pub const MAX_TEXT_SIDE: f32 = 8192.0;
/// The largest raster area (4096², 64 MiB of RGBA8): a wide and tall box is
/// drawn coarser rather than allocating hundreds of megabytes.
pub const MAX_TEXT_PIXELS: u64 = 1 << 24;
/// Scale steps per doubling: a raster is at most 2^(1/8) ≈ 9 % finer than the output needs.
const STEPS_PER_OCTAVE: f32 = 8.0;
/// Failed (key, step) pairs remembered; past this the set starts over.
const MAX_FAILED: usize = 4096;

/// Raster px per canvas px of a step.
pub fn step_scale(step: i32) -> f32 {
    (step as f32 / STEPS_PER_OCTAVE).exp2()
}

/// The raster a picture of box `size` draws at `scale`:
/// `max(1, ⌈|size| · scale⌉)` px each way (the `TextPicture` contract).
fn raster_pixels(w: f32, h: f32, scale: f32) -> u64 {
    let side = |v: f32| ((v.abs() * scale).ceil() as u64).max(1);
    side(w).saturating_mul(side(h))
}

/// The scale step of a text layer's raster: output px per canvas px times
/// the layer's larger |scale|, rounded up to a step; lowered until the
/// box's longer side fits [`MAX_TEXT_SIDE`] and its raster fits
/// [`MAX_TEXT_PIXELS`]. A non-finite or zero scale counts as 1; steps stay
/// within ±64 (2⁻⁸ … 2⁸).
pub fn text_scale_step(placement: &Placement, canvas: SizeU, output: SizeU) -> i32 {
    let k = if canvas.w == 0 || canvas.h == 0 { 1.0 } else { (output.w as f32 / canvas.w as f32).min(output.h as f32 / canvas.h as f32) };
    let s = placement.scale.x.abs().max(placement.scale.y.abs());
    let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
    let mut step = (((k * s).log2() * STEPS_PER_OCTAVE - 1e-4).ceil() as i32).clamp(-64, 64);
    let (w, h) = (placement.size.x, placement.size.y);
    let side = w.abs().max(h.abs()).max(1.0);
    while step > -64 && (side * step_scale(step) > MAX_TEXT_SIDE || raster_pixels(w, h, step_scale(step)) > MAX_TEXT_PIXELS) {
        step -= 1;
    }
    step
}

struct Entry {
    frame: Arc<CpuFrame>,
    /// LRU clock of the last use.
    used: u64,
    /// The prepared frame that last used it ([`TextRasters::begin`]).
    stamp: u64,
}

#[derive(Default)]
struct Inner {
    map: HashMap<(u64, i32), Entry>,
    failed: HashSet<(u64, i32)>,
    bytes: usize,
    clock: u64,
    stamp: u64,
    drawn: u64,
    hits: u64,
}

/// The text raster cache of one resolver.
pub struct TextRasters {
    pool: FramePool,
    max_bytes: usize,
    inner: Mutex<Inner>,
}

impl TextRasters {
    pub fn new(pool: FramePool, max_bytes: usize) -> Self {
        TextRasters { pool, max_bytes, inner: Mutex::new(Inner::default()) }
    }

    /// A new stamp for one prepared frame: rasters got with it are not
    /// evicted for one another (a frame never loses its own rasters, so a
    /// working set over the budget does not redraw every frame); the cache
    /// is brought back under its budget by later frames.
    pub fn begin(&self) -> u64 {
        let mut g = self.inner.lock();
        g.stamp += 1;
        g.stamp
    }

    /// The raster of `picture` at `step`: cached, or drawn now. `None`
    /// when the picture panicked drawing it (now or before).
    pub fn get(&self, picture: &dyn TextPicture, step: i32, stamp: u64) -> Option<Arc<CpuFrame>> {
        let key = (picture.key(), step);
        {
            let mut g = self.inner.lock();
            if g.failed.contains(&key) {
                return None;
            }
            g.clock += 1;
            let now = g.clock;
            if let Some(e) = g.map.get_mut(&key) {
                e.used = now;
                e.stamp = stamp;
                let f = e.frame.clone();
                g.hits += 1;
                return Some(f);
            }
        }
        // Drawn without the lock: other layers and threads go on meanwhile.
        let drawn = catch_unwind(AssertUnwindSafe(|| picture.render(step_scale(step), &self.pool)));
        let mut g = self.inner.lock();
        let frame = match drawn {
            Ok(f) => Arc::new(f),
            Err(panic) => {
                if g.failed.len() >= MAX_FAILED {
                    g.failed.clear();
                }
                if g.failed.insert(key) {
                    let why = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_default();
                    tracing::error!(key = key.0, step, why, "a text picture panicked while drawing; the layer draws nothing");
                }
                return None;
            }
        };
        g.drawn += 1;
        g.clock += 1;
        let entry = Entry { frame: frame.clone(), used: g.clock, stamp };
        g.bytes += frame.byte_len();
        if let Some(old) = g.map.insert(key, entry) {
            g.bytes -= old.frame.byte_len();
        }
        while g.bytes > self.max_bytes {
            let Some(oldest) = g.map.iter().filter(|(_, e)| e.stamp != stamp).min_by_key(|(_, e)| e.used).map(|(k, _)| *k) else { break };
            if let Some(e) = g.map.remove(&oldest) {
                g.bytes -= e.frame.byte_len();
            }
        }
        Some(frame)
    }

    /// (rasters drawn, cache hits) since the start.
    pub fn stats(&self) -> (u64, u64) {
        let g = self.inner.lock();
        (g.drawn, g.hits)
    }

    /// (rasters cached, their bytes).
    pub fn cached(&self) -> (usize, usize) {
        let g = self.inner.lock();
        (g.map.len(), g.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::ColorInfo;
    use kadr_scene::Vec2;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn placed(w: f32, h: f32, scale: f32) -> Placement {
        Placement { size: Vec2::new(w, h), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(0.0, 0.0), scale: Vec2::new(scale, -scale), rotation: 0.3 }
    }

    #[test]
    fn the_step_is_the_output_footprint_rounded_up() {
        let hd = SizeU::new(1920, 1080);
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 1.0), hd, hd), 0);
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 1.0), hd, SizeU::new(960, 540)), -8, "half quality: half scale");
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 1.0), hd, SizeU::new(3840, 2160)), 8, "a 4K export: twice");
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 3.0), hd, hd), 13, "3× rounds up to 2^(13/8) ≈ 3.08 (a mirrored scale counts by size)");
        assert_eq!(step_scale(13), (13.0f32 / 8.0).exp2());
        let wide = text_scale_step(&placed(5000.0, 40.0, 1.0), hd, SizeU::new(3840, 2160));
        assert!(5000.0 * step_scale(wide) <= MAX_TEXT_SIDE && 5000.0 * step_scale(wide + 1) > MAX_TEXT_SIDE, "the largest step that fits 8192 px: {wide}");
        assert_eq!(text_scale_step(&placed(800.0, 120.0, f32::NAN), hd, hd), 0, "a non-finite scale counts as 1");
    }

    /// TX-R1: a box wide and tall enough is capped by area, not only by side.
    #[test]
    fn a_large_box_is_capped_at_16_mpx() {
        let (hd, uhd) = (SizeU::new(1920, 1080), SizeU::new(3840, 2160));
        let step = text_scale_step(&placed(1700.0, 850.0, 2.4), hd, uhd);
        let pixels = |step: i32| raster_pixels(1700.0, 850.0, step_scale(step));
        assert!(pixels(step) <= MAX_TEXT_PIXELS, "step {step}: {} px", pixels(step));
        assert!(pixels(step + 1) > MAX_TEXT_PIXELS, "the largest step that fits: {step}");
        assert!(1700.0 * step_scale(step + 1) <= MAX_TEXT_SIDE, "the area, not the side, decided");
        assert!(step < 19, "below the footprint's step (4.8× → 19): {step}");
    }

    /// Draws a `w × h` raster per call, or panics.
    #[derive(Debug)]
    struct Pic {
        key: u64,
        w: u32,
        h: u32,
        panics: bool,
        calls: AtomicU32,
    }

    impl Pic {
        fn new(key: u64, w: u32, h: u32) -> Pic {
            Pic { key, w, h, panics: false, calls: AtomicU32::new(0) }
        }
    }

    impl TextPicture for Pic {
        fn key(&self) -> u64 {
            self.key
        }
        fn render(&self, _: f32, _: &FramePool) -> CpuFrame {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert!(!self.panics, "a broken picture");
            CpuFrame::from_rgba8(self.w, self.h, ColorInfo::WORKING_SDR, vec![0; (self.w * self.h * 4) as usize])
        }
    }

    #[test]
    fn rasters_are_counted_by_bytes_and_the_least_recent_of_older_frames_goes_first() {
        // Rasters of 400 bytes; room for three.
        let cache = TextRasters::new(FramePool::new(0), 1200);
        let pics: Vec<Pic> = (0..5).map(|k| Pic::new(k, 10, 10)).collect();
        for p in &pics[..3] {
            let s = cache.begin();
            assert_eq!(cache.get(p, 0, s).map(|f| f.byte_len()), Some(400));
        }
        assert_eq!(cache.cached(), (3, 1200));
        let s = cache.begin();
        cache.get(&pics[0], 0, s).unwrap(); // a hit: 0 is now the most recent
        cache.get(&pics[3], 0, s).unwrap();
        assert_eq!(cache.cached(), (3, 1200), "one evicted for the new one");
        let s = cache.begin();
        for p in [&pics[0], &pics[2], &pics[3]] {
            cache.get(p, 0, s).unwrap();
        }
        assert_eq!(pics[1].calls.load(Ordering::Relaxed), 1);
        assert_eq!((pics[0].calls.load(Ordering::Relaxed), pics[2].calls.load(Ordering::Relaxed)), (1, 1), "1 was the least recent, not 0 or 2");
        assert_eq!(cache.stats(), (4, 4));
        // Another step of the same picture is another raster.
        let s = cache.begin();
        assert_eq!(cache.get(&pics[0], 8, s).map(|f| f.byte_len()), Some(400));
        assert_eq!(pics[0].calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.cached(), (3, 1200));
    }

    #[test]
    fn a_frame_never_evicts_its_own_rasters() {
        let cache = TextRasters::new(FramePool::new(0), 1000);
        let pics: Vec<Pic> = (0..4).map(|k| Pic::new(k, 10, 10)).collect();
        let s = cache.begin();
        for p in &pics {
            cache.get(p, 0, s).unwrap();
        }
        assert_eq!(cache.cached(), (4, 1600), "over budget for this frame");
        let s = cache.begin();
        for p in &pics {
            cache.get(p, 0, s).unwrap();
        }
        assert!(pics.iter().all(|p| p.calls.load(Ordering::Relaxed) == 1), "the same frame again draws nothing");
        let s = cache.begin();
        cache.get(&Pic::new(9, 10, 10), 0, s).unwrap();
        assert_eq!(cache.cached(), (2, 800), "a later frame brings it back under budget");
    }

    /// TX-R2: a panicking picture is drawn once, then known as failed.
    #[test]
    fn a_panicking_picture_gives_no_raster_and_is_not_drawn_again() {
        let cache = TextRasters::new(FramePool::new(0), 1 << 20);
        let bad = Pic { panics: true, ..Pic::new(3, 4, 4) };
        for _ in 0..3 {
            let s = cache.begin();
            assert!(cache.get(&bad, 0, s).is_none());
        }
        assert_eq!(bad.calls.load(Ordering::Relaxed), 1, "never re-rendered");
        assert_eq!(cache.cached(), (0, 0));
        let good = Pic::new(4, 4, 4);
        let s = cache.begin();
        assert!(cache.get(&good, 0, s).is_some(), "other pictures are unaffected");
    }
}
