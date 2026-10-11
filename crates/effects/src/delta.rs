//! A motion effect's change of the clip's placement (effects spec §3:
//! "PlacementDelta умножается поверх Placement клипа: масштаб вокруг
//! якоря, сдвиг, поворот").

use kadr_scene::{Placement, Vec2};

/// Scale and rotation about `anchor`, then a shift — all inside the clip's
/// own placement, so the content point at `anchor` stays where the clip
/// shows it (moved by `offset`): a zoom keeps its subject in place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacementDelta {
    /// Uniform scale (1 = none).
    pub scale: f64,
    /// Fixed point of scale and rotation: fractions of the clip's picture
    /// (0, 0 = top left; the content rectangle before crop).
    pub anchor: (f64, f64),
    /// Shift after scaling: fractions of the picture's width and height.
    pub offset: (f64, f64),
    /// Degrees, clockwise on screen.
    pub rotation_deg: f64,
}

impl PlacementDelta {
    pub const IDENTITY: PlacementDelta = PlacementDelta { scale: 1.0, anchor: (0.5, 0.5), offset: (0.0, 0.0), rotation_deg: 0.0 };

    /// Moves nothing (whatever the anchor).
    pub fn is_identity(&self) -> bool {
        self.scale == 1.0 && self.offset == (0.0, 0.0) && self.rotation_deg == 0.0
    }

    /// `p` followed by this delta in its local space: local point `x` lands
    /// where `p` puts `P + d + R(θ)·s·(x − P)` (`P` = anchor, `d` = offset,
    /// in local pixels). Exact for a uniform `p.scale` — the evaluator's.
    pub fn apply(&self, p: &Placement) -> Placement {
        if self.is_identity() {
            return *p;
        }
        let (w, h) = (p.size.x as f64, p.size.y as f64);
        let (ax, ay) = (self.anchor.0 * w, self.anchor.1 * h);
        let (px, py) = p.to_canvas().apply(ax + self.offset.0 * w, ay + self.offset.1 * h);
        Placement {
            size: p.size,
            anchor: Vec2::new(self.anchor.0 as f32, self.anchor.1 as f32),
            position: Vec2::new(px as f32, py as f32),
            scale: Vec2::new((p.scale.x as f64 * self.scale) as f32, (p.scale.y as f64 * self.scale) as f32),
            rotation: (p.rotation as f64 + self.rotation_deg.to_radians()) as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_scene::{Affine2, SizeU};

    /// The local map a delta stands for: x ↦ P + d + R(θ)·s·(x − P).
    fn local(d: &PlacementDelta, w: f64, h: f64) -> Affine2 {
        let (ax, ay) = (d.anchor.0 * w, d.anchor.1 * h);
        Affine2::translate(ax + d.offset.0 * w, ay + d.offset.1 * h)
            .after(&Affine2::rotate(d.rotation_deg.to_radians()))
            .after(&Affine2::scale(d.scale, d.scale))
            .after(&Affine2::translate(-ax, -ay))
    }

    #[test]
    fn apply_composes_inside_the_clip_placement() {
        let p = Placement { size: Vec2::new(1600.0, 900.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(900.0, 520.0), scale: Vec2::new(0.8, 0.8), rotation: 0.3 };
        let d = PlacementDelta { scale: 1.2, anchor: (0.25, 0.7), offset: (0.03, -0.02), rotation_deg: 2.0 };
        let got = d.apply(&p).to_canvas();
        let want = p.to_canvas().after(&local(&d, 1600.0, 900.0));
        for (x, y) in [(0.0, 0.0), (1600.0, 0.0), (400.0, 630.0), (1600.0, 900.0)] {
            let (a, b) = (got.apply(x, y), want.apply(x, y));
            assert!((a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05, "{x},{y}: {a:?} vs {b:?}");
        }
    }

    #[test]
    fn the_anchor_stays_where_the_clip_shows_it() {
        let p = Placement::fill(SizeU::new(1920, 1080));
        let d = PlacementDelta { scale: 1.5, anchor: (0.2, 0.3), ..PlacementDelta::IDENTITY };
        let a = d.apply(&p).to_canvas().apply(0.2 * 1920.0, 0.3 * 1080.0);
        assert!((a.0 - 384.0).abs() < 0.01 && (a.1 - 324.0).abs() < 0.01, "{a:?}");
        assert_eq!(PlacementDelta::IDENTITY.apply(&p), p);
        assert!(PlacementDelta { anchor: (0.0, 1.0), ..PlacementDelta::IDENTITY }.is_identity(), "an anchor alone moves nothing");
    }
}
