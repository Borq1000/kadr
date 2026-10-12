//! A text clip's picture at one instant (titles spec §5.1–5.2): the
//! document, its layout, the glyphs' places and animated state, the bounds
//! the evaluator sizes the layer by, and the key the resolver caches by.

use crate::anim::{Clock, FrameAnim};
use crate::layout::Laid;
use crate::shape::Placed;
use kadr_core::{CpuFrame, FramePool};
use kadr_project::TextDoc;
use std::sync::Arc;

/// A rectangle in canvas px about the layout box centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Bounds {
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
    pub fn grow(self, m: f32) -> Bounds {
        Bounds { x0: self.x0 - m, y0: self.y0 - m, x1: self.x1 + m, y1: self.y1 + m }
    }
    pub fn union(self, o: Bounds) -> Bounds {
        Bounds { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    pub fn add_point(&mut self, x: f32, y: f32) {
        if x.is_finite() && y.is_finite() {
            *self = self.union(Bounds { x0: x, y0: y, x1: x, y1: y });
        }
    }
}

#[derive(Debug)]
pub struct TextFrame {
    doc: TextDoc,
    laid: Arc<Laid>,
    placed: Vec<Placed>,
    anim: FrameAnim,
    bounds: Bounds,
    key: u64,
}

impl TextFrame {
    pub fn new(doc: &TextDoc, clock: Clock) -> TextFrame {
        let laid = crate::layout::layout(doc);
        let placed = crate::shape::place(doc, &laid);
        let anim = crate::anim::animate(doc, &laid, clock);
        let finals = crate::raster::finals(&laid, &placed, &anim);
        let bounds = crate::raster::bounds(doc, &laid, &finals);
        let key = crate::hash_of(&(doc, anim.key(), crate::fonts::fonts().generation()));
        TextFrame { doc: doc.clone(), laid, placed, anim, bounds, key }
    }

    /// What the picture may cover, canvas px about the layout box centre.
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// The text block (what the preview's handles frame), canvas px.
    pub fn layout_size(&self) -> (f32, f32) {
        (self.laid.width, self.laid.height)
    }

    pub fn missing_fonts(&self) -> &[String] {
        &self.laid.missing
    }

    pub fn laid(&self) -> &Laid {
        &self.laid
    }

    pub fn anim(&self) -> &FrameAnim {
        &self.anim
    }

    /// The raster at `scale`, not pooled (tests, thumbnails).
    pub fn raster(&self, scale: f32) -> CpuFrame {
        kadr_scene::TextPicture::render(self, scale, &FramePool::new(0))
    }
}

/// The block the preview's handles frame (canvas px, centred on the
/// layout box centre): the layout box (Task 9 widens it to a curved or
/// tilted text's extent).
pub fn frame_size(doc: &TextDoc) -> (f32, f32) {
    let l = crate::layout::layout(doc);
    (l.width, l.height)
}

impl kadr_scene::TextPicture for TextFrame {
    fn key(&self) -> u64 {
        self.key
    }

    /// `⌈bounds · scale⌉` px with the bounds filling it (TX-R3).
    fn render(&self, scale: f32, pool: &FramePool) -> CpuFrame {
        crate::raster::render(&self.doc, &self.laid, &self.placed, &self.anim, self.bounds, scale, pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{Outline, Plaque, Rgba8, TextDoc};

    fn styled() -> TextDoc {
        let mut d = TextDoc::starter("Йога\nкурсивом", 1080);
        d.style.font.italic = true;
        d.style.outlines = vec![Outline { width: 6.0, colour: Rgba8::BLACK }, Outline { width: 4.0, colour: Rgba8::rgba(255, 0, 0, 255) }];
        d.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::rgba(0, 0, 0, 160), radius: 10.0, padding_x: 20.0, padding_y: 8.0 });
        d
    }

    fn alpha_at(f: &kadr_core::CpuFrame, x: u32, y: u32) -> u8 {
        f.row(y)[x as usize * 4 + 3]
    }

    #[test]
    fn bounds_hold_everything_drawn() {
        for d in [styled(), TextDoc::starter("🎉 Гол! ⚽", 1080), TextDoc::starter("", 1080)] {
            let f = TextFrame::new(&d, Clock::REST);
            let r = f.raster(1.0);
            let b = f.bounds();
            assert_eq!((r.width, r.height), (b.width().ceil() as u32, b.height().ceil() as u32));
            let (w, h) = (r.width, r.height);
            let edge = (0..w).all(|x| alpha_at(&r, x, 0) == 0 && alpha_at(&r, x, h - 1) == 0) && (0..h).all(|y| alpha_at(&r, 0, y) == 0 && alpha_at(&r, w - 1, y) == 0);
            assert!(edge, "{:?}: something touches the raster's edge", d.plain());
            let (lw, lh) = f.layout_size();
            assert!(b.x0 <= -lw / 2.0 && b.x1 >= lw / 2.0 && b.y0 <= -lh / 2.0 && b.y1 >= lh / 2.0, "the layout box is inside");
        }
    }

    #[test]
    fn a_raster_is_its_box_at_its_scale_and_premultiplied() {
        let f = TextFrame::new(&styled(), Clock::REST);
        let (one, two) = (f.raster(1.0), f.raster(2.0));
        assert_eq!((two.width, two.height), ((f.bounds().width() * 2.0).ceil() as u32, (f.bounds().height() * 2.0).ceil() as u32));
        assert!(one.width > 100 && two.width > one.width);
        assert_eq!(one.color, kadr_core::ColorInfo::WORKING_SDR);
        assert!((0..one.height).all(|y| one.row(y).as_chunks::<4>().0.iter().all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])), "premultiplied");
    }

    /// TX-R3: the picture fills its raster — the box is drawn with the
    /// per-axis scales `W / width` and `H / height`, not with the asked
    /// scale that rounding the raster up made a little smaller.
    #[test]
    fn the_picture_fills_its_raster_on_both_axes() {
        let mut d = TextDoc::starter("Плашка", 1080);
        d.style.shadow = None;
        d.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::BLACK, radius: 0.0, padding_x: 20.0, padding_y: 10.0 });
        let f = TextFrame::new(&d, Clock::REST);
        let b = f.bounds();
        let (lw, lh) = f.layout_size();
        let [(x0, y0, x1, y1)] = crate::raster::plaque_rects(&d, f.laid())[..] else { panic!("one plaque round the block") };
        for (axis, extent) in [(0, b.width()), (1, b.height())] {
            // A scale that leaves almost a whole raster px to round up on this axis.
            let s = ((extent * 0.5).floor() + 0.02) / extent;
            let r = f.raster(s);
            let (sx, sy) = (r.width as f32 / b.width(), r.height as f32 / b.height());
            // The plaque is opaque: the coverage summed across its middle is its length in raster px.
            let (drawn, want, squeezed) = if axis == 0 {
                let y = (((y0 + y1) / 2.0 - lh / 2.0 - b.y0) * sy) as u32;
                ((0..r.width).map(|x| alpha_at(&r, x, y) as f32 / 255.0).sum::<f32>(), (x1 - x0) * sx, (x1 - x0) * s)
            } else {
                let x = (((x0 + x1) / 2.0 - lw / 2.0 - b.x0) * sx) as u32;
                ((0..r.height).map(|y| alpha_at(&r, x, y) as f32 / 255.0).sum::<f32>(), (y1 - y0) * sy, (y1 - y0) * s)
            };
            assert!(want - squeezed > 0.5, "axis {axis}: the test scale rounds up enough to tell ({want} vs {squeezed})");
            assert!((drawn - want).abs() < 0.2, "axis {axis}: {drawn} raster px drawn, {want} filled, {squeezed} squeezed");
        }
    }

    #[test]
    fn the_key_follows_the_picture() {
        let _serial = crate::TEST_FONTS.lock();
        let d = styled();
        let k = |d: &TextDoc| kadr_scene::TextPicture::key(&TextFrame::new(d, Clock::REST));
        assert_eq!(k(&d), k(&d.clone()));
        let mut red = d.clone();
        red.style.fill.colour = Rgba8::rgba(255, 0, 0, 255);
        assert_ne!(k(&d), k(&red));
        let later = Clock { since_in: kadr_core::Time::from_secs(9), ..Clock::REST };
        assert_eq!(kadr_scene::TextPicture::key(&TextFrame::new(&d, later)), k(&d), "a still text is the same picture at every instant");
    }

    #[test]
    fn a_missing_font_is_reported_by_the_frame() {
        let mut d = TextDoc::new("x");
        d.style.font.family = "Nope Sans".into();
        assert_eq!(TextFrame::new(&d, Clock::REST).missing_fonts(), ["Nope Sans"]);
    }
}
