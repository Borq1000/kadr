//! Timeline view-model and interaction. Only visible clips are turned into
//! UI rectangles; hit-testing, dragging, trimming and snapping happen here
//! in time coordinates, and a single engine command is issued on release.

use crate::anim_ui;
use crate::app::App;
use crate::util::{fmt_ruler, fmt_secs, rgb};
use crate::{ClipMark, ClipView, KeyMark, MarkerView, TickView, TrackView, TransitionView};
use kadr_core::{ClipId, LinkId, MediaKind, Time, TimeRange, TrackId};
use kadr_project::{Clip, Ease, EdgeFades, FadeEdge, Sequence, SourceTime, TrackKind, Transition, TransitionKind};
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
    Fade(FadeDrag),
    Keys(KeyDrag),
}

/// A fade-handle drag. The fade boundary follows the pointer's movement
/// once it passed `DRAG_THRESHOLD`; `value` is live in the clip
/// (`App::live_edit`) and recorded as one command on release.
#[derive(Clone, Copy)]
pub struct FadeDrag {
    clip: ClipId,
    /// `None` while two overlapping handles wait for the first movement
    /// (`anim_ui::edge_for_move`).
    edge: Option<FadeEdge>,
    is_audio: bool,
    start_x: f64,
    /// Press − fade boundary, px, taken when the drag starts: grabbing
    /// never makes the fade jump.
    grab: f64,
    started: bool,
    value: Time,
}

/// A keyframe-diamond drag: the keyframes of one exact moment `at`, all
/// properties. Past `DRAG_THRESHOLD` they follow the pointer's movement on
/// whole frames, live in the clip (`App::live_edit`, recomputed from the
/// recorded clip each step); the release records one `MoveKeyframes`. A
/// press released without a drag is a click: select the clip, playhead to
/// the moment.
#[derive(Clone, Copy)]
pub struct KeyDrag {
    clip: ClipId,
    at: SourceTime,
    start_x: f64,
    started: bool,
    /// The target (timeline time); `None` while it is the keyframe's own frame.
    to: Option<Time>,
}

// Spec §6.2: a handle's grab zone is at least 10 px wide and starts where
// the trim zone around the clip edge ends.
const _: () = assert!(2.0 * anim_ui::FADE_R >= 10.0);
const _: () = assert!(anim_ui::HANDLE_INSET - anim_ui::FADE_R >= TRIM_ZONE);
// Diamonds are hit in a video clip's bottom strip (2 px margin + KEY_STRIP_H),
// fade handles in its top one (HEADER_H): the strips never meet.
const _: () = assert!(anim_ui::HEADER_H <= VIDEO_H - 2.0 - anim_ui::KEY_STRIP_H);
// A diamond's grab zone (2 · CLUSTER_PX) is at least 10 px wide too.
const _: () = assert!(2.0 * anim_ui::CLUSTER_PX >= 10.0);

/// The fades a clip shows on the timeline: the picture's on a video track,
/// the sound's on an audio track.
fn clip_fades(c: &Clip, is_audio: bool) -> (Time, Time) {
    if is_audio { (c.audio.fade_in, c.audio.fade_out) } else { (c.video_fade.fade_in, c.video_fade.fade_out) }
}

fn fade_slot(c: &mut Clip, is_audio: bool, edge: FadeEdge) -> &mut Time {
    match (is_audio, edge) {
        (false, FadeEdge::In) => &mut c.video_fade.fade_in,
        (false, FadeEdge::Out) => &mut c.video_fade.fade_out,
        (true, FadeEdge::In) => &mut c.audio.fade_in,
        (true, FadeEdge::Out) => &mut c.audio.fade_out,
    }
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
    /// The clip under the pointer (shows its fade handles).
    pub hover_clip: Option<ClipId>,
    /// The keyframe moment the keyframe menu acts on (its last right press).
    pub menu_key: Option<(ClipId, SourceTime)>,
}

impl TimelineUi {
    /// The fade handles (in, out) a clip offers: none on a locked track or
    /// in blade mode, otherwise `anim_ui::shown_handles` — each with its own
    /// fade, both while the clip is selected or hovered. The view and
    /// hit-testing share this, so a hidden handle is never hit.
    fn fade_handles(&self, id: ClipId, selected: bool, locked: bool, width: f64, (fi, fo): (Time, Time)) -> (bool, bool) {
        if locked || self.blade {
            return (false, false);
        }
        anim_ui::shown_handles(width, selected || self.hover_clip == Some(id), fi, fo)
    }
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
            hover_clip: None,
            menu_key: None,
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
    /// `edge: None` — two overlapping handles; the drag direction picks.
    Fade { id: ClipId, edge: Option<FadeEdge> },
    /// A keyframe diamond: one real moment (also inside a drawn cluster).
    Key { id: ClipId, at: SourceTime },
    Empty,
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
        let Some(ri) = rs.iter().position(|r| y >= r.y && y < r.y + r.h) else { return Hit::Empty };
        let track = &seq.tracks[rs[ri].track];
        let t = self.x_to_time(x);
        // Fade handles, in the clip's top strip only. Their zones lie
        // `HANDLE_INSET − FADE_R` ≥ `TRIM_ZONE` inside the clip, so they never
        // take a trim edge's place, and only the clip under `x` can have one there.
        if y < rs[ri].y + anim_ui::HEADER_H
            && let Some(c) = track.clip_at(t)
        {
            let (x0, x1) = (self.time_to_x(c.timeline_in), self.time_to_x(c.timeline_out));
            let fades = clip_fades(c, track.kind == TrackKind::Audio);
            let shown = self.tl.fade_handles(c.id, self.tl.selection.contains(&c.id), track.locked, x1 - x0, fades);
            let px = |f: Time| f.as_secs_f64() * self.tl.pps;
            match anim_ui::pick_handle(x, x0, x1, px(fades.0), px(fades.1), shown) {
                Some(anim_ui::HandleHit::Edge(edge)) => return Hit::Fade { id: c.id, edge: Some(edge) },
                Some(anim_ui::HandleHit::Both) => return Hit::Fade { id: c.id, edge: None },
                None => {}
            }
        }
        // Keyframe diamonds, in a video clip's bottom strip, of the clip under
        // `x` only: the nearest real moment within CLUSTER_PX. Here they come
        // before the trim edges (a keyframe on the clip's first frame stays
        // reachable); above the strip the edges are whole. Not on a locked
        // track or in blade mode, as the fade handles.
        if rs[ri].kind == TrackKind::Video
            && !track.locked
            && !self.tl.blade
            && y >= rs[ri].y + rs[ri].h - 2.0 - anim_ui::KEY_STRIP_H
            && let Some(c) = track.clip_at(t).filter(|c| !c.animation.is_empty())
            && let Some(at) = anim_ui::nearest_key(&self.key_positions(c), x, anim_ui::CLUSTER_PX)
        {
            return Hit::Key { id: c.id, at };
        }
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
            None => Hit::Empty,
        }
    }

    /// (moment, x in lane pixels) of the clip's visible keyframe moments.
    fn key_positions(&self, c: &Clip) -> Vec<(SourceTime, f64)> {
        anim_ui::visible_keys(c).into_iter().map(|(at, t)| (at, self.time_to_x(t))).collect()
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
                // Right click: select what's under the cursor for the context
                // menu; a diamond opens the keyframe menu for its moment.
                // Runs before Slint shows the menu, which reads `menu-kind`.
                let hit = self.hit(x, y);
                self.tl.menu_key = match hit {
                    Hit::Key { id, at } => Some((id, at)),
                    _ => None,
                };
                if let Some((id, at)) = self.tl.menu_key {
                    // The curve the menu checks: shared by every property there.
                    let ease = self.project.sequence().clip(id).map_or("", |c| anim_ui::moment_ease(c, at));
                    self.ui().set_tl_key_ease(ease.into());
                }
                self.ui().set_tl_menu_kind(self.tl.menu_key.is_some() as i32);
                if let Hit::Clip { id, .. } | Hit::Fade { id, .. } | Hit::Key { id, .. } = hit
                    && !self.tl.selection.contains(&id)
                {
                    self.tl.selection = vec![id];
                    self.refresh_timeline();
                    self.refresh_inspector();
                }
            }
            0 if button == 0 => self.pointer_down(x, y, shift, ctrl),
            1 => self.pointer_move(x, y),
            2 if button == 0 => self.pointer_up(),
            3 => match self.hit(x, y) {
                // A diamond's own moment, not the pointer's spot beside it.
                Hit::Key { id, at } => {
                    if let Some(t) = self.project.sequence().clip(id).map(|c| c.timeline_time_of(at.0)) {
                        self.set_playhead(t);
                    }
                }
                Hit::Clip { .. } | Hit::Fade { .. } => {
                    let t = self.x_to_time(x);
                    self.set_playhead(t);
                }
                Hit::Empty => {}
            },
            // The pointer left the lanes.
            4 => self.set_hover_clip(None),
            // The grab was lost (focus gone mid-drag): no release will come,
            // so the drag ends here as released — one command, nothing live left.
            5 => self.pointer_up(),
            _ => {}
        }
    }

    /// The clip under the pointer (its fade handles show); redraws on a change.
    fn set_hover_clip(&mut self, id: Option<ClipId>) {
        if id != self.tl.hover_clip {
            self.tl.hover_clip = id;
            self.refresh_timeline();
        }
    }

    fn clip_under(&self, x: f64, y: f64) -> Option<ClipId> {
        let seq = self.project.sequence();
        let r = rows(seq, self.tl.scroll_y).into_iter().find(|r| y >= r.y && y < r.y + r.h)?;
        seq.tracks[r.track].clip_at(self.x_to_time(x)).map(|c| c.id)
    }

    /// A dragged handle's or diamond's value beside the pointer.
    fn show_tip(&self, text: String, x: f64, y: f64) {
        let ui = self.ui();
        ui.set_tl_tip_text(text.into());
        ui.set_tl_tip_x(x as f32);
        ui.set_tl_tip_y(y as f32);
    }

    fn show_fade_tip(&self, edge: FadeEdge, value: Time, x: f64, y: f64) {
        let v = fmt_secs(value);
        let text = if edge == FadeEdge::In { tf("tl.fade.in_tip", &[("t", &v)]) } else { tf("tl.fade.out_tip", &[("t", &v)]) };
        self.show_tip(text, x, y);
    }

    /// A press on a handle (`edge: None` — two overlapping ones, picked by
    /// the first movement). Nothing changes before the drag threshold.
    fn start_fade_drag(&mut self, id: ClipId, edge: Option<FadeEdge>, x: f64, y: f64) {
        let seq = self.project.sequence();
        let Some((ti, ci)) = seq.locate_clip(id) else { return };
        let is_audio = seq.tracks[ti].kind == TrackKind::Audio;
        let (fi, fo) = clip_fades(&seq.tracks[ti].clips[ci], is_audio);
        let value = if edge == Some(FadeEdge::In) { fi } else { fo };
        self.tl.drag = Some(Drag::Fade(FadeDrag { clip: id, edge, is_audio, start_x: x, grab: 0.0, started: false, value }));
        if let Some(edge) = edge {
            self.show_fade_tip(edge, value, x, y);
        }
    }

    /// A fade-handle drag step: the new value goes live into the clip
    /// (preview, inspector and timeline follow); history is untouched.
    fn drag_fade(&mut self, mut d: FadeDrag, x: f64, y: f64) {
        let seq = self.project.sequence();
        let Some((ti, ci)) = seq.locate_clip(d.clip) else {
            // The clip is gone (an undo or an AI edit removed it): the drag ends.
            self.ui().set_tl_tip_text("".into());
            return;
        };
        let c = &seq.tracks[ti].clips[ci];
        let (fi, fo) = clip_fades(c, d.is_audio);
        let (x0, x1) = (self.time_to_x(c.timeline_in), self.time_to_x(c.timeline_out));
        if !d.started && (x - d.start_x).abs() > DRAG_THRESHOLD {
            // Past the threshold: overlapping handles are told apart by the
            // direction, and the fade boundary follows the pointer from here.
            let edge = *d.edge.get_or_insert(anim_ui::edge_for_move(x - d.start_x, fi, fo));
            let fade = if edge == FadeEdge::In { fi } else { fo };
            d.grab = d.start_x - anim_ui::fade_edge_x(x0, x1, fade.as_secs_f64() * self.tl.pps, edge);
            d.started = true;
        }
        let Some(edge) = d.edge else {
            self.tl.drag = Some(Drag::Fade(d)); // overlapping handles, not moved yet
            return;
        };
        let (now, other) = if edge == FadeEdge::In { (fi, fo) } else { (fo, fi) };
        d.value = if d.started {
            anim_ui::fade_drag_value(edge, x - d.grab, x0, x1, self.tl.pps, seq.frame_rate.frame_duration(), other, c.duration())
        } else {
            now // within the threshold: the fade stays as it is
        };
        self.tl.drag = Some(Drag::Fade(d));
        self.show_fade_tip(edge, d.value, x, y);
        if d.value == now {
            return;
        }
        self.live_edit.arm(self.project.sequence_mut(), d.clip);
        *fade_slot(&mut self.project.sequence_mut().tracks[ti].clips[ci], d.is_audio, edge) = d.value;
        if !d.is_audio {
            self.preview.invalidate();
            self.request_frame();
        }
        self.refresh_inspector();
        self.refresh_timeline();
    }

    /// The release of a fade-handle drag: the clip goes back to its recorded
    /// state and the new fade is applied as one undoable command — none for
    /// a click or an unchanged fade (the engine records every successful command).
    fn commit_fade(&mut self, FadeDrag { clip, edge, is_audio, started, value, .. }: FadeDrag) {
        self.ui().set_tl_tip_text("".into());
        let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
        let cmd = edge.filter(|_| started).and_then(|edge| {
            let c = self.project.sequence().clip(clip)?;
            let (fi, fo) = clip_fades(c, is_audio);
            let (old, other) = if edge == FadeEdge::In { (fi, fo) } else { (fo, fi) };
            // Measured against the recorded clip, which an edit during the
            // drag (undo, AI) may have changed.
            let new = value.min((c.duration() - other).max(Time::ZERO));
            if new == old {
                return None;
            }
            let (fade_in, fade_out) = if edge == FadeEdge::In { (new, fo) } else { (fi, new) };
            let prop = if is_audio {
                let mut a = c.audio.clone();
                (a.fade_in, a.fade_out) = (fade_in, fade_out);
                ClipProperty::Audio(a)
            } else {
                ClipProperty::VideoFade(EdgeFades { fade_in, fade_out })
            };
            let name = if edge == FadeEdge::In { t("inspector.fade_in") } else { t("inspector.fade_out") };
            let label = tf("cmd.fade_named", &[("name", &name), ("from", &fmt_secs(old)), ("to", &fmt_secs(new))]);
            Some(EditCommand::Batch { label, commands: vec![EditCommand::SetClipProperty { clip, prop }] })
        });
        let recorded = cmd.is_some_and(|cmd| self.execute(cmd));
        if !recorded && rolled_back {
            self.show_rolled_back();
        }
    }

    /// A diamond-drag step: the moment's editable keyframes (every property
    /// and effect number this build can change, ruling R8) go live to the
    /// target frame, computed from the recorded clip; history is untouched.
    /// A target taken by another keyframe of one of those tracks keeps the
    /// last preview and says so; the release then gets the engine's refusal
    /// (no partial move). Read-only keyframes stay (and still show).
    fn drag_keys(&mut self, mut d: KeyDrag, x: f64, y: f64) {
        if !d.started && (x - d.start_x).abs() <= DRAG_THRESHOLD {
            self.tl.drag = Some(Drag::Keys(d));
            return;
        }
        d.started = true;
        let seq = self.project.sequence();
        let fr = seq.frame_rate;
        let Some((ti, ci)) = seq.locate_clip(d.clip) else {
            // The clip is gone (an undo or an AI edit removed it): the drag ends.
            self.ui().set_tl_tip_text("".into());
            return;
        };
        // The recorded clip: the armed snapshot during the preview, the clip
        // itself before it (or after an undo / AI edit rolled it back).
        let live = self.live_edit.recorded(d.clip);
        let armed = live.is_some();
        let rec = live.unwrap_or(&seq.tracks[ti].clips[ci]);
        let params = kadr_timeline::editable_keys_at(rec, d.at);
        if params.is_empty() {
            // The moment is gone, or holds read-only keyframes only: the drag ends.
            self.ui().set_tl_tip_text("".into());
            return;
        }
        let from = rec.timeline_time_of(d.at.0);
        let dt = Time::from_secs_f64((x - d.start_x) / self.tl.pps);
        let to = anim_ui::key_drag_to(rec, from, dt, fr);
        let moved = to != anim_ui::key_drag_to(rec, from, Time::ZERO, fr);
        let prev = std::mem::replace(&mut d.to, moved.then_some(to));
        // `None`: back on its own frame; `Some(None)`: the target is taken.
        let preview = moved.then(|| {
            let mut anim = rec.animation.clone();
            let new_at = SourceTime(rec.source_time_at(to));
            params.iter().all(|p| anim.move_key(p, d.at, new_at).is_ok()).then_some(anim)
        });
        let tip = match preview {
            Some(None) => t("err.edit.keyframe_collision"),
            _ => tf("tl.key.tip", &[("t", &kadr_core::Timecode::from_time(to, fr).to_string())]),
        };
        self.tl.drag = Some(Drag::Keys(d));
        self.show_tip(tip, x, y);
        let changed = match preview {
            None => self.live_edit.roll_back(self.project.sequence_mut()),
            Some(None) => false,
            Some(Some(_)) if armed && prev == d.to => false, // already shown
            Some(Some(anim)) => {
                self.live_edit.arm(self.project.sequence_mut(), d.clip);
                self.project.sequence_mut().tracks[ti].clips[ci].animation = anim;
                true
            }
        };
        if changed {
            self.preview.invalidate();
            self.request_frame();
            self.refresh_inspector();
            self.refresh_timeline();
        }
    }

    /// The release of a diamond press: a click selects the clip and puts
    /// the playhead on the moment; a drag rolls its preview back and records
    /// one `MoveKeyframes` of the editable keyframes — none when they end
    /// where they started.
    fn commit_keys(&mut self, d: KeyDrag) {
        self.ui().set_tl_tip_text("".into());
        let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
        let seq = self.project.sequence();
        let frame = seq.frame_rate.frame_duration();
        let Some(c) = seq.clip(d.clip) else {
            if rolled_back {
                self.show_rolled_back();
            }
            return;
        };
        if !d.started {
            let t = c.timeline_time_of(d.at.0);
            if let Some(sel) = narrowed_on_release(&self.tl.selection, d.clip) {
                self.tl.selection = sel;
                self.refresh_inspector();
            }
            self.set_playhead(t);
            return;
        }
        // Measured against the recorded clip, which an edit during the drag
        // (undo, AI) may have changed: still on frames inside its visible range.
        let cmd = d.to.and_then(|to| {
            let to = to.max(c.timeline_in).min((c.timeline_out - frame).max(c.timeline_in));
            anim_ui::move_moment(c, d.at, SourceTime(c.source_time_at(to)))
        });
        let recorded = cmd.is_some_and(|cmd| self.execute(cmd));
        if !recorded && rolled_back {
            self.show_rolled_back();
        }
    }

    /// The keyframe menu (spec §6.2): `key-ease:<key>` sets the curve of the
    /// moment's keyframes that differ (R14), `key-delete` removes all of
    /// them — the picture at the playhead stays for a property losing its
    /// last keyframe. Only editable keyframes (ruling R8). One undo entry;
    /// nothing when nothing would change.
    fn key_menu_action(&mut self, action: &str) {
        let Some((clip, at)) = self.tl.menu_key else { return };
        let seq = self.project.sequence();
        let Some(c) = seq.clip(clip) else { return };
        let (label, commands): (String, Vec<EditCommand>) = match action.strip_prefix("key-ease:") {
            Some(key) => {
                let Some(ease) = Ease::from_key(key) else { return };
                let commands = anim_ui::ease_targets(c, at, &ease)
                    .into_iter()
                    .map(|param| EditCommand::SetKeyframeEase { clip, param, at, ease: ease.clone() })
                    .collect();
                (t("cmd.keyframe_ease"), commands)
            }
            None => {
                let shown = anim_ui::shown_time(c, self.playhead, seq.frame_rate.frame_duration());
                let commands = anim_ui::delete_moment(c, at, shown);
                (t(if commands.len() > 1 { "cmd.remove_keyframes" } else { "cmd.remove_keyframe" }), commands)
            }
        };
        if !commands.is_empty() {
            self.execute(EditCommand::Batch { label, commands });
        }
    }

    fn pointer_down(&mut self, x: f64, y: f64, shift: bool, ctrl: bool) {
        // A press while a drag is still on: its release never reached us —
        // end it as released first, so its live state is recorded, not lost.
        if self.tl.drag.is_some() {
            self.pointer_up();
        }
        let hit = match self.hit(x, y) {
            // Ctrl/Shift-click edits the selection, as anywhere on the clip.
            Hit::Fade { id, .. } | Hit::Key { id, .. } if ctrl || shift => Hit::Clip { id, edge: None },
            hit => hit,
        };
        if self.tl.blade {
            if let Hit::Clip { id, .. } = hit {
                let t = self.snap_time(self.x_to_time(x), &HashSet::new());
                self.tl.snap_line = None;
                self.execute(EditCommand::Split { at: t, clips: Some(vec![id]) });
            }
            return;
        }
        match hit {
            Hit::Fade { id, edge } => {
                if !self.tl.selection.contains(&id) {
                    self.tl.selection = vec![id];
                }
                self.start_fade_drag(id, edge, x, y);
            }
            Hit::Key { id, at } => {
                if !self.tl.selection.contains(&id) {
                    self.tl.selection = vec![id];
                }
                self.tl.drag = Some(Drag::Keys(KeyDrag { clip: id, at, start_x: x, started: false, to: None }));
            }
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
            Hit::Empty => {
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
                let hover = self.clip_under(x, y);
                self.set_hover_clip(hover);
                let c = if self.tl.blade {
                    0
                } else {
                    match self.hit(x, y) {
                        Hit::Clip { edge: Some(_), .. } | Hit::Fade { .. } | Hit::Key { .. } => 1,
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
            Some(Drag::Fade(d)) => self.drag_fade(d, x, y),
            Some(Drag::Keys(d)) => self.drag_keys(d, x, y),
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
            Some(Drag::Fade(d)) => self.commit_fade(d),
            Some(Drag::Keys(d)) => self.commit_keys(d),
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
                    self.show_media_tab();
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
            a if a == "key-delete" || a.starts_with("key-ease:") => self.key_menu_action(a),
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

    /// Alt+←/→: the previous / next visible keyframe moment of the
    /// inspected clip (spec §6.2); hidden keyframes are skipped.
    pub fn jump_keyframe(&mut self, forward: bool) {
        let seq = self.project.sequence();
        let Some(c) = self.inspected().and_then(|id| seq.clip(id)) else { return };
        let times: Vec<Time> = anim_ui::visible_keys(c).into_iter().map(|(_, t)| t).collect();
        if let Some(t) = anim_ui::visible_neighbour(&times, self.playhead, anim_ui::half_frame(seq.frame_rate.frame_duration()), forward) {
            self.stop_playback();
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
            if asset.kind() != MediaKind::Audio && !kadr_timeline::scene::has_video_at(&self.project, self.project.sequence(), self.playhead) {
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
        // The inspector follows the playhead (keyframes spec §6.1): animated
        // values and ◆ states ~10 Hz while playing, at once when paused or
        // scrubbing; entering or leaving the clip (◆ enabled, hint) at once.
        let Some(c) = self.inspected().and_then(|id| seq.clip(id)) else { return };
        let outside = crate::anim_ui::edit_time(c, self.playhead, seq.frame_rate.frame_duration()).is_none();
        let crossed = ui.get_inspector().outside != outside;
        if c.animation.is_empty() && !crossed {
            return;
        }
        let due = crossed || !self.playing || self.inspector_refreshed.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(100));
        if due {
            self.inspector_refreshed = Some(std::time::Instant::now());
            self.refresh_inspector();
        }
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
        let (moving, move_delta, move_tracks): (HashSet<ClipId>, Time, i32) = match &self.tl.drag {
            Some(Drag::Move { clips, delta, track_delta, started: true, .. }) => {
                (kadr_timeline::commands::with_links(&seq, clips).into_iter().collect(), *delta, *track_delta)
            }
            _ => (HashSet::new(), Time::ZERO, 0),
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
                // The range shown, in time: a move or trim drag previews its own.
                let (mut tin, mut tout) = (c.timeline_in, c.timeline_out);
                let mut row = r;
                let moved = moving.contains(&c.id);
                if moved {
                    tin += move_delta;
                    tout += move_delta;
                    if primary.contains(&c.id) && move_tracks != 0 {
                        // Show on the destination row of the same kind.
                        let same: Vec<&Row> = rs.iter().filter(|x| x.kind == r.kind).collect();
                        let pos = same.iter().position(|x| x.track == r.track).unwrap_or(0) as i32;
                        let step = if r.kind == TrackKind::Video { -move_tracks } else { move_tracks };
                        let np = (pos + step).clamp(0, same.len() as i32 - 1) as usize;
                        row = same[np];
                    }
                }
                if let Some((group, edge, d)) = &trim
                    && group.contains(&c.id)
                {
                    match edge {
                        TrimEdge::Start => tin += *d,
                        TrimEdge::End => tout += *d,
                    }
                }
                let (cin, cout) = (tin.as_secs_f64(), tout.as_secs_f64());
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
                let fades = clip_fades(c, is_audio);
                let (show_in, show_out) = self.tl.fade_handles(c.id, sel.contains(&c.id), t.locked, width, fades);
                let (fi_px, fo_px) = (fades.0.as_secs_f64() * self.tl.pps, fades.1.as_secs_f64() * self.tl.pps);
                // Keyframe diamonds: the visible moments (clusters merge on
                // screen only); hidden ones show as ‹ ›. The same positions
                // as `hit` without a drag; a move or trim drag previews them.
                let (keys, hidden_left, hidden_right) = if is_audio || c.animation.is_empty() {
                    (vec![], false, false)
                } else {
                    let shift = if moved { move_delta } else { Time::ZERO };
                    let (shown, before, after) = anim_ui::split_keys(c, shift, tin, tout);
                    let xs: Vec<f64> = shown.iter().map(|(_, kt)| (*kt - tin).as_secs_f64() * self.tl.pps).collect();
                    let marks = anim_ui::key_marks(&xs).into_iter().map(|(x, cluster)| KeyMark { x: x as f32, cluster }).collect();
                    (marks, before, after)
                };
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
                    fade_in: fi_px as f32,
                    fade_out: fo_px as f32,
                    show_fade_in_handle: show_in,
                    show_fade_out_handle: show_out,
                    fade_in_handle_x: anim_ui::handle_x(0.0, width, fi_px, FadeEdge::In) as f32,
                    fade_out_handle_x: anim_ui::handle_x(0.0, width, fo_px, FadeEdge::Out) as f32,
                    fade_handle_y: anim_ui::HANDLE_Y as f32,
                    offline: rt.is_some_and(|r| r.status == crate::app::AssetStatus::Missing),
                    marks: ModelRc::new(VecModel::from(marks)),
                    keys: ModelRc::new(VecModel::from(keys)),
                    key_y: (row.h - 4.0 - anim_ui::KEY_CY) as f32,
                    hidden_left,
                    hidden_right,
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

        crate::util::sync_rows(ui.get_tracks(), tracks, |m| ui.set_tracks(m));
        // Each clip keeps its previous marks and diamonds models (see `reuse_rows`).
        type Nested = (ModelRc<ClipMark>, ModelRc<KeyMark>);
        let old: std::collections::HashMap<slint::SharedString, Nested> = slint::Model::iter(&ui.get_clips()).map(|c| (c.id, (c.marks, c.keys))).collect();
        let clips: Vec<ClipView> = clips
            .into_iter()
            .map(|mut c| {
                let (marks, keys) = old.get(&c.id).cloned().unzip();
                c.marks = crate::util::reuse_rows(marks, slint::Model::iter(&c.marks).collect());
                c.keys = crate::util::reuse_rows(keys, slint::Model::iter(&c.keys).collect());
                c
            })
            .collect();
        crate::util::sync_rows_by_key(ui.get_clips(), clips, |c| c.id.clone(), |m| ui.set_clips(m));
        crate::util::sync_rows(ui.get_ticks(), ticks, |m| ui.set_ticks(m));
        crate::util::sync_rows(ui.get_markers(), markers, |m| ui.set_markers(m));
        crate::util::sync_rows(ui.get_transitions(), transitions, |m| ui.set_transitions(m));
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

    /// Spec §6.2: a handle's grab zone (≥ 10 px, asserted at compile time
    /// above) never reaches into a trim edge's zone — on the narrowest clip
    /// with handles too (a small window), whatever the fades.
    #[test]
    fn fade_handle_zones_stay_off_the_trim_zones() {
        for (x0, x1) in [(100.0, 100.0 + anim_ui::FADE_MIN_W), (100.0, 400.0)] {
            let w: f64 = x1 - x0;
            for (fi, fo) in [(0.0, 0.0), (3.0, 5.0), (w / 2.0, w / 2.0), (w, 0.0), (0.0, w), (w * 0.8, w * 0.1)] {
                let mut x = x0 - 2.0 * TRIM_ZONE;
                while x <= x1 + 2.0 * TRIM_ZONE {
                    if anim_ui::pick_handle(x, x0, x1, fi, fo, (true, true)).is_some() {
                        assert!((x - x0).abs() >= TRIM_ZONE && (x1 - x).abs() >= TRIM_ZONE, "x {x} on [{x0}, {x1}] with fades ({fi}, {fo})");
                    }
                    x += 0.25;
                }
            }
        }
    }

    /// Video tracks draw (and drag) the picture's fades, audio tracks the
    /// sound's — as before for audio.
    #[test]
    fn each_track_kind_shows_and_sets_its_own_fades() {
        let mut c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::ZERO);
        c.video_fade.fade_in = Time::from_secs(1);
        c.audio.fade_out = Time::from_secs(2);
        assert_eq!(clip_fades(&c, false), (Time::from_secs(1), Time::ZERO));
        assert_eq!(clip_fades(&c, true), (Time::ZERO, Time::from_secs(2)));
        *fade_slot(&mut c, true, FadeEdge::In) = Time::from_secs(3);
        *fade_slot(&mut c, false, FadeEdge::Out) = Time::from_secs(4);
        assert_eq!((c.audio.fade_in, c.video_fade.fade_out), (Time::from_secs(3), Time::from_secs(4)));
        assert_eq!((c.video_fade.fade_in, c.audio.fade_out), (Time::from_secs(1), Time::from_secs(2)), "the others stay");
    }

    /// Handles show on a wide enough, unlocked clip outside blade mode:
    /// each with its own fade (R18), both while selected or hovered.
    #[test]
    fn fade_handles_show_when_they_can_be_used() {
        let mut tl = TimelineUi::default();
        let id = ClipId::new();
        let none = (Time::ZERO, Time::ZERO);
        let faded_out = (Time::ZERO, Time::from_secs(1));
        assert_eq!(tl.fade_handles(id, false, false, 100.0, none), (false, false), "idle clip without fades");
        assert_eq!(tl.fade_handles(id, false, false, 100.0, faded_out), (false, true), "only the set fade's handle stays");
        assert_eq!(tl.fade_handles(id, true, false, 100.0, none), (true, true), "selected");
        assert_eq!(tl.fade_handles(id, true, false, anim_ui::FADE_MIN_W - 1.0, faded_out), (false, false), "too narrow");
        assert_eq!(tl.fade_handles(id, true, true, 100.0, faded_out), (false, false), "locked track");
        tl.hover_clip = Some(id);
        assert_eq!(tl.fade_handles(id, false, false, 100.0, none), (true, true), "hovered");
        tl.blade = true;
        assert_eq!(tl.fade_handles(id, true, false, 100.0, faded_out), (false, false), "the blade cuts, handles would mislead");
    }
}
