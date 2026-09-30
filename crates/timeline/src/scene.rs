//! Scene evaluator (render spec §4): the timeline at one instant as a
//! `FrameScene` — video tracks bottom to top, placements in canvas pixels,
//! references to media (not pixels). Pure: renderers and playback never see
//! the timeline, and the timeline never sees a renderer.

use kadr_core::color::AlphaMode;
use kadr_core::{MediaKind, Time, TimeRange};
use kadr_project::{Clip, ColorAdjust as ClipColor, Project, Sequence, Track, TrackKind, Transform, TransitionKind};
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
    cull(&mut scene);
    scene
}

/// Below this a layer cannot change an 8-bit pixel.
const MIN_OPACITY: f32 = 0.5 / 255.0;

/// Drops layers that cannot affect the frame (render spec §7.3), so nothing
/// invisible is ever decoded or drawn.
pub fn cull(scene: &mut FrameScene) {
    let canvas = RectF::new(0.0, 0.0, scene.canvas.w as f32, scene.canvas.h as f32);
    scene.layers.retain(|l| is_visible(l, &canvas));
    if let Some(top) = scene.layers.iter().rposition(|l| covers_opaquely(l, &canvas)) {
        scene.layers.drain(..top);
    }
}

fn is_visible(l: &Layer, canvas: &RectF) -> bool {
    if matches!(l.content, LayerContent::Transition(_)) {
        return true;
    }
    l.opacity >= MIN_OPACITY
        && l.placement.scale.x != 0.0
        && l.placement.scale.y != 0.0
        && !l.crop.is_empty()
        && !l.placement.canvas_bounds(&l.crop).intersect(canvas).is_empty()
}

fn covers_opaquely(l: &Layer, canvas: &RectF) -> bool {
    let opaque_content = match &l.content {
        LayerContent::Media { media, .. } => media.kind == SourceKind::Video && media.color.alpha == AlphaMode::Opaque,
        LayerContent::Solid(c) => c.a >= 1.0,
        LayerContent::Transition(_) => false,
    };
    opaque_content
        && l.opacity >= 1.0
        && l.blend == BlendMode::Normal
        && l.placement.rotation == 0.0
        && l.crop == l.placement.full_crop()
        && l.placement.canvas_bounds(&l.crop).contains_rect(canvas)
}

fn track_layer(project: &Project, seq: &Sequence, track: &Track, t: Time, canvas: SizeU) -> Option<Layer> {
    if let Some(layer) = transition_layer(project, seq, track, t, canvas) {
        return Some(layer);
    }
    let clip = track.clip_at(t).filter(|c| c.enabled)?;
    clip_layer(project, clip, t, canvas)
}

/// The window of a transition over the cut at `at` whose (clamped)
/// length is `d`: `[at − ⌊d/2⌋, at − ⌊d/2⌋ + d)`, centred on the cut.
fn window(at: Time, d: Time) -> TimeRange {
    let start = at - Time(d.flicks() / 2);
    TimeRange::new(start, start + d)
}

/// The transition active on `track` at `t`. A transition is valid only when
/// both clips at its cut exist and are enabled (the one ending at `at` and
/// the one starting there); stale ones are skipped, so they never hide a
/// valid one. Like export, its length is clamped to both neighbours —
/// `min(duration, outgoing, incoming)` — and under two frames there is no
/// transition. Among valid transitions whose clamped window contains `t`,
/// the earlier cut wins.
fn transition_layer(project: &Project, seq: &Sequence, track: &Track, t: Time, canvas: SizeU) -> Option<Layer> {
    let min_len = seq.frame_rate.frame_to_time(2);
    let (tr, outgoing, incoming, w) = seq
        .transitions
        .iter()
        .filter(|tr| tr.track == track.id)
        .filter_map(|tr| {
            let outgoing = track.clips.iter().find(|c| c.timeline_out == tr.at && c.enabled)?;
            let incoming = track.clips.iter().find(|c| c.timeline_in == tr.at && c.enabled)?;
            let d = tr.duration.min(outgoing.duration()).min(incoming.duration());
            let w = window(tr.at, d);
            (d >= min_len && w.contains(t)).then_some((tr, outgoing, incoming, w))
        })
        .min_by_key(|(tr, ..)| tr.at)?;
    let progress = ((t - w.start).flicks() as f64 / w.duration().flicks() as f64).clamp(0.0, 1.0) as f32;
    let op = match tr.kind {
        TransitionKind::CrossDissolve => TransitionOp::Dissolve,
        TransitionKind::DipToBlack => TransitionOp::DipToColor(Rgba::BLACK),
        // Edge moves right → left, the incoming clip enters from the right:
        // the same picture as export's `xfade=wipeleft`.
        TransitionKind::Wipe => TransitionOp::Wipe { angle: std::f32::consts::PI, softness: 0.0 },
    };
    let from = clip_layer(project, outgoing, t, canvas)?;
    let to = clip_layer(project, incoming, t, canvas)?;
    let placement = Placement::fill(canvas);
    Some(Layer {
        id: LayerId::from(tr.id),
        content: LayerContent::Transition(Box::new(TransitionLayer { op, progress, from: vec![from], to: vec![to] })),
        crop: placement.full_crop(),
        placement,
        opacity: 1.0,
        blend: BlendMode::Normal,
        effects: vec![],
    })
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

    use kadr_core::TransitionId;
    use kadr_project::{Transition, TransitionKind};

    fn add_transition(p: &mut Project, track: usize, at_ms: i64, dur_ms: i64, kind: TransitionKind) -> TransitionId {
        let id = TransitionId::new();
        let track_id = p.sequence().tracks[track].id;
        p.sequence_mut().transitions.push(Transition { id, kind, track: track_id, at: Time::from_millis(at_ms), duration: Time::from_millis(dur_ms) });
        id
    }

    fn transition(l: &Layer) -> &TransitionLayer {
        match &l.content {
            LayerContent::Transition(t) => t,
            other => panic!("not a transition: {other:?}"),
        }
    }

    #[test]
    fn dissolve_window_is_centred_on_the_cut_with_exact_progress() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 2_000);
        let b = place(&mut p, 0, &hd(), 10_000, 2_000, 2_000);
        let tid = add_transition(&mut p, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        for (ms, progress) in [(1_500, 0.0), (2_000, 0.5), (2_250, 0.75)] {
            let s = scene_at(&p, ms);
            assert_eq!(s.layers.len(), 1);
            assert_eq!(s.layers[0].id, LayerId::from(tid));
            let tr = transition(&s.layers[0]);
            assert_eq!(tr.op, TransitionOp::Dissolve);
            assert!((tr.progress - progress).abs() < 1e-6, "t={ms}: {}", tr.progress);
            assert_eq!((tr.from[0].id, tr.to[0].id), (LayerId::from(a), LayerId::from(b)));
        }
        assert_eq!(scene_at(&p, 1_499).layers[0].id, LayerId::from(a));
        assert_eq!(scene_at(&p, 2_500).layers[0].id, LayerId::from(b), "the window end is exclusive");
    }

    #[test]
    fn progress_is_frame_exact_at_29_97() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 4_000);
        place(&mut p, 0, &hd(), 0, 4_000, 4_000);
        let fr = FrameRate::FPS_29_97;
        let d = fr.frame_to_time(30);
        let track = p.sequence().tracks[0].id;
        p.sequence_mut().transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, track, at: Time::from_secs(4), duration: d });
        let start = Time::from_secs(4) - Time(d.flicks() / 2);
        for k in [0, 1, 7, 15, 29] {
            let t = start + fr.frame_to_time(k);
            let s = evaluate(&p, p.sequence(), t, &OutputSpec::new(SizeU::new(960, 540), RenderQuality::Export));
            assert!((transition(&s.layers[0]).progress - k as f32 / 30.0).abs() < 1e-6, "frame {k}");
        }
    }

    #[test]
    fn transition_source_times_extend_past_the_clip_and_clamp_at_zero() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 2_000);
        place(&mut p, 0, &hd(), 0, 2_000, 2_000); // incoming media has nothing before its in-point
        add_transition(&mut p, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        let before_cut = scene_at(&p, 1_750);
        assert_eq!(media(&transition(&before_cut.layers[0]).to[0]).1, Time::ZERO, "clamped, not negative");
        let after_cut = scene_at(&p, 2_250);
        assert_eq!(media(&transition(&after_cut.layers[0]).from[0]).1, Time::from_millis(2_250), "outgoing handle past its out-point");
    }

    #[test]
    fn missing_neighbour_or_disabled_clip_means_no_transition() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 2_000);
        place(&mut p, 0, &hd(), 0, 2_500, 2_000); // gap after the cut
        add_transition(&mut p, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        assert_eq!(scene_at(&p, 1_750).layers[0].id, LayerId::from(a));
        assert!(scene_at(&p, 2_100).layers.is_empty(), "gap stays a gap");
        let mut q = Project::new("t");
        let a = place(&mut q, 0, &hd(), 0, 0, 2_000);
        place(&mut q, 0, &hd(), 0, 2_000, 2_000);
        q.sequence_mut().tracks[0].clips[1].enabled = false;
        add_transition(&mut q, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        assert_eq!(scene_at(&q, 1_750).layers[0].id, LayerId::from(a));
    }

    #[test]
    fn adjacent_clamped_windows_touch_and_the_later_one_starts_at_its_edge() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 1_000);
        let b = place(&mut p, 0, &hd(), 0, 1_000, 400);
        let c = place(&mut p, 0, &hd(), 0, 1_400, 1_600);
        add_transition(&mut p, 0, 1_000, 1_000, TransitionKind::CrossDissolve);
        add_transition(&mut p, 0, 1_400, 1_000, TransitionKind::CrossDissolve);
        let tr = transition(&scene_at(&p, 1_100).layers[0]).clone();
        assert_eq!((tr.from[0].id, tr.to[0].id), (LayerId::from(a), LayerId::from(b)));
        assert!((tr.progress - 0.75).abs() < 1e-6, "{}", tr.progress);
        let tr = transition(&scene_at(&p, 1_200).layers[0]).clone();
        assert_eq!((tr.from[0].id, tr.to[0].id), (LayerId::from(b), LayerId::from(c)), "[800, 1200) and [1200, 1600) only touch");
        assert!(tr.progress.abs() < 1e-6, "{}", tr.progress);
    }

    #[test]
    fn stale_transition_does_not_hide_a_valid_one() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 1_000);
        let b = place(&mut p, 0, &hd(), 0, 1_000, 2_000);
        add_transition(&mut p, 0, 900, 1_000, TransitionKind::CrossDissolve); // no cut at 900 any more
        let valid = add_transition(&mut p, 0, 1_000, 1_000, TransitionKind::CrossDissolve);
        let s = scene_at(&p, 700);
        assert_eq!(s.layers[0].id, LayerId::from(valid));
        let tr = transition(&s.layers[0]);
        assert_eq!((tr.from[0].id, tr.to[0].id), (LayerId::from(a), LayerId::from(b)));
        assert!((tr.progress - 0.2).abs() < 1e-6, "{}", tr.progress);
    }

    #[test]
    fn a_transition_longer_than_a_neighbour_is_clamped_to_it() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 1_000);
        place(&mut p, 0, &hd(), 0, 1_000, 400);
        let c = place(&mut p, 0, &hd(), 0, 1_400, 1_600);
        add_transition(&mut p, 0, 1_000, 1_000, TransitionKind::CrossDissolve);
        assert_eq!(scene_at(&p, 1_450).layers[0].id, LayerId::from(c), "the clamped window [800, 1200) never reaches c");
        assert!(matches!(scene_at(&p, 1_200).layers[0].content, LayerContent::Media { .. }), "window end is exclusive");
        assert!(matches!(scene_at(&p, 799).layers[0].content, LayerContent::Media { .. }));
        assert!((transition(&scene_at(&p, 1_100).layers[0]).progress - 0.75).abs() < 1e-6);
    }

    #[test]
    fn a_transition_clamped_under_two_frames_is_dropped() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 1_000);
        let b = place(&mut p, 0, &hd(), 0, 1_000, 50); // 1.5 frames at 30 fps
        add_transition(&mut p, 0, 1_000, 1_000, TransitionKind::CrossDissolve);
        assert_eq!(scene_at(&p, 990).layers[0].id, LayerId::from(a));
        assert_eq!(scene_at(&p, 1_010).layers[0].id, LayerId::from(b));
    }


    #[test]
    fn transition_kinds_map_to_render_ops() {
        for (kind, op) in [
            (TransitionKind::DipToBlack, TransitionOp::DipToColor(Rgba::BLACK)),
            // Edge right → left, incoming from the right: export's `xfade=wipeleft`.
            (TransitionKind::Wipe, TransitionOp::Wipe { angle: std::f32::consts::PI, softness: 0.0 }),
        ] {
            let mut p = Project::new("t");
            place(&mut p, 0, &hd(), 0, 0, 2_000);
            place(&mut p, 0, &hd(), 0, 2_000, 2_000);
            add_transition(&mut p, 0, 2_000, 1_000, kind);
            assert_eq!(transition(&scene_at(&p, 2_000).layers[0]).op, op);
        }
    }

    fn ids(s: &FrameScene) -> Vec<LayerId> {
        s.layers.iter().map(|l| l.id).collect()
    }

    fn two_layers(edit_upper: impl FnOnce(&mut kadr_project::Clip)) -> (Project, ClipId, ClipId) {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let lower = place(&mut p, 0, &hd(), 0, 0, 5_000);
        let upper = place(&mut p, v2, &hd(), 0, 0, 5_000);
        edit_upper(&mut p.sequence_mut().tracks[v2].clips[0]);
        (p, lower, upper)
    }

    #[test]
    fn fully_transparent_zero_scale_and_offscreen_layers_are_dropped() {
        for edit in [
            (|c: &mut kadr_project::Clip| c.transform.opacity = 0.0) as fn(&mut kadr_project::Clip),
            |c| c.transform.scale = 0.0,
            |c| c.transform.x = 5_000.0,
            |c| c.transform.crop_left = 1.0,
        ] {
            let (p, lower, _) = two_layers(edit);
            assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(lower)]);
        }
    }

    #[test]
    fn an_opaque_full_frame_video_hides_everything_below() {
        let (p, _, upper) = two_layers(|_| {});
        assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(upper)], "nothing below is decoded");
    }

    #[test]
    fn translucent_rotated_cropped_scaled_or_letterboxed_layers_keep_what_is_below() {
        for edit in [
            (|c: &mut kadr_project::Clip| c.transform.opacity = 0.99) as fn(&mut kadr_project::Clip),
            |c| c.transform.rotation_deg = 1.0,
            |c| c.transform.crop_top = 0.1,
            |c| c.transform.scale = 0.5,
            |c| c.transform.x = 10.0,
        ] {
            let (p, lower, upper) = two_layers(edit);
            assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(lower), LayerId::from(upper)]);
        }
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let lower = place(&mut p, 0, &hd(), 0, 0, 5_000);
        let square = place(&mut p, v2, &asset(MediaKind::Video, 1080, 1080, 0, (1, 1)), 0, 0, 5_000);
        assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(lower), LayerId::from(square)], "pillarbox bars show the lower track");
    }

    #[test]
    fn png_logo_with_alpha_never_occludes() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let video = place(&mut p, 0, &hd(), 0, 0, 5_000);
        let logo = place(&mut p, v2, &asset(MediaKind::Image, 1920, 1080, 0, (1, 1)), 0, 0, 5_000);
        assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(video), LayerId::from(logo)]);
    }

    #[test]
    fn alpha_video_never_occludes() {
        use kadr_core::color::AlphaMode;
        use kadr_core::ColorInfo;
        let alpha_video = |color: Option<ColorInfo>| {
            let mut a = asset(MediaKind::Video, 1920, 1080, 0, (1, 1));
            let v = a.info.video.as_mut().unwrap();
            (v.pixel_format, v.color) = ("yuva444p10le".into(), color);
            a
        };
        let tagged = ColorInfo { alpha: AlphaMode::Straight, ..ColorInfo::guess_video(1920, 1080) };
        for (overlay, what) in [(alpha_video(None), "legacy project, colour guessed"), (alpha_video(Some(tagged)), "probed colour")] {
            let mut p = Project::new("t");
            let v2 = add_video_track(&mut p);
            let below = place(&mut p, 0, &hd(), 0, 0, 5_000);
            let above = place(&mut p, v2, &overlay, 0, 0, 5_000);
            assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(below), LayerId::from(above)], "{what}");
        }
    }

    #[test]
    fn transitions_are_kept_and_do_not_occlude() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let below = place(&mut p, 0, &hd(), 0, 0, 4_000);
        place(&mut p, v2, &hd(), 0, 0, 2_000);
        place(&mut p, v2, &hd(), 0, 2_000, 2_000);
        let tid = add_transition(&mut p, v2, 2_000, 1_000, TransitionKind::DipToBlack);
        assert_eq!(ids(&scene_at(&p, 2_000)), vec![LayerId::from(below), LayerId::from(tid)]);
    }
}
