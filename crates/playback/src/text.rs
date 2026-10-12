//! Rasters of text layers (titles spec §5.3): drawn by the layer's own
//! picture at the scale the output needs, kept by (key, scale step) in an
//! LRU by bytes. A static text is drawn once; an animated one once per
//! frame of its animation. Drawing runs on the calling thread (the player
//! or the export), synchronously: a text is never "not ready".

use kadr_core::{CpuFrame, FramePool};
use kadr_scene::{Placement, SizeU, TextPicture};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// The largest raster side: a title magnified past it is sampled up from this.
pub const MAX_TEXT_SIDE: f32 = 8192.0;
/// Scale steps per doubling: a raster is at most 2^(1/8) ≈ 9 % finer than the output needs.
const STEPS_PER_OCTAVE: f32 = 8.0;

/// Raster px per canvas px of a step.
pub fn step_scale(step: i32) -> f32 {
    (step as f32 / STEPS_PER_OCTAVE).exp2()
}

/// The scale step of a text layer's raster: output px per canvas px times
/// the layer's larger |scale|, rounded up to a step; lowered until the
/// box's longer side fits [`MAX_TEXT_SIDE`]. A non-finite or zero scale
/// counts as 1; steps stay within ±64 (2⁻⁸ … 2⁸).
pub fn text_scale_step(placement: &Placement, canvas: SizeU, output: SizeU) -> i32 {
    let k = if canvas.w == 0 || canvas.h == 0 { 1.0 } else { (output.w as f32 / canvas.w as f32).min(output.h as f32 / canvas.h as f32) };
    let s = placement.scale.x.abs().max(placement.scale.y.abs());
    let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
    let mut step = ((k * s).log2() * STEPS_PER_OCTAVE - 1e-4).ceil() as i32;
    let side = placement.size.x.abs().max(placement.size.y.abs()).max(1.0);
    while step > -64 && side * step_scale(step) > MAX_TEXT_SIDE {
        step -= 1;
    }
    step.clamp(-64, 64)
}

struct Inner {
    map: HashMap<(u64, i32), (Arc<CpuFrame>, u64)>,
    bytes: usize,
    clock: u64,
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
        TextRasters { pool, max_bytes, inner: Mutex::new(Inner { map: HashMap::new(), bytes: 0, clock: 0, drawn: 0, hits: 0 }) }
    }

    /// The raster of `picture` at `step`: cached, or drawn now.
    pub fn get(&self, picture: &dyn TextPicture, step: i32) -> Arc<CpuFrame> {
        let key = (picture.key(), step);
        {
            let mut g = self.inner.lock();
            g.clock += 1;
            let now = g.clock;
            if let Some((f, used)) = g.map.get_mut(&key) {
                *used = now;
                let f = f.clone();
                g.hits += 1;
                return f;
            }
        }
        let frame = Arc::new(picture.render(step_scale(step), &self.pool));
        let mut g = self.inner.lock();
        g.drawn += 1;
        let now = g.clock;
        g.bytes += frame.byte_len();
        if let Some((old, _)) = g.map.insert(key, (frame.clone(), now)) {
            g.bytes -= old.byte_len();
        }
        while g.bytes > self.max_bytes && g.map.len() > 1 {
            let Some(oldest) = g.map.iter().filter(|(k, _)| **k != key).min_by_key(|(_, (_, used))| *used).map(|(k, _)| *k) else { break };
            if let Some((f, _)) = g.map.remove(&oldest) {
                g.bytes -= f.byte_len();
            }
        }
        frame
    }

    /// (rasters drawn, cache hits) since the start.
    pub fn stats(&self) -> (u64, u64) {
        let g = self.inner.lock();
        (g.drawn, g.hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_scene::Vec2;

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
}
