//! Pixels (titles spec §5.1): glyphs through swash into coverage masks and
//! colour images, composed into a premultiplied float buffer, stored as
//! RGBA8 (contract rounding). Bottom to top: plaque, shadow, glow, the
//! text — its fill (a colour or a gradient) with outline 1 round it and
//! outline 2 round that, one layer, each outline knocked out of what is
//! inside it (TX-R5). The text layer is drawn first and the passes below
//! it under it. Colour emoji (COLR / colour bitmaps) draw their own
//! colours. A tilted text is that flat picture seen in perspective
//! (`effects::warp`).
//!
//! The picture fills its raster (the `TextPicture` contract, ruling TX-R3):
//! a raster of `W × H = ⌈bounds · scale⌉` px draws the bounds with the
//! per-axis scales `W / width` and `H / height`, so its edges are the
//! bounds' edges and the renderer's stretch over `placement.size` is exact.
//!
//! Memory (TX-R5): drawing a raster of N px holds the float canvas (16 B
//! per px) and at most two coverage masks at a time (4 B per px each), or
//! the canvas and the RGBA8 result (4 B per px) at the end — about 24 B
//! per raster px at the peak (a tilt: the flat canvas and the tilted
//! RGBA8 result, whose sizes differ by at most the tilt's magnification),
//! so the resolver's
//! 16 Mpx cap (`kadr_playback::MAX_TEXT_PIXELS`) peaks near 400 MB, not
//! the 64 MB of the finished raster. Past [`MAX_RASTER_PIXELS`] nothing is
//! allocated: the picture is a 1×1 transparent raster. Glyph images are
//! cached up to [`GLYPH_CACHE_BYTES`].

use crate::anim::FrameAnim;
use crate::frame::Bounds;
use crate::layout::{Face, Glyph, Laid};
use crate::shape::Placed;
use kadr_core::{ColorInfo, CpuFrame, FramePool};
use kadr_project::{Fill, Rgba8, TextDoc};
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use swash::scale::image::{Content, Image};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Join, Stroke, Transform, Vector};
use swash::FontRef;

/// A premultiplied RGBA float buffer.
pub(crate) struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 4]>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        let (w, h) = (w.max(1), h.max(1));
        Canvas { w, h, px: vec![[0.0; 4]; w * h] }
    }

    /// Source-over of premultiplied `c` scaled by coverage `cov`.
    #[inline]
    pub fn over(&mut self, i: usize, c: [f32; 4], cov: f32) {
        let d = &mut self.px[i];
        let k = 1.0 - c[3] * cov;
        for ch in 0..4 {
            d[ch] = c[ch] * cov + d[ch] * k;
        }
    }

    /// Premultiplied `c` scaled by coverage `cov` added beside what is
    /// there (a band of the text layer: its coverage is what the rest of
    /// the layer leaves of the pixel).
    #[inline]
    pub fn add(&mut self, i: usize, c: [f32; 4], cov: f32) {
        let d = &mut self.px[i];
        for ch in 0..4 {
            d[ch] += c[ch] * cov;
        }
    }

    /// Destination-over: premultiplied `c` scaled by coverage `cov` drawn
    /// under what is there. Over is associative, so drawing the lower
    /// passes under the text layer equals drawing them first.
    #[inline]
    pub fn under(&mut self, i: usize, c: [f32; 4], cov: f32) {
        let d = &mut self.px[i];
        let k = cov * (1.0 - d[3]);
        for ch in 0..4 {
            d[ch] += c[ch] * k;
        }
    }

    pub fn under_mask(&mut self, m: &Mask, c: [f32; 4]) {
        for (i, &cov) in m.a.iter().enumerate() {
            if cov > 0.0 {
                self.under(i, c, cov);
            }
        }
    }

    /// [`Canvas::under_mask`] of the mask shifted by whole pixels `(dx, dy)`.
    pub fn under_mask_at(&mut self, m: &Mask, dx: i32, dy: i32, c: [f32; 4]) {
        // In i64: a shift near i32's ends never overflows.
        let (dx, dy) = (dx as i64, dy as i64);
        let ys = 0.max(dy)..(self.h as i64).min(m.h as i64 + dy);
        let xs = 0.max(dx)..(self.w as i64).min(m.w as i64 + dx);
        for y in ys {
            let (row, src) = (y as usize * self.w, (y - dy) as usize * m.w);
            for x in xs.clone() {
                let cov = m.a[src + (x - dx) as usize];
                if cov > 0.0 {
                    self.under(row + x as usize, c, cov);
                }
            }
        }
    }

    /// RGBA8 in the working space (see [`rgba8`]).
    pub fn into_frame(self, pool: &FramePool) -> CpuFrame {
        let mut f = CpuFrame::rgba8(pool, self.w as u32, self.h as u32, ColorInfo::WORKING_SDR);
        for y in 0..self.h {
            let row = f.row_mut(y as u32);
            for x in 0..self.w {
                row[x * 4..x * 4 + 4].copy_from_slice(&rgba8(self.px[y * self.w + x]));
            }
        }
        f
    }
}

/// A premultiplied float pixel as RGBA8 in the working space:
/// `floor(clamp(v)·255 + 0.5)`.
pub(crate) fn rgba8(p: [f32; 4]) -> [u8; 4] {
    p.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

/// A coverage buffer (0..1).
#[derive(Clone)]
pub(crate) struct Mask {
    pub w: usize,
    pub h: usize,
    pub a: Vec<f32>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Mask {
        let (w, h) = (w.max(1), h.max(1));
        Mask { w, h, a: vec![0.0; w * h] }
    }

    /// Coverage `cov` over what the pixel has (`a + cov − a·cov`): the
    /// coverage of glyphs painted over each other.
    #[inline]
    pub fn over(&mut self, i: usize, cov: f32) {
        let a = &mut self.a[i];
        *a = cov + *a * (1.0 - cov);
    }

    /// Max-union of a glyph image's coverage (a colour image's alpha) at
    /// `(x0, y0)`, times `k`.
    pub fn union(&mut self, img: &Image, x0: i32, y0: i32, k: f32) {
        let p = img.placement;
        for row in 0..p.height as i32 {
            let y = y0 + row;
            if y < 0 || y >= self.h as i32 {
                continue;
            }
            for col in 0..p.width as i32 {
                let x = x0 + col;
                if x < 0 || x >= self.w as i32 {
                    continue;
                }
                let i = (row * p.width as i32 + col) as usize;
                let cov = match img.content {
                    Content::Mask => img.data[i] as f32 / 255.0,
                    Content::Color => img.data[i * 4 + 3] as f32 / 255.0,
                    Content::SubpixelMask => 0.0,
                } * k;
                let a = &mut self.a[y as usize * self.w + x as usize];
                *a = a.max(cov);
            }
        }
    }

    /// Max-union of a rounded rectangle (raster px), anti-aliased by its
    /// signed distance.
    pub fn round_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) {
        if !(x1 > x0 && y1 > y0) {
            return;
        }
        let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
        let (cx, cy, hx, hy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let (xa, xb) = ((x0.floor().max(0.0)) as usize, (x1.ceil().min(self.w as f32)) as usize);
        let (ya, yb) = ((y0.floor().max(0.0)) as usize, (y1.ceil().min(self.h as f32)) as usize);
        for y in ya..yb {
            for x in xa..xb {
                let qx = (x as f32 + 0.5 - cx).abs() - (hx - r);
                let qy = (y as f32 + 0.5 - cy).abs() - (hy - r);
                let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
                let a = &mut self.a[y * self.w + x];
                *a = a.max((0.5 - d).clamp(0.0, 1.0));
            }
        }
    }
}

/// A glyph as drawn: pen origin (layout px), rotation (radians, clockwise),
/// scale, opacity, blur (canvas px) and the colour replacing its fill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Final {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub scale: f32,
    pub opacity: f32,
    pub blur: f32,
    pub colour: Option<Rgba8>,
}

/// The glyph's centre relative to its pen origin, unrotated: half its
/// advance along the baseline, 0.35 em above it.
fn centre_offset(g: &Glyph) -> (f32, f32) {
    (g.advance / 2.0, -0.35 * g.size)
}

fn rot((x, y): (f32, f32), a: f32) -> (f32, f32) {
    let (s, c) = a.sin_cos();
    (x * c - y * s, x * s + y * c)
}

/// The glyphs along the shape, moved by the animation (titles spec §4):
/// each turns and scales about its own centre and shifts; then the whole
/// block turns, scales and shifts about the layout box centre.
pub(crate) fn finals(laid: &Laid, placed: &[Placed], anim: &FrameAnim) -> Vec<Final> {
    let (bx, by) = (laid.width / 2.0, laid.height / 2.0);
    let b = &anim.block;
    laid.glyphs
        .iter()
        .zip(placed)
        .zip(&anim.glyphs)
        .map(|((g, p), a)| {
            let u = centre_offset(g);
            let c0 = rot(u, p.rotation);
            let centre = (p.x + c0.0 + a.dx, p.y + c0.1 + a.dy);
            let r = p.rotation + a.rotation;
            let c1 = rot(u, r);
            let origin = (centre.0 - a.scale * c1.0, centre.1 - a.scale * c1.1);
            let o = rot((origin.0 - bx, origin.1 - by), b.rotation);
            Final {
                x: bx + b.scale * o.0 + b.dx,
                y: by + b.scale * o.1 + b.dy,
                rotation: r + b.rotation,
                scale: a.scale * b.scale,
                opacity: (a.opacity * b.opacity).clamp(0.0, 1.0),
                blur: a.blur.max(b.blur),
                colour: a.colour,
            }
        })
        .collect()
}

pub(crate) fn fill_of(doc: &TextDoc, run: usize) -> &Fill {
    doc.runs.get(run).and_then(|r| r.style.fill.as_ref()).unwrap_or(&doc.style.fill)
}

pub(crate) fn premul(c: Rgba8, opacity: f32) -> [f32; 4] {
    let [r, g, b, a] = c.to_f32();
    let a = a * opacity;
    [r * a, g * a, b * a, a]
}

/// The largest plaque padding, canvas px (TX-R5).
pub(crate) const MAX_PADDING: f32 = 10_000.0;

/// The plaque's rectangles in layout px: one per line with glyphs, or one
/// round the block; padding (0..=[`MAX_PADDING`]) added. None for a curved
/// text (titles spec §1).
pub(crate) fn plaque_rects(doc: &TextDoc, laid: &Laid) -> Vec<(f32, f32, f32, f32)> {
    let Some(p) = doc.style.plaque else { return vec![] };
    if doc.shape != kadr_project::TextShape::Straight {
        return vec![];
    }
    let pad = |v: f32| if v.is_finite() { v.clamp(0.0, MAX_PADDING) } else { 0.0 };
    let (px, py) = (pad(p.padding_x), pad(p.padding_y));
    let lines: Vec<_> = laid.lines.iter().filter(|l| l.right > l.left).collect();
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else { return vec![] };
    if p.per_line {
        lines.iter().map(|l| (l.left - px, l.top - py, l.right + px, l.bottom + py)).collect()
    } else {
        let (l, r) = lines.iter().fold((f32::MAX, f32::MIN), |(a, b), l| (a.min(l.left), b.max(l.right)));
        vec![(l - px, first.top - py, r + px, last.bottom + py)]
    }
}

/// The outlines' total width (at most two, each ≤ 200 px).
pub(crate) fn outline_reach(doc: &TextDoc) -> f32 {
    doc.style.outlines.iter().take(2).map(|o| if o.width.is_finite() { o.width.clamp(0.0, 200.0) } else { 0.0 }).sum()
}

/// What the flat picture (before any tilt) may cover, in canvas px about
/// the layout box centre: the layout box, every glyph's em box as placed
/// (a line's last letter without the letter spacing after it), the
/// plaque, room for the outlines and ink beyond the em box, the shadow
/// (shifted, 3 σ of blur round it) and the glow (3 × its radius round it).
pub(crate) fn bounds(doc: &TextDoc, laid: &Laid, finals: &[Final]) -> Bounds {
    let mut b = Bounds { x0: 0.0, y0: 0.0, x1: laid.width, y1: laid.height };
    for (g, f) in laid.glyphs.iter().zip(finals).filter(|(g, _)| g.visible) {
        let advance = laid.lines.get(g.line).map_or(g.advance, |l| g.advance.min(l.right - g.x).max(0.0));
        for corner in [(0.0, -g.size), (advance, -g.size), (advance, 0.35 * g.size), (0.0, 0.35 * g.size)] {
            let (x, y) = rot((corner.0 * f.scale, corner.1 * f.scale), f.rotation);
            b.add_point(f.x + x, f.y + y);
        }
    }
    for (x0, y0, x1, y1) in plaque_rects(doc, laid) {
        b = b.union(Bounds { x0, y0, x1, y1 });
    }
    let margin = outline_reach(doc) + 0.15 * laid.max_size + 2.0;
    let mut b = b.grow(margin);
    if let Some((_, dx, dy, blur)) = crate::effects::shadow(doc) {
        b = b.union(Bounds { x0: b.x0 + dx, y0: b.y0 + dy, x1: b.x1 + dx, y1: b.y1 + dy }.grow(3.0 * blur + 1.0));
    }
    if let Some((_, radius, _)) = crate::effects::glow(doc) {
        b = b.grow(3.0 * radius + 1.0);
    }
    Bounds { x0: b.x0 - laid.width / 2.0, y0: b.y0 - laid.height / 2.0, x1: b.x1 - laid.width / 2.0, y1: b.y1 - laid.height / 2.0 }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    blob: u64,
    index: u32,
    coords: u64,
    id: u16,
    size: i32,
    frac: u8,
    stroke: i32,
    embolden: bool,
    skew: u32,
    rotation: i32,
    scale: i32,
    stretch: i32,
}

/// The glyph cache's budget: the bytes of its images and entries (TX-R5).
pub(crate) const GLYPH_CACHE_BYTES: usize = 64 << 20;
/// A glyph image wider or taller than this is drawn but not kept.
pub(crate) const MAX_CACHED_SIDE: u32 = 512;
/// What an entry costs beyond its pixels (key, image header, map slot).
const ENTRY_BYTES: usize = 192;

/// Glyph images by what draws them, within a byte budget: an entry that
/// would pass it empties the cache first. Animated glyphs (turned or
/// scaled) and large images are never kept — each would be used once.
pub(crate) struct GlyphCache {
    map: HashMap<GlyphKey, Option<Arc<Image>>>,
    bytes: usize,
    budget: usize,
}

impl GlyphCache {
    pub fn new(budget: usize) -> GlyphCache {
        GlyphCache { map: HashMap::new(), bytes: 0, budget }
    }

    /// Bytes held (images and entries).
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    fn cost(img: &Option<Arc<Image>>) -> usize {
        ENTRY_BYTES + img.as_ref().map_or(0, |i| i.data.len())
    }

    fn get(&self, key: &GlyphKey) -> Option<Option<Arc<Image>>> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: GlyphKey, img: Option<Arc<Image>>) {
        if img.as_ref().is_some_and(|i| i.placement.width > MAX_CACHED_SIDE || i.placement.height > MAX_CACHED_SIDE) {
            return;
        }
        let cost = Self::cost(&img);
        if cost > self.budget {
            return;
        }
        if self.bytes + cost > self.budget {
            self.map.clear();
            self.bytes = 0;
        }
        if let Some(old) = self.map.insert(key, img) {
            self.bytes -= Self::cost(&old);
        }
        self.bytes += cost;
    }
}

static GLYPHS: LazyLock<Mutex<GlyphCache>> = LazyLock::new(|| Mutex::new(GlyphCache::new(GLYPH_CACHE_BYTES)));

thread_local! {
    static SCALE: RefCell<ScaleContext> = RefCell::new(ScaleContext::new());
}

/// A glyph's image from swash, cached: `size` px; a centred stroke of
/// `stroke` px with round joins when `stroke > 0` (outlines only); turned
/// by `rotation` (radians, clockwise on screen) and scaled by `scale`
/// about the pen origin, then stretched horizontally by `stretch` (the
/// raster's x scale over its y scale, TX-R3); shifted right by `frac`
/// (0..1, in quarters). It is drawn with exactly the quantised values it
/// is cached by, so a cache hit is the same pixels as a miss.
#[allow(clippy::too_many_arguments)]
pub(crate) fn glyph_image(face: &Face, id: u16, size: f32, frac: f32, stroke: f32, rotation: f32, scale: f32, stretch: f32) -> Option<Arc<Image>> {
    glyph_image_in(&GLYPHS, face, id, size, frac, stroke, rotation, scale, stretch)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn glyph_image_in(cache: &Mutex<GlyphCache>, face: &Face, id: u16, size: f32, frac: f32, stroke: f32, rotation: f32, scale: f32, stretch: f32) -> Option<Arc<Image>> {
    let q = |v: f32| if v.is_finite() { (v * 64.0).round().clamp(-1e9, 1e9) as i32 } else { 0 };
    let frac = if frac.is_finite() { ((frac * 4.0).floor() as i32).clamp(0, 3) as u8 } else { 0 };
    let key = GlyphKey {
        blob: face.data.id(),
        index: face.index,
        coords: face.coords_hash,
        id,
        size: q(size),
        frac,
        stroke: q(stroke),
        embolden: face.embolden,
        skew: face.skew.map_or(0, f32::to_bits),
        rotation: q(rotation * 16.0),
        scale: q(scale * 16.0),
        stretch: q(stretch * 16.0),
    };
    // Only still glyphs are kept: an animated one is turned or scaled anew each frame.
    let keep = key.rotation == 0 && key.scale == 1024;
    if keep && let Some(hit) = cache.lock().get(&key) {
        return hit;
    }
    let (size, stroke) = (key.size as f32 / 64.0, key.stroke as f32 / 64.0);
    let (rotation, scale, stretch) = (key.rotation as f32 / 1024.0, key.scale as f32 / 1024.0, key.stretch as f32 / 1024.0);
    let img = SCALE.with(|scx| {
        let mut scx = scx.borrow_mut();
        let font = FontRef::from_index(face.data.as_ref(), face.index as usize)?;
        let mut scaler = scx.builder_with_id(font, [face.data.id(), face.index as u64]).size(size).hint(false).normalized_coords(&face.coords).build();
        let colour = [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
        let plain = [Source::Outline];
        let mut r = Render::new(if stroke > 0.0 { &plain } else { &colour });
        r.format(Format::Alpha).offset(Vector::new(frac as f32 / 4.0, 0.0)).default_color([255, 255, 255, 255]);
        if face.embolden {
            r.embolden(size / 48.0);
        }
        let mut t = face.skew.map(|deg| Transform::skew(Angle::from_degrees(deg), Angle::ZERO));
        if rotation != 0.0 || scale != 1.0 {
            let m = Transform::scale(scale, scale).then(&Transform::rotation(Angle::from_radians(-rotation)));
            t = Some(t.map_or(m, |s| s.then(&m)));
        }
        if stretch != 1.0 {
            let m = Transform::scale(stretch, 1.0);
            t = Some(t.map_or(m, |s| s.then(&m)));
        }
        r.transform(t);
        if stroke > 0.0 {
            let mut st = Stroke::new(stroke);
            st.join(Join::Round);
            r.style(st);
        }
        r.render(&mut scaler, id)
    })
    .map(Arc::new);
    if keep {
        cache.lock().insert(key, img.clone());
    }
    img
}

/// Where the picture goes in the raster (TX-R3): raster px per canvas px
/// on each axis — `sx = W / width`, `sy = H / height` — and the layout px
/// at the raster's top left. Glyphs are drawn at `sy` px per canvas px and
/// stretched by `sx / sy`; lengths without an axis (strokes, radii) use `sy`.
pub(crate) struct Place {
    pub sx: f32,
    pub sy: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Place {
    /// The raster of `bounds` at `scale`: its size (`⌈bounds · scale⌉`, at
    /// least 1 px) and the place that fills it; `None` when that raster
    /// would pass [`MAX_RASTER_PIXELS`] (or its size is not a number).
    pub fn fill(laid: &Laid, bounds: Bounds, scale: f32) -> Option<(usize, usize, Place)> {
        let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let (bw, bh) = (bounds.width(), bounds.height());
        let (wf, hf) = ((bw * s).ceil().max(1.0), (bh * s).ceil().max(1.0));
        let pixels = wf as f64 * hf as f64;
        let finite = [laid.width, laid.height, bounds.x0, bounds.y0].iter().all(|v| v.is_finite());
        if !finite || pixels.is_nan() || pixels > MAX_RASTER_PIXELS as f64 {
            return None;
        }
        let (w, h) = (wf as usize, hf as usize);
        let per_axis = |n: usize, extent: f32| if extent > 0.0 { n as f32 / extent } else { s };
        Some((w, h, Place { sx: per_axis(w, bw), sy: per_axis(h, bh), ox: laid.width / 2.0 + bounds.x0, oy: laid.height / 2.0 + bounds.y0 }))
    }

    /// A point in layout px, in raster px.
    pub fn point(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.ox) * self.sx, (y - self.oy) * self.sy)
    }

    /// A length without an axis (a stroke, a radius), in raster px.
    pub fn len(&self, v: f32) -> f32 {
        v * self.sy
    }

    /// The glyph's image and its top-left in raster px.
    pub fn image(&self, laid: &Laid, g: &Glyph, f: &Final, stroke: f32) -> Option<(Arc<Image>, i32, i32)> {
        let (px, py) = self.point(f.x, f.y);
        let ix = px.floor();
        let img = glyph_image(&laid.faces[g.face], g.id, self.len(g.size), px - ix, self.len(stroke), f.rotation, f.scale, self.sx / self.sy)?;
        let (x0, y0) = (ix as i32 + img.placement.left, py.round() as i32 - img.placement.top);
        Some((img, x0, y0))
    }
}

/// The union of every visible glyph's coverage, stroked by `stroke` canvas px when > 0.
pub(crate) fn glyph_mask(laid: &Laid, finals: &[Final], at: &Place, w: usize, h: usize, stroke: f32) -> Mask {
    let mut m = Mask::new(w, h);
    for (g, f) in laid.glyphs.iter().zip(finals).filter(|(g, f)| g.visible && f.opacity > 0.0) {
        if let Some((img, x0, y0)) = at.image(laid, g, f, stroke) {
            m.union(&img, x0, y0, f.opacity);
        }
    }
    m
}

/// Draws one glyph's fill: its coverage in `colour` (premultiplied), or a
/// colour glyph's own colours, times its opacity; `cover` gathers the
/// coverage the fill gives each pixel (glyphs over each other).
fn paint_glyph(c: &mut Canvas, cover: &mut Mask, img: &Image, x0: i32, y0: i32, colour: impl Fn(usize, usize) -> [f32; 4], opacity: f32) {
    let p = img.placement;
    for row in 0..p.height as i32 {
        let y = y0 + row;
        if y < 0 || y >= c.h as i32 {
            continue;
        }
        for col in 0..p.width as i32 {
            let x = x0 + col;
            if x < 0 || x >= c.w as i32 {
                continue;
            }
            let i = (row * p.width as i32 + col) as usize;
            let at = y as usize * c.w + x as usize;
            match img.content {
                Content::Mask => {
                    let cov = img.data[i] as f32 / 255.0 * opacity;
                    if cov > 0.0 {
                        c.over(at, colour(x as usize, y as usize), cov);
                        cover.over(at, cov);
                    }
                }
                Content::Color => {
                    let s = &img.data[i * 4..i * 4 + 4];
                    let f = |v: u8| v as f32 / 255.0;
                    // COLR output is premultiplied; a colour bitmap is straight.
                    let k = if matches!(img.source, Source::ColorBitmap(_)) { f(s[3]) } else { 1.0 };
                    c.over(at, [f(s[0]) * k, f(s[1]) * k, f(s[2]) * k, f(s[3])], opacity);
                    cover.over(at, f(s[3]) * opacity);
                }
                Content::SubpixelMask => {}
            }
        }
    }
}

/// The largest raster a text draws (64 Mpx, TX-R5): past it the picture is
/// a 1×1 transparent raster and nothing is allocated. The resolver keeps
/// its rasters far below (`kadr_playback::MAX_TEXT_PIXELS`); this guards
/// any other caller and absurd documents.
pub(crate) const MAX_RASTER_PIXELS: u64 = 64 << 20;

/// Logged once per process: the first raster past [`MAX_RASTER_PIXELS`].
static TOO_LARGE: AtomicBool = AtomicBool::new(false);

/// The raster of a text at `scale` (titles spec §5.1): `⌈bounds · scale⌉`
/// px, the bounds filling it (TX-R3) — the flat picture of `flat`, or for
/// a tilted text that picture seen tilted (`bounds` its projection); 1×1
/// and transparent when either raster would pass [`MAX_RASTER_PIXELS`].
/// A tilt holds the flat float canvas and the RGBA8 result, never two
/// float canvases.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(doc: &TextDoc, laid: &Laid, placed: &[Placed], anim: &FrameAnim, flat: Bounds, bounds: Bounds, scale: f32, pool: &FramePool) -> CpuFrame {
    let tilt = crate::effects::tilt(doc);
    let flat_at = Place::fill(laid, flat, scale);
    let out = if tilt.is_some() { Place::fill(laid, bounds, scale).map(|(w, h, _)| (w, h)) } else { flat_at.as_ref().map(|(w, h, _)| (*w, *h)) };
    let (Some((w, h, at)), Some((ow, oh))) = (flat_at, out) else {
        if !TOO_LARGE.swap(true, Ordering::Relaxed) {
            tracing::warn!(width = bounds.width(), height = bounds.height(), scale, "a text raster would pass 64 Mpx: drawn as nothing");
        }
        return Canvas::new(1, 1).into_frame(pool);
    };
    let canvas = compose(doc, laid, placed, anim, &at, w, h);
    match tilt {
        None => canvas.into_frame(pool),
        Some(p) => {
            let mut f = CpuFrame::rgba8(pool, ow as u32, oh as u32, ColorInfo::WORKING_SDR);
            crate::effects::warp(&canvas, flat, bounds, p, crate::effects::camera_distance(laid), &mut f);
            f
        }
    }
}

/// The flat picture (before any tilt) in a `w × h` raster placed by `at`:
/// the text layer, then under it, top down, the glow, the shadow and the
/// plaque.
pub(crate) fn compose(doc: &TextDoc, laid: &Laid, placed: &[Placed], anim: &FrameAnim, at: &Place, w: usize, h: usize) -> Canvas {
    let finals = finals(laid, placed, anim);
    let (mut canvas, silhouette) = text_layer(doc, laid, &finals, at, w, h);
    // The glow (neon): the silhouette blurred by its radius, times its intensity.
    if let Some((colour, radius, k)) = crate::effects::glow(doc) {
        let mut m = silhouette.clone();
        crate::effects::blur(&mut m, at.len(radius));
        m.a.iter_mut().for_each(|a| *a = (*a * k).min(1.0));
        canvas.under_mask(&m, premul(colour, 1.0));
    }
    // The shadow: the silhouette (the letters with their outlines), blurred, shifted.
    if let Some((colour, dx, dy, blur)) = crate::effects::shadow(doc) {
        let mut m = silhouette;
        crate::effects::blur(&mut m, at.len(blur));
        canvas.under_mask_at(&m, (dx * at.sx).round() as i32, (dy * at.sy).round() as i32, premul(colour, 1.0));
    } else {
        drop(silhouette);
    }
    // The plaque, at the bottom.
    if let Some(p) = doc.style.plaque.filter(|p| p.colour.0[3] > 0) {
        let mut m = Mask::new(w, h);
        for (x0, y0, x1, y1) in plaque_rects(doc, laid) {
            let ((ax, ay), (bx, by)) = (at.point(x0, y0), at.point(x1, y1));
            m.round_rect(ax, ay, bx, by, at.len(p.radius.max(0.0)));
        }
        canvas.under_mask(&m, premul(p.colour, anim.plaque_opacity));
    }
    canvas
}

/// The text itself as one layer (ruling TX-R5, the knock-out): the fill,
/// then each outline as the band its stroke adds outside what is already
/// there — outline 1 outside the fill, outline 2 outside outline 1 — so a
/// translucent fill or inner outline shows what is under the text, never
/// another outline. The coverages nest (the fill `D₀` ⊆ `D₁` = the fill
/// with outline 1 ⊆ `D₂`) and band `k` is added with the coverage `Dₖ −
/// Dₖ₋₁` the inner region leaves of the pixel, so at an anti-aliased edge
/// the fill and the bands sum to the silhouette's coverage: no dark seam.
/// Returns the layer and the silhouette (`D` of the outermost outline).
/// Holds the canvas and at most two masks at a time.
fn text_layer(doc: &TextDoc, laid: &Laid, finals: &[Final], at: &Place, w: usize, h: usize) -> (Canvas, Mask) {
    let mut canvas = Canvas::new(w, h);
    let mut inside = Mask::new(w, h);
    // The fill paints through its colour or its gradient across the layout box (raster px).
    let ((bx0, by0), (bx1, by1)) = (at.point(0.0, 0.0), at.point(laid.width, laid.height));
    for (g, f) in laid.glyphs.iter().zip(finals).filter(|(g, f)| g.visible && f.opacity > 0.0) {
        if let Some((img, x0, y0)) = at.image(laid, g, f, 0.0) {
            let paint = match f.colour {
                Some(c) => crate::effects::Paint::Solid(premul(c, 1.0)),
                None => crate::effects::Paint::of(fill_of(doc, g.run), [bx0, by0, bx1, by1]),
            };
            paint_glyph(&mut canvas, &mut inside, &img, x0, y0, |x, y| paint.at(x, y), f.opacity);
        }
    }
    // Outline k's stroke is centred on the letters' edges, `2 ×` the
    // widths up to it wide; with what is inside it, it is the letters
    // grown by those widths.
    let mut reach = 0.0;
    for o in doc.style.outlines.iter().take(2) {
        let width = if o.width.is_finite() { o.width.clamp(0.0, 200.0) } else { 0.0 };
        if width <= 0.0 {
            continue;
        }
        reach += width;
        let mut grown = glyph_mask(laid, finals, at, w, h, 2.0 * reach);
        let c = premul(o.colour, 1.0);
        for (i, (d, &inner)) in grown.a.iter_mut().zip(&inside.a).enumerate() {
            *d = d.max(inner);
            if *d > inner {
                canvas.add(i, c, *d - inner);
            }
        }
        inside = grown;
    }
    (canvas, inside)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::TextDoc;

    /// A face and a glyph of it: Montserrat's «Ж».
    fn zhe() -> (Face, u16) {
        let l = crate::layout::layout(&TextDoc::new("Ж"));
        (l.faces[l.glyphs[0].face].clone(), l.glyphs[0].id)
    }

    #[test]
    fn the_glyph_cache_keeps_within_its_budget() {
        let (face, id) = zhe();
        let small = Mutex::new(GlyphCache::new(256 << 10));
        for size in 8..=400 {
            assert!(glyph_image_in(&small, &face, id, size as f32, 0.0, 0.0, 0.0, 1.0, 1.0).is_some());
            let bytes = small.lock().bytes();
            assert!(bytes <= 256 << 10, "{bytes} bytes at size {size}");
        }
        assert!(small.lock().len() > 0, "it still keeps what fits");
        for size in (8..=500).step_by(3) {
            glyph_image(&face, id, size as f32, 0.5, 2.0, 0.0, 1.0, 1.0);
        }
        assert!(GLYPHS.lock().bytes() <= GLYPH_CACHE_BYTES);
    }

    #[test]
    fn animated_and_large_glyphs_are_drawn_but_not_kept() {
        let (face, id) = zhe();
        let cache = Mutex::new(GlyphCache::new(GLYPH_CACHE_BYTES));
        assert!(glyph_image_in(&cache, &face, id, 40.0, 0.0, 0.0, 0.3, 1.0, 1.0).is_some(), "turned");
        assert!(glyph_image_in(&cache, &face, id, 40.0, 0.0, 0.0, 0.0, 1.5, 1.0).is_some(), "scaled");
        let big = glyph_image_in(&cache, &face, id, 900.0, 0.0, 0.0, 0.0, 1.0, 1.0).expect("drawn");
        assert!(big.placement.width > MAX_CACHED_SIDE || big.placement.height > MAX_CACHED_SIDE);
        assert_eq!(cache.lock().len(), 0);
        glyph_image_in(&cache, &face, id, 40.0, 0.0, 0.0, 0.0, 1.0, 1.0002);
        assert_eq!(cache.lock().len(), 1, "a still glyph (the raster's stretch included) is kept");
    }

    #[test]
    fn a_cache_hit_is_the_same_pixels_as_a_miss() {
        let (face, id) = zhe();
        // Two requests that differ below the cache's resolution share an entry.
        let (a, b) = ((40.004, 0.26, 3.004, 1.0004), (40.001, 0.49, 3.001, 1.0001));
        let first = Mutex::new(GlyphCache::new(GLYPH_CACHE_BYTES));
        let miss = glyph_image_in(&first, &face, id, a.0, a.1, a.2, 0.0, 1.0, a.3).unwrap();
        let hit = glyph_image_in(&first, &face, id, b.0, b.1, b.2, 0.0, 1.0, b.3).unwrap();
        assert!(Arc::ptr_eq(&miss, &hit), "the second request is a hit");
        let fresh = glyph_image_in(&Mutex::new(GlyphCache::new(GLYPH_CACHE_BYTES)), &face, id, b.0, b.1, b.2, 0.0, 1.0, b.3).unwrap();
        let at = |i: &Image| (i.placement.left, i.placement.top, i.placement.width, i.placement.height);
        assert_eq!((at(&hit), &hit.data), (at(&fresh), &fresh.data), "drawn afresh: the same pixels");
    }

    /// Thick capitals in `fill` with `outlines` (width, colour), inner first.
    fn outlined(fill: Rgba8, outlines: &[(f32, Rgba8)]) -> TextDoc {
        let mut d = TextDoc::new("НЖ");
        d.style.size = 160.0;
        d.style.fill = Fill::solid(fill);
        d.style.outlines = outlines.iter().map(|&(width, colour)| kadr_project::Outline { width, colour }).collect();
        d
    }

    /// The raster at scale 1, pixel by pixel. A transparent outline keeps
    /// its room in the bounds, so docs that differ only in colours have
    /// rasters of one size, pixel for pixel.
    fn pixels(d: &TextDoc) -> Vec<[u8; 4]> {
        let r = crate::TextFrame::new(d, crate::Clock::REST).raster(1.0);
        (0..r.height).flat_map(|y| r.row(y).as_chunks::<4>().0.to_vec()).collect()
    }

    /// TX-R5 (knock-out): an outline lies outside the letters, so a
    /// translucent fill shows what is under the text, never its outline.
    #[test]
    fn a_translucent_fill_shows_no_outline_through_it() {
        let half = Rgba8::rgba(255, 255, 255, 128);
        let alone = pixels(&outlined(half, &[(14.0, Rgba8::rgba(255, 0, 0, 0))]));
        let red = pixels(&outlined(half, &[(14.0, Rgba8::rgba(255, 0, 0, 255))]));
        assert_eq!(alone.len(), red.len());
        // Inside the letters the fill covers its pixel whole: half-transparent white.
        let inside: Vec<usize> = (0..alone.len()).filter(|&i| alone[i] == [128, 128, 128, 128]).collect();
        assert!(inside.len() > 1000, "{} pixels inside the letters", inside.len());
        for &i in &inside {
            assert!(red[i][0].abs_diff(red[i][1]) <= 1 && red[i][3].abs_diff(128) <= 1, "red under the fill at {i}: {:?}", red[i]);
        }
        assert!(red.iter().filter(|p| **p == [255, 0, 0, 255]).count() > 1000, "the outline is drawn round the letters");
    }

    /// TX-R5 (knock-out): outline 2 lies outside outline 1, so a
    /// translucent inner outline never shows the outer one through it.
    #[test]
    fn the_outer_outline_never_shows_inside_the_inner_one() {
        let green = Rgba8::rgba(0, 255, 0, 128);
        let inner = pixels(&outlined(Rgba8::WHITE, &[(6.0, green), (6.0, Rgba8::rgba(0, 0, 255, 0))]));
        let both = pixels(&outlined(Rgba8::WHITE, &[(6.0, green), (6.0, Rgba8::rgba(0, 0, 255, 255))]));
        assert_eq!(inner.len(), both.len());
        // Outline 1's band where it covers its pixel whole and nothing else is there.
        let band: Vec<usize> = (0..inner.len()).filter(|&i| inner[i] == [0, 128, 0, 128]).collect();
        assert!(band.len() > 500, "{} pixels of outline 1", band.len());
        for &i in &band {
            assert!(both[i][2] <= 1 && both[i][1].abs_diff(128) <= 1, "outline 2 inside outline 1 at {i}: {:?}", both[i]);
        }
        assert!(both.iter().filter(|p| **p == [0, 0, 255, 255]).count() > 500, "outline 2 is drawn outside outline 1");
    }

    /// The fill and its outline share their anti-aliased edge: where the
    /// letters' coverage is partial the outline makes up the rest, so a
    /// white outline round white letters has no seam.
    #[test]
    fn a_fill_and_its_outline_meet_without_a_seam() {
        let fill = pixels(&outlined(Rgba8::WHITE, &[(6.0, Rgba8::rgba(255, 255, 255, 0))]));
        let both = pixels(&outlined(Rgba8::WHITE, &[(6.0, Rgba8::WHITE)]));
        let edge: Vec<usize> = (0..fill.len()).filter(|&i| (1..255).contains(&fill[i][3])).collect();
        assert!(edge.len() > 200, "{} edge pixels", edge.len());
        for &i in &edge {
            assert!(both[i][3] >= 254, "a seam at {i}: {:?} (the fill alone {:?})", both[i], fill[i]);
        }
    }
}
