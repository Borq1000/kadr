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
    /// What the flat picture covers (before any tilt).
    flat: Bounds,
    /// What the picture covers: `flat`, or its projection when tilted.
    bounds: Bounds,
    key: u64,
}

/// The hash of everything in a document that [`crate::layout::shape_hash`]
/// leaves out: with it, the whole document, each part hashed once.
fn paint_hash(doc: &TextDoc) -> u64 {
    // No `..`: a new field of `TextDoc` must be hashed here (or shape the layout).
    let TextDoc { runs: _, style, paragraph: _, shape, perspective, anim, seed, extra } = doc;
    crate::hash_of(&(style, shape, perspective, anim, seed, extra))
}

impl TextFrame {
    pub fn new(doc: &TextDoc, clock: Clock) -> TextFrame {
        // One font generation for the layout and the key: a font added
        // meanwhile can never pair a new key with an old layout.
        let generation = crate::fonts::fonts().generation();
        let shape = crate::layout::shape_hash(doc);
        let laid = crate::layout::layout_keyed(doc, shape, generation);
        let placed = crate::shape::place(doc, &laid);
        let anim = crate::anim::animate(doc, &laid, clock);
        let finals = crate::raster::finals(&laid, &placed, &anim);
        let flat = crate::raster::bounds(doc, &laid, &finals);
        let bounds = match crate::effects::tilt(doc) {
            Some(p) => crate::effects::project_bounds(flat, p, crate::effects::camera_distance(&laid)),
            None => flat,
        };
        // The paint hash holds the style (shadow, glow), the shape and the perspective.
        let key = crate::hash_of(&(shape, paint_hash(doc), anim.key(), generation));
        TextFrame { doc: doc.clone(), laid, placed, anim, flat, bounds, key }
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
/// layout box centre): the layout box, widened to a curved text's glyphs
/// (their em boxes as placed, at rest) and then to its tilted image.
pub fn frame_size(doc: &TextDoc) -> (f32, f32) {
    let laid = crate::layout::layout(doc);
    let (bx, by) = (laid.width / 2.0, laid.height / 2.0);
    let (mut hw, mut hh) = (bx, by);
    if doc.shape != kadr_project::TextShape::Straight {
        let placed = crate::shape::place(doc, &laid);
        let finals = crate::raster::finals(&laid, &placed, &FrameAnim::rest(laid.glyphs.len()));
        for (g, f) in laid.glyphs.iter().zip(&finals).filter(|(g, _)| g.visible) {
            for (x, y) in [(0.0, -g.size), (g.advance, -g.size), (g.advance, 0.3 * g.size), (0.0, 0.3 * g.size)] {
                let (s, c) = f.rotation.sin_cos();
                let (px, py) = (f.x + x * c - y * s, f.y + x * s + y * c);
                if px.is_finite() && py.is_finite() {
                    (hw, hh) = (hw.max((px - bx).abs()), hh.max((py - by).abs()));
                }
            }
        }
    }
    if let Some(p) = crate::effects::tilt(doc) {
        // The projected box, cut where it would come too near the camera (TX-R5).
        let b = crate::effects::project_bounds(Bounds { x0: -hw, y0: -hh, x1: hw, y1: hh }, p, crate::effects::camera_distance(&laid));
        (hw, hh) = (b.x0.abs().max(b.x1.abs()), b.y0.abs().max(b.y1.abs()));
    }
    (2.0 * hw, 2.0 * hh)
}

impl kadr_scene::TextPicture for TextFrame {
    fn key(&self) -> u64 {
        self.key
    }

    /// `⌈bounds · scale⌉` px with the bounds filling it (TX-R3).
    fn render(&self, scale: f32, pool: &FramePool) -> CpuFrame {
        crate::raster::render(&self.doc, &self.laid, &self.placed, &self.anim, self.flat, self.bounds, scale, pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{Glow, Outline, Perspective, Plaque, Rgba8, Shadow, TextDoc, TextShape};

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

    fn nothing_on_the_edge(f: &kadr_core::CpuFrame) -> bool {
        let (w, h) = (f.width, f.height);
        (0..w).all(|x| alpha_at(f, x, 0) == 0 && alpha_at(f, x, h - 1) == 0) && (0..h).all(|y| alpha_at(f, 0, y) == 0 && alpha_at(f, w - 1, y) == 0)
    }

    #[test]
    fn bounds_hold_everything_drawn() {
        let starter = |text: &str, look: &dyn Fn(&mut TextDoc)| {
            let mut d = TextDoc::starter(text, 1080);
            look(&mut d);
            d
        };
        let docs = [
            styled(),
            TextDoc::starter("🎉 Гол! ⚽", 1080),
            TextDoc::starter("", 1080),
            starter("Неон", &|d| d.style.glow = Some(Glow { colour: Rgba8::rgba(255, 0, 200, 255), radius: 12.0, intensity: 1.5 })),
            starter("Тень", &|d| d.style.shadow = Some(Shadow { colour: Rgba8::BLACK, offset_x: 20.0, offset_y: 30.0, blur: 0.0 })),
            starter("Текст по дуге", &|d| d.shape = TextShape::Arc { bend: 0.8 }),
            starter("ПО КРУГУ • ПО КРУГУ •", &|d| d.shape = TextShape::Circle { radius: 160.0, start_deg: 0.0 }),
            starter("Наклон", &|d| d.perspective = Some(Perspective { tilt_x: 40.0, tilt_y: 40.0 })),
        ];
        for d in docs {
            let f = TextFrame::new(&d, Clock::REST);
            let r = f.raster(1.0);
            let b = f.bounds();
            assert_eq!((r.width, r.height), (b.width().ceil() as u32, b.height().ceil() as u32));
            assert!(nothing_on_the_edge(&r), "{:?}: something touches the raster's edge", d.plain());
            let (lw, lh) = f.layout_size();
            if d.perspective.is_none() {
                assert!(b.x0 <= -lw / 2.0 && b.x1 >= lw / 2.0 && b.y0 <= -lh / 2.0 && b.y1 >= lh / 2.0, "the layout box is inside");
            }
            assert!((0..r.height).any(|y| r.row(y).as_chunks::<4>().0.iter().any(|p| p[3] > 0)) || d.plain().is_empty(), "{:?} draws", d.plain());
        }
    }

    #[test]
    fn a_tilted_text_is_its_flat_picture_seen_in_perspective() {
        // Capitals only: every row of ink spans the word.
        let mut d = TextDoc::starter("ПЕРСПЕКТИВА", 1080);
        d.style.shadow = None;
        let flat = TextFrame::new(&d, Clock::REST);
        d.perspective = Some(Perspective { tilt_x: 35.0, tilt_y: 0.0 });
        let tilted = TextFrame::new(&d, Clock::REST);
        let (a, b) = (flat.bounds(), tilted.bounds());
        assert!(b.width() > a.width() && b.height() < a.height(), "the near (bottom) edge widens, the height shrinks: {a:?} → {b:?}");
        assert!(b.x0 < 0.0 && b.x1 > 0.0 && b.y0 < 0.0 && b.y1 > 0.0, "the centre stays inside");
        assert_ne!(kadr_scene::TextPicture::key(&flat), kadr_scene::TextPicture::key(&tilted));
        // Ink per row: the top row of letters is narrower than the bottom one.
        let r = tilted.raster(1.0);
        let ink: Vec<u32> = (0..r.height).map(|y| r.row(y).as_chunks::<4>().0.iter().filter(|p| p[3] > 128).count() as u32).collect();
        let rows: Vec<usize> = (0..ink.len()).filter(|&y| ink[y] > 0).collect();
        let span = |y: usize| {
            let row = r.row(y as u32).as_chunks::<4>().0;
            let lit: Vec<usize> = (0..row.len()).filter(|&x| row[x][3] > 128).collect();
            lit.last().unwrap() - lit.first().unwrap()
        };
        let (top, bottom) = (rows[rows.len() / 5], rows[rows.len() * 4 / 5]);
        assert!(span(top) + 4 < span(bottom), "a trapezoid narrowing upwards: {} vs {}", span(top), span(bottom));
    }

    #[test]
    fn the_frame_covers_a_curved_or_tilted_text() {
        let mut d = TextDoc::starter("ПО КРУГУ • ПО КРУГУ • ПО КРУГУ •", 1080);
        let (lw, lh) = (crate::layout(&d).width, crate::layout(&d).height);
        assert_eq!(frame_size(&d), (lw, lh), "straight: the layout box");
        d.shape = TextShape::Circle { radius: 150.0, start_deg: 0.0 };
        let (cw, ch) = frame_size(&d);
        assert!(ch > 300.0 && cw >= lw.min(300.0), "a circle's frame holds the circle: {cw} × {ch}");
        let mut t = TextDoc::starter("Наклон", 1080);
        let flat = frame_size(&t);
        t.perspective = Some(Perspective { tilt_x: 30.0, tilt_y: 0.0 });
        let tilted = frame_size(&t);
        assert!(tilted.0 > flat.0 && tilted.1 < flat.1 + 2.5, "{flat:?} → {tilted:?}");
    }

    /// TX-R5 for the looks: absurd shadows, glows, curves and tilts are
    /// clamped — finite bounds of a sane size, a raster drawn without
    /// panicking or hanging, nothing on its edge.
    #[test]
    fn absurd_looks_never_abort() {
        let base = TextDoc::starter("Абсурд", 1080);
        let reach = |d: &TextDoc| {
            let b = TextFrame::new(d, Clock::REST).bounds();
            assert!([b.x0, b.y0, b.x1, b.y1].iter().all(|v| v.is_finite()), "{b:?}");
            b.x0.abs().max(b.x1.abs()).max(b.y0.abs()).max(b.y1.abs())
        };
        let plain = reach(&base);
        for (offset, blur) in [(1e12, 1e12), (-1e30, f32::NAN), (f32::INFINITY, f32::NEG_INFINITY), (f32::NAN, 1e9)] {
            let mut d = base.clone();
            d.style.shadow = Some(Shadow { colour: Rgba8::BLACK, offset_x: offset, offset_y: offset, blur });
            assert!(reach(&d) <= plain + 10_000.0 + 3.0 * 500.0 + 2.0, "shadow {offset} {blur}: {}", reach(&d));
            assert!(nothing_on_the_edge(&TextFrame::new(&d, Clock::REST).raster(0.25)));
        }
        for (radius, intensity) in [(1e12, 1.0), (f32::NAN, f32::NAN), (f32::INFINITY, 1e9), (500.0, f32::NAN)] {
            let mut d = base.clone();
            d.style.glow = Some(Glow { colour: Rgba8::WHITE, radius, intensity });
            assert!(reach(&d) <= plain + 3.0 * 500.0 + 2.0, "glow {radius} {intensity}: {}", reach(&d));
            assert!(nothing_on_the_edge(&TextFrame::new(&d, Clock::REST).raster(0.25)));
        }
        for shape in [TextShape::Arc { bend: f32::NAN }, TextShape::Arc { bend: 1e9 }, TextShape::Circle { radius: 1e12, start_deg: f32::INFINITY }, TextShape::Circle { radius: f32::NAN, start_deg: 1e30 }] {
            let mut d = base.clone();
            d.shape = shape;
            assert!(reach(&d) < 50_000.0, "{shape:?}: {}", reach(&d));
            TextFrame::new(&d, Clock::REST).raster(0.05);
        }
        // Near-90° (and absurd) tilts are 60° at most: the picture stays in front of
        // the camera and its bounds within 4× the flat picture's reach.
        let mut far = base.clone();
        far.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::BLACK, radius: 0.0, padding_x: 3000.0, padding_y: 3000.0 });
        for d in [base.clone(), far] {
            let flat = reach(&d);
            for (tx, ty) in [(89.9, 0.0), (-89.9, 89.9), (1e9, -1e9), (f32::NAN, 89.0), (f32::INFINITY, f32::NAN)] {
                let mut t = d.clone();
                t.perspective = Some(Perspective { tilt_x: tx, tilt_y: ty });
                let f = TextFrame::new(&t, Clock::REST);
                assert!(reach(&t) <= 4.0 * flat + 2.0, "tilt {tx} {ty}: {} vs flat {flat}", reach(&t));
                let r = f.raster(0.05);
                assert_eq!((r.width, r.height), ((f.bounds().width() * 0.05).ceil() as u32, (f.bounds().height() * 0.05).ceil() as u32));
                let (w, h) = frame_size(&t);
                assert!(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0);
            }
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

    fn is_blank_pixel(f: &kadr_core::CpuFrame) -> bool {
        (f.width, f.height) == (1, 1) && f.row(0) == [0, 0, 0, 0]
    }

    #[test]
    fn absurd_documents_and_scales_never_abort() {
        // A wrap of 10¹² lays out at the widest wrap.
        let mut wide = TextDoc::starter("широко", 1080);
        wide.paragraph.wrap = Some(1e12);
        let f = TextFrame::new(&wide, Clock::REST);
        assert_eq!(f.layout_size().0, crate::layout::MAX_WRAP);
        let r = f.raster(0.01);
        assert_eq!((r.width, r.height), ((f.bounds().width() * 0.01).ceil() as u32, (f.bounds().height() * 0.01).ceil() as u32));
        // Paddings of 10¹² are 10 000 px: at scale 1 that raster passes 64 Mpx and is one transparent pixel.
        let mut padded = TextDoc::starter("плашка", 1080);
        padded.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::BLACK, radius: f32::INFINITY, padding_x: 1e12, padding_y: f32::NAN });
        let f = TextFrame::new(&padded, Clock::REST);
        let b = f.bounds();
        assert!(b.width() > 20_000.0 && b.width() < 21_000.0 && b.height() < 1_000.0, "{b:?}");
        padded.style.plaque.as_mut().unwrap().padding_y = 1e12;
        let f = TextFrame::new(&padded, Clock::REST);
        assert!(f.bounds().height() > 20_000.0 && f.bounds().height() < 21_000.0);
        assert!(is_blank_pixel(&f.raster(1.0)), "past 64 Mpx: nothing is drawn");
        assert!(f.raster(0.05).width > 1000, "a scale that fits draws it");
        // Huge scales: nothing; scales that are not numbers count as 1.
        let plain = TextFrame::new(&TextDoc::starter("Масштаб", 1080), Clock::REST);
        for s in [1e6, 1e30, f32::MAX] {
            assert!(is_blank_pixel(&plain.raster(s)), "scale {s}");
        }
        for s in [f32::INFINITY, f32::NAN, 0.0, -2.0] {
            assert_eq!(plain.raster(s).width, plain.bounds().width().ceil() as u32, "scale {s}");
        }
    }

    #[test]
    fn a_missing_font_is_reported_by_the_frame() {
        let mut d = TextDoc::new("x");
        d.style.font.family = "Nope Sans".into();
        assert_eq!(TextFrame::new(&d, Clock::REST).missing_fonts(), ["Nope Sans"]);
    }
}
