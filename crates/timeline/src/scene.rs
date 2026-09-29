//! Scene evaluator (render spec §4): the timeline at one instant as a
//! `FrameScene` — video tracks bottom to top, placements in canvas pixels,
//! references to media (not pixels). Pure: renderers and playback never see
//! the timeline, and the timeline never sees a renderer.

use kadr_core::{MediaKind, Time};
use kadr_project::{Clip, ColorAdjust as ClipColor, Project, Sequence, Track, TrackKind, Transform};
use kadr_scene::*;

pub fn evaluate(project: &Project, seq: &Sequence, t: Time, out: &OutputSpec) -> FrameScene {
    let canvas = SizeU::new(seq.width.max(1), seq.height.max(1));
    let mut scene = FrameScene::empty(t, canvas, *out);
    if t < Time::ZERO || t >= seq.duration() {
        return scene;
    }
    for track in seq.tracks.iter().filter(|tr| tr.kind == TrackKind::Video && !tr.muted) {
        if let Some(layer) = track_layer(project, seq, track, t, canvas) {
            scene.layers.push(layer);
        }
    }
    scene
}

fn track_layer(project: &Project, _seq: &Sequence, track: &Track, t: Time, canvas: SizeU) -> Option<Layer> {
    let clip = track.clip_at(t).filter(|c| c.enabled)?;
    clip_layer(project, clip, t, canvas)
}

/// One clip at timeline time `t` (which may lie outside the clip, for
/// transition handles: the source time is extrapolated, then clamped).
pub fn clip_layer(project: &Project, clip: &Clip, t: Time, canvas: SizeU) -> Option<Layer> {
    let asset = project.asset(clip.asset)?;
    let kind = match asset.info.kind {
        MediaKind::Video => SourceKind::Video,
        MediaKind::Image => SourceKind::Image,
        MediaKind::Audio => return None,
    };
    let (dw, dh) = asset.info.video.as_ref()?.display_size();
    if dw == 0 || dh == 0 {
        return None;
    }
    let display_size = SizeU::new(dw, dh);
    let source_time = match kind {
        SourceKind::Image => Time::ZERO,
        SourceKind::Video => clip.source_time_at(t).max(Time::ZERO).min(asset.info.duration),
    };
    let placement = placement_of(&clip.transform, fit_contain(display_size, canvas), canvas);
    let crop = crop_of(&clip.transform, placement.size)?;
    let mut effects = vec![];
    let color = color_of(&clip.color);
    if !color.is_neutral() {
        effects.push(Effect::ColorAdjust(color));
    }
    Some(Layer {
        id: LayerId::from(clip.id),
        content: LayerContent::Media {
            media: MediaRef { media: clip.asset, stream: 0, kind, display_size, color: asset.info.source_color() },
            source_time,
        },
        placement,
        crop,
        opacity: clip.transform.opacity.clamp(0.0, 1.0) as f32,
        blend: BlendMode::Normal,
        effects,
    })
}

/// Largest size with the source's aspect that fits the canvas (contain).
pub fn fit_contain(src: SizeU, canvas: SizeU) -> Vec2 {
    let s = (canvas.w as f64 / src.w as f64).min(canvas.h as f64 / src.h as f64);
    Vec2::new((src.w as f64 * s) as f32, (src.h as f64 * s) as f32)
}

/// The clip `Transform` (offset from centre, uniform scale, degrees) as a placement.
fn placement_of(tr: &Transform, size: Vec2, canvas: SizeU) -> Placement {
    Placement {
        size,
        anchor: Vec2::new(0.5, 0.5),
        position: Vec2::new((canvas.w as f64 / 2.0 + tr.x) as f32, (canvas.h as f64 / 2.0 + tr.y) as f32),
        scale: Vec2::new(tr.scale as f32, tr.scale as f32),
        rotation: tr.rotation_deg.to_radians() as f32,
    }
}

/// Crop fractions → rectangle in local pixels; `None` when nothing is left.
fn crop_of(tr: &Transform, size: Vec2) -> Option<RectF> {
    let f = |v: f64| v.clamp(0.0, 1.0) as f32;
    let r = RectF::new(f(tr.crop_left) * size.x, f(tr.crop_top) * size.y, (1.0 - f(tr.crop_right)) * size.x, (1.0 - f(tr.crop_bottom)) * size.y);
    (!r.is_empty()).then_some(r)
}

fn color_of(c: &ClipColor) -> ColorAdjust {
    ColorAdjust { exposure: c.exposure as f32, contrast: c.contrast as f32, saturation: c.saturation as f32, temperature: c.temperature as f32, tint: 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{ClipId, FrameRate, MediaInfo, MediaKind, TimeRange, VideoInfo};
    use kadr_project::{Clip, MediaAsset, Track, TrackKind};

    pub(crate) fn asset(kind: MediaKind, w: u32, h: u32, rotation: i32, sar: (u32, u32)) -> MediaAsset {
        let video = VideoInfo { width: w, height: h, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation, sar, color: None };
        let color = if kind == MediaKind::Image { Some(kadr_core::ColorInfo::IMAGE_SRGB) } else { None };
        MediaAsset::new("m", MediaInfo { kind, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(VideoInfo { color, ..video }), audio: None, timecode: None })
    }

    /// Adds a video track above the existing ones; returns its index.
    pub(crate) fn add_video_track(p: &mut Project) -> usize {
        let seq = p.sequence_mut();
        let at = seq.tracks.iter().take_while(|t| t.kind == TrackKind::Video).count();
        seq.tracks.insert(at, Track::new(TrackKind::Video, format!("V{}", at + 1)));
        at
    }

    /// Places `asset` on track `track` from `tl_ms` for `dur_ms`, reading the source from `src_ms`.
    pub(crate) fn place(p: &mut Project, track: usize, a: &MediaAsset, src_ms: i64, tl_ms: i64, dur_ms: i64) -> ClipId {
        if p.asset(a.id).is_none() {
            p.assets.push(a.clone());
        }
        let c = Clip::new(a.id, "c", TimeRange::new(Time::from_millis(src_ms), Time::from_millis(src_ms + dur_ms)), Time::from_millis(tl_ms));
        let id = c.id;
        let clips = &mut p.sequence_mut().tracks[track].clips;
        clips.push(c);
        clips.sort_by_key(|c| c.timeline_in);
        id
    }

    pub(crate) fn scene_at(p: &Project, ms: i64) -> FrameScene {
        evaluate(p, p.sequence(), Time::from_millis(ms), &OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewHigh))
    }

    fn hd() -> MediaAsset {
        asset(MediaKind::Video, 1920, 1080, 0, (1, 1))
    }

    fn media(l: &Layer) -> (MediaRef, Time) {
        match &l.content {
            LayerContent::Media { media, source_time } => (*media, *source_time),
            other => panic!("not media: {other:?}"),
        }
    }

    #[test]
    fn one_clip_becomes_one_media_layer_with_exact_source_time() {
        let mut p = Project::new("t");
        let a = hd();
        let id = place(&mut p, 0, &a, 10_000, 2_000, 5_000);
        let s = scene_at(&p, 3_500);
        assert_eq!(s.canvas, SizeU::new(1920, 1080));
        assert_eq!(s.layers.len(), 1);
        let l = &s.layers[0];
        assert_eq!(l.id, LayerId::from(id));
        let (m, src) = media(l);
        assert_eq!((m.media, m.kind, m.display_size), (a.id, SourceKind::Video, SizeU::new(1920, 1080)));
        assert_eq!(src, Time::from_millis(11_500));
        assert_eq!((l.placement.size, l.placement.position), (Vec2::new(1920.0, 1080.0), Vec2::new(960.0, 540.0)));
        assert_eq!(l.crop, l.placement.full_crop());
        assert_eq!((l.opacity, l.blend), (1.0, BlendMode::Normal));
        assert!(l.effects.is_empty());
    }

    #[test]
    fn upper_video_tracks_are_drawn_above_lower_ones() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let (a, b) = (hd(), hd());
        let lower = place(&mut p, 0, &a, 0, 0, 5_000);
        let upper = place(&mut p, v2, &b, 0, 0, 5_000);
        p.sequence_mut().tracks[v2].clips[0].transform.scale = 0.5; // picture in picture
        let ids: Vec<LayerId> = scene_at(&p, 1_000).layers.iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![LayerId::from(lower), LayerId::from(upper)]);
    }

    #[test]
    fn muted_tracks_disabled_clips_and_audio_only_assets_add_nothing() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        place(&mut p, 0, &hd(), 0, 0, 5_000);
        place(&mut p, v2, &hd(), 0, 0, 5_000);
        p.sequence_mut().tracks[0].muted = true;
        p.sequence_mut().tracks[v2].clips[0].enabled = false;
        assert!(scene_at(&p, 1_000).layers.is_empty());
        let mut q = Project::new("t");
        let mut audio = hd();
        audio.info.kind = MediaKind::Audio;
        audio.info.video = None;
        place(&mut q, 0, &audio, 0, 0, 5_000);
        assert!(scene_at(&q, 1_000).layers.is_empty());
    }

    #[test]
    fn outside_the_sequence_or_in_a_gap_the_scene_is_empty_black() {
        let empty = Project::new("t");
        let s = scene_at(&empty, 0);
        assert!(s.layers.is_empty());
        assert_eq!(s.background, Rgba::BLACK);
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 1_000);
        place(&mut p, 0, &hd(), 0, 2_000, 1_000);
        for ms in [-1, 1_500, 3_000, 10_000] {
            assert!(scene_at(&p, ms).layers.is_empty(), "t = {ms} ms");
        }
    }

    #[test]
    fn speed_maps_source_time_exactly() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 10_000);
        p.sequence_mut().tracks[0].clips[0].timeline_out = Time::from_secs(5); // 2× speed
        assert_eq!(media(&scene_at(&p, 1_500).layers[0]).1, Time::from_secs(3));
    }

    #[test]
    fn transform_maps_to_placement_and_crop_in_local_space() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 5_000);
        let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
        (tr.x, tr.y, tr.scale, tr.rotation_deg, tr.crop_left, tr.opacity) = (100.0, -50.0, 0.5, 90.0, 0.25, 0.8);
        let l = &scene_at(&p, 1_000).layers[0];
        assert_eq!(l.placement.position, Vec2::new(1060.0, 490.0));
        assert_eq!(l.placement.scale, Vec2::new(0.5, 0.5));
        assert!((l.placement.rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert_eq!(l.crop, RectF::new(480.0, 0.0, 1920.0, 1080.0), "crop is local pixels; the rest stays in place");
        assert!((l.opacity - 0.8).abs() < 1e-6);
    }

    #[test]
    fn rotated_and_anamorphic_sources_fit_by_display_size() {
        let mut p = Project::new("t");
        place(&mut p, 0, &asset(MediaKind::Video, 1920, 1080, 90, (1, 1)), 0, 0, 5_000);
        let l = &scene_at(&p, 0).layers[0];
        assert_eq!(media(l).0.display_size, SizeU::new(1080, 1920));
        assert_eq!(l.placement.size, Vec2::new(607.5, 1080.0), "portrait phone video, pillarboxed");
        let mut q = Project::new("t");
        place(&mut q, 0, &asset(MediaKind::Video, 720, 480, 0, (8, 9)), 0, 0, 5_000);
        assert_eq!(scene_at(&q, 0).layers[0].placement.size, Vec2::new(1440.0, 1080.0), "4:3 anamorphic NTSC");
    }

    #[test]
    fn images_show_their_only_frame_with_image_colour() {
        let mut p = Project::new("t");
        place(&mut p, 0, &asset(MediaKind::Image, 400, 200, 0, (1, 1)), 0, 1_000, 5_000);
        let (m, src) = media(&scene_at(&p, 3_000).layers[0]);
        assert_eq!((m.kind, src), (SourceKind::Image, Time::ZERO));
        assert_eq!(m.color, kadr_core::ColorInfo::IMAGE_SRGB);
    }

    #[test]
    fn color_adjust_becomes_an_effect_only_when_not_neutral() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 5_000);
        assert!(scene_at(&p, 0).layers[0].effects.is_empty());
        p.sequence_mut().tracks[0].clips[0].color.saturation = 0.0;
        match &scene_at(&p, 0).layers[0].effects[..] {
            [Effect::ColorAdjust(c)] => assert_eq!((c.saturation, c.contrast), (0.0, 1.0)),
            other => panic!("{other:?}"),
        }
    }
}
