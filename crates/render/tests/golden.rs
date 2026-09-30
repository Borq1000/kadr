//! Golden images: small scenes rendered by `CpuRenderer` and compared with
//! reviewed PNGs in `tests/golden/` within ±1 LSB per channel.
//! `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-render --test golden` (re)writes
//! them; look at every rewritten image before committing it.

use kadr_core::color::AlphaMode;
use kadr_core::{AssetId, ColorInfo, CpuFrame, Time};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::*;
use std::f32::consts::PI;
use std::path::PathBuf;
use std::sync::Arc;

const CANVAS: SizeU = SizeU::new(320, 180);
const OUT: SizeU = SizeU::new(160, 90);

fn frame(w: u32, h: u32, alpha: AlphaMode, px: impl Fn(f32, f32) -> [f32; 4]) -> Arc<CpuFrame> {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let bytes = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| px((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32).map(q)).collect();
    Arc::new(CpuFrame::from_rgba8(w, h, ColorInfo { alpha, ..ColorInfo::WORKING_SDR }, bytes))
}

/// "Video": a warm-to-cool diagonal gradient with a bright horizontal bar.
fn video() -> Arc<CpuFrame> {
    frame(320, 180, AlphaMode::Opaque, |x, y| {
        let bar = if (0.45..0.55).contains(&y) { 0.35 } else { 0.0 };
        [0.9 - 0.7 * x + bar, 0.2 + 0.5 * y + bar, 0.2 + 0.7 * x * (1.0 - y) + bar, 1.0]
    })
}

/// 8×8-cell checkerboard, green and white, with a red top-left cell to show orientation.
fn checker(w: u32, h: u32) -> Arc<CpuFrame> {
    frame(w, h, AlphaMode::Opaque, |x, y| {
        let (cx, cy) = ((x * 8.0) as u32, (y * 8.0) as u32);
        if cx == 0 && cy == 0 {
            [0.9, 0.1, 0.1, 1.0]
        } else if (cx + cy) % 2 == 0 {
            [0.1, 0.55, 0.25, 1.0]
        } else {
            [0.95, 0.95, 0.95, 1.0]
        }
    })
}

/// A straight-alpha "logo": a yellow disc with a soft edge and a dark ring, transparent outside.
fn logo() -> Arc<CpuFrame> {
    frame(96, 96, AlphaMode::Straight, |x, y| {
        let d = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt() * 2.0;
        let alpha = ((1.0 - d) / 0.15).clamp(0.0, 1.0);
        let ring = (0.55..0.7).contains(&d);
        if ring { [0.15, 0.1, 0.3, alpha] } else { [1.0, 0.85, 0.2, alpha] }
    })
}

fn placement(size: (f32, f32), position: (f32, f32), scale: f32, rotation: f32) -> Placement {
    Placement { size: Vec2::new(size.0, size.1), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(position.0, position.1), scale: Vec2::new(scale, scale), rotation }
}

fn layer(content: LayerContent, placement: Placement) -> Layer {
    Layer { id: LayerId(1), content, crop: placement.full_crop(), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
}

fn media(placement: Placement, alpha: AlphaMode) -> Layer {
    let size = SizeU::new(placement.size.x as u32, placement.size.y as u32);
    let media = MediaRef { media: AssetId::new(), stream: 0, kind: SourceKind::Video, display_size: size, color: ColorInfo { alpha, ..ColorInfo::WORKING_SDR } };
    layer(LayerContent::Media { media, source_time: Time::ZERO }, placement)
}

fn full(alpha: AlphaMode) -> Layer {
    media(Placement::fill(CANVAS), alpha)
}

fn solid(c: Rgba, placement: Placement) -> Layer {
    layer(LayerContent::Solid(c), placement)
}

fn rgb(r: f32, g: f32, b: f32) -> Rgba {
    Rgba { r, g, b, a: 1.0 }
}

fn transition(op: TransitionOp, progress: f32, from: Vec<Layer>, to: Vec<Layer>) -> Layer {
    layer(LayerContent::Transition(Box::new(TransitionLayer { op, progress, from, to })), Placement::fill(CANVAS))
}

fn scene(out: SizeU, layers: Vec<Layer>) -> FrameScene {
    FrameScene { time: Time::ZERO, canvas: CANVAS, output: OutputSpec::new(out, RenderQuality::Export), background: rgb(0.08, 0.08, 0.1), layers }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join(format!("{name}.png"))
}

fn read_png(path: &PathBuf) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::fs::File::open(path).ok()?);
    let mut reader = decoder.read_info().expect("golden PNG header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("golden PNG data");
    assert_eq!((info.color_type, info.bit_depth), (png::ColorType::Rgba, png::BitDepth::Eight), "{}: goldens are RGBA8", path.display());
    buf.truncate(info.buffer_size());
    Some((info.width, info.height, buf))
}

fn write_png(path: &PathBuf, w: u32, h: u32, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path).unwrap()), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(data).unwrap();
}

fn check(name: &str, scene: FrameScene, inputs: Vec<LayerInput>) {
    let (w, h) = (scene.output.size.w, scene.output.size.h);
    let mut out = vec![0u8; (w * h * 4) as usize];
    let inputs = RenderInputs { layers: inputs };
    CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(w, h, &mut out))).unwrap();
    let path = golden_path(name);
    if std::env::var_os("KADR_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        write_png(&path, w, h, &out);
        return;
    }
    let Some((gw, gh, golden)) = read_png(&path) else {
        panic!("no golden image {} — create it with KADR_UPDATE_GOLDEN=1 cargo test -p kadr-render --test golden, then look at it", path.display());
    };
    assert_eq!((gw, gh), (w, h), "{name}: golden size");
    let bad: Vec<_> = out.iter().zip(&golden).enumerate().filter(|(_, (a, b))| a.abs_diff(**b) > 1).collect();
    if let Some(&(i, (a, b))) = bad.first() {
        let p = i / 4;
        panic!("{name}: {} channels differ by more than 1 LSB; first at pixel ({}, {}) channel {}: rendered {a}, golden {b}", bad.len(), p as u32 % w, p as u32 / w, i % 4);
    }
}

#[test]
fn alpha_over() {
    let logo_layer = media(placement((96.0, 96.0), (220.0, 90.0), 1.25, 0.0), AlphaMode::Straight);
    check("alpha_over", scene(OUT, vec![full(AlphaMode::Opaque), logo_layer]), vec![LayerInput::Cpu(video()), LayerInput::Cpu(logo())]);
}

#[test]
fn multi_layer() {
    // Video, a picture-in-picture checkerboard top right with a slight tilt, a logo bottom left.
    let pip = media(placement((320.0, 180.0), (236.0, 52.0), 0.4, 0.05), AlphaMode::Opaque);
    let logo_layer = media(placement((96.0, 96.0), (60.0, 130.0), 0.8, 0.0), AlphaMode::Straight);
    let s = scene(OUT, vec![full(AlphaMode::Opaque), pip, logo_layer]);
    check("multi_layer", s, vec![LayerInput::Cpu(video()), LayerInput::Cpu(checker(320, 180)), LayerInput::Cpu(logo())]);
}

#[test]
fn transform_rotation() {
    // A 160×100 checkerboard (red cell top left) turned 30° clockwise about its centre.
    let l = media(placement((160.0, 100.0), (160.0, 90.0), 1.0, PI / 6.0), AlphaMode::Opaque);
    check("transform_rotation", scene(OUT, vec![l]), vec![LayerInput::Cpu(checker(160, 100))]);
}

#[test]
fn crop_in_place() {
    // A centred 200×120 checkerboard cropped to its right half and lower two thirds: the rest stays where it was.
    let mut l = media(placement((200.0, 120.0), (160.0, 90.0), 1.0, 0.0), AlphaMode::Opaque);
    l.crop = RectF::new(100.0, 40.0, 200.0, 120.0);
    let outline = solid(rgb(0.3, 0.3, 0.35), placement((204.0, 124.0), (160.0, 90.0), 1.0, 0.0));
    let hole = solid(rgb(0.08, 0.08, 0.1), placement((200.0, 120.0), (160.0, 90.0), 1.0, 0.0));
    check("crop_in_place", scene(OUT, vec![outline, hole, l]), vec![LayerInput::None, LayerInput::None, LayerInput::Cpu(checker(200, 120))]);
}

#[test]
fn opacity() {
    // The checkerboard at 30 % over the video.
    let mut c = full(AlphaMode::Opaque);
    c.opacity = 0.3;
    check("opacity", scene(OUT, vec![full(AlphaMode::Opaque), c]), vec![LayerInput::Cpu(video()), LayerInput::Cpu(checker(320, 180))]);
}

#[test]
fn blend_modes() {
    // One orange tile per quadrant over the video: Normal top left, Add top right, Multiply bottom left, Screen bottom right.
    let mut layers = vec![full(AlphaMode::Opaque)];
    let mut inputs = vec![LayerInput::Cpu(video())];
    for (n, blend) in [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen].into_iter().enumerate() {
        let (x, y) = (80.0 + 160.0 * (n % 2) as f32, 44.0 + 92.0 * (n / 2) as f32);
        let mut tile = solid(Rgba { r: 0.95, g: 0.5, b: 0.1, a: 0.9 }, placement((120.0, 60.0), (x, y), 1.0, 0.0));
        tile.blend = blend;
        tile.opacity = 0.85;
        layers.push(tile);
        inputs.push(LayerInput::None);
    }
    check("blend_modes", scene(OUT, layers), inputs);
}

fn two_clips(op: TransitionOp, progress: f32) -> (FrameScene, Vec<LayerInput>) {
    let t = transition(op, progress, vec![full(AlphaMode::Opaque)], vec![full(AlphaMode::Opaque)]);
    (scene(OUT, vec![t]), vec![LayerInput::Transition { from: vec![LayerInput::Cpu(video())], to: vec![LayerInput::Cpu(checker(320, 180))] }])
}

#[test]
fn dissolve_50() {
    let (s, i) = two_clips(TransitionOp::Dissolve, 0.5);
    check("dissolve_50", s, i);
}

#[test]
fn dip_to_black_25() {
    // Half-way into the dip: the video at half brightness.
    let (s, i) = two_clips(TransitionOp::DipToColor(Rgba::BLACK), 0.25);
    check("dip_to_black_25", s, i);
}

#[test]
fn wipe_40() {
    // Angle π: the edge moves right → left, the checkerboard (`to`) comes in from the right; 40 % of the sweep done, a 16 px soft band.
    let (s, i) = two_clips(TransitionOp::Wipe { angle: PI, softness: 16.0 }, 0.4);
    check("wipe_40", s, i);
}

#[test]
fn letterbox_margins() {
    // A 4:3 output of the 16:9 canvas: black bars above and below.
    let logo_layer = media(placement((96.0, 96.0), (220.0, 90.0), 1.25, 0.0), AlphaMode::Straight);
    check("letterbox_margins", scene(SizeU::new(160, 120), vec![full(AlphaMode::Opaque), logo_layer]), vec![LayerInput::Cpu(video()), LayerInput::Cpu(logo())]);
}

#[test]
fn missing_media() {
    // The picture-in-picture clip is offline: a dark red rectangle in its place.
    let pip = media(placement((320.0, 180.0), (220.0, 120.0), 0.4, 0.0), AlphaMode::Opaque);
    check("missing_media", scene(OUT, vec![full(AlphaMode::Opaque), pip]), vec![LayerInput::Cpu(video()), LayerInput::Missing(MissingReason::Offline)]);
}
