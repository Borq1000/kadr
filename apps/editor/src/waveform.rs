//! Waveform strips rendered on the CPU into small images, only for the
//! visible part of each audio clip, cached until zoom/scroll/gain change.

use kadr_analysis::AudioOverview;
use kadr_core::ClipId;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::collections::HashMap;

#[derive(Default)]
pub struct WaveCache {
    entries: HashMap<ClipId, (Key, Image)>,
}

#[derive(PartialEq, Clone, Copy)]
struct Key {
    s0: i64,
    s1: i64,
    w: u32,
    h: u32,
    gain: i32,
    ov: usize,
}

impl WaveCache {
    /// Image of `[s0, s1)` source seconds at `w×h` px.
    pub fn get(&mut self, clip: ClipId, ov: &AudioOverview, s0: f64, s1: f64, w: u32, h: u32, gain_db: f64) -> Image {
        let w = w.clamp(1, 4096);
        let h = h.clamp(4, 400);
        let key = Key {
            s0: (s0 * 1000.0) as i64,
            s1: (s1 * 1000.0) as i64,
            w,
            h,
            gain: (gain_db * 10.0) as i32,
            ov: ov as *const _ as usize,
        };
        if let Some((k, img)) = self.entries.get(&clip) {
            if *k == key {
                return img.clone();
            }
        }
        let img = render(ov, s0, s1, w, h, gain_db);
        if self.entries.len() > 512 {
            self.entries.clear();
        }
        self.entries.insert(clip, (key, img.clone()));
        img
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

fn render(ov: &AudioOverview, s0: f64, s1: f64, w: u32, h: u32, gain_db: f64) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let gain = 10f64.powf(gain_db / 20.0).sqrt() as f32; // peaks are sqrt-scaled
    let per = (s1 - s0) / w as f64;
    let mid = h as f32 / 2.0;
    let px = buf.make_mut_slice();
    for x in 0..w {
        let t0 = s0 + x as f64 * per;
        let p = (ov.peak_in(t0, t0 + per) * gain).min(1.0);
        let half = (p * mid).max(0.5);
        let (top, bot) = ((mid - half).floor().max(0.0) as u32, ((mid + half).ceil() as u32).min(h));
        for y in top..bot {
            // Premultiplied mint, brighter towards the centre line.
            let a = if y == top || y + 1 == bot { 150 } else { 220 };
            let f = a as f32 / 255.0;
            px[(y * w + x) as usize] = Rgba8Pixel { r: (159.0 * f) as u8, g: (227.0 * f) as u8, b: (199.0 * f) as u8, a };
        }
    }
    Image::from_rgba8_premultiplied(buf)
}
