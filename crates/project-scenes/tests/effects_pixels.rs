//! Evaluator → renderer with motion and colour effects (effects spec §9):
//! strength 0 and a switched-off effect are pixel-identical to no effect; a
//! split keeps every pixel, of a motion and of a Look with its grain; Smooth
//! Zoom magnifies about its point; every Look changes the picture, and
//! strength 0 or the «До» view do not. No decoder: the media layer gets a
//! synthetic gradient frame.

use kadr_core::color::AlphaMode;
use kadr_core::{ColorInfo, CpuFrame, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_playback::SceneSource;
use kadr_project::{AppliedEffect, Clip, MediaAsset, ParamValue, Project};
use kadr_project_scenes::ProjectScenes;
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{FrameScene, LayerContent, OutputSpec, RenderQuality, SizeU};
use kadr_timeline::ops::{split_clip, LinkRemap};
use kadr_timeline::scene::evaluate;
use std::collections::BTreeMap;
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 36;

/// One 4 s clip of a 64 × 36 picture on a 64 × 36, 30 fps sequence.
fn project(effects: Vec<AppliedEffect>) -> Project {
    let mut p = Project::new("px");
    let seq = p.sequence_mut();
    (seq.frame_rate, seq.width, seq.height) = (FrameRate::FPS_30, W, H);
    let video = VideoInfo { width: W, height: H, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("g.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None });
    let mut c = Clip::new(a.id, "g", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
    c.effects = effects;
    p.assets.push(a);
    p.sequence_mut().tracks[0].clips.push(c);
    p
}

/// Red grows left → right, green top → bottom: any move shows.
fn gradient() -> Arc<CpuFrame> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[(x * 4) as u8, (y * 7) as u8, 128, 255]);
        }
    }
    Arc::new(CpuFrame::from_rgba8(W, H, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, px))
}

fn out() -> OutputSpec {
    OutputSpec::new(SizeU::new(W, H), RenderQuality::Export)
}

fn render_scene(scene: &FrameScene) -> Vec<u8> {
    let g = gradient();
    let inputs = RenderInputs { layers: scene.layers.iter().map(|l| match l.content { LayerContent::Media { .. } => LayerInput::Cpu(g.clone()), _ => LayerInput::None }).collect() };
    let mut buf = vec![0u8; (W * H * 4) as usize];
    CpuRenderer::new().render(&PreparedFrame { scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(W, H, &mut buf))).unwrap();
    buf
}

fn render(p: &Project, t: Time) -> Vec<u8> {
    render_scene(&evaluate(p, p.sequence(), t, &out()))
}

fn fx(recipe: &str, set: &[(&str, ParamValue)]) -> AppliedEffect {
    let params: BTreeMap<String, ParamValue> = set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    AppliedEffect::new(recipe, 1, params)
}

#[test]
fn zero_strength_and_a_switched_off_effect_are_pixel_identical_to_no_effect() {
    let plain = project(vec![]);
    let mut off = fx("handheld", &[]);
    off.enabled = false;
    // Non-vacuity: the same effects at full strength do change the picture.
    let live = fx("smooth_zoom", &[("strength", ParamValue::Number(1.0))]);
    assert_ne!(render(&project(vec![live]), Time::from_millis(3_900)), render(&plain, Time::from_millis(3_900)));
    for t in [0, 1_000, 2_500, 3_900].map(Time::from_millis) {
        let want = render(&plain, t);
        assert_eq!(render(&project(vec![fx("smooth_zoom", &[("strength", ParamValue::Number(0.0))])]), t), want, "strength 0 at {t:?}");
        assert_eq!(render(&project(vec![off.clone()]), t), want, "switched off at {t:?}");
    }
}

/// Review focus 2 at the pixel level: the frames around a cut are the
/// frames of the whole clip (within the renderer's ±1 LSB).
#[test]
fn a_split_keeps_every_pixel() {
    let whole = project(vec![fx("handheld", &[("strength", ParamValue::Number(1.0))]), fx("smooth_zoom", &[])]);
    let mut split = whole.clone();
    let c = split.sequence().tracks[0].clips[0].clone();
    let (l, r) = split_clip(&c, FrameRate::FPS_30.frame_to_time(37), &mut LinkRemap::default());
    split.sequence_mut().tracks[0].clips = vec![l, r];
    let plain = project(vec![]);
    let mut moved = 0;
    for f in 30..45 {
        let t = FrameRate::FPS_30.frame_to_time(f);
        moved += (render(&whole, t) != render(&plain, t)) as u32;
        let worst = render(&whole, t).iter().zip(render(&split, t)).map(|(a, b)| a.abs_diff(b)).max().unwrap();
        assert!(worst <= 1, "frame {f}: off by {worst}");
    }
    assert!(moved > 10, "the effects really move the picture ({moved} of 15 frames differ from none)");
}

#[test]
fn smooth_zoom_magnifies_about_its_point() {
    let last = Time::from_secs(4) - FrameRate::FPS_30.frame_duration();
    let plain = render(&project(vec![]), last);
    let zoomed = render(&project(vec![fx("smooth_zoom", &[("strength", ParamValue::Number(1.0))])]), last);
    let red = |buf: &[u8], x: u32, y: u32| buf[((y * W + x) * 4) as usize] as i32;
    assert!(red(&zoomed, 0, 18) > red(&plain, 0, 18) + 8, "the left edge shows content from further in: {} vs {}", red(&zoomed, 0, 18), red(&plain, 0, 18));
    assert!((red(&zoomed, 32, 18) - red(&plain, 32, 18)).abs() <= 2, "the centre (the point) stays");
}

/// Spec §9: every Look changes the picture; strength 0 and the «До» view do not.
#[test]
fn a_look_changes_the_picture_and_strength_zero_or_the_before_view_do_not() {
    let t = Time::from_secs(2);
    let plain = render(&project(vec![]), t);
    for key in kadr_effects::look::preset_keys() {
        assert_ne!(render(&project(vec![fx("look", &[("preset", ParamValue::Choice(key.to_string()))])]), t), plain, "{key} changes the picture");
    }
    assert_eq!(render(&project(vec![fx("look", &[("strength", ParamValue::Number(0.0))])]), t), plain, "strength 0");
    let graded = project(vec![fx("look", &[]), fx("vignette", &[])]);
    assert_ne!(render(&graded, t), plain);
    assert_eq!(render_scene(&ProjectScenes::new(&graded, true).scene_at(t, &out())), plain, "«До» shows the clip without its Look");
}

/// Review focus 2 at the pixel level, with grain and a vignette.
#[test]
fn a_split_keeps_every_pixel_of_a_look() {
    let whole = project(vec![fx("look", &[("preset", ParamValue::Choice("vintage".into())), ("strength", ParamValue::Number(1.0))]), fx("vignette", &[])]);
    let mut split = whole.clone();
    let c = split.sequence().tracks[0].clips[0].clone();
    let (l, r) = split_clip(&c, FrameRate::FPS_30.frame_to_time(37), &mut LinkRemap::default());
    split.sequence_mut().tracks[0].clips = vec![l, r];
    let plain = project(vec![]);
    let mut graded = 0;
    for f in 30..45 {
        let t = FrameRate::FPS_30.frame_to_time(f);
        graded += (render(&whole, t) != render(&plain, t)) as u32;
        let worst = render(&whole, t).iter().zip(render(&split, t)).map(|(a, b)| a.abs_diff(b)).max().unwrap();
        assert!(worst <= 1, "frame {f}: off by {worst}");
    }
    assert_eq!(graded, 15, "the Look shows on every frame");
    // The Look and the vignette hold still: only the grain moves, so the
    // test keeps covering grain whatever looks.json's numbers become.
    let [a, b] = [40, 41].map(|f| render(&split, FrameRate::FPS_30.frame_to_time(f)));
    assert_ne!(a, b, "the grain moves from frame to frame");
}

/// The hover preview's snapshot carries colour too (spec §6.1): a Look put
/// on the snapshot's copy of the sequence reaches the layer's effects and
/// the picture; the project itself has none.
#[test]
fn the_hover_snapshot_carries_a_look_s_colour() {
    let p = project(vec![]);
    let t = Time::from_secs(2);
    let look = fx("look", &[("preset", ParamValue::Choice("dramatic".into()))]);
    let hovered = ProjectScenes::new_with(&p, false, |seq| {
        seq.tracks[0].clips[0].put_effect(look.clone());
    });
    let scene = hovered.scene_at(t, &out());
    assert!(!scene.layers[0].effects.is_empty(), "the Look's colour primitives");
    assert_ne!(render_scene(&scene), render(&p, t));
    assert!(ProjectScenes::new(&p, false).scene_at(t, &out()).layers[0].effects.is_empty(), "the project has none");
}
