//! Inspector: slider drags mutate the clip live (no history), release
//! commits exactly one undoable command (`SetClipProperty`, or the keyframe
//! at the playhead for an animated property). With nothing selected it
//! shows the sequence and its markers.

use crate::anim_ui;
use crate::app::{App, Prompt};
use crate::util::rgb;
use crate::{InspectorData, KeyState, MarkerRow};
use kadr_core::{Time, Timecode};
use kadr_i18n::{duration, t, tf, tn};
use kadr_project::{AnimParam, AudioProps, Clip, ColorAdjust, Ease, SourceTime, TrackKind, Transform};
use kadr_timeline::{ClipProperty, EditCommand};
use slint::{ModelRc, SharedString, VecModel};

fn apply(c: &mut Clip, prop: &str, v: f64, frame: Time) {
    // Fade durations from a slider: seconds → whole frames (rounded).
    let frames = |secs: f64| {
        let f = frame.flicks().max(1);
        Time((Time::from_secs_f64(secs.max(0.0)).flicks() + f / 2) / f * f)
    };
    let t = &mut c.transform;
    match prop {
        "x" => t.x = v,
        "y" => t.y = v,
        "scale" => t.scale = v,
        "rotation" => t.rotation_deg = v,
        "opacity" => t.opacity = v,
        "crop-l" => t.crop_left = v,
        "crop-r" => t.crop_right = v,
        "crop-t" => t.crop_top = v,
        "crop-b" => t.crop_bottom = v,
        "exposure" => c.color.exposure = v,
        "contrast" => c.color.contrast = v,
        "saturation" => c.color.saturation = v,
        "gain" => c.audio.gain_db = v,
        "pan" => c.audio.pan = v,
        // Each fade fits in what the other one leaves (fade_in + fade_out ≤ duration).
        "fade-in" => c.audio.fade_in = frames(v).min(c.duration() - c.audio.fade_out),
        "fade-out" => c.audio.fade_out = frames(v).min(c.duration() - c.audio.fade_in),
        "vfade-in" => c.video_fade.fade_in = frames(v).min(c.duration() - c.video_fade.fade_out),
        "vfade-out" => c.video_fade.fade_out = frames(v).min(c.duration() - c.video_fade.fade_in),
        _ => {}
    }
}

fn property_for(prop: &str, c: &Clip) -> ClipProperty {
    match prop {
        "exposure" | "contrast" | "saturation" => ClipProperty::Color(c.color.clone()),
        "gain" | "pan" | "fade-in" | "fade-out" => ClipProperty::Audio(c.audio.clone()),
        "vfade-in" | "vfade-out" => ClipProperty::VideoFade(c.video_fade),
        _ => ClipProperty::Transform(c.transform.clone()),
    }
}

impl App {
    /// The clip the Inspector edits: a single selected clip, or the video
    /// clip of one selected linked video+audio pair (a click selects both).
    pub fn inspected(&self) -> Option<kadr_core::ClipId> {
        let sel = &self.tl.selection;
        if sel.len() == 1 {
            return Some(sel[0]);
        }
        let seq = self.project.sequence();
        let clips: Vec<_> = sel.iter().filter_map(|id| seq.clip(*id)).collect();
        let link = clips.first()?.link?;
        if clips.len() != sel.len() || clips.iter().any(|c| c.link != Some(link)) {
            return None;
        }
        let videos: Vec<_> = sel
            .iter()
            .filter(|id| seq.locate_clip(**id).is_some_and(|(ti, _)| seq.tracks[ti].kind == TrackKind::Video))
            .collect();
        (videos.len() == 1).then(|| *videos[0])
    }

    pub fn refresh_inspector(&mut self) {
        let ui = self.ui();
        let seq = self.project.sequence();
        let fr = seq.frame_rate;
        let multi = if self.inspected().is_some() { 0 } else { self.tl.selection.len() as i32 };
        let mut d = InspectorData { multi_count: multi, ..Default::default() };
        // Sequence summary (always filled; shown when no single clip is selected).
        let clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
        let vt = seq.tracks_of(TrackKind::Video).count();
        let at = seq.tracks_of(TrackKind::Audio).count();
        d.seq_name = seq.name.clone().into();
        d.seq_format = format!("{}×{} · {} fps · {} kHz", seq.width, seq.height, fr, seq.sample_rate / 1000).into();
        d.seq_duration = format!("{}  ({})", Timecode::from_time(seq.duration(), fr), duration(seq.duration().as_secs_f64())).into();
        d.seq_stats = format!(
            "{} · {} · {}",
            tn("inspector.seq.clips", clips as i64, &[]),
            tn("inspector.seq.vtracks", vt as i64, &[]),
            tn("inspector.seq.atracks", at as i64, &[])
        )
        .into();
        let markers: Vec<MarkerRow> = seq
            .markers
            .iter()
            .enumerate()
            .map(|(i, m)| MarkerRow { index: i as i32, name: m.name.clone().into(), time: Timecode::from_time(m.time, fr).to_string().into(), color: rgb(&m.color) })
            .collect();
        d.markers = ModelRc::new(VecModel::from(markers));

        if let Some((ti, ci)) = self.inspected().and_then(|id| seq.locate_clip(id)) {
            let c = &seq.tracks[ti].clips[ci];
            let is_audio = seq.tracks[ti].kind == TrackKind::Audio;
            let asset = self.project.asset(c.asset);
            d.has_clip = true;
            d.is_audio = is_audio;
            d.has_video_props = !is_audio;
            d.name = c.name.clone().into();
            d.source = asset.map(|a| a.path.display().to_string()).unwrap_or_default().into();
            d.timing = tf(
                "inspector.timing_line",
                &[
                    ("in", &Timecode::from_time(c.timeline_in, fr).to_string()),
                    ("out", &Timecode::from_time(c.timeline_out, fr).to_string()),
                    ("dur", &duration(c.duration().as_secs_f64())),
                    ("src", &Timecode::from_time(c.source_in, fr).to_string()),
                ],
            )
            .into();
            // Animatable values at the playhead, or at the nearest clip edge
            // when the playhead is off the clip (review focus 1).
            let frame = fr.frame_duration();
            let at = anim_ui::shown_time(c, self.playhead, frame);
            let v = |p| c.anim_value(p, at) as f32;
            use kadr_project::AnimParam as P;
            d.x = v(P::X);
            d.y = v(P::Y);
            d.scale = v(P::Scale);
            d.rotation = v(P::Rotation);
            d.opacity = v(P::Opacity);
            d.crop_l = v(P::CropLeft);
            d.crop_r = v(P::CropRight);
            d.crop_t = v(P::CropTop);
            d.crop_b = v(P::CropBottom);
            d.exposure = v(P::Exposure);
            d.contrast = v(P::Contrast);
            d.saturation = v(P::Saturation);
            let outside = anim_ui::edit_time(c, self.playhead, frame).is_none();
            d.outside = outside;
            let keys: Vec<KeyState> = P::ALL
                .iter()
                .map(|p| {
                    let k = anim_ui::key_info(c, *p, self.playhead, frame);
                    let ease = anim_ui::key_here(c, *p, self.playhead, frame).map_or("", |k| anim_ui::ease_key(&k.ease));
                    KeyState { state: k.state, prev: k.prev.is_some(), next: k.next.is_some(), enabled: !outside, ease: ease.into() }
                })
                .collect();
            d.keys = ModelRc::new(VecModel::from(keys));
            d.vfade_in = c.video_fade.fade_in.as_secs_f64() as f32;
            d.vfade_out = c.video_fade.fade_out.as_secs_f64() as f32;
            d.fade_max = c.duration().as_secs_f64() as f32;
            d.speed = c.speed() as f32;
            d.gain = c.audio.gain_db as f32;
            d.pan = c.audio.pan as f32;
            d.fade_in = c.audio.fade_in.as_secs_f64() as f32;
            d.fade_out = c.audio.fade_out.as_secs_f64() as f32;
            d.enabled = c.enabled;
            let fx: Vec<SharedString> = c.effects.iter().map(|e| SharedString::from(e.kind.as_str())).collect();
            d.effects = ModelRc::new(VecModel::from(fx));
            if !is_audio {
                let (rows, graded) = self.inspector_shots(c);
                d.shots = ModelRc::new(VecModel::from(rows));
                d.shots_graded = graded;
            }
        }
        ui.set_inspector(d);
    }

    pub fn inspector_live(&mut self, prop: &str, v: f32) {
        let Some(id) = self.inspected() else { return };
        if prop == "speed" {
            return; // changes duration: applied on commit only
        }
        let Some((ti, ci)) = self.project.sequence().locate_clip(id) else { return };
        if self.project.sequence().tracks[ti].locked {
            return;
        }
        if self.inspector_snapshot.as_ref().is_none_or(|(sid, _)| *sid != id) {
            self.inspector_snapshot = Some((id, self.project.sequence().tracks[ti].clips[ci].clone()));
        }
        let frame = self.project.sequence().frame_rate.frame_duration();
        let playhead = self.playhead;
        let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
        match anim_ui::param_of(prop) {
            // An animated property moves its keyframe at the playhead —
            // never one at a hidden time (the slider is disabled there).
            Some(p) if c.animation.is_animated(p) => {
                let Some(tt) = anim_ui::edit_time(c, playhead, frame) else { return };
                let at = anim_ui::key_here(c, p, playhead, frame).map_or(SourceTime(c.source_time_at(tt)), |k| k.at);
                c.animation.set_key(p, at, v as f64, None);
            }
            _ => apply(c, prop, v as f64, frame),
        }
        self.preview.invalidate();
        self.refresh_inspector();
        if matches!(prop, "gain" | "fade-in" | "fade-out") {
            self.refresh_timeline();
        } else {
            self.request_frame();
        }
    }

    pub fn inspector_commit(&mut self, prop: &str, v: f32) {
        let Some(id) = self.inspected() else { return };
        let Some((ti, ci)) = self.project.sequence().locate_clip(id) else { return };
        // Restore the pre-drag state, then apply as one undoable command.
        if let Some((sid, orig)) = self.inspector_snapshot.take() {
            if sid == id {
                self.project.sequence_mut().tracks[ti].clips[ci] = orig;
            }
        }
        let seq = self.project.sequence();
        let frame = seq.frame_rate.frame_duration();
        let cur = &seq.tracks[ti].clips[ci];
        let cmd = match anim_ui::param_of(prop) {
            // An animated property: one keyframe at the playhead, none at a hidden time.
            Some(p) if cur.animation.is_animated(p) => {
                let Some(tt) = anim_ui::edit_time(cur, self.playhead, frame) else { return self.show_rolled_back() };
                let here = anim_ui::key_here(cur, p, self.playhead, frame);
                if here.is_some_and(|k| k.value == v as f64) {
                    return self.show_rolled_back();
                }
                let at = here.map_or(SourceTime(cur.source_time_at(tt)), |k| k.at);
                let label = tf("cmd.keyframe_named", &[("name", &t(anim_ui::label_key(p)))]);
                let set = EditCommand::SetKeyframe { clip: id, param: p, at, value: v as f64, ease: None };
                EditCommand::Batch { label, commands: vec![set] }
            }
            _ if prop == "speed" => EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Speed(v as f64) },
            _ => {
                let mut c = cur.clone();
                apply(&mut c, prop, v as f64, frame);
                if c == *cur {
                    return self.show_rolled_back(); // nothing changed: no undo entry
                }
                EditCommand::SetClipProperty { clip: id, prop: property_for(prop, &c) }
            }
        };
        if !self.execute(cmd) {
            self.show_rolled_back();
        }
    }

    /// The live drag's changes were rolled back: show the clip as it is.
    fn show_rolled_back(&mut self) {
        self.preview.invalidate();
        self.refresh_inspector();
        self.request_frame();
    }

    /// `◀ ◆ ▶` and the row menu of an animatable property: `prev`, `next`,
    /// `toggle` (keyframe at the playhead), `clear` (property animation),
    /// `ease:<key>` (curve of the keyframe at the playhead). One click = at
    /// most one undo entry; nothing is sent when nothing would change.
    pub fn inspector_key(&mut self, prop: &str, action: &str) {
        let (Some(id), Some(p)) = (self.inspected(), anim_ui::param_of(prop)) else { return };
        let seq = self.project.sequence();
        let Some(c) = seq.clip(id) else { return };
        let frame = seq.frame_rate.frame_duration();
        let info = anim_ui::key_info(c, p, self.playhead, frame);
        let edit = anim_ui::edit_time(c, self.playhead, frame);
        let here = anim_ui::key_here(c, p, self.playhead, frame);
        // The picture at the playhead (or the nearest edge) stays as it is:
        // a first keyframe takes this value, removing the last one keeps it.
        let value_here = c.anim_value(p, anim_ui::shown_time(c, self.playhead, frame));
        let cmd = match action {
            "prev" | "next" => {
                if let Some(tt) = if action == "prev" { info.prev } else { info.next } {
                    self.set_playhead(tt);
                }
                return;
            }
            "toggle" => match (edit, here) {
                (Some(_), Some(k)) => EditCommand::RemoveKeyframe { clip: id, param: p, at: k.at, keep: Some(value_here) },
                (Some(tt), None) => EditCommand::SetKeyframe { clip: id, param: p, at: SourceTime(c.source_time_at(tt)), value: value_here, ease: None },
                (None, _) => return,
            },
            "clear" if c.animation.is_animated(p) => EditCommand::ClearAnimation { clip: id, statics: vec![(p, value_here)] },
            a if a.starts_with("ease:") => {
                let (Some(k), Some(e)) = (here, Ease::from_key(&a[5..])) else { return };
                if k.ease == e {
                    return;
                }
                EditCommand::SetKeyframeEase { clip: id, param: p, at: k.at, ease: e }
            }
            _ => return,
        };
        let label = tf("cmd.keyframe_named", &[("name", &t(anim_ui::label_key(p)))]);
        self.execute(EditCommand::Batch { label, commands: vec![cmd] });
    }

    /// "Reset" of a section: defaults **and** no animation of its
    /// properties, as one undo step (keyframes spec §5).
    pub fn inspector_reset(&mut self, section: &str) {
        let Some(id) = self.inspected() else { return };
        let Some(c) = self.project.sequence().clip(id) else { return };
        let params: &[AnimParam] = match section {
            "transform" => &AnimParam::ALL[..9],
            "color" => &AnimParam::ALL[9..],
            _ => &[],
        };
        let (prop, unchanged, name) = match section {
            "transform" => (ClipProperty::Transform(Transform::default()), c.transform == Transform::default(), "inspector.transform"),
            "color" => (ClipProperty::Color(ColorAdjust::default()), c.color == ColorAdjust::default(), "inspector.video"),
            _ => (ClipProperty::Audio(AudioProps::default()), c.audio == AudioProps::default(), "inspector.audio"),
        };
        let animated: Vec<(AnimParam, f64)> = params.iter().filter(|p| c.animation.is_animated(**p)).map(|p| (*p, p.default_value())).collect();
        if unchanged && animated.is_empty() {
            return; // already at the defaults: no undo entry
        }
        let mut commands = vec![EditCommand::SetClipProperty { clip: id, prop }];
        if !animated.is_empty() {
            commands.push(EditCommand::ClearAnimation { clip: id, statics: animated });
        }
        let label = tf("cmd.reset_section", &[("name", &t(name))]);
        if self.execute(EditCommand::Batch { label, commands }) {
            self.toast(t("toast.reset_done"));
        }
    }

    pub fn inspector_toggle_enabled(&mut self) {
        let Some(id) = self.inspected() else { return };
        let en = self.project.sequence().clip(id).map(|c| c.enabled).unwrap_or(true);
        self.execute(EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Enabled(!en) });
    }

    pub fn marker_action(&mut self, i: usize, action: &str) {
        let Some(m) = self.project.sequence().markers.get(i).cloned() else { return };
        match action {
            "goto" => self.set_playhead(m.time),
            "rename" => self.prompt(&t("dlg.rename_marker.title"), &t("dlg.rename_marker.label"), &m.name, Prompt::RenameMarker(m.id)),
            "delete" => {
                self.execute(EditCommand::RemoveMarker(m.id));
            }
            _ => {}
        }
    }

    pub fn sequence_action(&mut self, action: &str) {
        if action == "rename" {
            let name = self.project.sequence().name.clone();
            self.prompt(&t("dlg.rename_seq.title"), &t("dlg.rename_seq.label"), &name, Prompt::RenameSequence);
        }
    }
}
