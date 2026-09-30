//! What a scene costs to draw (render spec §7.2): an estimate in abstract
//! per-pixel operations, so a future adaptive preview can lower quality
//! before frames drop. Interface and measurements only — no scheduler yet.

use kadr_scene::{Effect, FrameScene, Layer, LayerContent, RectF, RenderQuality};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CostClass {
    Cheap,
    Medium,
    Heavy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cost {
    pub class: CostClass,
    /// Estimated per-pixel operations (one bilinear sample + blend ≈ 1).
    pub ops: f64,
}

impl Cost {
    pub const ZERO: Cost = Cost { class: CostClass::Cheap, ops: 0.0 };

    fn of(ops: f64, output_pixels: u64) -> Cost {
        // Relative to one full-frame sampled layer at this output size.
        let frames = ops / output_pixels.max(1) as f64;
        let class = if frames <= 1.5 {
            CostClass::Cheap
        } else if frames <= 4.0 {
            CostClass::Medium
        } else {
            CostClass::Heavy
        };
        Cost { class, ops }
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;
    fn add(self, o: Cost) -> Cost {
        Cost { class: self.class.max(o.class), ops: self.ops + o.ops }
    }
}

/// Relative cost of one effect per covered pixel. `quality` changes only
/// how closely a future neighbourhood effect is approximated.
fn effect_ops(e: &Effect, _quality: RenderQuality) -> f64 {
    match e {
        Effect::ColorAdjust(c) if c.is_neutral() => 0.0,
        Effect::ColorAdjust(_) => 0.5,
        // A primitive this estimate does not know yet: assume a full extra pass.
        _ => 1.0,
    }
}

/// Output pixels a layer's bounding box touches, clamped to the output.
fn covered_pixels(scene: &FrameScene, layer: &Layer) -> f64 {
    let (cw, ch) = (scene.canvas.w as f32, scene.canvas.h as f32);
    let b = layer.placement.canvas_bounds(&layer.crop).intersect(&RectF::new(0.0, 0.0, cw, ch));
    if b.is_empty() {
        return 0.0;
    }
    let k = (scene.output.size.w as f64 / cw as f64).min(scene.output.size.h as f64 / ch as f64);
    b.width() as f64 * b.height() as f64 * k * k
}

/// Cost of drawing `layer` into `scene`'s output.
pub fn layer_cost(scene: &FrameScene, layer: &Layer) -> Cost {
    let out_px = scene.output.size.w as u64 * scene.output.size.h as u64;
    let px = covered_pixels(scene, layer);
    let per_px = 1.0 + layer.effects.iter().map(|e| effect_ops(e, scene.output.quality)).sum::<f64>();
    let own = match &layer.content {
        LayerContent::Transition(t) => {
            let inner: f64 = t.from.iter().chain(&t.to).map(|l| layer_cost(scene, l).ops).sum();
            // Two buffers cleared, mixed and composited.
            inner + 3.0 * out_px as f64
        }
        LayerContent::Media { .. } | LayerContent::Solid(_) => px * per_px,
    };
    Cost::of(own, out_px)
}

/// Cost of the whole scene.
pub fn scene_cost(scene: &FrameScene) -> Cost {
    let out_px = scene.output.size.w as u64 * scene.output.size.h as u64;
    let ops = scene.layers.iter().map(|l| layer_cost(scene, l).ops).sum::<f64>() + out_px as f64 * 0.25;
    Cost::of(ops, out_px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;
    use kadr_scene::*;

    fn solid(canvas: SizeU, scale: f32) -> Layer {
        let mut placement = Placement::fill(canvas);
        placement.scale = Vec2::new(scale, scale);
        Layer { id: LayerId(1), content: LayerContent::Solid(Rgba::BLACK), crop: placement.full_crop(), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
    }

    #[test]
    fn cost_grows_with_covered_area_and_effects() {
        let canvas = SizeU::new(1920, 1080);
        let mut scene = FrameScene::empty(Time::ZERO, canvas, OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewFast));
        let full = layer_cost(&scene, &solid(canvas, 1.0));
        let quarter = layer_cost(&scene, &solid(canvas, 0.5));
        assert!((full.ops - 960.0 * 540.0).abs() < 1.0, "{full:?}");
        assert!((quarter.ops - full.ops / 4.0).abs() < 1.0, "{quarter:?}");
        let mut graded = solid(canvas, 1.0);
        graded.effects.push(Effect::ColorAdjust(ColorAdjust { saturation: 0.0, ..ColorAdjust::NEUTRAL }));
        assert!(layer_cost(&scene, &graded).ops > full.ops);
        for _ in 0..6 {
            scene.layers.push(solid(canvas, 1.0));
        }
        assert_eq!(scene_cost(&scene).class, CostClass::Heavy);
        assert_eq!(full.class, CostClass::Cheap);
    }
}
