//! M2: `CpuRenderer` cost at 1080p and 4K for 1, 2 and 3 layers and for a
//! transition alone, plus frame-sized allocations per frame after warm-up.

use crate::alloc;
use crate::report::Report;
use kadr_core::color::AlphaMode;
use kadr_core::perf::Stats;
use kadr_core::{AssetId, ColorInfo, CpuFrame, Time};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::*;
use std::sync::Arc;
use std::time::Instant;

const WARMUP: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    OneLayer,
    OneLayerScaled,
    TwoLayers,
    ThreeLayers,
    Transition,
}

impl Row {
    const ALL: [Row; 5] = [Row::OneLayer, Row::OneLayerScaled, Row::TwoLayers, Row::ThreeLayers, Row::Transition];

    fn name(self) -> &'static str {
        match self {
            Row::OneLayer => "1 layer",
            Row::OneLayerScaled => "1 layer scaled",
            Row::TwoLayers => "2 layers",
            Row::ThreeLayers => "3 layers",
            Row::Transition => "transition",
        }
    }
}

/// Deterministic pseudo-noise (xorshift): frames are not pure gradients.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u8
    }
}

/// An opaque "video" frame: diagonal gradients plus a little noise.
fn video_frame(w: u32, h: u32, seed: u64) -> Arc<CpuFrame> {
    let mut noise = Noise(seed);
    let mut data = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let n = (noise.next() >> 5) as f32; // 0..7
            data.extend_from_slice(&[(fx * 240.0 + n) as u8, (fy * 240.0 + n) as u8, ((1.0 - fx * fy) * 240.0 + n) as u8, 255]);
        }
    }
    Arc::new(CpuFrame::from_rgba8(w, h, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, data))
}

/// A straight-alpha logo: a disc with a soft edge, colour varying across it.
fn logo_frame(size: u32) -> Arc<CpuFrame> {
    let mut data = Vec::with_capacity(size as usize * size as usize * 4);
    let r = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
            let d = (dx * dx + dy * dy).sqrt() / r;
            let alpha = ((1.0 - d) / 0.15).clamp(0.0, 1.0); // soft over the outer 15 %
            data.extend_from_slice(&[(255.0 * (1.0 - y as f32 / size as f32)) as u8, 200, (255.0 * x as f32 / size as f32) as u8, (alpha * 255.0 + 0.5) as u8]);
        }
    }
    Arc::new(CpuFrame::from_rgba8(size, size, ColorInfo { alpha: AlphaMode::Straight, ..ColorInfo::WORKING_SDR }, data))
}

/// Everything a bench scene needs, built once per output size.
struct Assets {
    size: SizeU,
    video: Arc<CpuFrame>,
    video2: Arc<CpuFrame>,
    logo: Arc<CpuFrame>,
}

impl Assets {
    fn new(w: u32, h: u32, logo: u32) -> Self {
        Assets { size: SizeU::new(w, h), video: video_frame(w, h, 0x9E37_79B9_7F4A_7C15), video2: video_frame(w, h, 0xD1B5_4A32_D192_ED03), logo: logo_frame(logo) }
    }
}

fn media_layer(id: u128, frame: &Arc<CpuFrame>, placement: Placement) -> (Layer, LayerInput) {
    let media = MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Video, display_size: SizeU::new(frame.width, frame.height), color: frame.color };
    let layer = Layer { id: LayerId(id), content: LayerContent::Media { media, source_time: Time::ZERO }, crop: placement.full_crop(), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] };
    (layer, LayerInput::Cpu(frame.clone()))
}

/// The scene and inputs of one bench row. Canvas = output = the video frame size.
fn build(row: Row, a: &Assets) -> (FrameScene, RenderInputs) {
    let canvas = a.size;
    let fill = Placement::fill(canvas);
    let (w, h) = (canvas.w as f32, canvas.h as f32);
    let mut layers = vec![];
    let mut inputs = vec![];
    let mut add = |(l, i): (Layer, LayerInput)| {
        layers.push(l);
        inputs.push(i);
    };
    match row {
        Row::OneLayer => add(media_layer(1, &a.video, fill)),
        Row::OneLayerScaled => add(media_layer(1, &a.video, Placement { scale: Vec2::new(0.98, 0.98), ..fill })),
        Row::TwoLayers | Row::ThreeLayers => {
            add(media_layer(1, &a.video, fill));
            add(media_layer(2, &a.video2, Placement { scale: Vec2::new(0.3, 0.3), rotation: 5.0_f32.to_radians(), position: Vec2::new(w * 0.75, h * 0.72), ..fill }));
            if row == Row::ThreeLayers {
                // The logo at scale 0.5 in the top-left corner, 5 % of the canvas height from the edges.
                let s = a.logo.width as f32;
                let half = s * 0.5 / 2.0;
                let p = Placement { size: Vec2::new(s, s), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(h * 0.05 + half, h * 0.05 + half), scale: Vec2::new(0.5, 0.5), rotation: 0.0 };
                add(media_layer(3, &a.logo, p));
            }
        }
        Row::Transition => {
            let (from, from_in) = media_layer(1, &a.video, fill);
            let (to, to_in) = media_layer(2, &a.video2, fill);
            let t = TransitionLayer { op: TransitionOp::Dissolve, progress: 0.5, from: vec![from], to: vec![to] };
            let layer = Layer { id: LayerId(3), content: LayerContent::Transition(Box::new(t)), crop: fill.full_crop(), placement: fill, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] };
            add((layer, LayerInput::Transition { from: vec![from_in], to: vec![to_in] }));
        }
    }
    let scene = FrameScene { time: Time::ZERO, canvas, output: OutputSpec::new(canvas, RenderQuality::Export), background: Rgba::BLACK, layers };
    (scene, RenderInputs { layers: inputs })
}

fn render_once(renderer: &mut CpuRenderer, frame: &PreparedFrame, out: &mut [u8], size: SizeU) -> Result<(), String> {
    renderer.render(frame, &mut RenderTarget::Cpu(CpuTarget::packed(size.w, size.h, out))).map(|_| ()).map_err(|e| e.to_string())
}

/// One output size: every row into one reused output buffer.
fn bench_size(r: &mut Report, scenario: &str, a: &Assets, frames: usize) -> Result<(), String> {
    let mut out = vec![0u8; a.size.w as usize * a.size.h as usize * 4];
    let mut renderer = CpuRenderer::new();
    for row in Row::ALL {
        let (scene, inputs) = build(row, a);
        let prepared = PreparedFrame { scene: &scene, inputs: &inputs };
        for _ in 0..WARMUP {
            render_once(&mut renderer, &prepared, &mut out, a.size)?;
        }
        let (allocs_before, pool_before) = (alloc::large_allocs(), renderer.pool_allocations());
        let mut samples = Vec::with_capacity(frames);
        for _ in 0..frames {
            let started = Instant::now();
            render_once(&mut renderer, &prepared, &mut out, a.size)?;
            samples.push(started.elapsed());
        }
        let (allocs, pool) = (alloc::large_allocs() - allocs_before, renderer.pool_allocations() - pool_before);
        std::hint::black_box(&out);
        r.stats(scenario, row.name(), &Stats::of(samples));
        r.push(scenario, row.name(), "frame_allocs_per_frame", allocs as f64 / frames as f64, "n");
        r.push(scenario, row.name(), "pool_allocs_delta", pool as f64, "n");
    }
    Ok(())
}

pub fn run() -> Result<Report, String> {
    let mut r = Report::new("m2-render");
    bench_size(&mut r, "render 1080p", &Assets::new(1920, 1080, 512), 120)?;
    bench_size(&mut r, "render 4K", &Assets::new(3840, 2160, 512), 60)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows are named by their layer counts: keep the bench scenes honest.
    #[test]
    fn bench_scenes_have_the_advertised_layers() {
        let a = Assets::new(64, 36, 16);
        let is_media = |l: &Layer| matches!(l.content, LayerContent::Media { .. });
        for (row, count) in [(Row::OneLayer, 1), (Row::OneLayerScaled, 1), (Row::TwoLayers, 2), (Row::ThreeLayers, 3), (Row::Transition, 1)] {
            let (scene, inputs) = build(row, &a);
            assert_eq!(scene.layers.len(), count, "{row:?}");
            assert_eq!(inputs.layers.len(), count, "{row:?} inputs are parallel");
            if row == Row::Transition {
                let LayerContent::Transition(t) = &scene.layers[0].content else { panic!("the transition row holds a transition layer") };
                assert_eq!((t.op, t.progress, t.from.len(), t.to.len()), (TransitionOp::Dissolve, 0.5, 1, 1));
                assert!(t.from.iter().chain(&t.to).all(is_media));
                assert!(matches!(&inputs.layers[0], LayerInput::Transition { from, to } if from.len() == 1 && to.len() == 1));
            } else {
                assert!(scene.layers.iter().all(is_media), "{row:?}");
            }
        }
    }

    #[test]
    fn geometry_and_frame_kinds_match_the_row_names() {
        let a = Assets::new(64, 36, 16);
        let (fast, _) = build(Row::OneLayer, &a);
        let (scaled, _) = build(Row::OneLayerScaled, &a);
        assert_eq!(fast.layers[0].placement, Placement::fill(a.size));
        assert_eq!(scaled.layers[0].placement.scale, Vec2::new(0.98, 0.98));
        let (three, _) = build(Row::ThreeLayers, &a);
        assert_eq!(three.layers[1].placement.scale, Vec2::new(0.3, 0.3));
        assert_eq!(three.layers[2].placement.scale, Vec2::new(0.5, 0.5));
        assert_eq!(a.logo.color.alpha, AlphaMode::Straight);
        assert_eq!(a.video.color.alpha, AlphaMode::Opaque);
    }

    #[test]
    fn a_small_run_reports_every_metric_of_every_row() {
        let a = Assets::new(64, 36, 16);
        let mut r = Report::new("t");
        bench_size(&mut r, "s", &a, 3).unwrap();
        assert_eq!(r.rows.len(), Row::ALL.len() * 6, "count, p50, p90, max, allocs per frame, pool delta");
    }
}
