//! Multicam in the editor: group creation with automatic sync, group cards,
//! the angle viewer under the preview, live cuts (keys 1–9) and logging of
//! human corrections to Jev camera picks.

use crate::app::{post, App};
use crate::{AngleView, AssetView, McAngleRow};
use kadr_analysis::sync::{align_envelopes, timecode_offset};
use kadr_core::{AssetId, ClipId, FrameRate, LinkId, MediaKind, MulticamId, Time, TimeRange};
use kadr_i18n::{duration, t, tf, tn};
use kadr_jobs::{JobSpec, Priority};
use kadr_project::{Clip, CorrectionKind, DecisionKind, EditorPreferenceEvent, MulticamAngle, MulticamGroup, Sequence, SyncMethod, TrackKind};
use kadr_timeline::multicam::{group_clip, group_span, group_time_at};
use kadr_timeline::{EditCommand, InsertMode};
use slint::Model;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ANGLE_THUMB_W: u32 = 192;
const ANGLE_THUMB_H: u32 = 108;

#[derive(Default)]
pub struct McState {
    pub dialog: Option<McDialog>,
    pub selected_group: Option<MulticamId>,
    view_key: Option<(MulticamId, i64, u32)>,
    view_gen: Arc<AtomicU64>,
    view_last: Option<Instant>,
    view_group: Option<MulticamId>,
    view_thumbs: Vec<Option<slint::Image>>,
    /// (available per angle, active angle, loaded thumbs): skip no-op model resets.
    view_sig: Option<(Vec<bool>, u32, usize)>,
}

pub struct McDialog {
    editing: Option<MulticamId>,
    rows: Vec<McRow>,
    syncing: bool,
}

struct McRow {
    asset: AssetId,
    name: String,
    checked: bool,
    label: String,
    description: String,
    sync: String,
    warn: bool,
}

/// Topmost enabled video clip at `t` if it is a multicam clip.
pub fn multicam_clip_at(seq: &Sequence, t: Time) -> Option<&Clip> {
    seq.tracks
        .iter()
        .rev()
        .filter(|tr| tr.kind == TrackKind::Video && !tr.muted)
        .find_map(|tr| tr.clip_at(t).filter(|c| c.enabled))
        .filter(|c| c.multicam.is_some())
}

fn offset_label(offset: Time, method: SyncMethod) -> String {
    let secs = offset.as_secs_f64();
    let off = format!("{}{:.2} s", if secs > 0.0 { "+" } else { "" }, secs);
    match method {
        SyncMethod::Timecode => tf("mc.sync.timecode", &[("offset", &off)]),
        SyncMethod::Waveform => tf("mc.sync.audio", &[("offset", &off)]),
        SyncMethod::Manual => t("mc.sync.manual"),
    }
}

impl App {
    fn next_cam_label(&self, d: &McDialog) -> String {
        (1..).map(|n| format!("CAM{n}")).find(|l| !d.rows.iter().any(|r| r.checked && &r.label == l)).unwrap()
    }

    /// Opens the group dialog. `preselect` starts checked (the clicked card).
    pub fn mc_open_create(&mut self, preselect: Option<AssetId>) {
        let pre = preselect.or(self.selected_asset);
        let rows: Vec<McRow> = self
            .project
            .assets
            .iter()
            .filter(|a| a.kind() == MediaKind::Video)
            .map(|a| McRow {
                asset: a.id,
                name: a.name.clone(),
                checked: false,
                label: String::new(),
                description: String::new(),
                sync: tf("mc.dlg.clip_info", &[("d", &duration(a.duration().as_secs_f64())), ("tc", a.info.timecode.as_deref().unwrap_or("—"))]),
                warn: false,
            })
            .collect();
        if rows.len() < 2 {
            return self.toast_warn(t("mc.need_two_videos"));
        }
        let mut d = McDialog { editing: None, rows, syncing: false };
        if let Some(i) = d.rows.iter().position(|r| Some(r.asset) == pre) {
            d.rows[i].checked = true;
            d.rows[i].label = "CAM1".into();
        }
        let n = self.project.multicam_groups.len() + 1;
        self.ui().set_mc_name(tf("mc.default_name", &[("n", &n.to_string())]).into());
        self.mc.dialog = Some(d);
        self.mc_refresh_dialog(true);
    }

    pub fn mc_open_edit(&mut self, id: MulticamId) {
        let Some(g) = self.project.multicam_groups.iter().find(|g| g.id == id) else { return };
        let rows = g
            .angles
            .iter()
            .map(|a| McRow {
                asset: a.asset,
                name: self.project.asset(a.asset).map(|x| x.name.clone()).unwrap_or_default(),
                checked: true,
                label: a.label.clone(),
                description: a.description.clone(),
                sync: offset_label(a.sync_offset, a.sync_method),
                warn: a.sync_method == SyncMethod::Manual,
            })
            .collect();
        self.ui().set_mc_name(g.name.clone().into());
        self.mc.dialog = Some(McDialog { editing: Some(id), rows, syncing: false });
        self.mc_refresh_dialog(true);
    }

    fn mc_refresh_dialog(&mut self, open: bool) {
        let ui = self.ui();
        let Some(d) = &self.mc.dialog else {
            ui.set_mc_open(false);
            return;
        };
        let rows: Vec<McAngleRow> = d
            .rows
            .iter()
            .map(|r| {
                let thumb = self.assets_rt.get(&r.asset).and_then(|x| x.thumb.clone());
                McAngleRow {
                    name: r.name.clone().into(),
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    checked: r.checked,
                    label: r.label.clone().into(),
                    description: r.description.clone().into(),
                    sync: r.sync.clone().into(),
                    sync_warn: r.warn,
                }
            })
            .collect();
        crate::util::sync_rows(ui.get_mc_rows(), rows, |m| ui.set_mc_rows(m));
        ui.set_mc_editing(d.editing.is_some());
        ui.set_mc_hint(if d.syncing { t("mc.dlg.syncing").into() } else { "".into() });
        if open {
            ui.set_mc_open(true);
        }
    }

    pub fn mc_toggle(&mut self, i: i32) {
        let Some(mut d) = self.mc.dialog.take() else { return };
        if let Some(r) = d.rows.get(i as usize).map(|r| r.checked) {
            let label = if r { String::new() } else { self.next_cam_label(&d) };
            let row = &mut d.rows[i as usize];
            row.checked = !r;
            if row.label.is_empty() || r {
                row.label = label;
            }
        }
        self.mc.dialog = Some(d);
        self.mc_refresh_dialog(false);
    }

    // Text edits update state only: re-setting the model would reset the
    // caret of the LineEdit being typed in.
    pub fn mc_set_label(&mut self, i: i32, s: &str) {
        if let Some(r) = self.mc.dialog.as_mut().and_then(|d| d.rows.get_mut(i as usize)) {
            r.label = s.trim().chars().take(16).collect();
        }
    }

    pub fn mc_set_description(&mut self, i: i32, s: &str) {
        if let Some(r) = self.mc.dialog.as_mut().and_then(|d| d.rows.get_mut(i as usize)) {
            r.description = s.trim().chars().take(80).collect();
        }
    }

    pub fn mc_dismiss(&mut self) {
        self.mc.dialog = None;
        self.ui().set_mc_open(false);
        self.ui().invoke_focus_editor();
    }

    pub fn mc_confirm(&mut self, name: &str) {
        let Some(d) = self.mc.dialog.as_ref() else { return };
        let name = if name.trim().is_empty() { t("mc.default_name_short") } else { name.trim().to_string() };
        let mut picked: Vec<&McRow> = d.rows.iter().filter(|r| r.checked).collect();
        // Angle order follows the names ("CAM1", "CAM2", … "CAM10"), not the list.
        picked.sort_by_key(|r| (r.label.len(), r.label.clone()));
        let mut labels: Vec<&str> = picked.iter().map(|r| r.label.as_str()).collect();
        labels.sort();
        labels.dedup();
        if labels.len() != picked.len() || labels.iter().any(|l| l.is_empty()) {
            self.ui().set_mc_hint(t("mc.dlg.labels_unique").into());
            return;
        }
        if let Some(id) = d.editing {
            let rows: Vec<(AssetId, String, String)> = picked.iter().map(|r| (r.asset, r.label.clone(), r.description.clone())).collect();
            if let Some(g) = self.project.multicam_groups.iter_mut().find(|g| g.id == id) {
                g.name = name;
                relabel_angles(&mut g.angles, &rows);
            }
            self.meta_dirty = true;
            self.mc.view_key = None;
            self.mc_dismiss();
            self.refresh_library();
            return;
        }
        if picked.len() < 2 {
            self.ui().set_mc_hint(t("mc.dlg.pick_two").into());
            return;
        }
        if d.syncing {
            return;
        }
        // Audio sync needs the loudness envelopes of every angle with audio.
        let pending: Vec<String> = picked
            .iter()
            .filter(|r| self.project.asset(r.asset).is_some_and(|a| a.has_audio()) && self.assets_rt.get(&r.asset).is_none_or(|x| x.overview.is_none()))
            .map(|r| r.name.clone())
            .collect();
        if !pending.is_empty() {
            self.ui().set_mc_hint(tf("mc.dlg.audio_pending", &[("names", &pending.join(", "))]).into());
            return;
        }
        let inputs: Vec<SyncInput> = picked
            .iter()
            .map(|r| SyncInput {
                timecode: self.project.asset(r.asset).and_then(|a| a.info.timecode.clone()),
                levels: self.assets_rt.get(&r.asset).and_then(|x| x.overview.as_ref().map(|o| o.levels_db.clone())),
                rate: self.project.asset(r.asset).and_then(|a| a.info.video.as_ref().and_then(|v| v.frame_rate)),
            })
            .collect();
        let angles: Vec<(AssetId, String, String, String)> =
            picked.iter().map(|r| (r.asset, r.label.clone(), r.description.clone(), r.name.clone())).collect();
        let rate = self.project.sequence().frame_rate;
        if let Some(d) = self.mc.dialog.as_mut() {
            d.syncing = true;
        }
        self.mc_refresh_dialog(false);
        self.jobs.submit(JobSpec::new(tf("jobs.title.mc_sync", &[("name", &name)]), "multicam").priority(Priority::High), move |_| {
            let outcomes = plan_sync(&inputs, rate);
            let (name, angles) = (name.clone(), angles.clone());
            post(move |app| app.mc_create_group(name, angles, outcomes));
            Ok(())
        });
    }

    fn mc_create_group(&mut self, name: String, angles: Vec<(AssetId, String, String, String)>, sync: Vec<SyncOutcome>) {
        let unsynced: Vec<String> = angles.iter().zip(&sync).filter(|(_, s)| !s.reliable).map(|(a, _)| a.3.clone()).collect();
        let group = MulticamGroup {
            id: MulticamId::new(),
            name: name.clone(),
            angles: angles
                .into_iter()
                .zip(&sync)
                .map(|((asset, label, description, _), s)| MulticamAngle { asset, label, description, sync_offset: s.offset, sync_method: s.method })
                .collect(),
            master_audio: None,
        };
        tracing::info!(group = %group.name, angles = group.angles.len(), ?sync, "multicam group created");
        let span = group_span(&group, &self.project.assets);
        if span.is_empty() {
            self.toast_error(t("mc.no_overlap"));
            if let Some(d) = self.mc.dialog.as_mut() {
                d.syncing = false;
            }
            return self.mc_refresh_dialog(false);
        }
        self.mc.selected_group = Some(group.id);
        self.selected_asset = None;
        self.project.multicam_groups.push(group);
        self.meta_dirty = true;
        self.mc_dismiss();
        self.refresh_library();
        self.toast(tf("mc.toast.created", &[("name", &name), ("d", &duration(span.duration().as_secs_f64()))]));
        if !unsynced.is_empty() {
            self.toast_warn(tn("mc.toast.unsynced", unsynced.len() as i64, &[("names", &unsynced.join(", "))]));
        }
    }

    /// Library cards for groups (shown in "All" only; groups have no bin).
    pub fn mc_cards(&self, query: &str) -> Vec<AssetView> {
        if self.bin_filter.is_some() {
            return vec![];
        }
        let used: std::collections::HashSet<MulticamId> =
            self.project.sequence().tracks.iter().flat_map(|t| t.clips.iter().filter_map(|c| c.multicam.as_ref().map(|m| m.group))).collect();
        self.project
            .multicam_groups
            .iter()
            .filter(|g| query.is_empty() || g.name.to_lowercase().contains(query))
            .map(|g| {
                let span = group_span(g, &self.project.assets);
                let thumb = g.angles.first().and_then(|a| self.assets_rt.get(&a.asset)).and_then(|r| r.thumb.clone());
                let unsynced = g.angles.iter().skip(1).filter(|a| a.sync_method == SyncMethod::Manual).count();
                AssetView {
                    id: crate::library::LibItem::Group(g.id).card_id().into(),
                    name: g.name.clone().into(),
                    kind: 3,
                    duration: duration(span.duration().as_secs_f64()).into(),
                    resolution: tn("mc.card.angles", g.angles.len() as i64, &[]).into(),
                    fps: "".into(),
                    codec: g.angles.iter().map(|a| a.label.as_str()).collect::<Vec<_>>().join(" · ").into(),
                    audio: "".into(),
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    status: if unsynced > 0 { tn("mc.card.unsynced", unsynced as i64, &[]).into() } else { t("mc.card.synced").into() },
                    status_kind: 1,
                    progress: 1.0,
                    proxy: "".into(),
                    selected: self.mc.selected_group == Some(g.id),
                    in_use: used.contains(&g.id),
                }
            })
            .collect()
    }

    /// Places a whole group: angle 1 on a video track plus its audio, linked.
    pub fn place_group(&mut self, id: MulticamId, at: Time, mode: InsertMode, prefer_row: Option<usize>) {
        let Some(group) = self.project.multicam_groups.iter().find(|g| g.id == id).cloned() else { return };
        let Some(mut v) = group_clip(&group, 0, &self.project.assets, self.project.sequence().frame_rate.snap(at)) else {
            return self.toast_error(t("mc.no_overlap"));
        };
        let seq = self.project.sequence();
        let rs = crate::timeline_ui::rows(seq, self.tl.scroll_y);
        let pref = prefer_row.and_then(|r| rs.get(r)).map(|r| r.track);
        let pick = |kind: TrackKind| {
            pref.filter(|&i| seq.tracks[i].kind == kind && !seq.tracks[i].locked)
                .map(|i| seq.tracks[i].id)
                .or_else(|| seq.tracks.iter().find(|t| t.kind == kind && !t.locked).map(|t| t.id))
        };
        let Some(vtrack) = pick(TrackKind::Video) else { return self.toast_warn(t("err.video_tracks_locked")) };
        let first = group.angles[0].asset;
        let mut cmds = vec![];
        if self.project.asset(first).is_some_and(|a| a.has_audio()) {
            let Some(atrack) = pick(TrackKind::Audio) else { return self.toast_warn(t("err.audio_tracks_locked")) };
            let link = LinkId::new();
            v.link = Some(link);
            let mut a = Clip::new(first, format!("{} · {}", group.name, group.angles[0].label), v.source_range(), v.timeline_in);
            a.link = Some(link);
            cmds.push(EditCommand::InsertClip { track: atrack, clip: a, mode });
        }
        cmds.insert(0, EditCommand::InsertClip { track: vtrack, clip: v, mode });
        if self.project.sequence().duration() == Time::ZERO {
            if let Some(info) = self.project.asset(first).and_then(|a| a.info.video.clone()) {
                let seq = self.project.sequence_mut();
                seq.width = info.width.max(2) & !1;
                seq.height = info.height.max(2) & !1;
                if let Some(fr) = info.frame_rate {
                    seq.frame_rate = fr;
                }
                self.meta_dirty = true;
            }
        }
        let label = match mode {
            InsertMode::Insert => tf("cmd.insert_named", &[("name", &group.name)]),
            InsertMode::Overwrite => tf("cmd.overwrite_named", &[("name", &group.name)]),
        };
        if self.execute(EditCommand::Batch { label, commands: cmds }) && kadr_timeline::composition::video_at(self.project.sequence(), self.playhead).is_none() {
            self.set_playhead(at);
        }
        self.refresh_library();
    }

    /// Cut to `angle` at the playhead on the multicam clip under it.
    pub fn cut_to_angle(&mut self, angle: u32) -> bool {
        let seq = self.project.sequence();
        let Some(clip) = multicam_clip_at(seq, self.playhead) else { return false };
        let sel = clip.multicam.clone().unwrap();
        let Some(group) = self.project.multicam_groups.iter().find(|g| g.id == sel.group).cloned() else { return false };
        if angle as usize >= group.angles.len() {
            return true;
        }
        let Some(cmd) = cut_to_angle_command(seq, clip.id, self.playhead, angle) else { return true };
        let at = match &cmd {
            EditCommand::SwitchAngle { at: Some(t), .. } => *t,
            _ => clip.timeline_in,
        };
        let g = group_time_at(&group, clip, at);
        if self.execute(cmd) {
            self.mc.view_key = None;
            if let Some(g) = g {
                self.log_camera_correction(&group, g, &group.angles[angle as usize].label);
            }
        }
        true
    }

    /// A manual angle switch where Jev had decided: the editor's taste,
    /// recorded as a precedent for future camera picks.
    fn log_camera_correction(&mut self, group: &MulticamGroup, g: Time, label: &str) {
        let Some(d) = self.project.jev_decisions.iter_mut().rev().find(|d| {
            d.kind == DecisionKind::CameraPick && parse_camera_subject(&d.subject).is_some_and(|(id, r)| id == group.id && r.contains(g))
        }) else {
            return;
        };
        if d.effective() == label {
            return;
        }
        d.human = Some(label.to_string());
        let ev = EditorPreferenceEvent {
            at_ms: kadr_project::now_ms(),
            action: None,
            kind: CorrectionKind::CameraChanged,
            context: d.features.clone(),
            features: serde_json::json!({"subject": d.subject}),
            ai_choice: d.value.clone(),
            ai_confidence: d.p_max,
            human_choice: label.to_string(),
            decision: Some(d.id),
        };
        tracing::info!(ai = %ev.ai_choice, human = %ev.human_choice, "camera correction recorded");
        self.project.preference_events.push(ev);
        self.meta_dirty = true;
    }

    fn clear_angles(&mut self) {
        if self.mc.view_group.take().is_some() || self.ui().get_angles().row_count() > 0 {
            self.mc.view_key = None;
            self.mc.view_sig = None;
            self.mc.view_thumbs.clear();
            let ui = self.ui();
            crate::util::sync_rows(ui.get_angles(), vec![], |m| ui.set_angles(m));
        }
    }

    /// Keeps the angle strip in sync with the playhead (7 Hz tick). Frames
    /// decode off the UI thread, latest request wins; during playback at
    /// most once a second from keyframes.
    pub fn tick_angles(&mut self) {
        let seq = self.project.sequence();
        let Some(clip) = multicam_clip_at(seq, self.playhead).cloned() else { return self.clear_angles() };
        let sel = clip.multicam.clone().unwrap();
        let Some(group) = self.project.multicam_groups.iter().find(|g| g.id == sel.group).cloned() else { return self.clear_angles() };
        let Some(g) = group_time_at(&group, &clip, self.playhead) else { return self.clear_angles() };
        if self.mc.view_group != Some(group.id) {
            self.mc.view_group = Some(group.id);
            self.mc.view_thumbs = vec![None; group.angles.len()];
            self.mc.view_key = None;
            self.mc.view_sig = None;
        }
        let avail: Vec<bool> = group
            .angles
            .iter()
            .map(|a| {
                let src = g + a.sync_offset;
                self.project.asset(a.asset).is_some_and(|x| src >= Time::ZERO && src < x.duration())
            })
            .collect();
        self.set_angle_views(&group, &avail, sel.angle);
        let key = (group.id, g.as_millis() / 40, sel.angle);
        let playing_throttled = self.playing && self.mc.view_last.is_some_and(|t| t.elapsed() < Duration::from_secs(1));
        if self.mc.view_key == Some(key) || playing_throttled {
            return;
        }
        let Some(media) = self.media.clone() else { return };
        self.mc.view_key = Some(key);
        self.mc.view_last = Some(Instant::now());
        let generation = self.mc.view_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let latest = self.mc.view_gen.clone();
        let fast = self.playing;
        let jobs: Vec<(usize, std::path::PathBuf, Time)> = group
            .angles
            .iter()
            .enumerate()
            .filter(|(i, _)| avail[*i])
            .filter_map(|(i, a)| Some((i, self.project.asset(a.asset)?.path.clone(), g + a.sync_offset)))
            .collect();
        let gid = group.id;
        std::thread::spawn(move || {
            for (i, path, at) in jobs {
                if latest.load(Ordering::SeqCst) != generation {
                    return;
                }
                let f = if fast {
                    media.thumbnails(&path, &[at], ANGLE_THUMB_H, &kadr_core::CancelToken::new()).ok().and_then(|mut v| v.pop())
                } else {
                    media.decode_frame(&path, at, ANGLE_THUMB_W, ANGLE_THUMB_H).ok()
                };
                if let Some(f) = f {
                    post(move |app| app.on_angle_frame(gid, generation, i, f));
                }
            }
        });
    }

    fn on_angle_frame(&mut self, group: MulticamId, generation: u64, i: usize, f: kadr_media::RgbaFrame) {
        if self.mc.view_group != Some(group) || self.mc.view_gen.load(Ordering::SeqCst) != generation {
            return;
        }
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&f.data, f.width, f.height);
        if let Some(slot) = self.mc.view_thumbs.get_mut(i) {
            *slot = Some(slint::Image::from_rgba8(buf));
        }
        if let Some(g) = self.project.multicam_groups.iter().find(|g| g.id == group).cloned() {
            let active = multicam_clip_at(self.project.sequence(), self.playhead).and_then(|c| c.multicam.as_ref()).map_or(0, |m| m.angle);
            let ui = self.ui();
            let avail: Vec<bool> = (0..g.angles.len()).map(|k| ui.get_angles().row_data(k).is_none_or(|v| v.available)).collect();
            self.set_angle_views(&g, &avail, active);
        }
    }

    fn set_angle_views(&mut self, group: &MulticamGroup, avail: &[bool], active: u32) {
        let sig = (avail.to_vec(), active, self.mc.view_thumbs.iter().filter(|t| t.is_some()).count());
        if self.mc.view_sig.as_ref() == Some(&sig) {
            return;
        }
        self.mc.view_sig = Some(sig);
        let views: Vec<AngleView> = group
            .angles
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let thumb = self.mc.view_thumbs.get(i).cloned().flatten();
                AngleView {
                    label: a.label.clone().into(),
                    description: a.description.clone().into(),
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    available: avail.get(i).copied().unwrap_or(true),
                }
            })
            .collect();
        let ui = self.ui();
        crate::util::sync_rows(ui.get_angles(), views, |m| ui.set_angles(m));
        ui.set_active_angle(active as i32);
    }
}

/// Stable address of a camera decision: group time survives every split.
pub fn camera_subject(group: MulticamId, g: TimeRange) -> String {
    format!("mc:{group}:{}-{}", g.start.as_millis(), g.end.as_millis())
}

pub fn parse_camera_subject(s: &str) -> Option<(MulticamId, TimeRange)> {
    let rest = s.strip_prefix("mc:")?;
    let (g, range) = rest.rsplit_once(':')?;
    let (a, b) = range.split_once('-')?;
    Some((MulticamId::parse(g)?, TimeRange::new(Time::from_millis(a.parse().ok()?), Time::from_millis(b.parse().ok()?))))
}

/// New names and descriptions from the dialog rows, matched by media: the
/// rows are sorted by name, the angle order (keys 1–9, clips) must not move.
pub fn relabel_angles(angles: &mut [MulticamAngle], rows: &[(AssetId, String, String)]) {
    for a in angles {
        if let Some((_, label, desc)) = rows.iter().find(|r| r.0 == a.asset) {
            a.label = label.clone();
            a.description = desc.clone();
        }
    }
}

/// Cut to `angle` at `at`: switch the whole clip when `at` is its start,
/// split and switch the rest otherwise; `None` when already on `angle`.
pub fn cut_to_angle_command(seq: &Sequence, clip: ClipId, at: Time, angle: u32) -> Option<EditCommand> {
    let c = seq.clip(clip)?;
    if c.multicam.as_ref()?.angle == angle {
        return None;
    }
    let at = (at > c.timeline_in && at < c.timeline_out).then_some(at);
    Some(EditCommand::SwitchAngle { clip, angle, at })
}

/// What sync can use for one angle.
pub struct SyncInput {
    /// Embedded start timecode.
    pub timecode: Option<String>,
    /// 10 ms loudness envelope (dB) from the audio proxy.
    pub levels: Option<Vec<f32>>,
    /// The camera's own frame rate: timecode frames count at this rate.
    pub rate: Option<FrameRate>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncOutcome {
    /// `MulticamAngle.sync_offset`: source = group + offset.
    pub offset: Time,
    pub method: SyncMethod,
    pub confidence: f32,
    /// False when nothing matched and the angle fell back to offset 0.
    pub reliable: bool,
}

/// Angle 1 defines group time. Others: timecode when both have one, else
/// audio correlation (±10 min), else manual offset 0 flagged unreliable.
pub fn plan_sync(inputs: &[SyncInput], rate: FrameRate) -> Vec<SyncOutcome> {
    let manual = SyncOutcome { offset: Time::ZERO, method: SyncMethod::Manual, confidence: 0.0, reliable: false };
    let Some(first) = inputs.first() else { return vec![] };
    let mut out = vec![SyncOutcome { reliable: true, confidence: 1.0, ..manual }];
    for inp in &inputs[1..] {
        // Each clock counts frames at its own camera's rate.
        let tc_time = |i: &SyncInput| timecode_offset("00:00:00:00", i.timecode.as_deref()?, i.rate.unwrap_or(rate));
        let by_tc = tc_time(first).zip(tc_time(inp)).map(|(a, b)| b - a);
        let outcome = match (by_tc, first.levels.as_deref(), inp.levels.as_deref()) {
            // B's clock started d later → group time t is B source t − d.
            (Some(d), _, _) => SyncOutcome { offset: Time::ZERO - d, method: SyncMethod::Timecode, confidence: 1.0, reliable: true },
            (None, Some(a), Some(b)) => {
                let r = align_envelopes(a, b, Time::from_millis(10), Time::from_secs(600));
                if r.confidence >= 0.15 {
                    SyncOutcome { offset: Time::ZERO - r.offset, method: SyncMethod::Waveform, confidence: r.confidence, reliable: true }
                } else {
                    manual
                }
            }
            _ => manual,
        };
        out.push(outcome);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, FrameRate, MulticamId, Time, TimeRange};
    use kadr_project::{Clip, MulticamSelection, Sequence};

    fn s(x: i64) -> Time {
        Time::from_secs(x)
    }

    #[test]
    fn renaming_angles_keeps_each_label_on_its_own_media() {
        let (a, b) = (AssetId::new(), AssetId::new());
        let angle = |asset, label: &str| MulticamAngle { asset, label: label.into(), description: String::new(), sync_offset: Time::ZERO, sync_method: kadr_project::SyncMethod::Manual };
        let mut angles = vec![angle(a, "CAM1"), angle(b, "CAM2")];
        // The dialog lists rows sorted by the new names: "Wide" (b) before "Close" (a).
        relabel_angles(&mut angles, &[(b, "Wide".into(), "stage".into()), (a, "Close".into(), "singer".into())]);
        assert_eq!((angles[0].asset, angles[0].label.as_str(), angles[0].description.as_str()), (a, "Close", "singer"));
        assert_eq!((angles[1].asset, angles[1].label.as_str(), angles[1].description.as_str()), (b, "Wide", "stage"));
    }

    #[test]
    fn timecode_counts_frames_at_the_camera_rate() {
        // 25 fps cameras in a 30 fps sequence: frame 20 is 0.8 s, not 0.667 s.
        let tc = |t: &str| SyncInput { timecode: Some(t.to_string()), levels: None, rate: Some(FrameRate::FPS_25) };
        let got = plan_sync(&[tc("10:00:00:00"), tc("10:00:05:20")], FrameRate::FPS_30);
        assert_eq!(got[1].offset, Time::ZERO - Time::from_millis(5_800));
    }

    #[test]
    fn camera_subject_roundtrip() {
        let g = MulticamId::new();
        let sub = camera_subject(g, TimeRange::new(Time::from_millis(12_000), Time::from_millis(16_500)));
        assert_eq!(parse_camera_subject(&sub), Some((g, TimeRange::new(Time::from_millis(12_000), Time::from_millis(16_500)))));
        assert_eq!(parse_camera_subject("asset:x:shot:1"), None);
    }

    #[test]
    fn cut_command_depends_on_where_the_playhead_is() {
        let mut seq = Sequence::new("s", FrameRate::FPS_25, 1920, 1080);
        let mut c = Clip::new(AssetId::new(), "CAM1", TimeRange::new(s(10), s(20)), s(0));
        c.multicam = Some(MulticamSelection { group: MulticamId::new(), angle: 0 });
        let id = c.id;
        seq.tracks[0].clips.push(c);
        assert_eq!(cut_to_angle_command(&seq, id, s(4), 1), Some(EditCommand::SwitchAngle { clip: id, angle: 1, at: Some(s(4)) }));
        assert_eq!(cut_to_angle_command(&seq, id, s(0), 1), Some(EditCommand::SwitchAngle { clip: id, angle: 1, at: None }));
        assert_eq!(cut_to_angle_command(&seq, id, s(4), 0), None, "already on that angle");
    }

    #[test]
    fn sync_prefers_timecode_then_audio_then_manual() {
        let rate = FrameRate::FPS_25;
        // Non-periodic loud/quiet runs (a periodic signal is genuinely ambiguous).
        let mut x = 7u64;
        let mut env: Vec<f32> = vec![];
        while env.len() < 3000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let (len, level) = (5 + (x >> 40) as usize % 40, -60.0 + ((x >> 20) % 54) as f32);
            env.extend(std::iter::repeat_n(level, len));
        }
        env.truncate(3000);
        let late: Vec<f32> = env[150..].to_vec(); // started 1.5 s later
        let tc = |t: &str| SyncInput { timecode: Some(t.to_string()), levels: None, rate: None };
        let got = plan_sync(&[tc("01:00:00:00"), tc("01:00:02:00")], rate);
        assert_eq!(got[1].offset, s(-2));
        assert_eq!(got[1].method, SyncMethod::Timecode);

        let au = |l: &[f32]| SyncInput { timecode: None, levels: Some(l.to_vec()), rate: None };
        let got = plan_sync(&[au(&env), au(&late)], rate);
        assert_eq!(got[0].offset, Time::ZERO);
        assert_eq!(got[1].offset, Time::from_millis(-1500), "source = group + offset");
        assert_eq!(got[1].method, SyncMethod::Waveform);

        let got = plan_sync(&[au(&env), au(&[-60.0; 3000])], rate);
        assert_eq!((got[1].offset, got[1].method, got[1].reliable), (Time::ZERO, SyncMethod::Manual, false));
    }
}
