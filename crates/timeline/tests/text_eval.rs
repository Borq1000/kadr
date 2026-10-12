//! Text clips in the evaluator (titles spec §5.2): a text layer sized by
//! the text's bounds, anchored at its block's centre, placed, faded,
//! animated by keyframes and filtered by effects like a media clip; never
//! occluding; the same picture key while nothing changes.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_scene::*;
use kadr_timeline::scene::evaluate;
use std::sync::Arc;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

fn project(doc: TextDoc) -> (Project, kadr_core::ClipId) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let c = Clip::new_text(doc, s(0), s(5));
    let id = c.id;
    p.sequence_mut().tracks[0].clips.push(c);
    (p, id)
}

fn layer_at(p: &Project, t: Time) -> Vec<Layer> {
    evaluate(p, p.sequence(), t, &OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh)).layers
}

fn text_of(l: &Layer) -> &TextLayer {
    match &l.content {
        LayerContent::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn a_text_clip_is_a_text_layer_sized_by_its_bounds_and_anchored_at_its_centre() {
    let doc = TextDoc::starter("Гол!", 1080);
    let (mut p, id) = project(doc.clone());
    let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
    (tr.x, tr.y, tr.scale, tr.rotation_deg) = (100.0, -200.0, 1.5, 10.0);
    let layers = layer_at(&p, s(1));
    assert_eq!(layers.len(), 1);
    let l = &layers[0];
    text_of(l);
    assert_eq!(l.id, LayerId::from(id));
    let b = kadr_text::TextFrame::new(&doc, kadr_text::Clock::REST).bounds();
    assert_eq!(l.placement.size, Vec2::new(b.width(), b.height()));
    assert_eq!(l.placement.anchor, Vec2::new(-b.x0 / b.width(), -b.y0 / b.height()));
    assert_eq!((l.placement.position, l.placement.scale), (Vec2::new(1060.0, 340.0), Vec2::new(1.5, 1.5)));
    assert!((l.placement.rotation - 10f32.to_radians()).abs() < 1e-6);
    // The layout box centre is local (−x0, −y0): it lands on the position.
    let (cx, cy) = l.placement.to_canvas().apply(-b.x0 as f64, -b.y0 as f64);
    assert!((cx - 1060.0).abs() < 1e-3 && (cy - 340.0).abs() < 1e-3);
    assert_eq!((l.opacity, l.blend, l.crop), (1.0, BlendMode::Normal, l.placement.full_crop()));
    assert!(layer_at(&p, s(6)).is_empty(), "outside the clip: nothing");
}

#[test]
fn keyframes_fades_colour_and_effects_apply_as_to_media() {
    let (mut p, _) = project(TextDoc::starter("Матч", 1080));
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    c.video_fade = EdgeFades { fade_in: s(1), fade_out: Time::ZERO };
    c.color.exposure = 0.5;
    c.animation.set_key(AnimParam::Scale, SourceTime(s(0)), 1.0, Some(Ease::Linear));
    c.animation.set_key(AnimParam::Scale, SourceTime(s(4)), 2.0, None);
    let mut zoom = AppliedEffect::new("smooth_zoom", 1, Default::default());
    zoom.params.insert("strength".into(), ParamValue::Number(1.0));
    c.effects.push(zoom);
    let l = layer_at(&p, Time::from_millis(500)).remove(0);
    assert!((l.opacity - 0.5).abs() < 0.02, "half way through the fade-in: {}", l.opacity);
    assert!(matches!(l.effects.first(), Some(Effect::ColorAdjust(_))), "the inspector's correction");
    assert!(l.placement.scale.x > 1.125, "keyframes (1.125 at 0.5 s) times the zoom: {}", l.placement.scale.x);
}

#[test]
fn text_never_hides_the_video_below() {
    let (mut p, _) = project(TextDoc::starter("Поверх", 1080));
    let v = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("v.mp4", MediaInfo { kind: MediaKind::Video, duration: s(60), container: "mp4".into(), size_bytes: 0, video: Some(v), audio: None, timecode: None });
    let seq = p.sequence_mut();
    seq.tracks.insert(0, Track::new(TrackKind::Video, "V0"));
    seq.tracks[0].clips.push(Clip::new(a.id, "v", TimeRange::new(Time::ZERO, s(5)), Time::ZERO));
    p.assets.push(a);
    p.sequence_mut().tracks[1].clips[0].transform.scale = 40.0; // the text box covers the canvas
    let layers = layer_at(&p, s(1));
    assert_eq!(layers.len(), 2, "the opaque video stays under the text");
    assert!(matches!(layers[0].content, LayerContent::Media { .. }));
}

#[test]
fn a_still_text_is_one_picture_and_an_edit_is_another() {
    let (mut p, _) = project(TextDoc::starter("Ключ", 1080));
    let (a, b) = (layer_at(&p, s(1)).remove(0), layer_at(&p, s(3)).remove(0));
    assert_eq!(text_of(&a), text_of(&b), "no animation: the same key at every instant");
    p.sequence_mut().tracks[0].clips[0].text.as_mut().unwrap().style.fill.colour = Rgba8::rgba(255, 0, 0, 255);
    assert_ne!(text_of(&layer_at(&p, s(1))[0]), text_of(&a));
}

fn frame() -> Time {
    FrameRate::FPS_25.frame_duration()
}

fn clock(c: &Clip, t: Time) -> kadr_text::Clock {
    kadr_timeline::text::text_clock(c, c.text.as_deref().unwrap(), t, frame())
}

/// TX-R6: clip-bound like the fades — the last visible frame (one frame
/// before the out-point) has `until_out` 0, so a disappearance finishes.
#[test]
fn the_clock_runs_from_the_visible_edges_and_the_source() {
    let (mut p, _) = project(TextDoc::new("x"));
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (s(2), s(6), s(-1), s(3));
    let k = clock(c, s(3));
    assert_eq!((k.since_in, k.until_out, k.source), (s(1), s(3) - frame(), s(0)));
    let last = clock(c, s(6) - frame());
    assert_eq!((last.since_in, last.until_out), (s(4) - frame(), Time::ZERO), "the last frame: the disappearance is over");
    let edge = clock(c, s(9));
    assert_eq!((edge.since_in, edge.until_out, edge.source), (s(4) - frame(), Time::ZERO, s(6)), "past the end: the last frame's state, the source runs on");
    let before = clock(c, s(1));
    assert_eq!((before.since_in, before.until_out, before.source), (Time::ZERO, s(4) - frame(), s(-2)), "before the start: the first frame's state, the source extrapolated");
}

/// M5: the highlight runs over its span from in to out, held beyond.
#[test]
fn the_highlight_runs_over_its_span() {
    let mut doc = TextDoc::new("раз два три");
    doc.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 210, 0, 255), scale: 1.0, span: (0.2, 0.8), extra: Default::default() });
    let (p, _) = project(doc);
    let c = &p.sequence().tracks[0].clips[0];
    let at = |t: Time| clock(c, t).highlight;
    assert!((at(Time::from_millis(2500)) - 0.5).abs() < 1e-12, "mid-clip");
    assert_eq!((at(s(0)), at(s(9)), at(s(-3))), (0.2, 0.8, 0.2), "the span's ends at and beyond the edges");
    assert_eq!(clock(&Clip::new_text(TextDoc::new("x"), s(0), s(5)), s(1)).highlight, 0.0, "no highlight: 0");
}

/// M5: another speed changes the source time only — the reveals run on
/// the timeline.
#[test]
fn a_speed_change_moves_the_source_time_only() {
    let (mut p, _) = project(TextDoc::new("x"));
    let normal = clock(&p.sequence().tracks[0].clips[0], s(2));
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    c.source_out = s(10); // twice as fast over the same five seconds
    let fast = clock(c, s(2));
    assert_eq!((normal.source, fast.source), (s(2), s(4)));
    assert_eq!((fast.since_in, fast.until_out), (normal.since_in, normal.until_out));
}

/// M5: a text clip on either side of a transition draws at the handle
/// with its edge's clock — the incoming one as at its first frame, the
/// outgoing one as at its last.
#[test]
fn text_on_both_sides_of_a_transition_holds_its_edge_state() {
    let (mut p, a) = project(TextDoc::starter("Первый", 1080));
    let second = Clip::new_text(TextDoc::starter("Второй", 1080), s(5), s(5));
    let b = second.id;
    p.sequence_mut().tracks[0].clips.push(second);
    p.sequence_mut().transitions.push(Transition::legacy(TransitionKind::CrossDissolve, a, b, s(2)));
    let (ca, cb) = (p.sequence().tracks[0].clips[0].clone(), p.sequence().tracks[0].clips[1].clone());
    let t = Time::from_millis(4500); // in the window, before the cut: the incoming clip's handle
    assert_eq!(clock(&cb, t).since_in, Time::ZERO, "incoming: its first frame");
    let late = Time::from_millis(5500); // after the cut: the outgoing clip's handle
    assert_eq!(clock(&ca, late).until_out, Time::ZERO, "outgoing: its last frame");
    for at in [t, late] {
        let layers = layer_at(&p, at);
        let LayerContent::Transition(tr) = &layers[0].content else { panic!("a transition at {at:?}: {layers:?}") };
        let side = |c: &Clip| TextLayer { picture: Arc::new(kadr_text::TextFrame::new(c.text.as_deref().unwrap(), clock(c, at))) };
        assert_eq!((tr.from.len(), tr.to.len()), (1, 1));
        assert_eq!((text_of(&tr.from[0]), text_of(&tr.to[0])), (&side(&ca), &side(&cb)), "both titles, at their clamped clocks");
    }
}

fn with_effect(p: &mut Project, id: &str, params: &[(&str, ParamValue)]) {
    let mut e = AppliedEffect::new(id, 1, Default::default());
    for (k, v) in params {
        e.params.insert((*k).into(), v.clone());
    }
    p.sequence_mut().tracks[0].clips[0].effects.push(e);
}

/// Where the layer draws the layout box centre (local `(−x0, −y0)`).
fn layout_centre(l: &Layer) -> (f64, f64) {
    let (a, s) = (l.placement.anchor, l.placement.size);
    l.placement.to_canvas().apply((a.x * s.x) as f64, (a.y * s.y) as f64)
}

/// TX-R7: a centred zoom scales the text block about its own centre —
/// the layout box's, wherever the room for ink, outlines and plaque lies.
#[test]
fn a_centred_zoom_keeps_the_text_blocks_centre_in_place() {
    let mut doc = TextDoc::starter("Гол\nи ещё строка", 1080);
    doc.style.outlines = vec![Outline { width: 12.0, colour: Rgba8::BLACK }];
    let (mut p, _) = project(doc.clone());
    let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
    (tr.x, tr.y, tr.rotation_deg) = (300.0, 150.0, 15.0);
    with_effect(&mut p, "smooth_zoom", &[("strength", ParamValue::Number(1.0))]);
    let l = layer_at(&p, s(4)).remove(0);
    assert!(l.placement.scale.x > 1.05, "zooming: {}", l.placement.scale.x);
    let b = kadr_text::TextFrame::new(&doc, kadr_text::Clock::REST).bounds();
    assert!((b.x0 + b.x1).abs() > 1.0 || (b.y0 + b.y1).abs() > 1.0, "the bounds are not centred on the block: {b:?}");
    let (cx, cy) = layout_centre(&l);
    assert!((cx - 1260.0).abs() < 1e-3 && (cy - 690.0).abs() < 1e-3, "({cx}, {cy})");
}

/// TX-R7: the motion effects move the layout box as the picture (window
/// = the block, point = a fraction of the block), and the layer is
/// anchored at the block's centre again — drawing exactly where the moved
/// block says.
#[test]
fn motion_effects_move_the_text_block_and_the_layer_stays_anchored_at_its_centre() {
    let doc = TextDoc::starter("Гол!", 1080);
    let (mut p, _) = project(doc.clone());
    let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
    (tr.x, tr.y, tr.scale, tr.rotation_deg) = (-250.0, 80.0, 1.3, -7.0);
    with_effect(&mut p, "smooth_zoom", &[("strength", ParamValue::Number(1.0)), ("point", ParamValue::Point { x: 0.2, y: 0.9 })]);
    with_effect(&mut p, "handheld", &[("strength", ParamValue::Number(1.0))]);
    let t = Time::from_millis(3300);
    let l = layer_at(&p, t).remove(0);
    let f = kadr_text::TextFrame::new(&doc, kadr_text::Clock::REST);
    let (b, (lw, lh)) = (f.bounds(), f.layout_size());
    assert_eq!(l.placement.anchor, Vec2::new(-b.x0 / b.width(), -b.y0 / b.height()), "anchored at the layout centre");
    let c = &p.sequence().tracks[0].clips[0];
    let block = Placement { size: Vec2::new(lw, lh), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(960.0 - 250.0, 540.0 + 80.0), scale: Vec2::new(1.3, 1.3), rotation: (-7f32).to_radians() };
    let moved = kadr_timeline::fx::place_with_effects(c, t, SizeU::new(1920, 1080), block);
    assert!(moved.scale.x > 1.4 && moved.rotation != block.rotation, "zoomed and shaken: {moved:?}");
    for (x, y) in [(0.0, 0.0), (b.width(), 0.0), (0.0, b.height()), (b.width(), b.height()), (-b.x0, -b.y0), (17.0, 3.5)] {
        let got = l.placement.to_canvas().apply(x as f64, y as f64);
        let want = moved.to_canvas().apply((x + b.x0 + lw / 2.0) as f64, (y + b.y0 + lh / 2.0) as f64);
        assert!((got.0 - want.0).abs() < 1e-2 && (got.1 - want.1).abs() < 1e-2, "({x}, {y}): {got:?} vs {want:?}");
    }
}

/// TX-R15: a curved title's motion picture is its own block (its placed
/// glyphs and centre, `TextFrame::block_box`), not the straight layout
/// strip — a zoom's point is a fraction of the arc as drawn — and the
/// layer stays anchored at the layout centre.
#[test]
fn motion_effects_move_a_curved_titles_own_block() {
    let mut doc = TextDoc::starter("Текст по дуге вверх", 1080);
    doc.shape = TextShape::Arc { bend: 0.6 };
    let (mut p, _) = project(doc.clone());
    let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
    (tr.x, tr.y, tr.scale) = (120.0, -60.0, 0.8);
    with_effect(&mut p, "smooth_zoom", &[("strength", ParamValue::Number(1.0)), ("point", ParamValue::Point { x: 0.1, y: 0.95 })]);
    let t = Time::from_millis(3700);
    let l = layer_at(&p, t).remove(0);
    let f = kadr_text::TextFrame::new(&doc, kadr_text::Clock::REST);
    let (b, k, lh) = (f.bounds(), f.block_box(), f.layout_size().1);
    assert!(k.y1 - k.y0 > lh + 100.0 && (k.y0 + k.y1).abs() > 50.0, "the arc's block is taller than the strip and not centred on it: {k:?}");
    assert_eq!(l.placement.anchor, Vec2::new(-b.x0 / b.width(), -b.y0 / b.height()), "anchored at the layout centre");
    let c = &p.sequence().tracks[0].clips[0];
    let (kw, kh) = (k.width(), k.height());
    let block = Placement { size: Vec2::new(kw, kh), anchor: Vec2::new(-k.x0 / kw, -k.y0 / kh), position: Vec2::new(960.0 + 120.0, 540.0 - 60.0), scale: Vec2::new(0.8, 0.8), rotation: 0.0 };
    let moved = kadr_timeline::fx::place_with_effects(c, t, SizeU::new(1920, 1080), block);
    assert!(moved.scale.x > 0.82, "zoomed: {moved:?}");
    for (x, y) in [(0.0, 0.0), (b.width(), b.height()), (-b.x0, -b.y0), (40.0, 7.5)] {
        let got = l.placement.to_canvas().apply(x as f64, y as f64);
        let want = moved.to_canvas().apply((x + b.x0 - k.x0) as f64, (y + b.y0 - k.y0) as f64);
        assert!((got.0 - want.0).abs() < 1e-2 && (got.1 - want.1).abs() < 1e-2, "({x}, {y}): {got:?} vs {want:?}");
    }
}

/// TX-R7: what lies around the block (a plaque here) does not change how
/// the block moves.
#[test]
fn a_plaque_does_not_change_how_the_text_moves() {
    let doc = TextDoc::starter("Гол", 1080);
    let mut boxed = doc.clone();
    boxed.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::rgba(0, 80, 255, 255), radius: 0.0, padding_x: 120.0, padding_y: 10.0 });
    let state = |doc: &TextDoc| {
        let (mut p, _) = project(doc.clone());
        with_effect(&mut p, "smooth_zoom", &[("strength", ParamValue::Number(1.0))]);
        with_effect(&mut p, "handheld", &[("strength", ParamValue::Number(1.0))]);
        let id = p.sequence().tracks[0].clips[0].effects[1].id;
        (p, id)
    };
    let (pa, shake) = state(&doc);
    let (mut pb, _) = state(&boxed);
    pb.sequence_mut().tracks[0].clips[0].effects[1].id = shake; // the same noise
    for ms in [700, 2100, 4400] {
        let (a, b) = (layer_at(&pa, Time::from_millis(ms)).remove(0), layer_at(&pb, Time::from_millis(ms)).remove(0));
        assert!(b.placement.size.x > a.placement.size.x + 200.0, "the plaque widens the bounds");
        let (ca, cb) = (layout_centre(&a), layout_centre(&b));
        assert!((ca.0 - cb.0).abs() < 1e-3 && (ca.1 - cb.1).abs() < 1e-3, "{ms} ms: {ca:?} vs {cb:?}");
        assert_eq!((a.placement.scale, a.placement.rotation), (b.placement.scale, b.placement.rotation), "{ms} ms");
    }
}
