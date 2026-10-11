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

/// Relative cost of one effect per covered pixel (one bilinear sample +
/// blend ≈ 1). Point operations run fused into the layer's pass, so each
/// costs less than a pass (refinement 11); `quality` will only change how
/// closely a future neighbourhood effect is approximated. Calibrated on the
/// E2 bench (`weights_follow_the_bench`): each primitive alone on a
/// full-frame sampled layer, release, 1080p and 4K agreeing.
fn effect_ops(e: &Effect, _quality: RenderQuality) -> f64 {
    if e.is_neutral() {
        return 0.0;
    }
    match e {
        Effect::ColorAdjust(_) => 0.2,
        Effect::ToneCurve(_) => 0.45,
        Effect::SplitTone(_) => 0.25,
        Effect::Vignette(_) => 0.4,
        Effect::Grain(_) => 0.5,
        // A primitive this estimate does not know yet: assume a full extra pass.
        _ => 1.0,
    }
}

/// Per-pixel operations of a layer's effect chain. The renderer folds a run
/// of corrections (neutral primitives between them skipped) into one affine
/// op, so the run costs one correction.
fn chain_ops(effects: &[Effect], quality: RenderQuality) -> f64 {
    let mut after_adjust = false;
    effects
        .iter()
        .filter(|e| !e.is_neutral())
        .map(|e| {
            let adjust = matches!(e, Effect::ColorAdjust(_));
            let folded = adjust && after_adjust;
            after_adjust = adjust;
            if folded { 0.0 } else { effect_ops(e, quality) }
        })
        .sum()
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
    let per_px = 1.0 + chain_ops(&layer.effects, scene.output.quality);
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

    /// Refinement 11: a colour primitive costs less than a pass (it is fused
    /// into the layer's one); a neutral one nothing; a full Look on a full
    /// frame is Medium.
    #[test]
    fn colour_primitives_cost_less_than_a_pass_and_neutral_ones_nothing() {
        let canvas = SizeU::new(1920, 1080);
        let scene = FrameScene::empty(Time::ZERO, canvas, OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewHigh));
        let plain = layer_cost(&scene, &solid(canvas, 1.0)).ops;
        let extra = |e: Effect| {
            let mut l = solid(canvas, 1.0);
            l.effects.push(e);
            layer_cost(&scene, &l).ops - plain
        };
        let live = [
            Effect::ToneCurve(ToneCurve { points: vec![(0.0, 0.1), (1.0, 0.9)] }),
            Effect::SplitTone(SplitTone { shadow_amount: 0.2, ..SplitTone::NEUTRAL }),
            Effect::Vignette(Vignette { amount: 0.3, midpoint: 0.45, feather: 0.5 }),
            Effect::Grain(Grain { amount: 0.05, size: 2.0, seed: 1 }),
        ];
        for e in live.clone() {
            let x = extra(e.clone());
            assert!(x > 0.0 && x < plain, "{e:?}: {x}");
        }
        for e in [Effect::ToneCurve(ToneCurve::identity()), Effect::SplitTone(SplitTone::NEUTRAL), Effect::Vignette(Vignette { amount: 0.0, midpoint: 0.5, feather: 0.5 }), Effect::Grain(Grain { amount: 0.0, size: 1.0, seed: 1 })] {
            assert_eq!(extra(e.clone()), 0.0, "{e:?}");
        }
        let mut look = solid(canvas, 1.0);
        look.effects = live.to_vec();
        look.effects.insert(0, Effect::ColorAdjust(ColorAdjust { saturation: 0.9, ..ColorAdjust::NEUTRAL }));
        assert_eq!(layer_cost(&scene, &look).class, CostClass::Medium);
    }

    /// The renderer folds consecutive corrections — the inspector's and a
    /// Look's, with neutral primitives between them skipped — into one
    /// affine op: the run costs one correction; a live primitive between
    /// two corrections splits the run.
    #[test]
    fn a_run_of_corrections_costs_one() {
        let canvas = SizeU::new(1920, 1080);
        let scene = FrameScene::empty(Time::ZERO, canvas, OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewHigh));
        let with = |effects: Vec<Effect>| {
            let mut l = solid(canvas, 1.0);
            l.effects = effects;
            layer_cost(&scene, &l).ops
        };
        let adjust = |saturation: f32| Effect::ColorAdjust(ColorAdjust { saturation, ..ColorAdjust::NEUTRAL });
        let curve = Effect::ToneCurve(ToneCurve { points: vec![(0.0, 0.1), (1.0, 0.9)] });
        let one = with(vec![adjust(0.5)]);
        assert!(one > with(vec![]));
        assert_eq!(with(vec![adjust(0.5), adjust(0.9)]), one, "two corrections: one affine op");
        assert_eq!(with(vec![adjust(0.5), Effect::ToneCurve(ToneCurve::identity()), adjust(0.9)]), one, "a neutral curve between them is skipped");
        assert_eq!(with(vec![adjust(0.5), adjust(1.0), adjust(0.9)]), one);
        let split = with(vec![adjust(0.5), curve.clone(), adjust(0.9)]);
        let two = with(vec![adjust(0.5), curve.clone()]) + (one - with(vec![]));
        assert!((split - two).abs() < 1e-6 * two, "a live curve between them: two ops, {split} vs {two}");
        assert_eq!(with(vec![adjust(0.5), adjust(0.9), curve.clone()]), with(vec![adjust(0.5), curve]));
    }

    /// The E2 bench (release, Винтаж's primitives each alone on a
    /// full-frame sampled 1080p / 4K layer, against the sampled layer
    /// without effects — both sizes agree): the correction adds ≈ 0.16 of a
    /// sampled layer, the curve ≈ 0.43, the split-tone ≈ 0.27, the vignette
    /// ≈ 0.42, the grain ≈ 0.53; the fused Look draws in ≈ 2.3 layers' time.
    /// Every weight stays within 2× of its measurement (the correction's
    /// 0.5 before E2 was 3× off).
    #[test]
    fn weights_follow_the_bench() {
        let q = RenderQuality::Export;
        let measured = [
            (Effect::ColorAdjust(ColorAdjust { saturation: 0.85, ..ColorAdjust::NEUTRAL }), 0.16),
            (Effect::ToneCurve(ToneCurve { points: vec![(0.0, 0.1), (0.5, 0.52), (1.0, 0.92)] }), 0.43),
            (Effect::SplitTone(SplitTone { shadow_amount: 0.2, ..SplitTone::NEUTRAL }), 0.27),
            (Effect::Vignette(Vignette { amount: 0.3, midpoint: 0.45, feather: 0.5 }), 0.42),
            (Effect::Grain(Grain { amount: 0.05, size: 2.0, seed: 1 }), 0.53),
        ];
        let within_2x = |w: f64, m: f64| w <= 2.0 * m && m <= 2.0 * w;
        for (e, m) in &measured {
            let w = effect_ops(e, q);
            assert!(within_2x(w, *m), "{e:?}: weight {w}, measured {m}");
        }
        let look = 1.0 + measured.iter().map(|(e, _)| effect_ops(e, q)).sum::<f64>();
        assert!(within_2x(look, 2.3), "a full Look: {look} layers predicted, 2.3 measured");
    }
}
