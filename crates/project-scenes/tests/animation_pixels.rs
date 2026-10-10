//! Evaluator → renderer: animated opacity and a clip fade produce the
//! expected pixels (keyframes spec §8 "пиксели"). No decoder: the media
//! layer gets a solid white frame.

use kadr_core::color::AlphaMode;
use kadr_core::{ColorInfo, CpuFrame, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::{AnimParam, Clip, EdgeFades, Ease, MediaAsset, Project, SourceTime};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{LayerContent, OutputSpec, RenderQuality, SizeU};
use kadr_timeline::scene::evaluate;
use std::sync::Arc;

fn project() -> Project {
    let mut p = Project::new("px");
    p.sequence_mut().frame_rate = FrameRate::FPS_30;
    p.sequence_mut().width = 64;
    p.sequence_mut().height = 36;
    let video = VideoInfo { width: 64, height: 36, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("w.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None });
    let mut c = Clip::new(a.id, "w", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
    // Opacity 0 → 1 over source 1..2 s; the fade-out spans the last second
    // (2.967 .. 3.967 s, the last frame starting at 4 s − 1/30 s).
    c.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_secs(1)), 0.0, Some(Ease::Linear));
    c.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_secs(2)), 1.0, None);
    c.video_fade = EdgeFades { fade_in: Time::ZERO, fade_out: Time::from_secs(1) };
    p.assets.push(a);
    p.sequence_mut().tracks[0].clips.push(c);
    p
}

/// Centre pixel's red channel at timeline time `t`.
fn red_at(p: &Project, t: Time) -> u8 {
    let out = OutputSpec::new(SizeU::new(64, 36), RenderQuality::Export);
    let scene = evaluate(p, p.sequence(), t, &out);
    let white = Arc::new(CpuFrame::from_rgba8(64, 36, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, vec![255; 64 * 36 * 4]));
    let inputs = RenderInputs { layers: scene.layers.iter().map(|l| match l.content { LayerContent::Media { .. } => LayerInput::Cpu(white.clone()), _ => LayerInput::None }).collect() };
    let mut buf = vec![0u8; 64 * 36 * 4];
    CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(64, 36, &mut buf))).unwrap();
    buf[(18 * 64 + 32) * 4]
}

#[test]
fn animated_opacity_and_fade_reach_the_pixels() {
    let p = project();
    let ms = Time::from_millis;
    assert_eq!(red_at(&p, ms(500)), 0, "before the first keyframe: opacity 0 (culled, black)");
    let half = red_at(&p, ms(1_500));
    assert!((half as i32 - 128).abs() <= 1, "halfway: {half}");
    assert_eq!(red_at(&p, ms(2_500)), 255, "opaque, before the fade-out");
    let mid_fade = red_at(&p, ms(3_500));
    assert!((mid_fade as i32 - 119).abs() <= 1, "opacity 1 × fade (3.967 − 3.5) / 1 ≈ 0.467 of white: {mid_fade}");
    let last = Time::from_secs(4) - FrameRate::FPS_30.frame_duration();
    assert_eq!(red_at(&p, last), 0, "last frame of the fade-out is transparent");
}
