//! The animation engine (titles spec §3–4): reveals per unit with stagger
//! and curve, shrunk to fit the clip; the out mirrors the in from the end;
//! loops in source time; the highlighted word; deterministic keys.

use kadr_core::Time;
use kadr_project::*;
use kadr_scene::TextPicture;
use kadr_text::anim::*;
use kadr_text::{layout, Clock, TextFrame};

fn ms(x: i64) -> Time {
    Time::from_millis(x)
}

fn clock(since: i64, until: i64) -> Clock {
    Clock { since_in: ms(since), until_out: ms(until), source: ms(since), highlight: 0.0 }
}

fn reveal(preset: &str, unit: Unit, d: i64, st: i64, ease: &str) -> Reveal {
    Reveal { preset: preset.into(), unit, duration: ms(d), stagger: ms(st), ease: ease.into(), extra: Default::default() }
}

fn three_words() -> TextDoc {
    let mut d = TextDoc::new("раз два три");
    d.style.size = 40.0;
    d
}

/// The opacity of the first visible glyph of each word.
fn word_opacities(d: &TextDoc, c: Clock) -> Vec<f32> {
    let l = layout(d);
    let a = animate(d, &l, c);
    // Rounded to 1e-4: a stagger of 0.05 s × 2 is 0.1 s only up to f64 dust.
    (0..3).map(|w| (l.glyphs.iter().zip(&a.glyphs).find(|(g, _)| g.visible && g.word == w).unwrap().1.opacity * 1e4).round() / 1e4).collect()
}

#[test]
fn reveals_run_per_unit_with_stagger_and_rest_between() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    assert_eq!(word_opacities(&d, clock(150, 5_000)), [0.375, 0.125, 0.0]);
    assert_eq!(word_opacities(&d, clock(0, 5_000)), [0.0, 0.0, 0.0], "hidden at the clip's first frame");
    assert!(animate(&d, &layout(&d), clock(2_000, 3_000)).is_rest(), "after the reveal: at rest");
}

#[test]
fn the_out_mirrors_the_in_from_the_end() {
    let mut d = three_words();
    d.anim.reveal_out = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    // Out total 0.6 s: word 0 leaves first, from 0.6 s before the end.
    assert_eq!(word_opacities(&d, clock(5_000, 450)), [0.625, 0.875, 1.0]);
    assert_eq!(word_opacities(&d, clock(5_000, 0)), [0.0, 0.0, 0.0]);
}

#[test]
fn reveals_that_do_not_fit_the_clip_shrink_together() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    d.anim.reveal_out = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    // In and out take 1.2 s; the clip is 0.6 s: both at half speed (0.2 s per word, 0.05 s apart).
    assert_eq!(word_opacities(&d, clock(100, 500)), [0.5, 0.25, 0.0]);
    assert_eq!(word_opacities(&d, clock(300, 300)), [1.0, 1.0, 1.0], "the in just ended, the out not begun");
}

#[test]
fn typewriter_reveals_letters_and_shows_a_cursor_while_typing() {
    let mut d = TextDoc::new("абв");
    d.anim.reveal_in = Some(reveal("typewriter", Unit::Letter, 1, 50, "linear"));
    let l = layout(&d);
    let a = animate(&d, &l, clock(60, 5_000));
    let shown: Vec<f32> = a.glyphs.iter().map(|g| g.opacity).collect();
    assert_eq!(shown, [1.0, 1.0, 0.0]);
    assert_eq!(a.cursor, Some(1));
    assert_eq!(animate(&d, &l, clock(500, 5_000)).cursor, None, "typed out: no cursor");
}

#[test]
fn unknown_presets_are_kept_and_do_nothing() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("teleport", Unit::Word, 400, 100, "linear"));
    d.anim.looping = Some(LoopAnim { preset: "spin3d".into(), unit: Unit::Block, speed: 1.0, amount: 1.0, extra: Default::default() });
    assert!(animate(&d, &layout(&d), clock(0, 5_000)).is_rest());
}

#[test]
fn loops_run_in_source_time() {
    let mut d = three_words();
    d.anim.looping = Some(LoopAnim { preset: "wave".into(), unit: Unit::Letter, speed: 1.0, amount: 1.0, extra: Default::default() });
    let l = layout(&d);
    let at = |since: i64, source: i64| animate(&d, &l, Clock { since_in: ms(since), until_out: ms(9_000), source: ms(source), highlight: 0.0 });
    assert_eq!(at(1_000, 2_250), at(4_000, 2_250), "only the source time moves a loop");
    assert_ne!(at(1_000, 2_250), at(1_000, 2_500));
    let pulse = TextDoc { anim: TextAnim { looping: Some(LoopAnim { preset: "pulse".into(), unit: Unit::Block, speed: 1.0, amount: 1.0, extra: Default::default() }), ..Default::default() }, ..three_words() };
    let a = animate(&pulse, &layout(&pulse), Clock { source: ms(250), ..Clock::REST });
    assert!((a.block.scale - 1.06).abs() < 1e-4, "a quarter cycle: the top of the pulse");
}

#[test]
fn the_highlight_walks_the_words() {
    let mut d = three_words();
    d.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.1, span: (0.0, 1.0), extra: Default::default() });
    let l = layout(&d);
    let lit = |p: f64| -> Vec<usize> {
        let a = animate(&d, &l, Clock { highlight: p, ..Clock::REST });
        l.glyphs.iter().zip(&a.glyphs).filter(|(g, x)| g.visible && x.colour.is_some()).map(|(g, _)| g.word).collect::<std::collections::BTreeSet<_>>().into_iter().collect()
    };
    assert_eq!((lit(0.1), lit(0.5), lit(0.99), lit(1.0)), (vec![0], vec![1], vec![2], vec![2]));
}

#[test]
fn every_preset_has_a_unique_id_and_rests_at_one() {
    let mut ids: Vec<&str> = REVEALS.iter().map(|r| r.id).chain(LOOPS.iter().map(|l| l.id)).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!((ids.len(), REVEALS.len(), LOOPS.len()), (n, 11, 6));
    for r in REVEALS {
        assert_eq!(r.look(1.0, 50.0, [0.3, -0.7, 0.9]), kadr_text::GlyphAnim::REST, "{} at rest", r.id);
        assert!(r.look(0.0, 50.0, [0.3, -0.7, 0.9]).opacity < 0.01 || r.id == "pop", "{} hidden at 0", r.id);
        assert_eq!(kadr_text::anim::reveal(r.id).map(|d| d.fresh().preset), Some(r.id.to_string()));
        assert!(curve(r.ease).is_some(), "{}: a known curve", r.ease);
    }
    for l in LOOPS {
        assert_eq!(l.look(0.37, 3, 0.0, 50.0, 7), kadr_text::GlyphAnim::REST, "{}: amount 0 is still", l.id);
    }
    assert!(EASES.iter().all(|e| curve(e).is_some()));
    // A finished reveal is exactly at rest: every curve ends at 1.
    assert!(EASES.iter().all(|e| curve(e).unwrap().apply(1.0) == 1.0));
}

/// Every reveal, alone or with the out, ends at rest — at the frame after
/// it, and long after.
#[test]
fn every_reveal_ends_exactly_at_rest() {
    for r in REVEALS {
        let mut d = three_words();
        d.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::BLACK, radius: 4.0, padding_x: 6.0, padding_y: 2.0 });
        d.anim.reveal_in = Some(r.fresh());
        d.anim.reveal_out = Some(r.fresh());
        let l = layout(&d);
        let n = match r.unit {
            Unit::Letter => l.letters,
            Unit::Word => l.words,
            Unit::Line => l.lines.len(),
            Unit::Block => 1,
        } as i64;
        let total = r.duration_ms + (n - 1) * r.stagger_ms;
        assert!(animate(&d, &l, clock(total, total)).is_rest(), "{}: the in just over, the out just to start", r.id);
        assert!(animate(&d, &l, clock(total + 40, 9_000)).is_rest(), "{}", r.id);
        assert!(!animate(&d, &l, clock(total / 2, 9_000)).is_rest(), "{}: moving half way", r.id);
    }
}

fn key(d: &TextDoc, c: Clock) -> u64 {
    TextFrame::new(d, c).key()
}

/// The raster's size and pixels at scale 1.
fn pixels(d: &TextDoc, c: Clock) -> (u32, u32, Vec<u8>) {
    let r = TextFrame::new(d, c).raster(1.0);
    (r.width, r.height, (0..r.height).flat_map(|y| r.row(y).to_vec()).collect())
}

/// An animated title's picture key changes exactly when its pixels do:
/// the frames of a finished reveal are one picture (the player and the
/// raster cache do not redraw it), the frames of a moving one are not.
#[test]
fn a_titles_key_changes_when_its_pixels_do() {
    let mut d = three_words();
    d.anim.reveal_in = kadr_text::anim::reveal("rise").map(|r| r.fresh());
    d.anim.reveal_out = kadr_text::anim::reveal("fade").map(|r| r.fresh());
    let frames = |from: i64| [from, from + 40].map(|t| clock(t, 5_000 - t));
    // Moving: another key, other pixels.
    let [a, b] = frames(100);
    assert_ne!(key(&d, a), key(&d, b));
    assert_ne!(pixels(&d, a), pixels(&d, b));
    // Finished: one key, the same pixels — those of the title at rest.
    let [a, b] = frames(2_000);
    assert_eq!(key(&d, a), key(&d, b));
    assert_eq!(key(&d, a), key(&d, Clock::REST));
    assert_eq!(pixels(&d, a), pixels(&d, Clock::REST));
    let mut still = d.clone();
    still.anim = TextAnim::default();
    assert_eq!(pixels(&d, a), pixels(&still, Clock::REST), "a finished reveal draws the still title");
    // The typewriter between two letters (no plaque): the same picture.
    let mut typed = TextDoc::new("абвгд");
    typed.anim.reveal_in = Some(reveal("typewriter", Unit::Letter, 1, 100, "linear"));
    let (a, b) = (clock(110, 4_000), clock(150, 4_000));
    assert_eq!(animate(&typed, &layout(&typed), a), animate(&typed, &layout(&typed), b));
    assert_eq!(key(&typed, a), key(&typed, b), "the same letters and cursor: one picture");
    assert_ne!(key(&typed, b), key(&typed, clock(210, 4_000)), "the next letter: another");
    // A highlight: one picture while the same word is lit.
    let mut lit = three_words();
    lit.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.1, span: (0.0, 1.0), extra: Default::default() });
    let at = |p: f64| key(&lit, Clock { highlight: p, ..Clock::REST });
    assert_eq!(at(0.4), at(0.6));
    assert_ne!(at(0.6), at(0.7));
}

/// What draws nothing never moves the key: a glyph not yet shown is
/// hidden wherever it would fly from; a plaque's fade counts only with a
/// plaque.
#[test]
fn what_draws_nothing_is_not_in_the_key() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("fade", Unit::Word, 400, 400, "linear"));
    let l = layout(&d);
    let (a, b) = (animate(&d, &l, clock(100, 9_000)), animate(&d, &l, clock(140, 9_000)));
    assert_eq!(a.plaque_opacity, 1.0, "no plaque: nothing to fade");
    for (g, x) in l.glyphs.iter().zip(&a.glyphs).filter(|(g, _)| !g.visible || g.word > 0) {
        assert!(*x == GlyphAnim::REST || *x == GlyphAnim::HIDDEN, "{g:?}: {x:?}");
    }
    assert_ne!(a.key(), b.key(), "word 0 fades in");
    let mut scattered = three_words();
    scattered.anim.reveal_in = kadr_text::anim::reveal("scatter").map(|r| r.fresh());
    let l = layout(&scattered);
    assert!(animate(&scattered, &l, clock(0, 9_000)).glyphs.iter().all(|g| *g == GlyphAnim::HIDDEN || *g == GlyphAnim::REST));
    let mut boxed = d.clone();
    boxed.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::BLACK, radius: 0.0, padding_x: 4.0, padding_y: 4.0 });
    assert!(animate(&boxed, &layout(&boxed), clock(100, 9_000)).plaque_opacity < 0.2, "a plaque fades in with the text");
}

/// TX-R7: the text block the motion effects move is measured at rest —
/// animated glyphs never move it, straight or curved.
#[test]
fn the_block_box_is_measured_at_rest() {
    for shape in [TextShape::Straight, TextShape::Arc { bend: 0.5 }, TextShape::Circle { radius: 150.0, start_deg: 0.0 }] {
        let mut d = three_words();
        d.shape = shape;
        d.anim.reveal_in = kadr_text::anim::reveal("scatter").map(|r| r.fresh());
        d.anim.looping = kadr_text::anim::looping("float").map(|l| l.fresh());
        let rest = TextFrame::new(&d, Clock::REST).block_box();
        for t in [0, 100, 250, 400] {
            let f = TextFrame::new(&d, clock(t, 9_000));
            assert_eq!(f.block_box(), rest, "{shape:?} at {t} ms");
            assert_eq!(f.block_box(), kadr_text::frame_box(&d));
        }
    }
}

/// The room of a moving title: a letter flying in, growing past full size
/// or blurred is inside the bounds (nothing touches the raster's edge).
#[test]
fn bounds_hold_every_animated_glyph() {
    for r in REVEALS {
        let mut d = TextDoc::new("Ж раз два");
        d.style.size = 60.0;
        d.seed = 3;
        d.style.outlines = vec![Outline { width: 3.0, colour: Rgba8::BLACK }];
        d.anim.reveal_in = Some(r.fresh());
        for t in [20, 90, 160, 240, 330] {
            let f = TextFrame::new(&d, clock(t, 9_000));
            let px = f.raster(1.0);
            let edge = |x: u32, y: u32| px.row(y)[x as usize * 4 + 3];
            let (w, h) = (px.width, px.height);
            assert!((0..w).all(|x| edge(x, 0) == 0 && edge(x, h - 1) == 0) && (0..h).all(|y| edge(0, y) == 0 && edge(w - 1, y) == 0), "{} at {t} ms touches the edge", r.id);
        }
    }
}

/// Blur-in: a glyph mid-reveal is drawn soft (fewer opaque pixels, more
/// partly covered ones), at rest it is the sharp glyph again.
#[test]
fn blur_in_softens_the_glyphs_mid_reveal() {
    let mut d = TextDoc::new("ЖЖ");
    d.style.size = 80.0;
    d.anim.reveal_in = Some(reveal("blur_in", Unit::Block, 1_000, 0, "linear"));
    let count = |c: Clock| {
        let r = TextFrame::new(&d, c).raster(1.0);
        let a: Vec<u8> = (0..r.height).flat_map(|y| r.row(y).as_chunks::<4>().0.iter().map(|p| p[3]).collect::<Vec<_>>()).collect();
        (a.iter().filter(|&&v| v >= 250).count(), a.iter().filter(|&&v| v > 0).count())
    };
    let (sharp_full, sharp_any) = count(Clock::REST);
    // u = 0.5: blur 0.15 × 80 = 12 px, opacity 0.5.
    let mid = TextFrame::new(&d, clock(500, 9_000));
    assert!(mid.anim().glyphs.iter().filter(|g| g.opacity > 0.0).all(|g| (g.blur - 12.0).abs() < 1e-3 && (g.opacity - 0.5).abs() < 1e-3));
    let (soft_full, soft_any) = count(clock(500, 9_000));
    assert_eq!(soft_full, 0, "half transparent and soft: nothing opaque");
    assert!(soft_any > sharp_any * 2, "the blur spreads the ink: {soft_any} vs {sharp_any} px");
    assert!(sharp_full > 500);
    assert!(mid.bounds().width() >= TextFrame::new(&d, Clock::REST).bounds().width() + 2.0 * 3.0 * 12.0 - 0.01, "room for 3 σ");
}

/// The typewriter's cursor is drawn after the last letter typed.
#[test]
fn the_typewriter_cursor_is_drawn_after_the_last_letter() {
    let mut d = TextDoc::new("ЖЖЖЖ");
    d.style.size = 60.0;
    d.anim.reveal_in = Some(reveal("typewriter", Unit::Letter, 1, 100, "linear"));
    let f = TextFrame::new(&d, clock(110, 9_000));
    assert_eq!(f.anim().cursor, Some(1));
    let laid = f.laid();
    let r = f.raster(1.0);
    let b = f.bounds();
    // The column just after the second letter's advance (raster px).
    let g = &laid.glyphs[1];
    let x = ((g.x + g.advance + 0.08 * g.size) - laid.width / 2.0 - b.x0).round() as u32;
    let column = |r: &kadr_core::CpuFrame, x: u32| (0..r.height).filter(|&y| r.row(y)[x as usize * 4 + 3] > 200).count();
    assert!(column(&r, x) as f32 > 0.6 * g.size, "a bar about 0.95 em tall: {} px", column(&r, x));
    let mut no_cursor = d.clone();
    no_cursor.anim.reveal_in = Some(reveal("fade", Unit::Letter, 1, 100, "linear"));
    let plain = TextFrame::new(&no_cursor, clock(110, 9_000)).raster(1.0);
    assert_eq!(column(&plain, x), 0, "without the typewriter: nothing there");
}

fn absurd_docs() -> Vec<TextDoc> {
    let times = [Time(i64::MAX), Time(i64::MIN), ms(1_000_000_000_000), ms(-5_000), Time::ZERO];
    let mut out = vec![];
    for (i, r) in REVEALS.iter().enumerate() {
        for (j, &t) in times.iter().enumerate() {
            let mut d = TextDoc::new("Абсурд всё");
            d.style.size = 50.0;
            d.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::BLACK, radius: 2.0, padding_x: 4.0, padding_y: 2.0 });
            d.anim.reveal_in = Some(Reveal { duration: t, stagger: times[(j + i) % times.len()], ..r.fresh() });
            d.anim.reveal_out = Some(Reveal { duration: times[(j + 1) % times.len()], stagger: t, ease: "nonsense".into(), ..r.fresh() });
            out.push(d);
        }
    }
    let numbers = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e12, -1e12, -1.0, 0.0];
    for l in LOOPS {
        for (i, &v) in numbers.iter().enumerate() {
            let mut d = TextDoc::new("Абсурд всё");
            d.style.size = 50.0;
            d.anim.looping = Some(LoopAnim { speed: v, amount: numbers[(i + 3) % numbers.len()], ..l.fresh() });
            d.anim.highlight = Some(Highlight { colour: Rgba8::WHITE, scale: numbers[(i + 1) % numbers.len()] as f32, span: (v, -v), extra: Default::default() });
            out.push(d);
        }
    }
    out
}

/// TX-R5 for the animations: absurd durations, staggers, speeds, amounts
/// and highlight scales never abort, never hang, and keep the picture's
/// bounds finite and within a sane reach of the title at rest.
#[test]
fn absurd_animations_never_abort() {
    let clocks = [
        Clock::REST,
        clock(0, 0),
        clock(1, 1),
        clock(300, 9_000),
        Clock { since_in: Time(i64::MAX), until_out: Time(i64::MIN), source: Time(i64::MIN), highlight: f64::NAN },
        Clock { since_in: Time(i64::MIN), until_out: Time(i64::MAX), source: Time(i64::MAX), highlight: f64::INFINITY },
        Clock { since_in: ms(77), until_out: ms(33), source: ms(-123_456_789), highlight: -3.0 },
    ];
    for d in absurd_docs() {
        let rest = TextFrame::new(&d, Clock::REST).bounds();
        let reach = |b: kadr_text::Bounds| b.x0.abs().max(b.x1.abs()).max(b.y0.abs()).max(b.y1.abs());
        for &c in &clocks {
            let f = TextFrame::new(&d, c);
            let b = f.bounds();
            assert!([b.x0, b.y0, b.x1, b.y1].iter().all(|v| v.is_finite()), "{:?} at {c:?}: {b:?}", d.anim);
            // A glyph flies at most 2 sizes off, grows at most 10× (zoom × highlight), blurs 3 × 0.3 sizes.
            assert!(reach(b) <= reach(rest) * 12.0 + 50.0 * 12.0, "{:?} at {c:?}: {b:?} vs {rest:?}", d.anim);
            let a = f.anim();
            assert!(a.glyphs.iter().chain([&a.block]).all(|g| [g.dx, g.dy, g.scale, g.rotation, g.opacity, g.blur].iter().all(|v| v.is_finite())), "{:?} at {c:?}", d.anim);
            assert!((0.0..=1.0).contains(&a.plaque_opacity));
            let r = f.raster(0.25);
            assert_eq!((r.width, r.height), ((b.width() * 0.25).ceil() as u32, (b.height() * 0.25).ceil() as u32));
        }
    }
}
