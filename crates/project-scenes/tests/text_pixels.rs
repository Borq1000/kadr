//! Text clips end to end (titles spec §5, §9): project → evaluator →
//! resolver (the text picture draws itself) → CPU renderer. The preview's
//! frame equals the export's; a 4K export is drawn at 4K, not upscaled; a
//! font this machine lacks still renders and is kept; the text block's
//! centre lands where the transform puts it; what is not text shows what
//! lies below. Goldens hold bundled fonts only (system emoji vary by
//! machine: they get a tolerant check).

use kadr_core::perf::FramePerf;
use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_playback::testing::{FakeDecoders, FakeMedia};
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

/// A 60 s 1080p video (a temporary file, so it is online) on a new
/// bottom video track, decoded by the fake as a flat colour (R = 70, G =
/// the frame index, B = 0).
fn with_background(p: &mut Project, dir: &tempfile::TempDir) {
    let path = dir.path().join("bg.mp4");
    std::fs::write(&path, b"x").unwrap();
    let v = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new(path, MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(v), audio: None, timecode: None });
    let seq = p.sequence_mut();
    seq.tracks.insert(0, Track::new(TrackKind::Video, "V0"));
    seq.tracks[0].clips.push(Clip::new(a.id, "bg", TimeRange::new(Time::ZERO, Time::from_secs(5)), Time::ZERO));
    p.assets.push(a);
}

fn render(p: &Project, bypass: bool, out: SizeU, mode: Mode) -> Vec<u8> {
    let scenes = ProjectScenes::new(p, bypass);
    let fake = FakeDecoders::new(Duration::ZERO, Duration::ZERO);
    for a in &p.assets {
        fake.add(&a.path, FakeMedia { tag: 70, ..Default::default() });
    }
    let r = Resolver::new(Arc::new(fake), ResolverConfig::default());
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

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

/// The bounding box `(x0, y0, x1, y1)` (ends exclusive) of the pixels `f` picks.
fn bbox(px: &[u8], w: u32, f: impl Fn(&[u8; 4]) -> bool) -> (u32, u32, u32, u32) {
    let mut b = (u32::MAX, u32::MAX, 0, 0);
    for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
        if f(p) {
            let (x, y) = (i as u32 % w, i as u32 / w);
            b = (b.0.min(x), b.1.min(y), b.2.max(x + 1), b.3.max(y + 1));
        }
    }
    b
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
    let p = project(TextDoc::starter("Привет, мир! Hello", 1080), 1920, 1080);
    let out = SizeU::new(480, 270);
    let preview = render(&p, false, out, Mode::Scrub { generation: 1 });
    assert!(lit(&preview) > 200, "white letters on black");
    assert_eq!(preview, render(&p, false, out, Mode::Export), "one picture: preview and export");
    golden::check("text_title_quarter", &preview, out);
}

/// Colour emoji come from the system's emoji font (titles spec §5.1), so
/// they are checked loosely, not by a golden: in colour, after the word.
#[test]
fn an_emoji_draws_in_colour() {
    let p = project(TextDoc::starter("Гол 🎉", 1080), 1920, 1080);
    let out = SizeU::new(480, 270);
    let px = render(&p, false, out, Mode::Export);
    assert_eq!(px, render(&p, false, out, Mode::Scrub { generation: 1 }), "preview = export");
    let colourful = |p: &[u8; 4]| p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap() > 80;
    let count = px.as_chunks::<4>().0.iter().filter(|p| colourful(p)).count();
    assert!(count > 40, "coloured pixels: {count}");
    let (x0, _, x1, _) = bbox(&px, out.w, |p| p[0] > 128 || colourful(p));
    let (cx0, _, cx1, _) = bbox(&px, out.w, colourful);
    assert!(cx0 > (x0 + x1) / 2 && cx1 <= x1, "the colour is the emoji's, after the word: {cx0}..{cx1} in {x0}..{x1}");
}

/// M1: the text block — framed here by a block plaque — is centred on the
/// canvas centre + (x, y), whatever its lines; a wrapped left- or
/// right-aligned block keeps its box (the wrap width) there, its lines
/// flush with the box's edge.
#[test]
fn the_text_block_lands_where_the_transform_puts_it() {
    let (px, py) = (30.0f32, 12.0f32);
    let plaque = Plaque { per_line: false, colour: Rgba8::rgba(0, 80, 255, 255), radius: 0.0, padding_x: px, padding_y: py };
    let wrapped = |align: Align| {
        let mut d = TextDoc::starter("Длинный заголовок на несколько строк подряд", 720);
        (d.paragraph.wrap, d.paragraph.align) = (Some(500.0), align);
        d
    };
    let cases = [
        ("one line", TextDoc::starter("Гол", 720), None),
        ("two lines", TextDoc::starter("Гол\nи ещё строка", 720), None),
        ("left", wrapped(Align::Left), Some(Align::Left)),
        ("right", wrapped(Align::Right), Some(Align::Right)),
    ];
    for (name, mut doc, align) in cases {
        doc.style.plaque = Some(plaque);
        let mut p = project(doc.clone(), 1280, 720);
        let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
        (tr.x, tr.y) = (150.0, -100.0);
        let out = SizeU::new(1280, 720);
        let frame = render(&p, false, out, Mode::Export);
        let b = bbox(&frame, out.w, |p| p[0] < 100 && p[2] > 128);
        let (cx, cy) = (640.0 + 150.0, 360.0 - 100.0);
        let lh = kadr_text::layout(&doc).height as f64;
        let near = |a: f64, b: f64| (a - b).abs() <= 1.0;
        // The plaque spans the block and its padding (± its anti-aliased rows).
        assert!(near((b.1 + b.3) as f64 / 2.0, cy) && ((b.3 - b.1) as f64 - (lh + 2.0 * py as f64)).abs() <= 3.0, "{name}: rows {}..{} (the block is {lh} px high)", b.1, b.3);
        let ok = match align {
            None => near((b.0 + b.2) as f64 / 2.0, cx),
            Some(Align::Left) => near(b.0 as f64, cx - 250.0 - px as f64),
            _ => near(b.2 as f64, cx + 250.0 + px as f64),
        };
        assert!(ok, "{name}: columns {}..{} (centre {cx})", b.0, b.2);
    }
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

/// Rich runs, a coloured outline and the shadow over a picture: between
/// the letters the picture below shows (the raster is transparent there).
#[test]
fn rich_text_full_resolution_golden() {
    let mut doc = TextDoc::starter("Счёт 2:1 — победа!", 1080);
    doc.style_range(9..12, &RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 210, 0, 255))), size: Some(130.0), ..Default::default() });
    doc.style.outlines = vec![Outline { width: 5.0, colour: Rgba8::rgba(0, 80, 255, 255) }];
    let mut p = project(doc.clone(), 1920, 1080);
    let dir = tempfile::tempdir().unwrap();
    with_background(&mut p, &dir);
    let out = SizeU::new(960, 540);
    let px = render(&p, false, out, Mode::Export);
    let bg = pixel(&px, out.w, 5, 5);
    assert!(bg[0] > 40 && bg[1] > 10 && bg[2] < 10, "the picture below: {bg:?}");
    // The middle of the widest gap between two glyphs of the line, half
    // way down the line: inside the text's bounds, outside its ink.
    let laid = kadr_text::layout(&doc);
    let mut boxes: Vec<(f32, f32)> = laid.glyphs.iter().filter(|g| g.visible).map(|g| (g.x, g.x + g.advance)).collect();
    boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let gap = boxes.windows(2).map(|w| (w[0].1, w[1].0)).max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0))).unwrap();
    assert!(gap.1 - gap.0 > 15.0, "a word gap: {gap:?}");
    let line = &laid.lines[0];
    let (gx, gy) = ((gap.0 + gap.1) / 2.0 - laid.width / 2.0, (line.top + line.bottom) / 2.0 - laid.height / 2.0);
    let (x, y) = (((960.0 + gx) / 2.0) as u32, ((540.0 + gy) / 2.0) as u32);
    let between = pixel(&px, out.w, x, y);
    assert!(between.iter().zip(bg).all(|(a, b)| a.abs_diff(b) <= 12), "between the letters at ({x}, {y}): {between:?}, the picture: {bg:?}");
    golden::check("text_rich_full", &px, out);
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
