//! Inspector: slider drags mutate the clip live (no history), release
//! commits exactly one undoable command (`SetClipProperty`, `UpdateEffect`
//! for an effect's number, or the keyframe at the playhead for an animated
//! property or effect number). With nothing selected it shows the sequence
//! and its markers.

use crate::anim_ui;
use crate::app::{App, Prompt};
use crate::util::{fmt_secs, rgb};
use crate::{InspectorData, KeyState, MarkerRow};
use kadr_core::{ClipId, Time, Timecode};
use kadr_i18n::{duration, t, tf, tn};
use kadr_project::{AnimParam, AudioProps, Clip, ColorAdjust, Ease, ParamValue, Sequence, SourceTime, TrackKey, TrackKind, Transform};
use kadr_timeline::commands::duration_at_speed;
use kadr_timeline::{ClipProperty, EditCommand};
use slint::{Model, ModelRc, VecModel};
use std::collections::BTreeMap;

fn apply(c: &mut Clip, prop: &str, v: f64, frame: Time) {
    // Fade durations from a slider: seconds → whole frames (rounded).
    let frames = |secs: f64| anim_ui::whole_frames(Time::from_secs_f64(secs.max(0.0)), frame);
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

/// The recorded (pre-drag) state of the clip a drag changes live — an
/// inspector slider, a timeline fade handle or keyframe diamond (one
/// pointer, so one drag at a time). A drag ends with a commit, which rolls back and records one
/// command; anything else that edits the project rolls back first, so live
/// changes never outlive their drag and no stale state is ever restored.
#[derive(Default)]
pub struct LiveEdit(Option<(ClipId, Clip)>);

impl LiveEdit {
    /// Before a live change of `id`: remembers its recorded state once per
    /// drag. An abandoned drag on another clip is rolled back first.
    pub fn arm(&mut self, seq: &mut Sequence, id: ClipId) {
        if self.0.as_ref().is_some_and(|(sid, _)| *sid != id) {
            self.roll_back(seq);
        }
        if self.0.is_none() {
            self.0 = seq.clip(id).map(|c| (id, c.clone()));
        }
    }

    /// The recorded state of `id` while a live change of it is armed — a
    /// drag that recomputes its preview from the start (timeline diamonds).
    pub fn recorded(&self, id: ClipId) -> Option<&Clip> {
        self.0.as_ref().filter(|(sid, _)| *sid == id).map(|(_, c)| c)
    }

    /// Restores the recorded state and disarms; `true` when a clip was restored.
    pub fn roll_back(&mut self, seq: &mut Sequence) -> bool {
        let Some((id, orig)) = self.0.take() else { return false };
        let Some((ti, ci)) = seq.locate_clip(id) else { return false };
        seq.tracks[ti].clips[ci] = orig;
        true
    }

    /// Drops the drag without touching the project (another one was loaded).
    pub fn forget(&mut self) {
        self.0 = None;
    }
}

/// `old` when it already holds `rows`: a new model makes Slint rebuild every
/// row of its `for` (a press in flight is lost, hovers reset), which a
/// periodic refresh during playback must not do.
fn same_or_new<T: Clone + PartialEq + 'static>(old: ModelRc<T>, rows: Vec<T>) -> ModelRc<T> {
    if old.row_count() == rows.len() && old.iter().zip(&rows).all(|(a, b)| a == *b) {
        old
    } else {
        ModelRc::new(VecModel::from(rows))
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

/// What a fade slider's commit changes: the i18n key of the fade's name,
/// the old and the new length; `None` for every other property.
fn fade_change(prop: &str, before: &Clip, after: &Clip) -> Option<(&'static str, Time, Time)> {
    Some(match prop {
        "fade-in" => ("inspector.fade_in", before.audio.fade_in, after.audio.fade_in),
        "fade-out" => ("inspector.fade_out", before.audio.fade_out, after.audio.fade_out),
        "vfade-in" => ("inspector.fade_in", before.video_fade.fade_in, after.video_fade.fade_in),
        "vfade-out" => ("inspector.fade_out", before.video_fade.fade_out, after.video_fade.fade_out),
        _ => return None,
    })
}

/// History label (with the property's `{name}`) of a keyframe action from
/// `◆` or the row menu: each action says what it did.
fn key_label(cmd: &EditCommand) -> &'static str {
    match cmd {
        EditCommand::RemoveKeyframe { .. } => "cmd.remove_keyframe_named",
        EditCommand::ClearAnimation { .. } => "cmd.clear_animation_named",
        EditCommand::SetKeyframeEase { .. } => "cmd.keyframe_ease_named",
        _ => "cmd.keyframe_named",
    }
}

/// History label of a section's "Reset" (sentence case, not the title).
fn reset_label(section: &str) -> &'static str {
    match section {
        "transform" => "cmd.reset_transform",
        "color" => "cmd.reset_color",
        _ => "cmd.reset_audio",
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
        // Lists keep their model when unchanged (`same_or_new`): this runs ~10
        // times a second during playback of an animated clip.
        let shown = ui.get_inspector();
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
        d.markers = same_or_new(shown.markers, markers);

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
            d.outside_hint = anim_ui::outside_hint(c, self.playhead, frame);
            let keys: Vec<KeyState> = P::ALL
                .iter()
                .map(|p| {
                    let k = anim_ui::key_info(c, *p, self.playhead, frame);
                    let ease = anim_ui::key_here(c, *p, self.playhead, frame).map_or("", |k| anim_ui::ease_key(&k.ease));
                    KeyState { state: k.state, prev: k.prev.is_some(), next: k.next.is_some(), enabled: !outside, ease: ease.into() }
                })
                .collect();
            d.keys = same_or_new(shown.keys, keys);
            d.vfade_in = c.video_fade.fade_in.as_secs_f64() as f32;
            d.vfade_out = c.video_fade.fade_out.as_secs_f64() as f32;
            d.fade_max = c.duration().as_secs_f64() as f32;
            d.speed = c.speed() as f32;
            d.gain = c.audio.gain_db as f32;
            d.pan = c.audio.pan as f32;
            d.fade_in = c.audio.fade_in.as_secs_f64() as f32;
            d.fade_out = c.audio.fade_out.as_secs_f64() as f32;
            d.enabled = c.enabled;
            if !is_audio {
                d.fx = self.fx_rows(c, shown.fx);
                let (rows, graded) = self.inspector_shots(c);
                d.shots = same_or_new(shown.shots, rows);
                d.shots_graded = graded;
            }
        }
        ui.set_inspector(d);
        // A selection change, an edit, undo/redo and a language change all
        // pass here: the effect cards follow (fit, hints, the hover preview).
        self.refresh_effects();
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
        self.live_edit.arm(self.project.sequence_mut(), id);
        let frame = self.project.sequence().frame_rate.frame_duration();
        let playhead = self.playhead;
        let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
        match anim_ui::track_of(prop) {
            // An animated property moves its keyframe at the playhead —
            // never one at a hidden time (the slider is disabled there).
            Some(k) if c.animation.is_animated(&k) => {
                let Some(tt) = anim_ui::edit_time(c, playhead, frame) else { return };
                let at = anim_ui::key_here(c, &k, playhead, frame).map_or(SourceTime(c.source_time_at(tt)), |key| key.at);
                let value = anim_ui::slider_value(&k, v);
                c.animation.set_key(&k, at, value, None);
            }
            // An effect's number: its stored value, live.
            Some(k @ TrackKey::Fx(_)) => {
                c.write_static(&k, anim_ui::slider_value(&k, v));
            }
            _ => apply(c, prop, v as f64, frame),
        }
        self.preview.invalidate();
        self.refresh_inspector();
        // The timeline draws the waveform (gain), fade diagonals and handles;
        // sound-only changes leave the picture as it is.
        let sound_only = matches!(prop, "gain" | "fade-in" | "fade-out");
        if sound_only || matches!(prop, "vfade-in" | "vfade-out") {
            self.refresh_timeline();
        }
        if !sound_only {
            self.request_frame();
        }
    }

    pub fn inspector_commit(&mut self, prop: &str, v: f32) {
        // Restore the pre-drag state, then apply as one undoable command.
        let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
        let Some((id, (ti, ci))) = self.inspected().and_then(|id| Some((id, self.project.sequence().locate_clip(id)?))) else {
            if rolled_back {
                self.show_rolled_back();
            }
            return;
        };
        let seq = self.project.sequence();
        let frame = seq.frame_rate.frame_duration();
        let cur = &seq.tracks[ti].clips[ci];
        let cmd = match anim_ui::track_of(prop) {
            // An animated property: one keyframe at the playhead, none at a hidden time.
            Some(k) if cur.animation.is_animated(&k) => {
                let Some(tt) = anim_ui::edit_time(cur, self.playhead, frame) else { return self.show_rolled_back() };
                let value = anim_ui::slider_value(&k, v);
                let here = anim_ui::key_here(cur, &k, self.playhead, frame);
                if here.is_some_and(|key| key.value == value) {
                    return self.show_rolled_back();
                }
                let at = here.map_or(SourceTime(cur.source_time_at(tt)), |key| key.at);
                let label = tf("cmd.keyframe_named", &[("name", &anim_ui::track_label(cur, &k))]);
                EditCommand::Batch { label, commands: vec![EditCommand::SetKeyframe { clip: id, param: k, at, value, ease: None }] }
            }
            // An effect's number: one UpdateEffect («Эффект: Плавный наезд»).
            Some(TrackKey::Fx(f)) => {
                let k = TrackKey::Fx(f.clone());
                let value = anim_ui::slider_value(&k, v);
                let Some(e) = cur.effect(f.effect) else { return self.show_rolled_back() };
                if cur.static_value(&k) == Some(value) {
                    return self.show_rolled_back(); // nothing changed: no undo entry
                }
                let label = tf("cmd.effect_named", &[("name", &crate::effects_ui::fx_name(e))]);
                let set = EditCommand::UpdateEffect { clip: id, id: f.effect, params: BTreeMap::from([(f.key.clone(), ParamValue::Number(value))]), enabled: None };
                EditCommand::Batch { label, commands: vec![set] }
            }
            _ if prop == "speed" => {
                // The engine frame-snaps the new length: a speed that keeps it
                // changes nothing, so no undo entry.
                let speed = v as f64;
                if speed > 0.0 && duration_at_speed(cur.source_out - cur.source_in, speed, seq.frame_rate) == cur.duration() {
                    return self.show_rolled_back();
                }
                EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Speed(speed) }
            }
            _ => {
                let mut c = cur.clone();
                apply(&mut c, prop, v as f64, frame);
                if c == *cur {
                    return self.show_rolled_back(); // nothing changed: no undo entry
                }
                let set = EditCommand::SetClipProperty { clip: id, prop: property_for(prop, &c) };
                // A fade is labelled like the timeline handle's: «Появление: 0,5 с → 1 с».
                match fade_change(prop, cur, &c) {
                    Some((name, old, new)) => {
                        let label = tf("cmd.fade_named", &[("name", &t(name)), ("from", &fmt_secs(old)), ("to", &fmt_secs(new))]);
                        EditCommand::Batch { label, commands: vec![set] }
                    }
                    None => set,
                }
            }
        };
        if !self.execute(cmd) {
            self.show_rolled_back();
        }
    }

    /// The live drag's changes were rolled back: show the clip as it is —
    /// picture, inspector and timeline (fade diagonals, handles, waveform).
    pub fn show_rolled_back(&mut self) {
        self.preview.invalidate();
        self.refresh_inspector();
        self.refresh_timeline();
        self.request_frame();
    }

    /// Any other edit (command, undo, redo) first ends a live drag (inspector
    /// slider, timeline fade handle) whose release never came: its live
    /// changes were never recorded, so the edit and its undo must not see
    /// them, and no stale snapshot may be restored over the edit later.
    pub fn end_live_edit(&mut self) {
        if self.live_edit.roll_back(self.project.sequence_mut()) {
            self.show_rolled_back();
        }
    }

    /// `◀ ◆ ▶` and the row menu of an animatable property: `prev`, `next`,
    /// `toggle` (keyframe at the playhead), `clear` (property animation),
    /// `ease:<key>` (curve of the keyframe at the playhead). One click = at
    /// most one undo entry; nothing is sent when nothing would change.
    pub fn inspector_key(&mut self, prop: &str, action: &str) {
        let (Some(id), Some(p)) = (self.inspected(), anim_ui::track_of(prop)) else { return };
        let seq = self.project.sequence();
        let Some(c) = seq.clip(id) else { return };
        let frame = seq.frame_rate.frame_duration();
        let info = anim_ui::key_info(c, &p, self.playhead, frame);
        let edit = anim_ui::edit_time(c, self.playhead, frame);
        let here = anim_ui::key_here(c, &p, self.playhead, frame);
        // The picture at the playhead (or the nearest edge) stays as it is:
        // a first keyframe takes this value, removing the last one keeps it.
        let Some(value_here) = c.track_value(&p, anim_ui::shown_time(c, self.playhead, frame)) else { return };
        let cmd = match action {
            "prev" | "next" => {
                if let Some(tt) = if action == "prev" { info.prev } else { info.next } {
                    self.set_playhead(tt);
                }
                return;
            }
            "toggle" => match (edit, here) {
                (Some(_), Some(k)) => EditCommand::RemoveKeyframe { clip: id, param: p.clone(), at: k.at, keep: Some(value_here) },
                (Some(tt), None) => EditCommand::SetKeyframe { clip: id, param: p.clone(), at: SourceTime(c.source_time_at(tt)), value: value_here, ease: None },
                (None, _) => return,
            },
            "clear" if c.animation.is_animated(&p) => EditCommand::ClearAnimation { clip: id, statics: vec![(p.clone(), value_here)] },
            a if a.starts_with("ease:") => {
                let (Some(k), Some(e)) = (here, Ease::from_key(&a[5..])) else { return };
                if k.ease == e {
                    return;
                }
                EditCommand::SetKeyframeEase { clip: id, param: p.clone(), at: k.at, ease: e }
            }
            _ => return,
        };
        let label = tf(key_label(&cmd), &[("name", &anim_ui::track_label(c, &p))]);
        self.execute(EditCommand::Batch { label, commands: vec![cmd] });
    }

    /// "Reset" of a section: defaults **and** no animation of its
    /// properties, as one undo step (keyframes spec §5).
    pub fn inspector_reset(&mut self, section: &str) {
        let Some(id) = self.inspected() else { return };
        if section == "effects" {
            return self.fx_ask_clear(id);
        }
        let Some(c) = self.project.sequence().clip(id) else { return };
        let params: &[AnimParam] = match section {
            "transform" => &AnimParam::ALL[..9],
            "color" => &AnimParam::ALL[9..],
            _ => &[],
        };
        let (prop, unchanged) = match section {
            "transform" => (ClipProperty::Transform(Transform::default()), c.transform == Transform::default()),
            "color" => (ClipProperty::Color(ColorAdjust::default()), c.color == ColorAdjust::default()),
            _ => (ClipProperty::Audio(AudioProps::default()), c.audio == AudioProps::default()),
        };
        let animated: Vec<(TrackKey, f64)> = params.iter().filter(|p| c.animation.is_animated(**p)).map(|p| ((*p).into(), p.default_value())).collect();
        if unchanged && animated.is_empty() {
            return; // already at the defaults: no undo entry
        }
        let mut commands = vec![EditCommand::SetClipProperty { clip: id, prop }];
        if !animated.is_empty() {
            commands.push(EditCommand::ClearAnimation { clip: id, statics: animated });
        }
        if self.execute(EditCommand::Batch { label: t(reset_label(section)), commands }) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, FrameRate, TimeRange};

    fn seq_with_two_clips() -> (Sequence, ClipId, ClipId) {
        let mut seq = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let src = TimeRange::new(Time::ZERO, Time::from_secs(5));
        let a = Clip::new(AssetId::new(), "a", src, Time::ZERO);
        let b = Clip::new(AssetId::new(), "b", src, Time::from_secs(5));
        let ids = (a.id, b.id);
        seq.tracks[0].clips = vec![a, b];
        (seq, ids.0, ids.1)
    }

    fn opacity(seq: &Sequence, id: ClipId) -> f64 {
        seq.clip(id).unwrap().transform.opacity
    }

    fn set_opacity(seq: &mut Sequence, id: ClipId, v: f64) {
        let (ti, ci) = seq.locate_clip(id).unwrap();
        seq.tracks[ti].clips[ci].transform.opacity = v;
    }

    /// Fix round 1: a drag whose release never comes (slider disabled
    /// mid-drag) must not leave live, unrecorded changes behind, nor a stale
    /// snapshot that a later commit would restore over recorded edits.
    #[test]
    fn rolling_back_restores_the_pre_drag_clip_once() {
        let (mut seq, a, _) = seq_with_two_clips();
        let mut live = LiveEdit::default();
        live.arm(&mut seq, a);
        set_opacity(&mut seq, a, 0.3);
        live.arm(&mut seq, a); // the next live step keeps the first snapshot
        set_opacity(&mut seq, a, 0.2);
        assert!(live.roll_back(&mut seq));
        assert_eq!(opacity(&seq, a), 1.0, "the recorded state is back");
        set_opacity(&mut seq, a, 0.5); // a recorded edit made afterwards
        assert!(!live.roll_back(&mut seq), "disarmed: nothing stale to restore");
        assert_eq!(opacity(&seq, a), 0.5);
    }

    #[test]
    fn a_live_change_of_another_clip_rolls_the_abandoned_one_back() {
        let (mut seq, a, b) = seq_with_two_clips();
        let mut live = LiveEdit::default();
        live.arm(&mut seq, a);
        set_opacity(&mut seq, a, 0.3);
        live.arm(&mut seq, b);
        assert_eq!(opacity(&seq, a), 1.0, "a's unrecorded change is gone");
        set_opacity(&mut seq, b, 0.4);
        assert!(live.roll_back(&mut seq));
        assert_eq!(opacity(&seq, b), 1.0);
    }

    /// A diamond drag recomputes its preview from the recorded clip, never
    /// from the previous preview.
    #[test]
    fn the_recorded_state_is_there_only_while_armed_for_that_clip() {
        let (mut seq, a, b) = seq_with_two_clips();
        let mut live = LiveEdit::default();
        assert!(live.recorded(a).is_none());
        live.arm(&mut seq, a);
        set_opacity(&mut seq, a, 0.3);
        assert_eq!(live.recorded(a).map(|c| c.transform.opacity), Some(1.0));
        assert!(live.recorded(b).is_none(), "another clip's drag");
        live.roll_back(&mut seq);
        assert!(live.recorded(a).is_none());
    }

    #[test]
    fn rolling_back_a_deleted_clip_just_disarms() {
        let (mut seq, a, _) = seq_with_two_clips();
        let mut live = LiveEdit::default();
        live.arm(&mut seq, a);
        seq.tracks[0].clips.remove(0);
        assert!(!live.roll_back(&mut seq));
        assert!(!live.roll_back(&mut seq));
    }

    /// Each keyframe action names what it did in the history, and a section
    /// reset has a sentence-case label, not its uppercase title.
    #[test]
    fn keyframe_actions_and_section_resets_have_their_own_labels() {
        let (clip, param, at) = (ClipId::new(), TrackKey::from(AnimParam::Scale), SourceTime(Time::ZERO));
        assert_eq!(key_label(&EditCommand::SetKeyframe { clip, param: param.clone(), at, value: 1.0, ease: None }), "cmd.keyframe_named");
        assert_eq!(key_label(&EditCommand::RemoveKeyframe { clip, param: param.clone(), at, keep: None }), "cmd.remove_keyframe_named");
        assert_eq!(key_label(&EditCommand::ClearAnimation { clip, statics: vec![] }), "cmd.clear_animation_named");
        assert_eq!(key_label(&EditCommand::SetKeyframeEase { clip, param: param.clone(), at, ease: Ease::Linear }), "cmd.keyframe_ease_named");
        assert_eq!(["transform", "color", "audio"].map(reset_label), ["cmd.reset_transform", "cmd.reset_color", "cmd.reset_audio"]);
    }

    /// A fade slider's commit is labelled with the fade's name and its old
    /// and new length (whole frames, as applied), like the timeline handle.
    #[test]
    fn fade_slider_commits_know_the_old_and_new_length() {
        let (seq, a, _) = seq_with_two_clips();
        let frame = seq.frame_rate.frame_duration();
        let before = seq.clip(a).unwrap().clone();
        for (prop, name) in [("vfade-in", "inspector.fade_in"), ("vfade-out", "inspector.fade_out"), ("fade-in", "inspector.fade_in"), ("fade-out", "inspector.fade_out")] {
            let mut after = before.clone();
            apply(&mut after, prop, 0.51, frame);
            assert_eq!(fade_change(prop, &before, &after), Some((name, Time::ZERO, frame.mul_ratio(15, 1))), "{prop}: 0.51 s → 15 frames");
        }
        assert_eq!(fade_change("opacity", &before, &before), None);
    }

    /// Rows a periodic refresh did not change keep their model: a new one
    /// makes Slint rebuild every row (a press in flight is lost).
    #[test]
    fn unchanged_rows_keep_the_model_on_screen() {
        let old: ModelRc<i32> = ModelRc::new(VecModel::from(vec![1, 2]));
        assert!(same_or_new(old.clone(), vec![1, 2]) == old);
        let changed = same_or_new(old.clone(), vec![1, 3]);
        assert!(changed != old);
        assert_eq!(changed.iter().collect::<Vec<_>>(), vec![1, 3]);
        assert!(same_or_new(old.clone(), vec![1]) != old);
    }
}
