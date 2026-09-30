//! Inspector: slider drags mutate the clip live (no history), release
//! commits exactly one undoable `SetClipProperty`. With nothing selected it
//! shows the sequence and its markers.

use crate::app::{App, Prompt};
use crate::util::rgb;
use crate::{InspectorData, MarkerRow};
use kadr_core::{Time, Timecode};
use kadr_i18n::{duration, t, tf, tn};
use kadr_project::{AudioProps, Clip, ColorAdjust, TrackKind, Transform};
use kadr_timeline::{ClipProperty, EditCommand};
use slint::{ModelRc, SharedString, VecModel};

fn apply(c: &mut Clip, prop: &str, v: f64) {
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
        "fade-in" => c.audio.fade_in = Time::from_secs_f64(v).min(c.duration()),
        "fade-out" => c.audio.fade_out = Time::from_secs_f64(v).min(c.duration()),
        _ => {}
    }
}

fn property_for(prop: &str, c: &Clip) -> ClipProperty {
    match prop {
        "exposure" | "contrast" | "saturation" => ClipProperty::Color(c.color.clone()),
        "gain" | "pan" | "fade-in" | "fade-out" => ClipProperty::Audio(c.audio.clone()),
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
            let t = &c.transform;
            d.x = t.x as f32;
            d.y = t.y as f32;
            d.scale = t.scale as f32;
            d.rotation = t.rotation_deg as f32;
            d.opacity = t.opacity as f32;
            d.crop_l = t.crop_left as f32;
            d.crop_r = t.crop_right as f32;
            d.crop_t = t.crop_top as f32;
            d.crop_b = t.crop_bottom as f32;
            d.speed = c.speed() as f32;
            d.exposure = c.color.exposure as f32;
            d.contrast = c.color.contrast as f32;
            d.saturation = c.color.saturation as f32;
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
        apply(&mut self.project.sequence_mut().tracks[ti].clips[ci], prop, v as f64);
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
        let cmd = if prop == "speed" {
            EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Speed(v as f64) }
        } else {
            let mut c = self.project.sequence().tracks[ti].clips[ci].clone();
            apply(&mut c, prop, v as f64);
            EditCommand::SetClipProperty { clip: id, prop: property_for(prop, &c) }
        };
        if !self.execute(cmd) {
            // The live drag's changes were rolled back above.
            self.preview.invalidate();
            self.refresh_inspector();
            self.request_frame();
        }
    }

    pub fn inspector_reset(&mut self, section: &str) {
        let Some(id) = self.inspected() else { return };
        let prop = match section {
            "transform" => ClipProperty::Transform(Transform::default()),
            "color" => ClipProperty::Color(ColorAdjust::default()),
            _ => ClipProperty::Audio(AudioProps::default()),
        };
        if self.execute(EditCommand::SetClipProperty { clip: id, prop }) {
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
