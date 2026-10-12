//! Text clips in the evaluator (titles spec §5.2): a text layer sized by
//! the text's bounds, anchored at its block's centre, placed, faded,
//! animated by keyframes and filtered by effects like a media clip; never
//! occluding; the same picture key while nothing changes.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_scene::*;
use kadr_timeline::scene::evaluate;

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
fn growing_text_keeps_its_centre() {
    let centre = |text: &str| {
        let (p, _) = project(TextDoc::starter(text, 1080));
        let l = layer_at(&p, s(1)).remove(0);
        let a = l.placement.anchor;
        l.placement.to_canvas().apply((a.x * l.placement.size.x) as f64, (a.y * l.placement.size.y) as f64)
    };
    let (a, b) = (centre("Гол"), centre("Гол!!!!!!!!\nи ещё строка"));
    assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3, "{a:?} vs {b:?}");
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

#[test]
fn the_clock_runs_from_the_visible_edges_and_the_source() {
    let (mut p, _) = project(TextDoc::new("x"));
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (s(2), s(6), s(-1), s(3));
    let doc = c.text.as_deref().unwrap().clone();
    let k = kadr_timeline::text::text_clock(c, &doc, s(3));
    assert_eq!((k.since_in, k.until_out, k.source), (s(1), s(3), s(0)));
    let edge = kadr_timeline::text::text_clock(c, &doc, s(9));
    assert_eq!((edge.since_in, edge.until_out), (s(4), Time::ZERO), "past the end: the end's state");
}
