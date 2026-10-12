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
    let (w, h, px) = picture(&TextFrame::new(doc, clock), scale);
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
    caps.style.font = FontRef { family: "Oswald".into(), weight: 500, italic: false };
    check("caps_spacing", &caps, Clock::REST, 1.0);
    check("title_plain_x2", &base("Привет, мир!"), Clock::REST, 2.0);
}
