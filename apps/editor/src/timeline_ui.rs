//! Timeline view-model and interaction. Only visible clips are turned into
//! UI rectangles; hit-testing, dragging, trimming and snapping happen here
//! in time coordinates, and a single engine command is issued on release.

use crate::app::App;
use crate::util::{fmt_ruler, rgb};
use crate::{ClipMark, ClipView, MarkerView, TickView, TrackView, TransitionView};
use kadr_core::{ClipId, LinkId, MediaKind, Time, TimeRange, TrackId};
use kadr_project::{Clip, Sequence, TrackKind, Transition, TransitionKind};
use kadr_timeline::snap::{snap, snap_move, snap_points};
use kadr_timeline::{ClipProperty, EditCommand, InsertMode, TrackFlag, TrimEdge};
use kadr_i18n::{t, tf};
use slint::{ModelRc, VecModel};
use std::collections::HashSet;

pub const VIDEO_H: f64 = 62.0;
pub const AUDIO_H: f64 = 54.0;
const TRIM_ZONE: f64 = 6.0;
const SNAP_PX: f64 = 9.0;
const DRAG_THRESHOLD: f64 = 3.0;

pub enum Drag {
    Scrub,
    Move { clips: Vec<ClipId>, anchor_t: f64, start_x: f64, start_y: f64, start_row: usize, delta: Time, track_delta: i32, started: bool, clicked: Option<ClipId> },
    Trim { clip: ClipId, edge: TrimEdge, to: Time },
}

/// A plain click on a clip inside a larger selection narrows the selection
/// to that clip when released without dragging (the selection stays whole
/// while it may still become a group move).
fn narrowed_on_release(selection: &[ClipId], clicked: ClipId) -> Option<Vec<ClipId>> {
    (selection.len() > 1 && selection.contains(&clicked)).then(|| vec![clicked])
}

pub struct TimelineUi {
    pub pps: f64,
    pub scroll: f64,
    pub scroll_y: f64,
    pub selection: Vec<ClipId>,
    pub drag: Option<Drag>,
    pub snapping: bool,
    pub ripple: bool,
    pub blade: bool,
    pub cursor: i32,
    pub snap_line: Option<Time>,
    pub last_mouse_x: f64,
    pub ghost: Option<(f64, f64, f64, f64)>,
    pub renaming_track: Option<TrackId>,
}

impl Default for TimelineUi {
    fn default() -> Self {
        TimelineUi {
            pps: 40.0,
            scroll: 0.0,
            scroll_y: 0.0,
            selection: vec![],
            drag: None,
            snapping: true,
            ripple: false,
            blade: false,
            cursor: 0,
            snap_line: None,
            last_mouse_x: 0.0,
            ghost: None,
            renaming_track: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub track: usize,
    pub kind: TrackKind,
    pub y: f64,
    pub h: f64,
}

/// Display order: highest video track on top … V1, then A1 … An.
pub fn rows(seq: &Sequence, scroll_y: f64) -> Vec<Row> {
    let mut order: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).map(|(i, _)| i).collect();
    order.reverse();
    order.extend(seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Audio).map(|(i, _)| i));
    let mut y = -scroll_y;
    order
        .into_iter()
        .map(|i| {
            let kind = seq.tracks[i].kind;
            let h = if kind == TrackKind::Video { VIDEO_H } else { AUDIO_H };
            let r = Row { track: i, kind, y, h };
            y += h;
            r
        })
        .collect()
}

enum Hit {
    Clip { id: ClipId, edge: Option<TrimEdge> },
    Empty { row: Option<usize> },
}

impl App {
    fn lanes_size(&self) -> (f64, f64) {
        let ui = self.ui();
        (ui.get_lanes_width().max(100.0) as f64, ui.get_lanes_height().max(50.0) as f64)
    }

    pub fn x_to_time(&self, x: f64) -> Time {
        Time::from_secs_f64((self.tl.scroll + x / self.tl.pps).max(0.0))
    }

    fn time_to_x(&self, t: Time) -> f64 {
        (t.as_secs_f64() - self.tl.scroll) * self.tl.pps
    }

    fn row_at(&self, y: f64) -> Option<usize> {
        rows(self.project.sequence(), self.tl.scroll_y).iter().position(|r| y >= r.y && y < r.y + r.h)
    }

    fn hit(&self, x: f64, y: f64) -> Hit {
        let seq = self.project.sequence();
        let rs = rows(seq, self.tl.scroll_y);
        let Some(ri) = rs.iter().position(|r| y >= r.y && y < r.y + r.h) else { return Hit::Empty { row: None } };
        let track = &seq.tracks[rs[ri].track];
        let t = self.x_to_time(x);
        let tol = Time::from_secs_f64(TRIM_ZONE / self.tl.pps);
        // Prefer edges (even slightly outside the clip) for precise trimming.
        for c in &track.clips {
            let (x0, x1) = (self.time_to_x(c.timeline_in), self.time_to_x(c.timeline_out));
            let narrow = x1 - x0 < TRIM_ZONE * 3.0;
            if !narrow && (x - x0).abs() <= TRIM_ZONE {
                return Hit::Clip { id: c.id, edge: Some(TrimEdge::Start) };
            }
            if !narrow && (x - x1).abs() <= TRIM_ZONE {
                return Hit::Clip { id: c.id, edge: Some(TrimEdge::End) };
            }
        }
        match track.clip_at(t).or_else(|| track.clip_at(t + tol)) {
            Some(c) => Hit::Clip { id: c.id, edge: None },
            None => Hit::Empty { row: Some(ri) },
        }
    }

    fn snap_threshold(&self) -> Time {
        Time::from_secs_f64(SNAP_PX / self.tl.pps)
    }

    fn snap_time(&mut self, t: Time, exclude: &HashSet<ClipId>) -> Time {
        let seq = self.project.sequence();
        if !self.tl.snapping {
            self.tl.snap_line = None;
            return seq.frame_rate.snap(t);
        }
        let pts = snap_points(seq, exclude, &[self.playhead]);
        match snap(t, &pts, self.snap_threshold()) {
            Some(p) => {
                self.tl.snap_line = Some(p);
                p
            }
            None => {
                self.tl.snap_line = None;
                seq.frame_rate.snap(t)
            }
        }
    }

    pub fn set_playhead(&mut self, t: Time) {
        let seq = self.project.sequence();
        let t = seq.frame_rate.snap(t.max(Time::ZERO).min(seq.duration().max(Time::ZERO)));
        if t == self.playhead {
            return;
        }
        self.playhead = t;
        if self.playing {
            self.restart_playback();
        } else {
            self.request_frame();
        }
        self.refresh_playhead();
    }

    pub fn tl_pointer(&mut self, kind: i32, x: f32, y: f32, button: i32, shift: bool, ctrl: bool, _alt: bool) {
        let (x, y) = (x as f64, y as f64);
        self.tl.last_mouse_x = x;
        match kind {
            0 if button == 1 => {
                // Right click: select what's under the cursor for the context menu.
                if let Hit::Clip { id, .. } = self.hit(x, y) {
                    if !self.tl.selection.contains(&id) {
                        self.tl.selection = vec![id];
                        self.refresh_timeline();
                        self.refresh_inspector();
                    }
                }
            }
            0 if button == 0 => self.pointer_down(x, y, shift, ctrl),
            1 => self.pointer_move(x, y),
            2 if button == 0 => self.pointer_up(),
            3 => {
                if let Hit::Clip { .. } = self.hit(x, y) {
                    let t = self.x_to_time(x);
                    self.set_playhead(t);
                }
            }
            _ => {}
        }
    }

    fn pointer_down(&mut self, x: f64, y: f64, shift: bool, ctrl: bool) {
        let hit = self.hit(x, y);
        if self.tl.blade {
            if let Hit::Clip { id, .. } = hit {
                let t = self.snap_time(self.x_to_time(x), &HashSet::new());
                self.tl.snap_line = None;
                self.execute(EditCommand::Split { at: t, clips: Some(vec![id]) });
            }
            return;
        }
        match hit {
            Hit::Clip { id, edge: Some(edge) } => {
                if !self.tl.selection.contains(&id) {
                    self.tl.selection = vec![id];
                }
                let c = self.project.sequence().clip(id).unwrap();
                let to = if edge == TrimEdge::Start { c.timeline_in } else { c.timeline_out };
                self.tl.drag = Some(Drag::Trim { clip: id, edge, to });
            }
            Hit::Clip { id, edge: None } => {
                if ctrl {
                    if let Some(p) = self.tl.selection.iter().position(|c| *c == id) {
                        self.tl.selection.remove(p);
                    } else {
                        self.tl.selection.push(id);
                    }
                } else if shift {
                    if !self.tl.selection.contains(&id) {
                        self.tl.selection.push(id);
                    }
                } else if !self.tl.selection.contains(&id) {
                    self.tl.selection = vec![id];
                }
                let row = self.row_at(y).unwrap_or(0);
                self.tl.drag = Some(Drag::Move {
                    clips: self.tl.selection.clone(),
                    anchor_t: self.tl.scroll + x / self.tl.pps,
                    start_x: x,
                    start_y: y,
                    start_row: row,
                    delta: Time::ZERO,
                    track_delta: 0,
                    started: false,
                    clicked: (!ctrl && !shift).then_some(id),
                });
            }
            Hit::Empty { .. } => {
                if !ctrl && !shift {
                    self.tl.selection.clear();
                }
                self.tl.drag = Some(Drag::Scrub);
                let t = self.x_to_time(x);
                self.set_playhead(t);
            }
        }
        self.refresh_timeline();
        self.refresh_inspector();
    }

    fn pointer_move(&mut self, x: f64, y: f64) {
        match self.tl.drag.take() {
            None => {
                let c = if self.tl.blade {
                    0
                } else {
                    match self.hit(x, y) {
                        Hit::Clip { edge: Some(_), .. } => 1,
                        _ => 0,
                    }
                };
                if c != self.tl.cursor {
                    self.tl.cursor = c;
                    self.ui().set_cursor_kind(c);
                }
            }
            Some(Drag::Scrub) => {
                let t = self.x_to_time(x);
                self.set_playhead(t);
                self.tl.drag = Some(Drag::Scrub);
            }
            Some(Drag::Trim { clip, edge, .. }) => {
                let ex: HashSet<ClipId> = std::iter::once(clip).chain(self.project.sequence().linked_clips(clip)).collect();
                let to = self.snap_time(self.x_to_time(x), &ex);
                self.tl.drag = Some(Drag::Trim { clip, edge, to });
                self.refresh_timeline();
            }
            Some(Drag::Move { clips, anchor_t, start_x, start_y, start_row, started, clicked, .. }) => {
                let started = started || (x - start_x).abs() > DRAG_THRESHOLD || (y - start_y).abs() > DRAG_THRESHOLD;
                let mut delta = Time::ZERO;
                let mut track_delta = 0;
                if started {
                    let seq = self.project.sequence();
                    let raw = Time::from_secs_f64(self.tl.scroll + x / self.tl.pps - anchor_t);
                    let all = kadr_timeline::commands::with_links(seq, &clips);
                    let block_start = all.iter().filter_map(|c| seq.clip(*c)).map(|c| c.timeline_in).min().unwrap_or(Time::ZERO);
                    let block_end = all.iter().filter_map(|c| seq.clip(*c)).map(|c| c.timeline_out).max().unwrap_or(Time::ZERO);
                    delta = raw.max(-block_start);
                    self.tl.snap_line = None;
                    if self.tl.snapping {
                        let ex: HashSet<ClipId> = all.iter().copied().collect();
                        let pts = snap_points(seq, &ex, &[self.playhead]);
                        let snapped = snap_move(block_start, block_end, delta, &pts, self.snap_threshold());
                        if snapped != delta {
                            let edge = if (block_start + snapped).as_secs_f64() == 0.0 { block_start + snapped } else {
                                if pts.contains(&(block_start + snapped)) { block_start + snapped } else { block_end + snapped }
                            };
                            self.tl.snap_line = Some(edge);
                        }
                        delta = snapped;
                    }
                    delta = seq.frame_rate.snap(delta);
                    // Track change within the same kind.
                    let rs = rows(seq, self.tl.scroll_y);
                    if let (Some(r1), Some(r0)) = (self.row_at(y), rs.get(start_row)) {
                        let r1 = rs[r1];
                        if r1.kind == r0.kind {
                            let d = r0.y - r1.y; // positive when moving up
                            let steps = (d / r0.h).round() as i32;
                            track_delta = if r0.kind == TrackKind::Video { steps } else { -steps };
                        }
                    }
                }
                self.tl.drag = Some(Drag::Move { clips, anchor_t, start_x, start_y, start_row, delta, track_delta, started, clicked });
                self.refresh_timeline();
            }
        }
    }

    fn pointer_up(&mut self) {
        let drag = self.tl.drag.take();
        self.tl.snap_line = None;
        match drag {
            Some(Drag::Move { clips, delta, track_delta, started: true, .. }) => {
                if delta != Time::ZERO || track_delta != 0 {
                    self.execute(EditCommand::MoveClips { clips, delta, track_delta });
                }
            }
            Some(Drag::Move { started: false, clicked: Some(id), .. }) => {
                if let Some(sel) = narrowed_on_release(&self.tl.selection, id) {
                    self.tl.selection = sel;
                    self.refresh_inspector();
                }
            }
            Some(Drag::Trim { clip, edge, to }) => {
                let ripple = self.tl.ripple;
                self.execute(EditCommand::TrimClip { clip, edge, to, ripple });
            }
            _ => {}
        }
        self.refresh_timeline();
    }

    pub fn tl_scroll(&mut self, dx: f32, dy: f32, ctrl: bool, shift: bool) {
        let (dx, dy) = (dx as f64, dy as f64);
        if ctrl {
            let factor = if dy > 0.0 { 1.25 } else { 0.8 };
            self.zoom_at(self.tl.last_mouse_x, factor);
        } else if shift {
            self.tl.scroll_y = (self.tl.scroll_y - dy).max(0.0);
            let max = (self.content_height() - self.lanes_size().1 + 20.0).max(0.0);
            self.tl.scroll_y = self.tl.scroll_y.min(max);
        } else {
            let d = if dx.abs() > dy.abs() { dx } else { dy };
            self.tl.scroll = (self.tl.scroll - d / self.tl.pps).max(0.0);
        }
        self.refresh_timeline();
    }

    fn content_height(&self) -> f64 {
        rows(self.project.sequence(), 0.0).last().map_or(0.0, |r| r.y + r.h)
    }

    pub fn zoom_at(&mut self, x: f64, factor: f64) {
        let t = self.tl.scroll + x / self.tl.pps;
        self.tl.pps = (self.tl.pps * factor).clamp(0.2, 2400.0);
        self.tl.scroll = (t - x / self.tl.pps).max(0.0);
        self.refresh_timeline();
    }

    pub fn zoom_fit(&mut self) {
        let (w, _) = self.lanes_size();
        let dur = self.project.sequence().duration().as_secs_f64().max(10.0);
        self.tl.pps = ((w - 40.0) / dur).clamp(0.2, 2400.0);
        self.tl.scroll = 0.0;
        self.refresh_timeline();
    }

    pub fn tl_scrollbar(&mut self, f: f32) {
        let total = self.scroll_extent();
        self.tl.scroll = (f as f64 * total).clamp(0.0, total);
        self.refresh_timeline();
    }

    fn scroll_extent(&self) -> f64 {
        let (w, _) = self.lanes_size();
        (self.project.sequence().duration().as_secs_f64() + w / self.tl.pps * 0.5).max(w / self.tl.pps)
    }

    pub fn tl_ruler(&mut self, kind: i32, x: f32) {
        if kind == 2 {
            return;
        }
        if kind == 3 {
            // Double-click on a marker renames it.
            if let Some(i) = self.marker_near_x(x as f64) {
                self.marker_action(i, "rename");
            }
            return;
        }
        let t = self.x_to_time(x as f64);
        self.set_playhead(t);
    }

    pub fn tl_track_toggle(&mut self, index: i32, which: &str) {
        let seq = self.project.sequence();
        let Some(t) = seq.tracks.get(index as usize) else { return };
        let (flag, value) = match which {
            "mute" => (TrackFlag::Mute, !t.muted),
            "solo" => (TrackFlag::Solo, !t.solo),
            _ => (TrackFlag::Lock, !t.locked),
        };
        let track = t.id;
        self.execute(EditCommand::SetTrackFlag { track, flag, value });
    }

    pub fn tl_tool(&mut self, tool: &str) {
        match tool {
            "undo" => self.undo(),
            "redo" => self.redo(),
            "blade" => {
                self.tl.blade = !self.tl.blade;
                self.toast(if self.tl.blade { t("toast.blade_on") } else { t("toast.blade_off") });
            }
            "snap" => {
                self.tl.snapping = !self.tl.snapping;
                self.toast(if self.tl.snapping { t("toast.snap_on") } else { t("toast.snap_off") });
                self.settings.snapping = self.tl.snapping;
                self.settings.save(&self.dirs.settings_file());
            }
            "ripple" => {
                self.tl.ripple = !self.tl.ripple;
                self.toast(if self.tl.ripple { t("toast.ripple_on") } else { t("toast.ripple_off") });
            }
            "marker" => self.add_marker(),
            "zoom-in" => {
                let x = self.time_to_x(self.playhead).clamp(0.0, self.lanes_size().0);
                self.zoom_at(x, 1.5);
            }
            "zoom-out" => {
                let x = self.time_to_x(self.playhead).clamp(0.0, self.lanes_size().0);
                self.zoom_at(x, 1.0 / 1.5);
            }
            "zoom-fit" => self.zoom_fit(),
            _ => {}
        }
        self.refresh_timeline();
    }

    pub fn add_marker(&mut self) {
        let n = self.project.sequence().markers.len() + 1;
        self.execute(EditCommand::AddMarker(kadr_project::Marker::new(self.playhead, tf("marker.default_name", &[("n", &n.to_string())]))));
    }

    pub fn split_at_playhead(&mut self) {
        let clips = (!self.tl.selection.is_empty()).then(|| self.tl.selection.clone());
        if !self.execute(EditCommand::Split { at: self.playhead, clips: clips.clone() }) && clips.is_some() {
            // Selected clips aren't under the playhead: split everything there.
            self.execute(EditCommand::Split { at: self.playhead, clips: None });
        }
    }

    pub fn delete_selection(&mut self, ripple: bool) {
        if self.tl.selection.is_empty() {
            // With an In/Out range and nothing selected, delete the range.
            if let Some(r) = self.project.sequence().in_out {
                self.execute(EditCommand::DeleteRange { range: r, ripple });
                self.project.sequence_mut().in_out = None;
                self.refresh_timeline();
            } else {
                self.toast(t("toast.select_clips_to_delete"));
            }
            return;
        }
        let clips = std::mem::take(&mut self.tl.selection);
        self.execute(EditCommand::DeleteClips { clips, ripple });
    }

    pub fn tl_context(&mut self, action: &str) {
        match action {
            "split" => self.split_at_playhead(),
            "delete" => self.delete_selection(false),
            "ripple-delete" => self.delete_selection(true),
            "marker" => self.add_marker(),
            "select-all" => self.select_all(),
            "toggle-enabled" => {
                let cmds: Vec<EditCommand> = self
                    .tl
                    .selection
                    .iter()
                    .filter_map(|id| self.project.sequence().clip(*id).map(|c| (id, c.enabled)))
                    .map(|(id, en)| EditCommand::SetClipProperty { clip: *id, prop: ClipProperty::Enabled(!en) })
                    .collect();
                if !cmds.is_empty() {
                    self.execute(EditCommand::Batch { label: t("cmd.toggle_enabled"), commands: cmds });
                }
            }
            "unlink" => {
                let seq = self.project.sequence();
                let ids = kadr_timeline::commands::with_links(seq, &self.tl.selection);
                let cmds: Vec<EditCommand> =
                    ids.into_iter().map(|clip| EditCommand::SetClipProperty { clip, prop: ClipProperty::Link(None) }).collect();
                if !cmds.is_empty() {
                    self.execute(EditCommand::Batch { label: t("cmd.unlink"), commands: cmds });
                }
            }
            "link" => match kadr_timeline::commands::link_selection(self.project.sequence(), &self.tl.selection) {
                Some(commands) => {
                    self.execute(EditCommand::Batch { label: t("cmd.link"), commands });
                }
                None => self.toast(t("toast.link_needs_two")),
            },
            "delete-video" | "delete-audio" => {
                let (kind, label) = if action == "delete-video" { (TrackKind::Video, "cmd.delete_video") } else { (TrackKind::Audio, "cmd.delete_audio") };
                match kadr_timeline::commands::delete_part(self.project.sequence(), &self.tl.selection, kind) {
                    Some(commands) => {
                        self.execute(EditCommand::Batch { label: t(label), commands });
                        let seq = self.project.sequence();
                        self.tl.selection.retain(|id| seq.clip(*id).is_some());
                        self.refresh_timeline();
                        self.refresh_inspector();
                    }
                    None => self.toast(t(if kind == TrackKind::Video { "toast.no_video_part" } else { "toast.no_audio_part" })),
                }
            }
            "dissolve" => self.add_dissolve(),
            "rename-clip" => {
                if let Some(&id) = self.tl.selection.first() {
                    let name = self.project.sequence().clip(id).map(|c| c.name.clone()).unwrap_or_default();
                    self.prompt(&t("dlg.rename_clip.title"), &t("dlg.rename_clip.label"), &name, crate::app::Prompt::RenameClip(id));
                }
            }
            "reveal-media" => {
                if let Some(c) = self.tl.selection.first().and_then(|id| self.project.sequence().clip(*id)) {
                    self.selected_asset = Some(c.asset);
                    self.bin_filter = None;
                    self.media_search.clear();
                    self.ui().set_media_search("".into());
                    self.refresh_library();
                }
            }
            "rename-marker" | "delete-marker" => match self.marker_near_playhead() {
                Some(i) => self.marker_action(i, if action == "rename-marker" { "rename" } else { "delete" }),
                None => self.toast(t("toast.no_marker_near")),
            },
            "set-in" => self.set_in_out(true),
            "set-out" => self.set_in_out(false),
            "clear-in-out" => self.clear_in_out(),
            _ => {}
        }
    }

    fn marker_near_x(&self, x: f64) -> Option<usize> {
        self.project
            .sequence()
            .markers
            .iter()
            .enumerate()
            .map(|(i, m)| (i, (self.time_to_x(m.time) - x).abs()))
            .filter(|(_, d)| *d <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Marker within ~10 px of the playhead.
    fn marker_near_playhead(&self) -> Option<usize> {
        self.marker_near_x(self.time_to_x(self.playhead))
    }

    pub fn tl_track_action(&mut self, index: i32, action: &str) {
        match action {
            "add-video" => {
                self.execute(EditCommand::AddTrack { kind: TrackKind::Video });
            }
            "add-audio" => {
                self.execute(EditCommand::AddTrack { kind: TrackKind::Audio });
            }
            "rename" => {
                if let Some(t) = self.project.sequence().tracks.get(index as usize) {
                    self.tl.renaming_track = Some(t.id);
                    self.refresh_timeline();
                }
            }
            "delete" => {
                if let Some(t) = self.project.sequence().tracks.get(index as usize) {
                    let track = t.id;
                    self.execute(EditCommand::RemoveTrack { track });
                }
            }
            _ => {}
        }
    }

    pub fn tl_track_rename(&mut self, index: i32, name: &str) {
        self.tl.renaming_track = None;
        if let Some(t) = self.project.sequence().tracks.get(index as usize) {
            let track = t.id;
            if !self.execute(EditCommand::RenameTrack { track, name: name.to_string() }) {
                self.refresh_timeline();
            }
        }
    }

    fn add_dissolve(&mut self) {
        let seq = self.project.sequence();
        // Nearest cut to the playhead on a video track.
        let mut best: Option<(TrackId, Time)> = None;
        for t in seq.tracks.iter().filter(|t| t.kind == TrackKind::Video) {
            for w in t.clips.windows(2) {
                if w[0].timeline_out == w[1].timeline_in {
                    let cut = w[0].timeline_out;
                    if best.is_none_or(|(_, b)| (cut - self.playhead).abs() < (b - self.playhead).abs()) {
                        best = Some((t.id, cut));
                    }
                }
            }
        }
        match best {
            Some((track, at)) => {
                self.execute(EditCommand::AddTransition(Transition {
                    id: kadr_core::TransitionId::new(),
                    kind: TransitionKind::CrossDissolve,
                    track,
                    at,
                    duration: Time::from_secs(1),
                }));
                self.toast(t("toast.dissolve_added"));
            }
            None => self.toast(t("toast.no_cut_near")),
        }
    }

    pub fn select_all(&mut self) {
        self.tl.selection = self.project.sequence().tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
        self.refresh_timeline();
        self.refresh_inspector();
    }

    /// Previous/next edit point (clip boundaries and markers).
    pub fn jump_edit(&mut self, forward: bool) {
        let pts = snap_points(self.project.sequence(), &HashSet::new(), &[]);
        let t = if forward {
            pts.into_iter().find(|p| *p > self.playhead)
        } else {
            pts.into_iter().rev().find(|p| *p < self.playhead)
        };
        if let Some(t) = t {
            self.set_playhead(t);
        }
    }

    /// Places an asset on the timeline (linked A/V when it has both).
    pub fn place_asset(&mut self, asset_id: kadr_core::AssetId, at: Time, mode: InsertMode, prefer_row: Option<usize>) {
        let Some(asset) = self.project.asset(asset_id).cloned() else { return };
        let seq = self.project.sequence();
        let rs = rows(seq, self.tl.scroll_y);
        let pref_track = prefer_row.and_then(|r| rs.get(r)).map(|r| r.track);
        let pick = |kind: TrackKind| -> Option<TrackId> {
            if let Some(i) = pref_track.filter(|&i| seq.tracks[i].kind == kind && !seq.tracks[i].locked) {
                return Some(seq.tracks[i].id);
            }
            seq.tracks.iter().find(|t| t.kind == kind && !t.locked).map(|t| t.id)
        };
        let dur = asset.duration();
        if dur <= Time::ZERO {
            self.toast_error(t("err.media_no_duration"));
            return;
        }
        let range = TimeRange::new(Time::ZERO, dur);
        let at = seq.frame_rate.snap(at);
        let link = (asset.has_video() && asset.has_audio()).then(LinkId::new);
        let mut cmds = vec![];
        if asset.kind() != MediaKind::Audio {
            match pick(TrackKind::Video) {
                Some(track) => {
                    let mut c = Clip::new(asset.id, &asset.name, range, at);
                    c.link = link;
                    cmds.push(EditCommand::InsertClip { track, clip: c, mode });
                }
                None => return self.toast_warn(t("err.video_tracks_locked")),
            }
        }
        if asset.has_audio() {
            match pick(TrackKind::Audio) {
                Some(track) => {
                    let mut c = Clip::new(asset.id, &asset.name, range, at);
                    c.link = link;
                    cmds.push(EditCommand::InsertClip { track, clip: c, mode });
                }
                None => return self.toast_warn(t("err.audio_tracks_locked")),
            }
        }
        let was_empty = self.project.sequence().duration() == Time::ZERO;
        // The first clip defines the sequence format (like most NLEs).
        if was_empty {
            if let Some(v) = asset.info.video.as_ref().filter(|_| asset.kind() == MediaKind::Video) {
                let seq = self.project.sequence_mut();
                seq.width = v.width.max(2) & !1;
                seq.height = v.height.max(2) & !1;
                if let Some(fr) = v.frame_rate {
                    seq.frame_rate = fr;
                }
                self.meta_dirty = true;
            }
        }
        let label = match mode {
            InsertMode::Insert => tf("cmd.insert_named", &[("name", &asset.name)]),
            InsertMode::Overwrite => tf("cmd.overwrite_named", &[("name", &asset.name)]),
        };
        if self.execute(EditCommand::Batch { label, commands: cmds }) {
            let ids: Vec<ClipId> = {
                let seq = self.project.sequence();
                seq.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| c.asset == asset_id && c.timeline_in == at).map(|c| c.id).collect()
            };
            self.tl.selection = ids;
            if was_empty {
                self.zoom_fit();
                self.refresh_status();
            }
            // Show what was just placed instead of "no clip under playhead".
            if asset.kind() != MediaKind::Audio && kadr_timeline::composition::video_at(self.project.sequence(), self.playhead).is_none() {
                self.set_playhead(at);
            }
            self.refresh_timeline();
            self.refresh_inspector();
        }
    }

    // ------------------------------------------------------------------ view

    pub fn refresh_playhead(&mut self) {
        let ui = self.ui();
        let x = self.time_to_x(self.playhead);
        ui.set_playhead_x(x as f32);
        ui.set_show_playhead(x >= -1.0);
        let seq = self.project.sequence();
        ui.set_timecode(kadr_core::Timecode::from_time(self.playhead, seq.frame_rate).to_string().into());
        let dur = seq.duration();
        ui.set_duration_label(format!("/ {}", kadr_core::Timecode::from_time(dur, seq.frame_rate)).into());
        ui.set_scrub_fraction(if dur > Time::ZERO { (self.playhead.as_secs_f64() / dur.as_secs_f64()) as f32 } else { 0.0 });
    }

    pub fn refresh_timeline(&mut self) {
        let ui = self.ui();
        let (w, h) = self.lanes_size();
        let seq = self.project.sequence().clone();
        let rs = rows(&seq, self.tl.scroll_y);
        let t0 = self.tl.scroll;
        let t1 = t0 + w / self.tl.pps;
        let sel: HashSet<ClipId> = self.tl.selection.iter().copied().collect();

        // Live drag offsets.
        let (moving, move_delta, move_tracks): (HashSet<ClipId>, f64, i32) = match &self.tl.drag {
            Some(Drag::Move { clips, delta, track_delta, started: true, .. }) => {
                (kadr_timeline::commands::with_links(&seq, clips).into_iter().collect(), delta.as_secs_f64(), *track_delta)
            }
            _ => (HashSet::new(), 0.0, 0),
        };
        let primary: HashSet<ClipId> = match &self.tl.drag {
            Some(Drag::Move { clips, .. }) => clips.iter().copied().collect(),
            _ => HashSet::new(),
        };
        let trim = match &self.tl.drag {
            Some(Drag::Trim { clip, edge, to }) => {
                let group: HashSet<ClipId> = std::iter::once(*clip).chain(seq.linked_clips(*clip)).collect();
                let anchor = seq.clip(*clip).map(|c| if *edge == TrimEdge::Start { c.timeline_in } else { c.timeline_out });
                anchor.map(|a| (group, *edge, *to - a))
            }
            _ => None,
        };

        let mut tracks = vec![];
        let mut clips = vec![];
        for (ri, r) in rs.iter().enumerate() {
            let t = &seq.tracks[r.track];
            tracks.push(TrackView {
                index: r.track as i32,
                name: t.name.clone().into(),
                is_audio: t.kind == TrackKind::Audio,
                muted: t.muted,
                solo: t.solo,
                locked: t.locked,
                y: r.y as f32,
                height: r.h as f32,
                empty: t.clips.is_empty(),
                renaming: self.tl.renaming_track == Some(t.id),
            });
            if r.y + r.h < 0.0 || r.y > h {
                continue; // vertically off-screen
            }
            let first = t.clips.partition_point(|c| c.timeline_out.as_secs_f64() < t0 - 1.0);
            for c in &t.clips[first..] {
                let mut cin = c.timeline_in.as_secs_f64();
                let mut cout = c.timeline_out.as_secs_f64();
                let mut row = r;
                let moved = moving.contains(&c.id);
                if moved {
                    cin += move_delta;
                    cout += move_delta;
                    if primary.contains(&c.id) && move_tracks != 0 {
                        // Show on the destination row of the same kind.
                        let same: Vec<&Row> = rs.iter().filter(|x| x.kind == r.kind).collect();
                        let pos = same.iter().position(|x| x.track == r.track).unwrap_or(0) as i32;
                        let step = if r.kind == TrackKind::Video { -move_tracks } else { move_tracks };
                        let np = (pos + step).clamp(0, same.len() as i32 - 1) as usize;
                        row = same[np];
                    }
                }
                if let Some((group, edge, d)) = &trim {
                    if group.contains(&c.id) {
                        match edge {
                            TrimEdge::Start => cin += d.as_secs_f64(),
                            TrimEdge::End => cout += d.as_secs_f64(),
                        }
                    }
                }
                if cin > t1 + 1.0 && !moved {
                    break;
                }
                if cout < t0 || cin > t1 {
                    continue;
                }
                let x = (cin - t0) * self.tl.pps;
                let width = ((cout - cin) * self.tl.pps).max(2.0);
                let is_audio = t.kind == TrackKind::Audio;
                let rt = self.assets_rt.get(&c.asset);
                let thumb = rt.and_then(|r| r.thumb.clone()).filter(|_| !is_audio);
                let (wave, wave_x, wave_w) = if is_audio {
                    let vis0 = x.max(0.0);
                    let vis1 = (x + width).min(w);
                    match rt.and_then(|r| r.overview.clone()) {
                        Some(ov) if vis1 > vis0 => {
                            let s0 = c.source_in.as_secs_f64() + (vis0 - x) / self.tl.pps * c.speed();
                            let s1 = c.source_in.as_secs_f64() + (vis1 - x) / self.tl.pps * c.speed();
                            let img = self.waves.get(c.id, &ov, s0, s1, (vis1 - vis0) as u32, (row.h - 20.0) as u32, c.audio.gain_db);
                            (Some(img), vis0 - x, vis1 - vis0)
                        }
                        _ => (None, 0.0, 0.0),
                    }
                } else {
                    (None, 0.0, 0.0)
                };
                let speed = c.speed();
                let marks: Vec<ClipMark> = if is_audio { vec![] } else { self.clip_marks(c, width) };
                clips.push(ClipView {
                    id: c.id.to_string().into(),
                    x: x as f32,
                    y: row.y as f32,
                    width: width as f32,
                    height: row.h as f32,
                    name: c.name.clone().into(),
                    is_audio,
                    selected: sel.contains(&c.id),
                    enabled: c.enabled,
                    linked: c.link.is_some(),
                    speed_label: if (speed - 1.0).abs() > 0.001 { format!("{speed:.2}×").into() } else { "".into() },
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    has_wave: wave.is_some(),
                    wave: wave.unwrap_or_default(),
                    wave_x: wave_x as f32,
                    wave_width: wave_w as f32,
                    fade_in: (c.audio.fade_in.as_secs_f64() * self.tl.pps) as f32,
                    fade_out: (c.audio.fade_out.as_secs_f64() * self.tl.pps) as f32,
                    offline: rt.is_some_and(|r| r.status == crate::app::AssetStatus::Missing),
                    marks: ModelRc::new(VecModel::from(marks)),
                });
                let _ = ri;
            }
        }

        // Ruler ticks.
        let fps = seq.frame_rate.as_f64();
        let steps = [1.0 / fps, 2.0 / fps, 5.0 / fps, 10.0 / fps, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0];
        let major = steps.iter().copied().find(|s| s * self.tl.pps >= 90.0).unwrap_or(3600.0);
        let minor = if major <= 1.0 / fps * 1.01 { major } else { major / if major >= 60.0 { 6.0 } else { 5.0 } };
        let mut ticks = vec![];
        let mut k = (t0 / minor).floor() as i64;
        loop {
            let t = k as f64 * minor;
            if t > t1 {
                break;
            }
            if t >= 0.0 {
                let is_major = ((t / major) - (t / major).round()).abs() < 1e-6;
                ticks.push(TickView {
                    x: ((t - t0) * self.tl.pps) as f32,
                    label: if is_major { fmt_ruler(t, major, fps).into() } else { "".into() },
                    major: is_major,
                });
            }
            k += 1;
            if ticks.len() > 600 {
                break;
            }
        }

        let markers: Vec<MarkerView> = seq
            .markers
            .iter()
            .map(|m| MarkerView { x: self.time_to_x(m.time) as f32, name: m.name.clone().into(), color: rgb(&m.color) })
            .collect();
        let transitions: Vec<TransitionView> = seq
            .transitions
            .iter()
            .filter_map(|tr| {
                let r = rs.iter().find(|r| seq.tracks[r.track].id == tr.track)?;
                let half = tr.duration.as_secs_f64() / 2.0;
                Some(TransitionView {
                    x: ((tr.at.as_secs_f64() - half - t0) * self.tl.pps) as f32,
                    y: r.y as f32,
                    width: (tr.duration.as_secs_f64() * self.tl.pps) as f32,
                    height: r.h as f32,
                })
            })
            .collect();

        ui.set_tracks(ModelRc::new(VecModel::from(tracks)));
        ui.set_clips(ModelRc::new(VecModel::from(clips)));
        ui.set_ticks(ModelRc::new(VecModel::from(ticks)));
        ui.set_markers(ModelRc::new(VecModel::from(markers)));
        ui.set_transitions(ModelRc::new(VecModel::from(transitions)));
        match self.tl.snap_line {
            Some(t) => {
                ui.set_snap_x(self.time_to_x(t) as f32);
                ui.set_show_snap(true);
            }
            None => ui.set_show_snap(false),
        }
        match seq.in_out {
            Some(r) => {
                ui.set_in_x(self.time_to_x(r.start) as f32);
                ui.set_out_x(self.time_to_x(r.end) as f32);
                ui.set_has_in_out(true);
            }
            None => ui.set_has_in_out(false),
        }
        match self.tl.ghost {
            Some((gx, gy, gw, gh)) => {
                ui.set_ghost_x(gx as f32);
                ui.set_ghost_y(gy as f32);
                ui.set_ghost_w(gw as f32);
                ui.set_ghost_h(gh as f32);
                ui.set_show_ghost(true);
            }
            None => ui.set_show_ghost(false),
        }
        ui.set_snapping(self.tl.snapping);
        ui.set_ripple_mode(self.tl.ripple);
        ui.set_blade_mode(self.tl.blade);
        ui.set_zoom_label(zoom_label(self.tl.pps, fps).into());
        let extent = self.scroll_extent();
        ui.set_scroll_fraction((self.tl.scroll / extent) as f32);
        ui.set_view_fraction(((w / self.tl.pps) / extent) as f32);
        ui.set_can_undo(self.engine.can_undo());
        ui.set_can_redo(self.engine.can_redo());
        ui.set_has_selection(!self.tl.selection.is_empty());
        ui.set_timeline_empty(seq.duration() == Time::ZERO);
        self.refresh_playhead();
    }

    /// Keeps the playhead visible during playback (page-style follow).
    pub fn follow_playhead(&mut self) {
        let (w, _) = self.lanes_size();
        let x = self.time_to_x(self.playhead);
        if x > w - 20.0 || x < 0.0 {
            self.tl.scroll = (self.playhead.as_secs_f64() - 20.0 / self.tl.pps).max(0.0);
            self.refresh_timeline();
        }
    }

    // ----------------------------------------------------- library drag-drop

    /// Returns (time, row) if window coordinates are over the lanes.
    pub fn drop_target(&self, wx: f64, wy: f64) -> Option<(Time, usize)> {
        let ui = self.ui();
        let (lx, ly) = (ui.get_lanes_abs_x() as f64, ui.get_lanes_abs_y() as f64);
        let (w, h) = self.lanes_size();
        let (x, y) = (wx - lx, wy - ly);
        if x < 0.0 || y < 0.0 || x > w || y > h {
            return None;
        }
        let row = self.row_at(y).unwrap_or(0);
        Some((self.x_to_time(x), row))
    }
}

fn zoom_label(pps: f64, fps: f64) -> String {
    let frame_px = pps / fps;
    if frame_px >= 4.0 {
        tf("tl.zoom.px_fr", &[("v", &format!("{frame_px:.0}"))])
    } else if pps >= 1.0 {
        tf("tl.zoom.px_s", &[("v", &format!("{pps:.0}"))])
    } else {
        tf("tl.zoom.px_s", &[("v", &format!("{pps:.1}"))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_click_inside_a_multi_selection_narrows_it_on_release() {
        let (v, a) = (ClipId::new(), ClipId::new());
        assert_eq!(narrowed_on_release(&[v, a], a), Some(vec![a]), "pick the audio after unlinking");
        assert_eq!(narrowed_on_release(&[v], v), None, "already alone");
        assert_eq!(narrowed_on_release(&[v], a), None, "not part of the selection");
    }
}
