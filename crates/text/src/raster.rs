//! Pixels (titles spec §5.1): glyphs through swash into coverage masks and
//! colour images, composed into a premultiplied float buffer, stored as
//! RGBA8 (contract rounding). Bottom to top: plaque, shadow, glow, the
//! text — its fill (a colour or a gradient) with outline 1 round it and
//! outline 2 round that, one layer, each outline knocked out of what is
//! inside it (TX-R5). The text layer is drawn first and the passes below
//! it under it; the shadow and the glow are made of its alpha (TX-R13).
//! Colour emoji (COLR / colour bitmaps) draw their own colours. A tilted
//! text is that flat picture seen in perspective (`effects::warp`).
//!
//! The picture fills its raster (the `TextPicture` contract, ruling TX-R3):
//! a raster of `W × H = ⌈bounds · scale⌉` px draws the bounds with the
//! per-axis scales `W / width` and `H / height`, so its edges are the
//! bounds' edges and the renderer's stretch over `placement.size` is exact.
//!
//! Memory (TX-R5): drawing a raster of N px holds the float canvas (16 B
//! per px) and at most two coverage masks at a time (4 B per px each), or
//! the canvas and the RGBA8 result (4 B per px) at the end — about 24 B
//! per raster px at the peak, so the resolver's 16 Mpx cap
//! (`kadr_playback::MAX_TEXT_PIXELS`) peaks near 400 MB, not the 64 MB of
//! the finished raster. A tilted text draws its flat picture in a canvas
//! of at most max(N, [`TILT_FLAT_PIXELS`]) px (Task 9 review, Minor 8) and
//! then holds it with the N px RGBA8 result: at most 24 B × max(N, 4 Mpx).
//! The blur's scratch is a few rows. Past [`MAX_RASTER_PIXELS`] nothing is
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

    /// The canvas's alpha shifted by whole pixels `(dx, dy)`, as a mask of
    /// its size (what is shifted out is gone, what comes in is 0).
    pub fn alpha_at(&self, dx: i64, dy: i64) -> Mask {
        let mut m = Mask::new(self.w, self.h);
        let (w, h) = (self.w as i64, self.h as i64);
        let xs = 0.max(dx)..w.min(w + dx);
        for y in 0.max(dy)..h.min(h + dy) {
            let (dst, src) = (y as usize * self.w, (y - dy) as usize * self.w);
            for x in xs.clone() {
                m.a[dst + x as usize] = self.px[src + (x - dx) as usize][3];
            }
        }
        m
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
/// scale, opacity, blur (canvas px) and the colour replacing its fill;
/// `still` when the animation neither turns nor scales it (its turn is
/// the shape's, the same every frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Final {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub scale: f32,
    pub opacity: f32,
    pub blur: f32,
    pub colour: Option<Rgba8>,
    pub still: bool,
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
                still: a.rotation + b.rotation == 0.0 && a.scale * b.scale == 1.0,
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
/// the layout box centre: the text block (`block`, [`crate::shape::block`]:
/// the layout box, or a curved text's glyphs as placed — TX-R15), every
/// glyph's em box as drawn (a line's last letter without the letter
/// spacing after it), the plaque, room for the outlines and ink beyond the
/// em box — the text — then the glow round the text (3 × its radius) and
/// the shadow (the text shifted, 3 σ of blur round it; the glow is not
/// in the shadow).
pub(crate) fn bounds(doc: &TextDoc, laid: &Laid, block: Bounds, finals: &[Final]) -> Bounds {
    let (cx, cy) = (laid.width / 2.0, laid.height / 2.0);
    let mut b = Bounds { x0: block.x0 + cx, y0: block.y0 + cy, x1: block.x1 + cx, y1: block.y1 + cy };
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
    let text = b.grow(margin);
    let mut b = text;
    if let Some((_, radius, _)) = crate::effects::glow(doc) {
        b = text.grow(3.0 * radius + 1.0);
    }
    if let Some((_, dx, dy, blur)) = crate::effects::shadow(doc) {
        b = b.union(Bounds { x0: text.x0 + dx, y0: text.y0 + dy, x1: text.x1 + dx, y1: text.y1 + dy }.grow(3.0 * blur + 1.0));
    }
    Bounds { x0: b.x0 - cx, y0: b.y0 - cy, x1: b.x1 - cx, y1: b.y1 - cy }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    blob: u64,
    index: u32,
    coords: u64,
    id: u16,
    size: i32,
    frac: u8,
    frac_y: u8,
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
/// would pass it empties the cache first. Glyphs the animation turns or
/// scales and large images are never kept — each would be used once; a
/// curved text's still glyphs are (their turn is part of the key).
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

/// How a glyph image is drawn, raster px: its size; a centred stroke of
/// `stroke` px with round joins when `stroke > 0` (outlines only); turned
/// by `rotation` (radians, clockwise on screen) and scaled by `scale`
/// about the pen origin, then stretched horizontally by `stretch` (the
/// raster's x scale over its y scale, TX-R3); shifted right by `frac.0`
/// and down by `frac.1` (0..1, in quarters); `still` when the animation
/// neither turns nor scales it (then it is cached, its turn in the key).
#[derive(Clone, Copy, Debug)]
pub(crate) struct GlyphDraw {
    pub size: f32,
    pub frac: (f32, f32),
    pub stroke: f32,
    pub rotation: f32,
    pub scale: f32,
    pub stretch: f32,
    pub still: bool,
}

impl GlyphDraw {
    /// An upright, unscaled, still glyph of `size` px.
    #[cfg(test)]
    pub fn plain(size: f32) -> GlyphDraw {
        GlyphDraw { size, frac: (0.0, 0.0), stroke: 0.0, rotation: 0.0, scale: 1.0, stretch: 1.0, still: true }
    }
}

/// A glyph's image from swash, cached (see [`GlyphDraw`]). It is drawn
/// with exactly the quantised values it is cached by, so a cache hit is
/// the same pixels as a miss.
pub(crate) fn glyph_image(face: &Face, id: u16, d: GlyphDraw) -> Option<Arc<Image>> {
    glyph_image_in(&GLYPHS, face, id, d)
}

pub(crate) fn glyph_image_in(cache: &Mutex<GlyphCache>, face: &Face, id: u16, d: GlyphDraw) -> Option<Arc<Image>> {
    let q = |v: f32| if v.is_finite() { (v * 64.0).round().clamp(-1e9, 1e9) as i32 } else { 0 };
    let quarters = |f: f32, round: bool| if f.is_finite() { ((if round { (f * 4.0).round() } else { (f * 4.0).floor() }) as i32).clamp(0, 3) as u8 } else { 0 };
    let key = GlyphKey {
        blob: face.data.id(),
        index: face.index,
        coords: face.coords_hash,
        id,
        size: q(d.size),
        frac: quarters(d.frac.0, false),
        frac_y: quarters(d.frac.1, true),
        stroke: q(d.stroke),
        embolden: face.embolden,
        skew: face.skew.map_or(0, f32::to_bits),
        rotation: q(d.rotation * 16.0),
        scale: q(d.scale * 16.0),
        stretch: q(d.stretch * 16.0),
    };
    // Glyphs the animation turns or scales are not kept: each is drawn anew
    // every frame. A still glyph's turn (a curve's) is in its key (Minor 6).
    let keep = d.still && key.scale == 1024;
    if keep && let Some(hit) = cache.lock().get(&key) {
        return hit;
    }
    let frac = key.frac;
    let (size, stroke) = (key.size as f32 / 64.0, key.stroke as f32 / 64.0);
    let (rotation, scale, stretch) = (key.rotation as f32 / 1024.0, key.scale as f32 / 1024.0, key.stretch as f32 / 1024.0);
    let img = SCALE.with(|scx| {
        let mut scx = scx.borrow_mut();
        let font = FontRef::from_index(face.data.as_ref(), face.index as usize)?;
        let mut scaler = scx.builder_with_id(font, [face.data.id(), face.index as u64]).size(size).hint(false).normalized_coords(&face.coords).build();
        let colour = [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
        let plain = [Source::Outline];
        let mut r = Render::new(if stroke > 0.0 { &plain } else { &colour });
        // The outline's y points up: a shift down is a negative y.
        r.format(Format::Alpha).offset(Vector::new(frac as f32 / 4.0, -(key.frac_y as f32) / 4.0)).default_color([255, 255, 255, 255]);
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

    /// The glyph's image and its top-left in raster px. An upright glyph
    /// sits on its line's baseline rounded to the pixel (the whole line
    /// alike); a turned one (a curve's) keeps its place below a pixel
    /// vertically too, in quarters (Minor 11) — its neighbours sit at
    /// other heights, so rounding would make the curve wobble.
    pub fn image(&self, laid: &Laid, g: &Glyph, f: &Final, stroke: f32) -> Option<(Arc<Image>, i32, i32)> {
        let (px, py) = self.point(f.x, f.y);
        let ix = px.floor();
        let (iy, fy) = if f.rotation != 0.0 {
            let q = (py * 4.0).round() / 4.0;
            (q.floor(), q - q.floor())
        } else {
            (py.round(), 0.0)
        };
        let draw = GlyphDraw { size: self.len(g.size), frac: (px - ix, fy), stroke: self.len(stroke), rotation: f.rotation, scale: f.scale, stretch: self.sx / self.sy, still: f.still };
        let img = glyph_image(&laid.faces[g.face], g.id, draw)?;
        let (x0, y0) = (ix as i32 + img.placement.left, iy as i32 - img.placement.top);
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

/// A tilt's flat canvas is drawn at the output's scale unless it would
/// hold more pixels than the larger of the tilted raster and this (4 Mpx):
/// then at a scale that gives it that many (Minor 8). A tilt that shrinks
/// the picture (steep, or on both axes) has a flat picture larger than
/// its image; so memory stays within the bound in the module's doc, and
/// only a large title (an export) can be drawn that little coarser — its
/// near edge, which the warp magnifies, a touch softer.
pub(crate) const TILT_FLAT_PIXELS: f64 = (4 << 20) as f64;

/// The scale of a tilt's flat canvas for `flat` bounds at `scale` with a
/// tilted raster of `out_pixels` (see [`TILT_FLAT_PIXELS`]).
pub(crate) fn tilt_flat_scale(flat: Bounds, scale: f32, out_pixels: f64) -> f32 {
    let pixels = |s: f32| (flat.width() * s).ceil().max(1.0) as f64 * (flat.height() * s).ceil().max(1.0) as f64;
    let allowed = out_pixels.max(TILT_FLAT_PIXELS);
    let have = pixels(scale);
    if have.is_nan() || have <= allowed {
        return scale;
    }
    // ⌈⌉ adds up to a pixel per side: shrink until it fits.
    let mut s = scale * (allowed / have).sqrt() as f32;
    while s > 0.0 && pixels(s) > allowed {
        s *= 0.999;
    }
    s
}

/// The raster of a text at `scale` (titles spec §5.1): `⌈bounds · scale⌉`
/// px, the bounds filling it (TX-R3) — the flat picture of `flat`, or for
/// a tilted text that picture seen tilted (`bounds` its projection; the
/// flat canvas at most [`TILT_FLAT_PIXELS`] or the tilted raster's size);
/// 1×1 and transparent when either raster would pass
/// [`MAX_RASTER_PIXELS`]. A tilt holds the flat float canvas and the RGBA8
/// result, never two float canvases.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(doc: &TextDoc, laid: &Laid, placed: &[Placed], anim: &FrameAnim, flat: Bounds, bounds: Bounds, scale: f32, pool: &FramePool) -> CpuFrame {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let tilt = crate::effects::tilt(doc);
    let out = tilt.and_then(|_| Place::fill(laid, bounds, scale).map(|(w, h, _)| (w, h)));
    let flat_scale = match out {
        Some((ow, oh)) => tilt_flat_scale(flat, scale, ow as f64 * oh as f64),
        None => scale,
    };
    let flat_at = Place::fill(laid, flat, flat_scale);
    let out = if tilt.is_some() { out } else { flat_at.as_ref().map(|(w, h, _)| (*w, *h)) };
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
/// plaque. The glow and the shadow are of the text layer as drawn — its
/// alpha (TX-R13): a hollow text (a transparent fill in its outline) casts
/// a hollow shadow, a half-transparent one a half shadow, a transparent
/// outline none. The shadow is shifted, then blurred (TX-R14): the bounds
/// hold the shifted text and its 3 σ, so nothing of its fringe is cut.
/// Holds the canvas and at most two masks at a time.
pub(crate) fn compose(doc: &TextDoc, laid: &Laid, placed: &[Placed], anim: &FrameAnim, at: &Place, w: usize, h: usize) -> Canvas {
    let finals = finals(laid, placed, anim);
    let block = crate::shape::block(doc, laid, placed);
    let mut canvas = text_layer(doc, laid, block, &finals, at, w, h);
    // The shadow's mask is taken before the glow goes under the text.
    let shadow = crate::effects::shadow(doc).map(|(colour, dx, dy, blur)| {
        let mut m = canvas.alpha_at((dx * at.sx).round() as i64, (dy * at.sy).round() as i64);
        crate::effects::blur(&mut m, at.len(blur));
        (m, premul(colour, 1.0))
    });
    // The glow (neon): the text blurred by its radius, times its intensity.
    if let Some((colour, radius, k)) = crate::effects::glow(doc) {
        let mut m = canvas.alpha_at(0, 0);
        crate::effects::blur(&mut m, at.len(radius));
        m.a.iter_mut().for_each(|a| *a = (*a * k).min(1.0));
        canvas.under_mask(&m, premul(colour, 1.0));
    }
    if let Some((m, colour)) = shadow {
        canvas.under_mask(&m, colour);
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
/// Holds the canvas and at most two masks at a time.
fn text_layer(doc: &TextDoc, laid: &Laid, block: Bounds, finals: &[Final], at: &Place, w: usize, h: usize) -> Canvas {
    let mut canvas = Canvas::new(w, h);
    let mut inside = Mask::new(w, h);
    // The fill paints through its colour or its gradient across the text
    // block (raster px; a curved text's own block, TX-R15) — each run's
    // paint made once (Minor 14); runs past the document's paint as its base.
    let (cx, cy) = (laid.width / 2.0, laid.height / 2.0);
    let ((bx0, by0), (bx1, by1)) = (at.point(cx + block.x0, cy + block.y0), at.point(cx + block.x1, cy + block.y1));
    let paints: Vec<crate::effects::Paint> = (0..=doc.runs.len()).map(|run| crate::effects::Paint::of(fill_of(doc, run), [bx0, by0, bx1, by1])).collect();
    for (g, f) in laid.glyphs.iter().zip(finals).filter(|(g, f)| g.visible && f.opacity > 0.0) {
        if let Some((img, x0, y0)) = at.image(laid, g, f, 0.0) {
            let solid;
            let paint = match f.colour {
                Some(c) => {
                    solid = crate::effects::Paint::Solid(premul(c, 1.0));
                    &solid
                }
                None => &paints[g.run.min(doc.runs.len())],
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
    canvas
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
            assert!(glyph_image_in(&small, &face, id, GlyphDraw::plain(size as f32)).is_some());
            let bytes = small.lock().bytes();
            assert!(bytes <= 256 << 10, "{bytes} bytes at size {size}");
        }
        assert!(small.lock().len() > 0, "it still keeps what fits");
        for size in (8..=500).step_by(3) {
            glyph_image(&face, id, GlyphDraw { frac: (0.5, 0.0), stroke: 2.0, ..GlyphDraw::plain(size as f32) });
            // A curve's still glyphs are kept too, each turn its own entry.
            glyph_image(&face, id, GlyphDraw { rotation: size as f32 / 100.0, frac: (0.5, 0.25), ..GlyphDraw::plain(size as f32) });
        }
        assert!(GLYPHS.lock().bytes() <= GLYPH_CACHE_BYTES);
    }

    #[test]
    fn animated_and_large_glyphs_are_drawn_but_not_kept() {
        let (face, id) = zhe();
        let cache = Mutex::new(GlyphCache::new(GLYPH_CACHE_BYTES));
        let moving = GlyphDraw { still: false, ..GlyphDraw::plain(40.0) };
        assert!(glyph_image_in(&cache, &face, id, GlyphDraw { rotation: 0.3, ..moving }).is_some(), "turned");
        assert!(glyph_image_in(&cache, &face, id, GlyphDraw { scale: 1.5, ..moving }).is_some(), "scaled");
        let big = glyph_image_in(&cache, &face, id, GlyphDraw::plain(900.0)).expect("drawn");
        assert!(big.placement.width > MAX_CACHED_SIDE || big.placement.height > MAX_CACHED_SIDE);
        assert_eq!(cache.lock().len(), 0);
        glyph_image_in(&cache, &face, id, GlyphDraw { stretch: 1.0002, ..GlyphDraw::plain(40.0) });
        assert_eq!(cache.lock().len(), 1, "a still glyph (the raster's stretch included) is kept");
    }

    /// Minor 6: a curved text's glyphs are still when the animation leaves
    /// them alone — kept, the shape's turn part of the key.
    #[test]
    fn a_curves_still_glyphs_are_kept_by_their_turn() {
        let (face, id) = zhe();
        let cache = Mutex::new(GlyphCache::new(GLYPH_CACHE_BYTES));
        let on_curve = |rotation: f32| GlyphDraw { rotation, ..GlyphDraw::plain(40.0) };
        let a = glyph_image_in(&cache, &face, id, on_curve(0.3)).unwrap();
        let b = glyph_image_in(&cache, &face, id, on_curve(0.6)).unwrap();
        assert_eq!(cache.lock().len(), 2, "one entry per turn");
        assert!(Arc::ptr_eq(&a, &glyph_image_in(&cache, &face, id, on_curve(0.3)).unwrap()), "the same turn is a hit");
        assert_ne!((a.placement.width, a.placement.height, &a.data), (b.placement.width, b.placement.height, &b.data));
        // `finals`: still where the animation neither turns nor scales a glyph.
        let d = TextDoc { shape: kadr_project::TextShape::Circle { radius: 150.0, start_deg: 0.0 }, ..TextDoc::new("круг") };
        let l = crate::layout::layout(&d);
        let placed = crate::shape::place(&d, &l);
        let mut anim = FrameAnim::rest(l.glyphs.len());
        assert!(finals(&l, &placed, &anim).iter().all(|f| f.still && f.rotation != 0.0));
        anim.glyphs[1].rotation = 0.2;
        anim.glyphs[2].scale = 1.1;
        let f = finals(&l, &placed, &anim);
        assert_eq!(f.iter().map(|f| f.still).collect::<Vec<_>>(), [true, false, false, true]);
        anim.glyphs[1].rotation = 0.0;
        anim.block.rotation = 0.1;
        assert!(finals(&l, &placed, &anim).iter().all(|f| !f.still), "the whole block turning");
    }

    /// Minor 11: a turned glyph keeps its place below a pixel vertically
    /// (an upright one sits on its rounded baseline, the line alike).
    #[test]
    fn a_turned_glyph_keeps_its_sub_pixel_height() {
        let l = crate::layout::layout(&TextDoc::new("Ж"));
        let g = &l.glyphs[0];
        let at = Place { sx: 1.0, sy: 1.0, ox: 0.0, oy: 0.0 };
        // The ink's centroid height in raster px.
        let centre = |f: &Final| {
            let (img, _, y0) = at.image(&l, g, f, 0.0).unwrap();
            let w = img.placement.width as usize;
            let (mut sum, mut moment) = (0.0, 0.0);
            for (i, &a) in img.data.iter().enumerate() {
                (sum, moment) = (sum + a as f32, moment + a as f32 * (y0 as f32 + (i / w) as f32 + 0.5));
            }
            moment / sum
        };
        let base = Final { x: 20.0, y: 50.0, rotation: 0.4, scale: 1.0, opacity: 1.0, blur: 0.0, colour: None, still: true };
        for dy in [0.25, 0.5, 0.75] {
            let moved = centre(&Final { y: 50.0 + dy, ..base }) - centre(&base);
            assert!((moved - dy).abs() < 0.06, "turned, {dy} px lower: the ink moves {moved}");
        }
        let upright = Final { rotation: 0.0, ..base };
        assert_eq!(centre(&Final { y: 50.25, ..upright }), centre(&upright), "upright: on the rounded baseline");
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

    /// The raster at scale 1 with its width.
    fn raster(d: &TextDoc) -> (usize, Vec<[u8; 4]>) {
        let r = crate::TextFrame::new(d, crate::Clock::REST).raster(1.0);
        (r.width as usize, (0..r.height).flat_map(|y| r.row(y).as_chunks::<4>().0.to_vec()).collect())
    }

    /// Pixels at least `depth` px inside the letters of `opaque` (every
    /// pixel within `depth` of them is the opaque fill `inner`).
    fn deep_inside(w: usize, opaque: &[[u8; 4]], inner: [u8; 4], depth: usize) -> Vec<usize> {
        let h = opaque.len() / w;
        let d = depth as i64;
        (0..opaque.len())
            .filter(|&i| {
                let (x, y) = ((i % w) as i64, (i / w) as i64);
                (-d..=d).all(|dy| (-d..=d).all(|dx| {
                    let (xx, yy) = (x + dx, y + dy);
                    xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64 && opaque[(yy * w as i64 + xx) as usize] == inner
                }))
            })
            .collect()
    }

    /// TX-R13: the shadow is cast by the text as drawn — a hollow text (a
    /// transparent fill in a 3 px outline) casts a hollow shadow, so with
    /// no offset and no blur the inside of a thick stem stays clear.
    #[test]
    fn a_hollow_text_casts_a_hollow_shadow() {
        let mut d = outlined(Rgba8::WHITE, &[(3.0, Rgba8::BLACK)]);
        d.style.font = kadr_project::FontRef::new("Montserrat", 900, false);
        d.style.shadow = Some(kadr_project::Shadow { colour: Rgba8::rgba(255, 0, 0, 255), offset_x: 0.0, offset_y: 0.0, blur: 0.0 });
        let (w, opaque) = raster(&d);
        let deep = deep_inside(w, &opaque, [255, 255, 255, 255], 4);
        assert!(deep.len() > 200, "{} pixels deep inside the stems", deep.len());
        d.style.fill = Fill::solid(Rgba8::rgba(255, 255, 255, 0));
        let (_, hollow) = raster(&d);
        for &i in &deep {
            assert_eq!(hollow[i][3], 0, "the shadow shows inside the hollow letters at {i}: {:?}", hollow[i]);
        }
        assert!(hollow.iter().filter(|p| **p == [0, 0, 0, 255]).count() > 500, "the outline is drawn");
    }

    /// TX-R13: so is the glow — round the outline, not filling the stems.
    #[test]
    fn a_hollow_text_glows_round_its_outline_only() {
        let mut d = outlined(Rgba8::WHITE, &[(3.0, Rgba8::BLACK)]);
        d.style.font = kadr_project::FontRef::new("Montserrat", 900, false);
        d.style.glow = Some(kadr_project::Glow { colour: Rgba8::rgba(0, 255, 0, 255), radius: 1.5, intensity: 1.0 });
        let (w, opaque) = raster(&d);
        let deep = deep_inside(w, &opaque, [255, 255, 255, 255], 7);
        assert!(deep.len() > 50, "{} pixels deep inside the stems", deep.len());
        d.style.fill = Fill::solid(Rgba8::rgba(255, 255, 255, 0));
        let (_, hollow) = raster(&d);
        for &i in &deep {
            assert!(hollow[i][3] <= 2, "the glow fills the hollow letters at {i}: {:?}", hollow[i]);
        }
    }

    /// TX-R13: a half-transparent fill casts a half shadow, and a
    /// transparent outline does not fatten it.
    #[test]
    fn the_shadow_is_as_opaque_and_as_wide_as_the_text_drawn() {
        let shadowed = |fill: Rgba8, outline: f32| {
            let mut d = outlined(fill, &[(outline, Rgba8::rgba(255, 0, 0, 0))]);
            d.style.shadow = Some(kadr_project::Shadow { colour: Rgba8::BLACK, offset_x: 300.0, offset_y: 0.0, blur: 0.0 });
            raster(&d).1
        };
        // The shadow lies clear of the letters: its pixels are black (r = 0).
        let shadow = |px: &[[u8; 4]]| px.iter().filter(|p| p[0] == 0 && p[3] > 0).map(|p| p[3]).collect::<Vec<u8>>();
        let half = shadow(&shadowed(Rgba8::rgba(255, 255, 255, 128), 0.0));
        assert!(half.len() > 1000 && half.iter().all(|&a| a <= 129), "a half fill: a half shadow ({} px, max {:?})", half.len(), half.iter().max());
        let (thin, fat) = (shadow(&shadowed(Rgba8::WHITE, 0.0)).len(), shadow(&shadowed(Rgba8::WHITE, 12.0)).len());
        assert!((fat as f32 / thin as f32 - 1.0).abs() < 0.02, "a transparent outline widens the shadow: {thin} → {fat} px");
    }

    /// TX-R14: the shadow is shifted, then blurred — an offset beyond 3 σ
    /// plus the margin leaves no hard step where the unshifted picture's
    /// edge was: the fringe falls off smoothly all round.
    #[test]
    fn a_far_shadow_keeps_its_whole_fringe() {
        let mut d = TextDoc::new("Тень");
        d.style.size = 97.0;
        d.style.shadow = Some(kadr_project::Shadow { colour: Rgba8::BLACK, offset_x: 100.0, offset_y: 100.0, blur: 20.0 });
        let (w, px) = raster(&d);
        let h = px.len() / w;
        let shadow_only = |i: usize| px[i][0] == 0;
        let mut worst = 0;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                for j in [(x + 1 < w).then_some(i + 1), (y + 1 < h).then_some(i + w)].into_iter().flatten() {
                    if shadow_only(i) && shadow_only(j) {
                        worst = worst.max(px[i][3].abs_diff(px[j][3]));
                    }
                }
            }
        }
        assert!(worst <= 8, "a step of {worst} LSB in the shadow's fringe");
        assert!(px.iter().any(|p| p[0] == 0 && p[3] > 100), "the shadow is drawn");
    }

    /// Minor 4: a shadow's or a glow's blur is as wide at scale 1 as at
    /// scale 2 (a preview and an export of one title). The text's
    /// silhouette at scale 2 and the same averaged down to scale 1 (one
    /// shape, no glyph placed apart by rounding), blurred by σ and 2σ: the
    /// scale-2 blur averaged down agrees within 2.5 LSB from σ = 1.5
    /// canvas px. Below, a pixel grid cannot hold an edge's blur finer than
    /// a pixel, so the scales part whatever the kernel (a true Gaussian at
    /// both: 20, 9, 5, 3 LSB at σ 0.5, 0.75, 1, 1.25); the bounds there are
    /// that floor with some room (the blur before this fix: 104, 79, 22, 22).
    #[test]
    fn a_blur_looks_alike_at_scale_1_and_2() {
        let mut d = TextDoc::new("Мягко");
        d.style.size = 30.0;
        let r = crate::TextFrame::new(&d, crate::Clock::REST).raster(2.0);
        // Room round the text for the widest blur.
        let pad = 30;
        let (w, h) = ((r.width / 2) as usize + 2 * pad, (r.height / 2) as usize + 2 * pad);
        let mut two = Mask::new(2 * w, 2 * h);
        for y in 0..(r.height as usize / 2) * 2 {
            for x in 0..(r.width as usize / 2) * 2 {
                two.a[(y + 2 * pad) * 2 * w + x + 2 * pad] = r.row(y as u32)[x * 4 + 3] as f32 / 255.0;
            }
        }
        let down = |m: &Mask| {
            let mut one = Mask::new(w, h);
            for (i, a) in one.a.iter_mut().enumerate() {
                let (x, y) = (2 * (i % w), 2 * (i / w));
                *a = (m.a[y * 2 * w + x] + m.a[y * 2 * w + x + 1] + m.a[(y + 1) * 2 * w + x] + m.a[(y + 1) * 2 * w + x + 1]) / 4.0;
            }
            one
        };
        let one = down(&two);
        for (sigma, bound) in [(0.5, 21.0), (0.75, 11.5), (1.0, 6.5), (1.25, 6.5), (1.5, 2.5), (2.0, 2.5), (2.5, 2.5), (3.0, 2.5), (4.0, 2.5), (6.0, 2.5)] {
            let (mut a, mut b) = (one.clone(), two.clone());
            crate::effects::blur(&mut a, sigma);
            crate::effects::blur(&mut b, 2.0 * sigma);
            let worst = a.a.iter().zip(&down(&b).a).map(|(p, q)| (p - q).abs() * 255.0).fold(0.0f32, f32::max);
            assert!(worst <= bound, "σ {sigma}: scale 1 and 2 differ by {worst} LSB");
        }
    }

    /// Minor 10: the glow's room is round the text, the shadow's round
    /// the shifted text — the glow does not grow the shadow's side.
    #[test]
    fn the_glow_room_is_round_the_text_not_its_shadow() {
        let b = |glow: bool, shadow: bool| {
            let mut d = TextDoc::new("Неон");
            d.style.glow = glow.then_some(kadr_project::Glow { colour: Rgba8::WHITE, radius: 10.0, intensity: 1.0 });
            d.style.shadow = shadow.then_some(kadr_project::Shadow { colour: Rgba8::BLACK, offset_x: 200.0, offset_y: 0.0, blur: 0.0 });
            crate::TextFrame::new(&d, crate::Clock::REST).bounds()
        };
        let (both, glow, shadow) = (b(true, true), b(true, false), b(false, true));
        assert_eq!((both.x0, both.y0, both.y1), (glow.x0, glow.y0, glow.y1), "the glow's side");
        assert_eq!(both.x1, shadow.x1, "the shadow's side: no glow room past it");
    }
}
