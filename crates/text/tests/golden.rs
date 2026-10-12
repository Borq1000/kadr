//! Golden images of text rasters (titles spec §9): each composited over an
//! opaque #303040 so it can be looked at, compared within ±1 LSB with
//! `tests/golden/<name>.png`. `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-text
//! --test golden` (re)writes them; look at every rewritten image before
//! committing it. Fallback glyphs (emoji, CJK) come from this machine's
//! system fonts (Windows 11, Segoe UI Emoji).

use kadr_project::*;
use kadr_text::{Clock, TextFrame};

const BG: [f32; 3] = [48.0, 48.0, 64.0];

pub fn picture(frame: &TextFrame, scale: f32) -> (u32, u32, Vec<u8>) {
    let r = frame.raster(scale);
    let mut out = Vec::with_capacity((r.width * r.height * 4) as usize);
    for y in 0..r.height {
        for p in r.row(y).as_chunks::<4>().0 {
            let a = p[3] as f32 / 255.0;
            out.extend((0..3).map(|c| (p[c] as f32 + BG[c] * (1.0 - a)).round().clamp(0.0, 255.0) as u8));
            out.push(255);
        }
    }
    (r.width, r.height, out)
}

pub fn check(name: &str, doc: &TextDoc, clock: Clock, scale: f32) {
    check_px(name, picture(&TextFrame::new(doc, clock), scale));
}

/// Compares (or with `KADR_UPDATE_GOLDEN=1` writes) an opaque RGBA picture.
fn check_px(name: &str, (w, h, px): (u32, u32, Vec<u8>)) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.png"));
    if std::env::var("KADR_UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        return;
    }
    let file = std::fs::File::open(&path).unwrap_or_else(|_| panic!("{} missing: run with KADR_UPDATE_GOLDEN=1 and look at it", path.display()));
    let mut dec = png::Decoder::new(file).read_info().unwrap();
    let mut want = vec![0u8; dec.output_buffer_size()];
    let info = dec.next_frame(&mut want).unwrap();
    assert_eq!((info.width, info.height), (w, h), "{name}: size");
    let bad = px.iter().zip(&want).filter(|(a, b)| a.abs_diff(**b) > 1).count();
    assert_eq!(bad, 0, "{name}: {bad} bytes differ by more than 1");
}

/// A starter title for a 540-line canvas, without its shadow (Task 9 draws
/// shadows; these goldens stay valid through it).
fn base(text: &str) -> TextDoc {
    let mut d = TextDoc::starter(text, 540);
    d.style.shadow = None;
    d.seed = 1;
    d
}

#[test]
fn text_goldens() {
    check("title_plain", &base("Привет, мир!"), Clock::REST, 1.0);
    let mut rich = base("Победа нашей команды");
    rich.style_range(13..23, &RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 200, 0, 255))), family: Some("Playfair Display".into()), size: Some(60.0), italic: Some(true), ..Default::default() });
    check("rich_runs", &rich, Clock::REST, 1.0);
    let mut outlined = base("ОБВОДКА");
    outlined.style.outlines = vec![Outline { width: 4.0, colour: Rgba8::BLACK }, Outline { width: 3.0, colour: Rgba8::rgba(255, 60, 60, 255) }];
    outlined.style.shadow = None;
    check("outlines_two", &outlined, Clock::REST, 1.0);
    let mut block = base("Плашка под блоком\nвторая строка");
    block.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::rgba(20, 20, 20, 200), radius: 12.0, padding_x: 24.0, padding_y: 10.0 });
    check("plaque_block", &block, Clock::REST, 1.0);
    let mut lines = block.clone();
    lines.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::rgba(255, 220, 0, 255), radius: 6.0, padding_x: 14.0, padding_y: 4.0 });
    lines.style.fill = Fill::solid(Rgba8::BLACK);
    lines.paragraph.align = Align::Left;
    check("plaque_lines", &lines, Clock::REST, 1.0);
    let mut wrapped = base("Длинный текст переносится по ширине блока и выравнивается по центру");
    wrapped.paragraph.wrap = Some(420.0);
    wrapped.style.size = 32.0;
    check("wrap_center", &wrapped, Clock::REST, 1.0);
    check("cyrillic_emoji", &base("Гол! ⚽🔥 Ура 🎉"), Clock::REST, 1.0);
    let mut caps = base("разрядка");
    caps.paragraph.caps = true;
    caps.paragraph.letter_spacing = 12.0;
    caps.style.font = FontRef::new("Oswald", 500, false);
    check("caps_spacing", &caps, Clock::REST, 1.0);
    check("title_plain_x2", &base("Привет, мир!"), Clock::REST, 2.0);
}

#[test]
fn look_goldens() {
    let mut grad = base("ГРАДИЕНТ");
    grad.style.font = FontRef::new("Unbounded", 800, false);
    grad.style.fill = Fill { colour: Rgba8::WHITE, gradient: Some(GradientFill::new(vec![GradientStop { pos: 0.0, colour: Rgba8::rgba(255, 140, 0, 255) }, GradientStop { pos: 1.0, colour: Rgba8::rgba(255, 0, 128, 255) }], 0.0)), extra: Default::default() };
    check("gradient_fill", &grad, Clock::REST, 1.0);
    let mut soft = base("Мягкая тень");
    soft.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 200), offset_x: 4.0, offset_y: 6.0, blur: 8.0 });
    check("shadow_soft", &soft, Clock::REST, 1.0);
    let mut hard = base("ЖЁСТКАЯ");
    hard.style.font = FontRef::new("Russo One", 400, false);
    hard.style.outlines = vec![Outline { width: 3.0, colour: Rgba8::BLACK }];
    hard.style.shadow = Some(Shadow { colour: Rgba8::rgba(255, 0, 80, 255), offset_x: 6.0, offset_y: 6.0, blur: 0.0 });
    check("shadow_hard_outline", &hard, Clock::REST, 1.0);
    let mut neon = base("НЕОН");
    neon.style.font = FontRef::new("Comfortaa", 700, false);
    neon.style.fill = Fill::solid(Rgba8::rgba(255, 230, 255, 255));
    neon.style.glow = Some(Glow { colour: Rgba8::rgba(255, 40, 200, 255), radius: 10.0, intensity: 1.4 });
    check("glow_neon", &neon, Clock::REST, 1.0);
    let mut rainbow = base("Текст по дуге вверх");
    rainbow.shape = TextShape::Arc { bend: 0.6 };
    check("arc_rainbow", &rainbow, Clock::REST, 1.0);
    let mut smile = rainbow.clone();
    smile.shape = TextShape::Arc { bend: -0.6 };
    check("arc_smile", &smile, Clock::REST, 1.0);
    let mut circle = base("ПО КРУГУ • ПО КРУГУ • ПО КРУГУ •");
    circle.style.size = 28.0;
    circle.shape = TextShape::Circle { radius: 110.0, start_deg: 0.0 };
    check("circle", &circle, Clock::REST, 1.0);
    let mut tilt = base("Перспектива");
    tilt.perspective = Some(Perspective { tilt_x: 35.0, tilt_y: 0.0 });
    check("tilt_x", &tilt, Clock::REST, 1.0);
    tilt.perspective = Some(Perspective { tilt_x: 0.0, tilt_y: -30.0 });
    check("tilt_y", &tilt, Clock::REST, 1.0);
    let mut emoji = base("Гол! ⚽🔥");
    emoji.style.outlines = vec![Outline { width: 3.0, colour: Rgba8::BLACK }];
    emoji.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 160), offset_x: 3.0, offset_y: 4.0, blur: 4.0 });
    check("emoji_styled", &emoji, Clock::REST, 1.0);
}

/// An opaque RGBA picture: width, height, pixels.
type Picture = (u32, u32, Vec<u8>);

/// Frames stacked top to bottom over the background colour, each placed
/// so that its anchor (the layout box centre) lies at one point of its
/// row — the motion shows against the title's place: the title at
/// `at_ms` into a clip of `clip_ms` (loops in step, the highlight's
/// progress across the clip), at half size.
fn strip(doc: &TextDoc, at_ms: &[i64], clip_ms: i64) -> Picture {
    let frames: Vec<(Picture, (u32, u32))> = at_ms
        .iter()
        .map(|&t| {
            let ms = kadr_core::Time::from_millis;
            let c = Clock { since_in: ms(t), until_out: ms(clip_ms - t), source: ms(t), highlight: t as f64 / clip_ms as f64 };
            let f = TextFrame::new(doc, c);
            let (w, h, px) = picture(&f, 0.5);
            let b = f.bounds();
            let anchor = ((-b.x0 * w as f32 / b.width()).round() as u32, (-b.y0 * h as f32 / b.height()).round() as u32);
            ((w, h, px), anchor)
        })
        .collect();
    let (left, right) = (frames.iter().map(|f| f.1.0).max().unwrap(), frames.iter().map(|f| f.0.0 - f.1.0).max().unwrap());
    let (top, bottom) = (frames.iter().map(|f| f.1.1).max().unwrap(), frames.iter().map(|f| f.0.1 - f.1.1).max().unwrap());
    let (w, ch) = (left + right, top + bottom);
    let h = ch * frames.len() as u32;
    let mut out = vec![0u8; (w * h * 4) as usize];
    for p in out.as_chunks_mut::<4>().0 {
        *p = [BG[0] as u8, BG[1] as u8, BG[2] as u8, 255];
    }
    for (i, ((fw, fh, px), (ax, ay))) in frames.into_iter().enumerate() {
        let (x0, y0) = (left - ax, i as u32 * ch + top - ay);
        for y in 0..fh {
            let dst = (((y0 + y) * w + x0) * 4) as usize;
            out[dst..dst + (fw * 4) as usize].copy_from_slice(&px[(y * fw * 4) as usize..((y + 1) * fw * 4) as usize]);
        }
    }
    (w, h, out)
}

/// Pictures in a grid of `cols` columns, each centred in a cell as large
/// as the largest, over the background colour.
fn sheet(pics: Vec<Picture>, cols: usize) -> Picture {
    let (cw, ch) = (pics.iter().map(|p| p.0).max().unwrap(), pics.iter().map(|p| p.1).max().unwrap());
    let rows = pics.len().div_ceil(cols) as u32;
    let (w, h) = (cw * cols as u32, ch * rows);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for p in out.as_chunks_mut::<4>().0 {
        *p = [BG[0] as u8, BG[1] as u8, BG[2] as u8, 255];
    }
    for (i, (pw, ph, px)) in pics.into_iter().enumerate() {
        let (x0, y0) = ((i % cols) as u32 * cw + (cw - pw) / 2, (i / cols) as u32 * ch + (ch - ph) / 2);
        for y in 0..ph {
            let dst = (((y0 + y) * w + x0) * 4) as usize;
            out[dst..dst + (pw * 4) as usize].copy_from_slice(&px[(y * pw * 4) as usize..((y + 1) * pw * 4) as usize]);
        }
    }
    (w, h, out)
}

#[test]
fn motion_goldens() {
    let mut rise = base("Раз два три");
    rise.anim.reveal_in = kadr_text::anim::reveal("rise").map(|r| r.fresh());
    check_px("anim_rise_strip", strip(&rise, &[0, 120, 240, 360, 480, 700], 4_000));
    let mut pop = base("ХЛОП");
    pop.anim.reveal_in = kadr_text::anim::reveal("pop").map(|r| r.fresh());
    check_px("anim_pop_strip", strip(&pop, &[0, 60, 120, 180, 240, 450], 4_000));
    let mut scatter = base("Рассыпание");
    scatter.anim.reveal_in = kadr_text::anim::reveal("scatter").map(|r| r.fresh());
    check_px("anim_scatter_strip", strip(&scatter, &[0, 150, 300, 450, 900], 4_000));
    let mut hl = base("каждое слово по очереди");
    hl.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.12, span: (0.0, 1.0), extra: Default::default() });
    check_px("anim_highlight_strip", strip(&hl, &[100, 1_100, 2_100, 3_100], 4_000));
}

/// Every built-in style at rest on a 960×540 frame (half of 1080p), four
/// to a row, on its gallery's kind of text.
#[test]
fn styles_sheet_golden() {
    let pics: Vec<Picture> = kadr_text::styles::builtin()
        .iter()
        .map(|s| {
            let sample = if s.sample == "text.sample.score" { "2 : 1" } else { "Кадр 2026" };
            let mut d = s.new_doc(sample, 960, 540);
            d.seed = 1; // a fresh doc's seed is random: flicker would differ run to run
            picture(&TextFrame::new(&d, Clock { highlight: 0.3, ..Clock::REST }), 1.0)
        })
        .collect();
    assert_eq!(pics.len(), 22);
    check_px("styles_sheet", sheet(pics, 4));
}
