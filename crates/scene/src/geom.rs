//! Geometry in canvas pixels (render spec §5). X and Y are the same unit
//! (square sequence pixels), so rotation, uniform scale and circles are
//! aspect-correct. Normalized coordinates appear only in `Placement::anchor`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SizeU {
    pub w: u32,
    pub h: u32,
}

impl SizeU {
    pub const fn new(w: u32, h: u32) -> Self {
        SizeU { w, h }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }
}

/// Axis-aligned rectangle by its min (x0, y0) and max (x1, y1) corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl RectF {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        RectF { x0, y0, x1, y1 }
    }
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn intersect(&self, o: &RectF) -> RectF {
        RectF::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }
    pub fn contains_rect(&self, o: &RectF) -> bool {
        self.x0 <= o.x0 && self.y0 <= o.y0 && self.x1 >= o.x1 && self.y1 >= o.y1
    }
}

/// 2-D affine map: `x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`. f64 so
/// composed transforms stay exact to well below a pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    pub fn translate(x: f64, y: f64) -> Self {
        Affine2 { tx: x, ty: y, ..Self::IDENTITY }
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine2 { a: sx, d: sy, ..Self::IDENTITY }
    }

    /// Positive angles turn clockwise on screen (the y axis points down).
    pub fn rotate(rad: f64) -> Self {
        let (s, c) = rad.sin_cos();
        Affine2 { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 }
    }

    /// `self ∘ inner`: apply `inner` first, then `self`.
    pub fn after(&self, inner: &Affine2) -> Affine2 {
        Affine2 {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            tx: self.a * inner.tx + self.c * inner.ty + self.tx,
            ty: self.b * inner.tx + self.d * inner.ty + self.ty,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.tx, self.b * x + self.d * y + self.ty)
    }

    pub fn inverse(&self) -> Option<Affine2> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine2 { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }

    /// Axis-aligned bounds of `r` after the map.
    pub fn map_bounds(&self, r: &RectF) -> RectF {
        let corners = [(r.x0, r.y0), (r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)].map(|(x, y)| self.apply(x as f64, y as f64));
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in corners {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        RectF::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
    }
}

/// Where a layer's content lands on the canvas (render spec §5):
/// `canvas = position + R(rotation) · diag(scale) · (local − anchor·size)`.
/// Local space is the content rectangle `[0, size.x] × [0, size.y]` in canvas
/// pixels at scale 1; `anchor` is normalized to that rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub size: Vec2,
    pub anchor: Vec2,
    pub position: Vec2,
    pub scale: Vec2,
    /// Radians, clockwise on screen.
    pub rotation: f32,
}

impl Placement {
    /// Content exactly covering the canvas, untransformed.
    pub fn fill(canvas: SizeU) -> Placement {
        let (w, h) = (canvas.w as f32, canvas.h as f32);
        Placement { size: Vec2::new(w, h), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(w / 2.0, h / 2.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 }
    }

    /// Local pixels → canvas pixels.
    pub fn to_canvas(&self) -> Affine2 {
        let (ax, ay) = ((self.anchor.x * self.size.x) as f64, (self.anchor.y * self.size.y) as f64);
        Affine2::translate(self.position.x as f64, self.position.y as f64)
            .after(&Affine2::rotate(self.rotation as f64))
            .after(&Affine2::scale(self.scale.x as f64, self.scale.y as f64))
            .after(&Affine2::translate(-ax, -ay))
    }

    /// The whole content rectangle, in local pixels.
    pub fn full_crop(&self) -> RectF {
        RectF::new(0.0, 0.0, self.size.x, self.size.y)
    }

    /// Canvas-space bounds of the visible (cropped) content.
    pub fn canvas_bounds(&self, crop: &RectF) -> RectF {
        self.to_canvas().map_bounds(crop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn rect_close(a: RectF, b: RectF) -> bool {
        [(a.x0, b.x0), (a.y0, b.y0), (a.x1, b.x1), (a.y1, b.y1)].iter().all(|(p, q)| (p - q).abs() < 1e-3)
    }

    #[test]
    fn fill_placement_maps_the_canvas_onto_itself() {
        let m = Placement::fill(SizeU::new(1920, 1080)).to_canvas();
        assert!(close(m.apply(0.0, 0.0), (0.0, 0.0)));
        assert!(close(m.apply(1920.0, 1080.0), (1920.0, 1080.0)));
    }

    #[test]
    fn rotation_is_aspect_correct() {
        // A 200×100 layer rotated 90° becomes 100 wide and 200 tall, not a squashed square.
        let p = Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(500.0, 500.0), scale: Vec2::new(1.0, 1.0), rotation: std::f32::consts::FRAC_PI_2 };
        let b = p.canvas_bounds(&p.full_crop());
        assert!(rect_close(b, RectF::new(450.0, 400.0, 550.0, 600.0)), "{b:?}");
        // Positive angles turn clockwise on screen (y points down): the right edge goes down.
        assert!(close(p.to_canvas().apply(200.0, 50.0), (500.0, 600.0)));
    }

    #[test]
    fn scale_happens_about_the_anchor() {
        let p = Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.25, 0.75), position: Vec2::new(300.0, 300.0), scale: Vec2::new(0.5, 0.5), rotation: 0.0 };
        assert!(close(p.to_canvas().apply(50.0, 75.0), (300.0, 300.0)), "the anchor point lands on `position`");
        assert!(close(p.to_canvas().apply(150.0, 75.0), (350.0, 300.0)));
    }

    #[test]
    fn inverse_round_trips() {
        let p = Placement { size: Vec2::new(640.0, 360.0), anchor: Vec2::new(0.3, 0.6), position: Vec2::new(123.0, 456.0), scale: Vec2::new(1.7, 0.8), rotation: 0.7 };
        let m = p.to_canvas();
        let inv = m.inverse().unwrap();
        let (x, y) = m.apply(10.0, 20.0);
        assert!(close(inv.apply(x, y), (10.0, 20.0)));
        assert!(Affine2::scale(0.0, 1.0).inverse().is_none());
    }

    #[test]
    fn crop_keeps_the_remaining_part_in_place() {
        let p = Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(500.0, 500.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 };
        assert!(rect_close(p.canvas_bounds(&p.full_crop()), RectF::new(400.0, 450.0, 600.0, 550.0)));
        let right_half = RectF::new(100.0, 0.0, 200.0, 100.0);
        assert!(rect_close(p.canvas_bounds(&right_half), RectF::new(500.0, 450.0, 600.0, 550.0)), "no re-centring");
    }

    #[test]
    fn rect_intersection_containment_and_emptiness() {
        let a = RectF::new(0.0, 0.0, 10.0, 10.0);
        assert!(a.intersect(&RectF::new(20.0, 20.0, 30.0, 30.0)).is_empty());
        assert_eq!(a.intersect(&RectF::new(5.0, 5.0, 30.0, 30.0)), RectF::new(5.0, 5.0, 10.0, 10.0));
        assert!(RectF::new(-1.0, -1.0, 11.0, 11.0).contains_rect(&a));
        assert!(!a.contains_rect(&RectF::new(-1.0, 0.0, 5.0, 5.0)));
        assert!(RectF::new(3.0, 0.0, 3.0, 5.0).is_empty());
        assert_eq!((a.width(), a.height()), (10.0, 10.0));
    }
}
