//! `SceneSource` over the project (render foundation M4): an immutable
//! snapshot the preview player reads on its own thread. A new snapshot is
//! taken whenever what the preview shows changes (an edit, undo/redo, a
//! live inspector drag, the bypass toggle, opening a project); the player
//! never sees the live `Project`.

use kadr_core::{AssetId, FrameRate, MediaKind, Time};
use kadr_playback::{MediaSource, SceneSource};
use kadr_project::{Project, TrackKind};
use kadr_scene::{FrameScene, Layer, LayerContent, LayerId, OutputSpec, SizeU, SourceKind};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ProjectScenes {
    /// The active sequence and the assets its clips use — nothing else
    /// (analysis, history and AI records are not copied).
    project: Arc<Project>,
    /// "Before" view: the clips' effects (colour) are dropped, geometry is kept.
    bypass: bool,
    media: HashMap<AssetId, MediaSource>,
    duration: Time,
    rate: FrameRate,
    canvas: SizeU,
}

impl ProjectScenes {
    /// Snapshots `project`'s active sequence. Whether each file exists is
    /// checked here, once.
    pub fn new(project: &Project, bypass: bool) -> Self {
        let seq = project.sequence();
        let used: std::collections::HashSet<AssetId> = seq.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.asset)).collect();
        let mut slim = Project::new(project.name.clone());
        slim.id = project.id;
        slim.assets = project.assets.iter().filter(|a| used.contains(&a.id)).cloned().collect();
        slim.sequences = vec![seq.clone()];
        slim.active_sequence = seq.id;
        let rate = seq.frame_rate;
        let media = slim.assets.iter().filter_map(|a| Some((a.id, media_source(a, rate)?))).collect();
        ProjectScenes {
            duration: seq.duration(),
            rate,
            canvas: SizeU::new(seq.width.max(1), seq.height.max(1)),
            project: Arc::new(slim),
            bypass,
            media,
        }
    }

    pub fn bypass(&self) -> bool {
        self.bypass
    }

    /// False for media missing at snapshot time (or unknown).
    pub fn is_online(&self, id: AssetId) -> bool {
        self.media.get(&id).is_some_and(|m| m.online)
    }

    pub fn media_sources(&self) -> &HashMap<AssetId, MediaSource> {
        &self.media
    }

    /// Any media layer of `scene` whose file is missing.
    pub fn any_offline(&self, scene: &FrameScene) -> bool {
        let mut offline = false;
        visit_media(&scene.layers, &mut |l| {
            if let LayerContent::Media { media, .. } = &l.content {
                offline |= !self.is_online(media.media);
            }
        });
        offline
    }

    /// The clip of the top-most media layer of `scene` (inside a
    /// transition, the incoming side from half way on).
    pub fn top_clip(&self, scene: &FrameScene) -> Option<kadr_core::ClipId> {
        let id = top_media_layer(&scene.layers)?;
        self.project.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter()).find(|c| LayerId::from(c.id) == id).map(|c| c.id)
    }
}

/// The resolver's view of an asset; `None` for audio and assets without a picture.
pub fn media_source(a: &kadr_project::MediaAsset, sequence_rate: FrameRate) -> Option<MediaSource> {
    let kind = match a.info.kind {
        MediaKind::Video => SourceKind::Video,
        MediaKind::Image => SourceKind::Image,
        MediaKind::Audio => return None,
    };
    let v = a.info.video.as_ref()?;
    let (w, h) = v.display_size();
    // A variable or undeclared rate has no exact frame grid of its own: the
    // sequence's is the best one (the decoder resamples to it).
    let rate = match v.frame_rate {
        Some(r) if !v.variable_frame_rate && r.num > 0 && r.den > 0 => r,
        _ => sequence_rate,
    };
    Some(MediaSource {
        path: a.path.clone(),
        kind,
        rate,
        duration: a.info.duration,
        display_size: SizeU::new(w, h),
        color: a.info.source_color(),
        online: a.exists(),
    })
}

fn visit_media(layers: &[Layer], f: &mut impl FnMut(&Layer)) {
    for l in layers {
        match &l.content {
            LayerContent::Transition(t) => {
                visit_media(&t.from, f);
                visit_media(&t.to, f);
            }
            _ => f(l),
        }
    }
}

fn top_media_layer(layers: &[Layer]) -> Option<LayerId> {
    layers.iter().rev().find_map(|l| match &l.content {
        LayerContent::Media { .. } => Some(l.id),
        LayerContent::Transition(t) => {
            let (first, second) = if t.progress >= 0.5 { (&t.to, &t.from) } else { (&t.from, &t.to) };
            top_media_layer(first).or_else(|| top_media_layer(second))
        }
        _ => None,
    })
}

fn strip_effects(layers: &mut [Layer]) {
    for l in layers {
        l.effects.clear();
        if let LayerContent::Transition(t) = &mut l.content {
            strip_effects(&mut t.from);
            strip_effects(&mut t.to);
        }
    }
}

impl SceneSource for ProjectScenes {
    fn scene_at(&self, t: Time, out: &OutputSpec) -> FrameScene {
        let mut scene = kadr_timeline::scene::evaluate(&self.project, self.project.sequence(), t, out);
        if self.bypass {
            strip_effects(&mut scene.layers);
        }
        scene
    }

    fn media(&self, id: AssetId) -> Option<MediaSource> {
        self.media.get(&id).cloned()
    }

    fn duration(&self) -> Time {
        self.duration
    }

    fn frame_rate(&self) -> FrameRate {
        self.rate
    }

    fn canvas(&self) -> SizeU {
        self.canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{MediaInfo, TimeRange, VideoInfo};
    use kadr_project::{Clip, MediaAsset, Transition, TransitionKind};
    use kadr_scene::{Effect, RenderQuality};

    fn asset(path: &std::path::Path, kind: MediaKind, w: u32, h: u32, rate: Option<FrameRate>, vfr: bool) -> MediaAsset {
        let video = (kind != MediaKind::Audio).then(|| VideoInfo {
            width: w,
            height: h,
            frame_rate: rate,
            variable_frame_rate: vfr,
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            rotation: 0,
            sar: (1, 1),
            color: None,
        });
        MediaAsset::new(path, MediaInfo { kind, duration: Time::from_secs(10), container: "mp4".into(), size_bytes: 0, video, audio: None, timecode: None })
    }

    fn place(p: &mut Project, track: usize, a: &MediaAsset, tl_ms: i64, dur_ms: i64) -> kadr_core::ClipId {
        if p.asset(a.id).is_none() {
            p.assets.push(a.clone());
        }
        let c = Clip::new(a.id, format!("clip{track}"), TimeRange::new(Time::ZERO, Time::from_millis(dur_ms)), Time::from_millis(tl_ms));
        let id = c.id;
        p.sequence_mut().tracks[track].clips.push(c);
        p.sequence_mut().tracks[track].clips.sort_by_key(|c| c.timeline_in);
        id
    }

    fn out() -> OutputSpec {
        OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewHigh)
    }

    fn with_v2(p: &mut Project) {
        let seq = p.sequence_mut();
        seq.tracks.insert(1, kadr_project::Track::new(TrackKind::Video, "V2"));
    }

    #[test]
    fn media_answers_from_the_snapshot_with_rate_size_colour_and_online() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.mp4");
        std::fs::write(&present, b"x").unwrap();
        let mut p = Project::new("t");
        let a = asset(&present, MediaKind::Video, 1920, 1080, Some(FrameRate::FPS_25), false);
        let vfr = asset(&dir.path().join("gone.mp4"), MediaKind::Video, 1280, 720, Some(FrameRate::FPS_25), true);
        let unused = asset(&present, MediaKind::Video, 64, 64, None, false);
        place(&mut p, 0, &a, 0, 2000);
        place(&mut p, 0, &vfr, 2000, 2000);
        p.assets.push(unused.clone());
        let s = ProjectScenes::new(&p, false);
        let m = s.media(a.id).unwrap();
        assert_eq!((m.kind, m.rate, m.display_size, m.duration, m.online), (SourceKind::Video, FrameRate::FPS_25, SizeU::new(1920, 1080), Time::from_secs(10), true));
        assert_eq!(m.color, a.info.source_color());
        assert_eq!(m.path, present);
        let v = s.media(vfr.id).unwrap();
        assert_eq!(v.rate, p.sequence().frame_rate, "a variable rate falls back to the sequence's");
        assert!(!v.online && !s.is_online(vfr.id));
        assert!(s.media(unused.id).is_none(), "assets no clip uses are not in the snapshot");
        assert_eq!(s.duration(), Time::from_secs(4));
        assert_eq!(s.canvas(), SizeU::new(1920, 1080));
        // Deleting the file afterwards does not change the snapshot: online is decided once.
        std::fs::remove_file(&present).unwrap();
        assert!(s.media(a.id).unwrap().online);
    }

    #[test]
    fn images_rotation_and_unknown_rates() {
        let mut p = Project::new("t");
        let mut phone = asset(std::path::Path::new("p.mp4"), MediaKind::Video, 1920, 1080, None, false);
        phone.info.video.as_mut().unwrap().rotation = 90;
        let img = asset(std::path::Path::new("logo.png"), MediaKind::Image, 400, 200, None, false);
        let audio = asset(std::path::Path::new("a.wav"), MediaKind::Audio, 0, 0, None, false);
        place(&mut p, 0, &phone, 0, 1000);
        let at = p.sequence().tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        place(&mut p, at, &audio, 0, 1000);
        with_v2(&mut p);
        place(&mut p, 1, &img, 0, 1000);
        let s = ProjectScenes::new(&p, false);
        let m = s.media(phone.id).unwrap();
        assert_eq!((m.display_size, m.rate), (SizeU::new(1080, 1920), p.sequence().frame_rate));
        assert_eq!(s.media(img.id).unwrap().kind, SourceKind::Image);
        assert!(s.media(audio.id).is_none());
    }

    #[test]
    fn scenes_are_the_evaluators_and_bypass_drops_effects_but_keeps_geometry() {
        let mut p = Project::new("t");
        let a = asset(std::path::Path::new("a.mp4"), MediaKind::Video, 1920, 1080, Some(FrameRate::FPS_30), false);
        let logo = asset(std::path::Path::new("logo.png"), MediaKind::Image, 400, 200, None, false);
        let base = place(&mut p, 0, &a, 0, 4000);
        with_v2(&mut p);
        let top = place(&mut p, 1, &logo, 0, 4000);
        {
            let seq = p.sequence_mut();
            let (ti, ci) = seq.locate_clip(top).unwrap();
            let c = &mut seq.tracks[ti].clips[ci];
            c.transform.scale = 0.25;
            c.transform.x = 500.0;
            c.color.saturation = 0.2;
        }
        let t = Time::from_millis(1000);
        let normal = ProjectScenes::new(&p, false);
        let scene = normal.scene_at(t, &out());
        assert_eq!(scene, kadr_timeline::scene::evaluate(&p, p.sequence(), t, &out()));
        assert_eq!(scene.layers.len(), 2);
        assert!(matches!(scene.layers[1].effects[..], [Effect::ColorAdjust(_)]));
        let before = ProjectScenes::new(&p, true).scene_at(t, &out());
        assert!(before.layers.iter().all(|l| l.effects.is_empty()));
        assert_eq!(before.layers[1].placement, scene.layers[1].placement, "geometry is kept (a PiP stays a PiP)");
        assert_eq!(normal.top_clip(&scene), Some(top));
        assert_eq!(normal.top_clip(&ProjectScenes::new(&p, false).scene_at(Time::from_millis(5000), &out())), None, "past the end: nothing");
        let _ = base;
    }

    #[test]
    fn top_clip_follows_a_transition_and_offline_is_reported() {
        let mut p = Project::new("t");
        let a = asset(std::path::Path::new("missing-a.mp4"), MediaKind::Video, 1920, 1080, Some(FrameRate::FPS_30), false);
        let b = asset(std::path::Path::new("missing-b.mp4"), MediaKind::Video, 1920, 1080, Some(FrameRate::FPS_30), false);
        let ca = place(&mut p, 0, &a, 0, 2000);
        let cb = place(&mut p, 0, &b, 2000, 2000);
        let track = p.sequence().tracks[0].id;
        p.sequence_mut().transitions.push(Transition { id: kadr_core::TransitionId::new(), kind: TransitionKind::CrossDissolve, track, at: Time::from_secs(2), duration: Time::from_secs(1) });
        let s = ProjectScenes::new(&p, false);
        let early = s.scene_at(Time::from_millis(1600), &out());
        assert!(matches!(early.layers[0].content, LayerContent::Transition(_)));
        assert_eq!(s.top_clip(&early), Some(ca));
        assert_eq!(s.top_clip(&s.scene_at(Time::from_millis(2400), &out())), Some(cb));
        assert!(s.any_offline(&early), "the files do not exist");
        assert!(!s.any_offline(&s.scene_at(Time::from_secs(9), &out())), "an empty scene has no offline media");
    }
}
