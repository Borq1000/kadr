//! Text clips end to end (titles spec §5, §9): project → evaluator →
//! resolver (the text picture draws itself) → CPU renderer. The preview's
//! frame equals the export's; a 4K export is drawn at 4K, not upscaled; a
//! font this machine lacks still renders and is kept.

use kadr_core::perf::FramePerf;
use kadr_core::{FrameRate, Time};
use kadr_playback::testing::FakeDecoders;
use kadr_playback::{Mode, Resolver, ResolverConfig, SceneSource};
use kadr_project::*;
use kadr_project_scenes::ProjectScenes;
use kadr_render::{CpuRenderer, CpuTarget, PreparedFrame, RenderTarget, Renderer};
use kadr_scene::{LayerContent, OutputSpec, RenderQuality, SizeU};
use std::sync::Arc;
use std::time::Duration;

fn project(doc: TextDoc, w: u32, h: u32) -> Project {
    let mut p = Project::new("text");
    let seq = p.sequence_mut();
    (seq.frame_rate, seq.width, seq.height) = (FrameRate::FPS_25, w, h);
    seq.tracks[0].clips.push(Clip::new_text(doc, Time::ZERO, Time::from_secs(5)));
    p
}

fn render(p: &Project, bypass: bool, out: SizeU, mode: Mode) -> Vec<u8> {
    let scenes = ProjectScenes::new(p, bypass);
    let r = Resolver::new(Arc::new(FakeDecoders::new(Duration::ZERO, Duration::ZERO)), ResolverConfig::default());
    let quality = if mode == Mode::Export { RenderQuality::Export } else { RenderQuality::PreviewHigh };
    let scene = scenes.scene_at(Time::from_secs(1), &OutputSpec::new(out, quality));
    let inputs = r.prepare(&scene, &scenes, mode, &mut FramePerf::default());
    let mut buf = vec![0u8; (out.w * out.h * 4) as usize];
    CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(out.w, out.h, &mut buf))).unwrap();
    buf
}

fn lit(px: &[u8]) -> usize {
    px.as_chunks::<4>().0.iter().filter(|p| p[0] > 128).count()
}

/// Σ of squared horizontal steps of the red channel: a sharper edge of
/// the same height scores higher (a step of H over one pixel: H²; over two: H²/2).
fn edge_energy(px: &[u8], w: u32) -> f64 {
    px.chunks_exact(w as usize * 4).map(|row| row.as_chunks::<4>().0.windows(2).map(|p| (p[1][0] as f64 - p[0][0] as f64).powi(2)).sum::<f64>()).sum()
}

/// 2× bilinear upscale (texel centres), RGBA.
fn upscale2(px: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w2, h2) = (w * 2, h * 2);
    let mut out = vec![0u8; (w2 * h2 * 4) as usize];
    let at = |x: i64, y: i64, c: usize| px[((y.clamp(0, h as i64 - 1) as u32 * w + x.clamp(0, w as i64 - 1) as u32) * 4) as usize + c] as f64;
    for y in 0..h2 {
        for x in 0..w2 {
            let (u, v) = ((x as f64 + 0.5) / 2.0 - 0.5, (y as f64 + 0.5) / 2.0 - 0.5);
            let (i, j) = (u.floor() as i64, v.floor() as i64);
            let (f, g) = (u - i as f64, v - j as f64);
            for c in 0..4 {
                let val = at(i, j, c) * (1.0 - f) * (1.0 - g) + at(i + 1, j, c) * f * (1.0 - g) + at(i, j + 1, c) * (1.0 - f) * g + at(i + 1, j + 1, c) * f * g;
                out[((y * w2 + x) * 4) as usize + c] = val.round() as u8;
            }
        }
    }
    out
}

#[test]
fn a_title_renders_and_the_preview_matches_the_export() {
    let p = project(TextDoc::starter("Привет, мир! 🎉", 1080), 1920, 1080);
    let out = SizeU::new(480, 270);
    let preview = render(&p, false, out, Mode::Scrub { generation: 1 });
    assert!(lit(&preview) > 200, "white letters on black");
    assert_eq!(preview, render(&p, false, out, Mode::Export), "one picture: preview and export");
    golden::check("text_title_quarter", &preview, out);
}

#[test]
fn a_4k_export_of_a_title_is_sharper_than_an_upscaled_1080p_one() {
    let mut doc = TextDoc::starter("Чёткость", 360);
    doc.style.shadow = None;
    let p = project(doc, 640, 360);
    let native = render(&p, false, SizeU::new(1280, 720), Mode::Export);
    let up = upscale2(&render(&p, false, SizeU::new(640, 360), Mode::Export), 640, 360);
    let (n, u) = (edge_energy(&native, 1280), edge_energy(&up, 1280));
    assert!(n > 1.4 * u, "drawn at the output size, not stretched: {n:.0} vs {u:.0}");
}

#[test]
fn a_missing_font_renders_and_round_trips() {
    let mut doc = TextDoc::starter("Шрифт", 1080);
    doc.style.font.family = "Nope Sans".into();
    let p = project(doc.clone(), 1920, 1080);
    let json = kadr_project::io::to_json(&p);
    let back = kadr_project::io::parse(&json, std::path::Path::new("m.kadr")).unwrap();
    assert!(lit(&render(&back, false, SizeU::new(480, 270), Mode::Export)) > 100, "drawn in the default font");
    assert_eq!(kadr_text::layout(&doc).missing, ["Nope Sans"], "the inspector can say so");
    let saved = String::from_utf8(kadr_project::io::to_json(&back)).unwrap();
    assert!(saved.contains("\"family\": \"Nope Sans\"") || saved.contains("\"family\":\"Nope Sans\""), "the name is kept");
}

#[test]
fn before_view_keeps_the_text_and_drops_its_effects() {
    let mut p = project(TextDoc::starter("До", 1080), 1920, 1080);
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    c.color.exposure = 1.0;
    let mut look = AppliedEffect::new("look", 1, Default::default());
    look.params.insert("preset".into(), ParamValue::Choice("vintage".into()));
    c.effects.push(look);
    let scenes = ProjectScenes::new(&p, true);
    let scene = scenes.scene_at(Time::from_secs(1), &OutputSpec::new(SizeU::new(480, 270), RenderQuality::PreviewHigh));
    assert_eq!(scene.layers.len(), 1);
    assert!(matches!(scene.layers[0].content, LayerContent::Text(_)) && scene.layers[0].effects.is_empty());
    assert_eq!(scenes.top_clip(&scene), Some(p.sequence().tracks[0].clips[0].id), "the preview names the text clip");
}

#[test]
fn rich_text_full_resolution_golden() {
    let mut doc = TextDoc::starter("Счёт 2:1 — победа!", 1080);
    doc.style_range(9..12, &RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 210, 0, 255))), size: Some(130.0), ..Default::default() });
    doc.style.outlines = vec![Outline { width: 5.0, colour: Rgba8::BLACK }];
    let p = project(doc, 1920, 1080);
    let out = SizeU::new(960, 540);
    golden::check("text_rich_full", &render(&p, false, out, Mode::Export), out);
}

mod golden {
    use kadr_scene::SizeU;

    /// `tests/golden/<name>.png` within ±1 LSB; `KADR_UPDATE_GOLDEN=1` rewrites it (look at it).
    pub fn check(name: &str, px: &[u8], size: SizeU) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.png"));
        if std::env::var("KADR_UPDATE_GOLDEN").as_deref() == Ok("1") {
            let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()), size.w, size.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(px).unwrap();
            return;
        }
        let file = std::fs::File::open(&path).unwrap_or_else(|_| panic!("{} missing: KADR_UPDATE_GOLDEN=1, then look at it", path.display()));
        let mut dec = png::Decoder::new(file).read_info().unwrap();
        let mut want = vec![0u8; dec.output_buffer_size()];
        dec.next_frame(&mut want).unwrap();
        let bad = px.iter().zip(&want).filter(|(a, b)| a.abs_diff(**b) > 1).count();
        assert_eq!(bad, 0, "{name}: {bad} bytes off by more than 1");
    }
}
