//! Editing on the preview (titles spec §1, §6.2): the selected clip's frame
//! (a text clip's block — `kadr_text::frame_box`, tight round curved or
//! tilted text, TX-R15 — any other clip's crop) with four corner handles
//! and a rotation handle; a drag moves (snapping to the canvas centre and
//! edges, the 90 % / 93 % safe areas and other texts' blocks; Shift frees
//! it), scales about the centre or rotates (15° steps), live, and is one
//! undo entry on release; a click selects the topmost clip under the
//! pointer; a double-click on a text types into it on the frame. Geometry
//! here is pure, in canvas px.

use crate::app::App;
use kadr_core::{ClipId, Time};
use kadr_i18n::t;
use kadr_project::{Clip, SourceTime};
use kadr_scene::{FrameScene, Layer, LayerContent, LayerId, RectF};
use kadr_text::Bounds;
use kadr_timeline::{ClipProperty, EditCommand};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Top-left, top-right, bottom-right, bottom-left of the box, canvas px.
    pub corners: [(f64, f64); 4],
    /// The point scaling and rotation turn about: the placement's position.
    pub centre: (f64, f64),
    /// The rotation handle.
    pub rotate: (f64, f64),
}

impl Frame {
    /// Inside the (convex) box.
    pub fn contains(&self, p: (f64, f64)) -> bool {
        let c = &self.corners;
        let side = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let s: Vec<f64> = (0..4).map(|i| side(c[i], c[(i + 1) % 4])).collect();
        s.iter().all(|v| *v >= 0.0) || s.iter().all(|v| *v <= 0.0)
    }

    /// Axis-aligned bounds `[x0, y0, x1, y1]`.
    pub fn bounds(&self) -> [f64; 4] {
        let xs = self.corners.map(|c| c.0);
        let ys = self.corners.map(|c| c.1);
        [xs.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max), ys.iter().cloned().fold(f64::MIN, f64::max)]
    }

    /// The middle of the top edge.
    pub fn top(&self) -> (f64, f64) {
        let c = &self.corners;
        ((c[0].0 + c[1].0) / 2.0, (c[0].1 + c[1].1) / 2.0)
    }

    /// The unit vector from the bottom edge's middle to the top's: the
    /// rotation handle stands off the top edge along it.
    pub fn up(&self) -> (f64, f64) {
        let c = &self.corners;
        let top = self.top();
        let bottom = ((c[2].0 + c[3].0) / 2.0, (c[2].1 + c[3].1) / 2.0);
        let (ux, uy) = (top.0 - bottom.0, top.1 - bottom.1);
        let len = ux.hypot(uy);
        if len > 1e-9 { (ux / len, uy / len) } else { (0.0, -1.0) }
    }
}

/// The box the handles frame, in the layer's local px: a text layer's
/// block (`kadr_text::frame_box`: canvas px about the layout centre, which
/// is the layer's anchor — titles refinement 2, TX-R7) placed at the
/// anchor, so a curved or tilted text is framed round its real block
/// (TX-R15); any other layer's crop.
pub fn local_box(layer: &Layer, block: Option<Bounds>) -> RectF {
    match block {
        Some(b) => {
            let (ax, ay) = (layer.placement.anchor.x * layer.placement.size.x, layer.placement.anchor.y * layer.placement.size.y);
            RectF::new(ax + b.x0, ay + b.y0, ax + b.x1, ay + b.y1)
        }
        None => layer.crop,
    }
}

/// The layer's frame on the canvas; the rotation handle `gap` canvas px
/// beyond the top edge's middle, away from the centre.
pub fn frame_of(layer: &Layer, block: Option<Bounds>, gap: f64) -> Frame {
    let b = local_box(layer, block);
    let m = layer.placement.to_canvas();
    let corners = [(b.x0, b.y0), (b.x1, b.y0), (b.x1, b.y1), (b.x0, b.y1)].map(|(x, y)| m.apply(x as f64, y as f64));
    let centre = (layer.placement.position.x as f64, layer.placement.position.y as f64);
    let mut f = Frame { corners, centre, rotate: (0.0, 0.0) };
    let (top, up) = (f.top(), f.up());
    f.rotate = (top.0 + up.0 * gap, top.1 + up.1 * gap);
    f
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grab {
    Move,
    Corner(usize),
    Rotate,
}

pub fn grab_at(f: &Frame, p: (f64, f64), tol: f64) -> Option<Grab> {
    let d = |q: (f64, f64)| (q.0 - p.0).hypot(q.1 - p.1);
    if d(f.rotate) <= tol {
        return Some(Grab::Rotate);
    }
    if let Some(i) = (0..4).find(|&i| d(f.corners[i]) <= tol) {
        return Some(Grab::Corner(i));
    }
    f.contains(p).then_some(Grab::Move)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Guide {
    /// A vertical line at x (canvas px).
    V(f64),
    /// A horizontal line at y.
    H(f64),
}

/// The shift that snaps a moved box (`[x0, y0, x1, y1]`) — its edges or
/// centre — to the nearest line within `tol`: the canvas centre and edges,
/// the lines of the 90 % and 93 % safe areas (the preview's overlay),
/// other boxes' edges and centres; each axis on its own. With the guide
/// lines hit.
pub fn snap_move(b: [f64; 4], canvas: (f64, f64), others: &[[f64; 4]], tol: f64) -> ((f64, f64), Vec<Guide>) {
    let lines = |len: f64, pick: fn(&[f64; 4]) -> [f64; 3]| {
        let mut v = vec![0.0, len / 2.0, len, len * 0.05, len * 0.95, len * 0.035, len * 0.965];
        for o in others {
            v.extend(pick(o));
        }
        v
    };
    let axis = |feats: [f64; 3], lines: Vec<f64>| -> Option<(f64, f64)> {
        let mut best: Option<(f64, f64)> = None;
        for f in feats {
            for &l in &lines {
                let d = l - f;
                if d.abs() <= tol && best.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    best = Some((d, l));
                }
            }
        }
        best
    };
    let xs = axis([b[0], (b[0] + b[2]) / 2.0, b[2]], lines(canvas.0, |o| [o[0], (o[0] + o[2]) / 2.0, o[2]]));
    let ys = axis([b[1], (b[1] + b[3]) / 2.0, b[3]], lines(canvas.1, |o| [o[1], (o[1] + o[3]) / 2.0, o[3]]));
    // Rounded to 1e-9 px: 1920 · 0.95 is 1824 on screen, whatever f64 makes of it.
    let round = |v: f64| (v * 1e9).round() / 1e9;
    let mut guides = vec![];
    if let Some((_, l)) = xs {
        guides.push(Guide::V(round(l)));
    }
    if let Some((_, l)) = ys {
        guides.push(Guide::H(round(l)));
    }
    ((round(xs.map_or(0.0, |s| s.0)), round(ys.map_or(0.0, |s| s.0))), guides)
}

/// Rotation lands on a multiple of 15° within 3°.
pub fn snap_angle(deg: f64) -> f64 {
    let k = (deg / 15.0).round() * 15.0;
    if (deg - k).abs() <= 3.0 { k } else { deg }
}

/// The same turn as `deg`, within (−180°, 180°] — the inspector's range.
fn wrap_degrees(deg: f64) -> f64 {
    let w = deg.rem_euclid(360.0);
    if w > 180.0 { w - 360.0 } else { w }
}

/// A drag's start: what was grabbed, where, the clip's values then.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragStart {
    pub grab: Grab,
    pub at: (f64, f64),
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub rotation: f64,
    pub frame: Frame,
}

/// The values a drag to `p` gives: `x`, `y` (moved, snapped unless `free`),
/// `scale` (about the centre, ≥ 0.01) or `rotation` (degrees, clockwise,
/// within ±180°, 15° steps unless `free`); with the guides to draw.
pub fn drag_to(s: &DragStart, p: (f64, f64), canvas: (f64, f64), others: &[[f64; 4]], tol: f64, free: bool) -> (Vec<(&'static str, f64)>, Vec<Guide>) {
    let c = s.frame.centre;
    match s.grab {
        Grab::Move => {
            let (mut dx, mut dy) = (p.0 - s.at.0, p.1 - s.at.1);
            let mut guides = vec![];
            if !free {
                let b = s.frame.bounds();
                let ((sx, sy), g) = snap_move([b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy], canvas, others, tol);
                (dx, dy, guides) = (dx + sx, dy + sy, g);
            }
            (vec![("x", s.x + dx), ("y", s.y + dy)], guides)
        }
        Grab::Corner(_) => {
            let r0 = (s.at.0 - c.0).hypot(s.at.1 - c.1).max(1e-6);
            let r1 = (p.0 - c.0).hypot(p.1 - c.1);
            (vec![("scale", (s.scale * r1 / r0).max(0.01))], vec![])
        }
        Grab::Rotate => {
            let a0 = (s.at.1 - c.1).atan2(s.at.0 - c.0);
            let a1 = (p.1 - c.1).atan2(p.0 - c.0);
            // Wrapped: crossing atan2's seam is a small turn, never a 360° jump of the value.
            let deg = wrap_degrees(s.rotation + (a1 - a0).to_degrees());
            (vec![("rotation", if free { deg } else { wrap_degrees(snap_angle(deg)) })], vec![])
        }
    }
}

/// The one command a drag records (titles spec §1, Review Focus 4): an
/// animated property gets its keyframe at the playhead (none when the
/// playhead is off the clip), the static ones one `SetClipProperty`; all in
/// one batch. `None` when nothing would change.
pub fn transform_command(c: &Clip, changes: &[(&str, f64)], playhead: Time, frame: Time, label: String) -> Option<EditCommand> {
    let mut cmds = vec![];
    let mut statics = c.clone();
    for &(prop, v) in changes {
        let k = crate::anim_ui::track_of(prop)?;
        if c.animation.is_animated(&k) {
            let tt = crate::anim_ui::edit_time(c, playhead, frame)?;
            let here = crate::anim_ui::key_here(c, &k, playhead, frame);
            if here.is_some_and(|key| key.value == v) {
                continue;
            }
            let at = here.map_or(SourceTime(c.source_time_at(tt)), |key| key.at);
            cmds.push(EditCommand::SetKeyframe { clip: c.id, param: k, at, value: v, ease: None });
        } else {
            crate::inspector::apply(&mut statics, prop, v, frame);
        }
    }
    if statics.transform != c.transform {
        cmds.push(EditCommand::SetClipProperty { clip: c.id, prop: ClipProperty::Transform(statics.transform) });
    }
    (!cmds.is_empty()).then_some(EditCommand::Batch { label, commands: cmds })
}

/// A drag in progress on the preview.
struct Drag {
    clip: ClipId,
    start: DragStart,
    /// Past the click threshold: the release records it.
    moved: bool,
    /// The sequence's canvas, and the other texts' blocks it snaps to
    /// (they stay put while it moves).
    canvas: (f64, f64),
    others: Vec<[f64; 4]>,
}

/// The preview editing state: a drag in progress and the clip being typed into.
#[derive(Default)]
pub struct PreviewEdit {
    drag: Option<Drag>,
    /// The values the drag last showed: what its release records.
    last: Vec<(&'static str, f64)>,
    /// The title being typed into on the frame.
    pub typing: Option<ClipId>,
    /// The clip whose frame was shown last: a selection change between
    /// frames (a click on the timeline) moves the frame.
    framed: Option<ClipId>,
}

impl PreviewEdit {
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

/// Handle and guide tolerances, the rotation handle's distance and the
/// click-or-drag threshold, in view px.
const GRAB_PX: f64 = 8.0;
const SNAP_PX: f64 = 6.0;
const ROTATE_GAP_PX: f64 = 24.0;
const DRAG_PX: f64 = 3.0;

/// The scene's layers with a transition's two sides in place of it, bottom to top.
fn flat_layers<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>) {
    for l in layers {
        match &l.content {
            LayerContent::Transition(t) => {
                flat_layers(&t.from, out);
                flat_layers(&t.to, out);
            }
            _ => out.push(l),
        }
    }
}

impl App {
    /// The block a text clip's handles frame (`None` for media): its tight
    /// box, `kadr_text::frame_box` (TX-R15).
    fn text_block(&self, id: ClipId) -> Option<Bounds> {
        Some(kadr_text::frame_box(self.project.sequence().clip(id)?.text.as_deref()?))
    }

    /// The scene the preview shows at the playhead: its own snapshot, so
    /// «До» and a hovered effect card count (like an effect's point).
    fn scene_now(&mut self) -> FrameScene {
        let scenes = self.preview_scenes();
        let seq = self.project.sequence();
        let t = self.playhead.min((seq.duration() - seq.frame_rate.frame_duration()).max(Time::ZERO));
        kadr_playback::SceneSource::scene_at(&*scenes, t, &self.preview.output(&self.project))
    }

    /// Frames of the scene's clip layers (also inside a transition),
    /// topmost first, with whether each is a text.
    fn frames(&self, scene: &FrameScene, px: f64) -> Vec<(ClipId, Frame, bool)> {
        let seq = self.project.sequence();
        let mut layers = vec![];
        flat_layers(&scene.layers, &mut layers);
        layers
            .into_iter()
            .rev()
            .filter_map(|l| {
                let c = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| LayerId::from(c.id) == l.id)?;
                let is_text = matches!(l.content, LayerContent::Text(_));
                Some((c.id, frame_of(l, if is_text { self.text_block(c.id) } else { None }, ROTATE_GAP_PX * px), is_text))
            })
            .collect()
    }

    fn track_locked(&self, id: ClipId) -> bool {
        let seq = self.project.sequence();
        seq.locate_clip(id).is_some_and(|(ti, _)| seq.tracks[ti].locked)
    }

    /// The selected clip's frame and handles on the preview (a locked
    /// track's clip: the frame, no handles); while typing on the frame,
    /// the field over the text instead. `scene`: the frame shown, when the
    /// caller has evaluated it (`request_frame`).
    pub fn refresh_preview_frame(&mut self, scene: Option<&FrameScene>) {
        let sel = self.inspected();
        self.preview_edit.framed = sel;
        if self.preview.cpu.is_none() {
            return self.ui().set_frame_shown(false); // no preview (no FFmpeg): nothing to frame
        }
        let own;
        let scene = match scene {
            Some(s) => s,
            None => {
                own = self.scene_now();
                &own
            }
        };
        let ui = self.ui();
        if let Some(id) = self.preview_edit.typing {
            ui.set_frame_shown(false);
            self.place_inline_field(scene, id);
            return;
        }
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let shown = sel.and_then(|id| {
            let l = crate::effects_ui::layer_of(&scene.layers, id)?;
            let block = if matches!(l.content, LayerContent::Text(_)) { self.text_block(id) } else { None };
            Some((id, frame_of(l, block, 0.0)))
        });
        let Some((id, f)) = shown else {
            ui.set_frame_shown(false);
            return;
        };
        let k = |x: f64, len: f64| (x / len * 1000.0) as f32;
        let c = &f.corners;
        let path = format!("M {} {} L {} {} L {} {} L {} {} Z", k(c[0].0, cw), k(c[0].1, ch), k(c[1].0, cw), k(c[1].1, ch), k(c[2].0, cw), k(c[2].1, ch), k(c[3].0, cw), k(c[3].1, ch));
        ui.set_frame_path(path.into());
        let mut rows: Vec<crate::HandleView> = vec![];
        if !self.track_locked(id) {
            rows = c.iter().map(|p| crate::HandleView { x: (p.0 / cw) as f32, y: (p.1 / ch) as f32, rotate: false, dx: 0.0, dy: 0.0 }).collect();
            // The rotation handle: ROTATE_GAP_PX off the top edge's middle, at any zoom.
            let (top, up) = (f.top(), f.up());
            rows.push(crate::HandleView { x: (top.0 / cw) as f32, y: (top.1 / ch) as f32, rotate: true, dx: up.0 as f32, dy: up.1 as f32 });
        }
        ui.set_handles(crate::util::reuse_rows(Some(ui.get_handles()), rows));
        ui.set_frame_shown(true);
    }

    /// After the inspector refresh (a selection change, an edit): typing on
    /// the frame ends when its title is no longer the one inspected, and
    /// the frame follows a selection made between frames.
    pub fn preview_frame_follow(&mut self) {
        if let Some(id) = self.preview_edit.typing
            && (self.inspected() != Some(id) || self.project.sequence().clip(id).is_none())
        {
            return self.finish_inline_typing();
        }
        if !self.playing && self.preview_edit.framed != self.inspected() {
            self.refresh_preview_frame(None);
        }
    }

    /// Typing ends and the keyboard goes back to the editor (TX-R20): the
    /// field on the frame closes, the inspector's field lets go, the
    /// session is recorded. A press on the preview, a modal opening.
    pub fn give_keyboard_to_editor(&mut self) {
        self.finish_inline_typing();
        self.text_field_focused = false;
        self.commit_text_session();
        self.ui().invoke_focus_editor();
    }

    /// A pointer event on the preview (canvas fractions; `view_w` = the
    /// canvas's width in view px). 0 down, 1 move, 2 up (or cancel).
    pub fn preview_pointer(&mut self, kind: i32, fx: f32, fy: f32, shift: bool, view_w: f32) {
        if self.playing {
            return; // no editing while playing (the overlay is hidden too)
        }
        if self.preview_edit.typing.is_some() {
            if kind == 0 {
                self.finish_inline_typing(); // a click outside the field
            }
            return;
        }
        match kind {
            0 => self.preview_press(fx, fy, view_w),
            1 => self.preview_drag(fx, fy, shift, view_w),
            _ => self.finish_preview_drag(),
        }
    }

    fn preview_press(&mut self, fx: f32, fy: f32, view_w: f32) {
        // Typing is recorded and the keys are the editor's again (TX-R20):
        // Space, S or M after a click here never go into a title.
        self.give_keyboard_to_editor();
        self.preview_edit.drag = None;
        let scene = self.scene_now();
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let px = cw / (view_w.max(1.0) as f64); // canvas px per view px
        let p = (fx as f64 * cw, fy as f64 * ch);
        let frames = self.frames(&scene, px);
        let sel = self.inspected();
        let hit = frames.iter().find(|(id, f, _)| Some(*id) == sel && grab_at(f, p, GRAB_PX * px).is_some()).or_else(|| frames.iter().find(|(_, f, _)| f.contains(p)));
        let Some(&(id, frame, _)) = hit else { return }; // empty canvas: the selection stays
        if Some(id) != sel {
            self.tl.selection = vec![id];
            self.refresh_timeline();
            self.refresh_inspector();
            self.refresh_status();
        }
        if self.track_locked(id) {
            return; // selected; a locked track's clip does not move
        }
        let grab = if Some(id) == sel { grab_at(&frame, p, GRAB_PX * px).unwrap_or(Grab::Move) } else { Grab::Move };
        let Some(c) = self.project.sequence().clip(id) else { return };
        let frame_t = self.project.sequence().frame_rate.frame_duration();
        let at = crate::anim_ui::shown_time(c, self.playhead, frame_t);
        use kadr_project::AnimParam as P;
        let v = |q| c.anim_value(q, at);
        let start = DragStart { grab, at: p, x: v(P::X), y: v(P::Y), scale: v(P::Scale), rotation: v(P::Rotation), frame };
        let others = frames.iter().filter(|(o, _, text)| *o != id && *text).map(|(_, f, _)| f.bounds()).collect();
        self.preview_edit.drag = Some(Drag { clip: id, start, moved: false, canvas: (cw, ch), others });
        self.preview_edit.last.clear();
    }

    fn preview_drag(&mut self, fx: f32, fy: f32, shift: bool, view_w: f32) {
        let Some(d) = self.preview_edit.drag.as_mut() else { return };
        let (cw, ch) = d.canvas;
        let px = cw / (view_w.max(1.0) as f64);
        let p = (fx as f64 * cw, fy as f64 * ch);
        // A click that wobbles a pixel is still a click (Review Focus 4).
        if !d.moved && (p.0 - d.start.at.0).hypot(p.1 - d.start.at.1) < DRAG_PX * px {
            return;
        }
        d.moved = true;
        let (values, guides) = drag_to(&d.start, p, d.canvas, &d.others, SNAP_PX * px, shift);
        let id = d.clip;
        self.preview_live(id, &values);
        let ui = self.ui();
        let rows: Vec<crate::GuideView> = guides
            .iter()
            .map(|g| match g {
                Guide::V(x) => crate::GuideView { vertical: true, at: (x / cw) as f32 },
                Guide::H(y) => crate::GuideView { vertical: false, at: (y / ch) as f32 },
            })
            .collect();
        ui.set_guides(crate::util::reuse_rows(Some(ui.get_guides()), rows));
    }

    /// The drag ends (release, cancel, playback starting): the values it
    /// last showed become one undo entry; a click records nothing.
    pub fn finish_preview_drag(&mut self) {
        let ui = self.ui();
        ui.set_guides(crate::util::reuse_rows(Some(ui.get_guides()), vec![]));
        let Some(d) = self.preview_edit.drag.take() else { return };
        let last = std::mem::take(&mut self.preview_edit.last);
        let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
        if !d.moved || last.is_empty() {
            if rolled_back {
                self.show_rolled_back();
            }
            return; // a click: selection only
        }
        let label = t(match d.start.grab {
            Grab::Move => "cmd.preview_move",
            Grab::Corner(_) => "cmd.preview_scale",
            Grab::Rotate => "cmd.preview_rotate",
        });
        let frame_t = self.project.sequence().frame_rate.frame_duration();
        let cmd = self.project.sequence().clip(d.clip).and_then(|c| transform_command(c, &last, self.playhead, frame_t, label));
        match cmd {
            Some(cmd) => {
                if !self.execute(cmd) {
                    self.show_rolled_back();
                }
            }
            None => self.show_rolled_back(),
        }
    }

    /// The drag's values, live (no history): like an inspector slider.
    fn preview_live(&mut self, id: ClipId, values: &[(&'static str, f64)]) {
        if self.track_locked(id) {
            return;
        }
        // Typing is recorded before the drag's pre-drag state is taken, so
        // the typing never rides inside the drag's entry.
        self.commit_text_session();
        let Some((ti, ci)) = self.project.sequence().locate_clip(id) else { return };
        self.live_edit.arm(self.project.sequence_mut(), id);
        let frame = self.project.sequence().frame_rate.frame_duration();
        let playhead = self.playhead;
        let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
        for &(prop, v) in values {
            match crate::anim_ui::track_of(prop) {
                Some(k) if c.animation.is_animated(&k) => {
                    let Some(tt) = crate::anim_ui::edit_time(c, playhead, frame) else { continue };
                    let at = crate::anim_ui::key_here(c, &k, playhead, frame).map_or(SourceTime(c.source_time_at(tt)), |key| key.at);
                    c.animation.set_key(&k, at, v, None);
                }
                _ => crate::inspector::apply(c, prop, v, frame),
            }
        }
        self.preview_edit.last = values.to_vec();
        self.preview.invalidate();
        self.refresh_inspector();
        self.request_frame();
    }

    /// A double-click: on a text clip, type into it on the frame.
    pub fn preview_double(&mut self, fx: f32, fy: f32, view_w: f32) {
        if self.playing {
            return;
        }
        let scene = self.scene_now();
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let p = (fx as f64 * cw, fy as f64 * ch);
        let px = cw / (view_w.max(1.0) as f64);
        if let Some((id, _, _)) = self.frames(&scene, px).into_iter().find(|(_, f, text)| *text && f.contains(p)) {
            self.begin_inline_typing(id);
        }
    }

    /// Opens the field over the text block (its axis-aligned bounds), the
    /// whole text selected: typing replaces it. Playback pauses first; a
    /// locked track's title is not typed into.
    pub fn begin_inline_typing(&mut self, id: ClipId) {
        self.stop_playback();
        if self.track_locked(id) {
            return;
        }
        let Some(plain) = self.project.sequence().clip(id).and_then(|c| c.text.as_deref()).map(|d| d.plain()) else { return };
        // One session at a time, shared with the inspector's field: another
        // title's typing is recorded first.
        if self.text_session.clip().is_some_and(|c| c != id) {
            self.commit_text_session();
        }
        let scene = self.scene_now();
        if crate::effects_ui::layer_of(&scene.layers, id).is_none() {
            return; // not in the frame shown
        }
        // The title typed into is the one inspected (its frame, its field).
        if self.inspected() != Some(id) {
            self.tl.selection = vec![id];
            self.refresh_timeline();
            self.refresh_inspector();
            self.refresh_status();
        }
        self.preview_edit.drag = None;
        self.preview_edit.typing = Some(id);
        self.text_field_focused = false;
        let ui = self.ui();
        ui.set_type_text(plain.into());
        self.place_inline_field(&scene, id);
        ui.set_frame_shown(false);
        ui.set_typing(true);
        ui.invoke_begin_inline_typing();
    }

    /// The field over title `id`'s block (axis-aligned), in the text's size
    /// as shown; while no session runs (just recorded, or undone), it shows
    /// the title's text.
    fn place_inline_field(&self, scene: &FrameScene, id: ClipId) {
        let Some(l) = crate::effects_ui::layer_of(&scene.layers, id) else { return };
        let Some(doc) = self.project.sequence().clip(id).and_then(|c| c.text.as_deref()) else { return };
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let b = frame_of(l, self.text_block(id), 0.0).bounds();
        let ui = self.ui();
        ui.set_type_x((b[0] / cw) as f32);
        ui.set_type_y((b[1] / ch) as f32);
        ui.set_type_w(((b[2] - b[0]) / cw) as f32);
        ui.set_type_font((doc.style.size as f64 * l.placement.scale.x.abs() as f64 / ch).clamp(0.0, 0.2) as f32);
        if self.text_session.clip() != Some(id) {
            let plain = doc.plain();
            if ui.get_type_text().as_str() != plain {
                ui.set_type_text(plain.into());
            }
        }
    }

    /// The field on the frame changed: the same typing session as the
    /// inspector's field (one entry, the name rule TX-R9).
    pub fn inline_typed(&mut self, text: &str) {
        let Some(id) = self.preview_edit.typing else { return };
        if self.track_locked(id) {
            return;
        }
        if self.text_session.clip().is_some_and(|c| c != id) {
            self.commit_text_session();
        }
        if self.text_session.type_text(&mut self.project, id, text) {
            // The inspector's field shows what is typed here: the two never fork the session.
            let ui = self.ui();
            if self.inspected() == Some(id) && ui.get_text_field().as_str() != text {
                ui.set_text_field(text.into());
            }
            self.show_typed_name(id);
            self.preview.invalidate();
            self.refresh_timeline();
            self.request_frame();
        }
    }

    /// Esc, Ctrl+Enter, a click outside, a dialog: one entry for the typing
    /// (none without changes); the selection stays and the keyboard goes
    /// back to the editor.
    pub fn finish_inline_typing(&mut self) {
        if self.end_inline_typing() {
            self.ui().invoke_focus_editor();
        }
    }

    /// The field lost the keyboard to something else (another text field,
    /// the timeline): the typing is recorded, the keyboard stays where it went.
    pub fn inline_typing_left(&mut self) {
        self.end_inline_typing();
    }

    /// Closes the field and records the typing; `false` when there was none.
    fn end_inline_typing(&mut self) -> bool {
        if self.preview_edit.typing.take().is_none() {
            return false;
        }
        self.ui().set_typing(false);
        self.commit_text_session();
        self.refresh_inspector();
        self.request_frame();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{AnimParam, TextDoc};
    use kadr_scene::{BlendMode, Placement, Vec2};

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn solid_layer(p: Placement) -> Layer {
        Layer { id: LayerId(1), content: LayerContent::Solid(kadr_scene::Rgba::BLACK), crop: p.full_crop(), placement: p, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
    }

    #[test]
    fn a_text_frame_is_its_layout_box_turned_and_scaled_about_the_anchor() {
        let p = Placement { size: Vec2::new(300.0, 120.0), anchor: Vec2::new(0.4, 0.5), position: Vec2::new(960.0, 540.0), scale: Vec2::new(2.0, 2.0), rotation: std::f32::consts::FRAC_PI_2 };
        let l = solid_layer(p);
        let block = Bounds { x0: -100.0, y0: -40.0, x1: 100.0, y1: 40.0 };
        assert_eq!(local_box(&l, Some(block)), RectF::new(20.0, 20.0, 220.0, 100.0), "the 200 × 80 block round the anchor (120, 60)");
        // A curved or tilted text's block is not symmetric about its anchor (TX-R15): placed as it is.
        assert_eq!(local_box(&l, Some(Bounds { x0: -50.0, y0: -10.0, x1: 150.0, y1: 70.0 })), RectF::new(70.0, 50.0, 270.0, 130.0));
        let f = frame_of(&l, Some(block), 24.0);
        assert!(near(f.centre, (960.0, 540.0)));
        // Turned 90° clockwise and doubled: the block's top-left (−100, −40) from the centre lands at (+80, −200).
        assert!(near(f.corners[0], (1040.0, 340.0)), "{:?}", f.corners[0]);
        assert!(near(f.corners[2], (880.0, 740.0)));
        assert!(f.contains((960.0, 540.0)) && !f.contains((960.0, 300.0)));
        let b = f.bounds();
        assert!(b.iter().zip([880.0, 340.0, 1040.0, 740.0]).all(|(a, e)| (a - e).abs() < 1e-3), "{b:?}");
        let media = frame_of(&solid_layer(Placement::fill(kadr_scene::SizeU::new(1920, 1080))), None, 24.0);
        assert!(near(media.corners[0], (0.0, 0.0)) && near(media.corners[2], (1920.0, 1080.0)), "a media layer is framed by its crop");
        assert!(near(media.rotate, (960.0, -24.0)), "the rotation handle sits above the top edge's middle");
        assert!(near(media.top(), (960.0, 0.0)) && near(media.up(), (0.0, -1.0)), "from the top edge's middle, away from the bottom edge");
        assert!(near(f.up(), (1.0, 0.0)), "turned 90° clockwise, the top faces right: {:?}", f.up());
    }

    #[test]
    fn grabbing_prefers_the_rotation_handle_then_a_corner_then_the_inside() {
        let f = frame_of(&solid_layer(Placement::fill(kadr_scene::SizeU::new(400, 200))), None, 24.0);
        assert_eq!(grab_at(&f, (200.0, -22.0), 8.0), Some(Grab::Rotate));
        assert_eq!(grab_at(&f, (396.0, 197.0), 8.0), Some(Grab::Corner(2)));
        assert_eq!(grab_at(&f, (100.0, 100.0), 8.0), Some(Grab::Move));
        assert_eq!(grab_at(&f, (500.0, 100.0), 8.0), None);
    }

    #[test]
    fn moves_snap_to_centre_edges_safe_areas_and_other_texts() {
        let canvas = (1920.0, 1080.0);
        // A 200 × 100 box whose centre is 4 px right of the canvas centre.
        let ((dx, dy), guides) = snap_move([864.0, 300.0, 1064.0, 400.0], canvas, &[], 6.0);
        assert_eq!((dx, dy), (-4.0, 0.0));
        assert_eq!(guides, [Guide::V(960.0)]);
        let ((dx, _), g) = snap_move([1720.0, 0.0, 1827.0, 50.0], canvas, &[], 6.0);
        assert_eq!((dx, g[0]), (-3.0, Guide::V(1824.0)), "the right edge to the 90 % safe area's line (1920 · 0.95)");
        let ((dx, _), g) = snap_move([1720.0, 200.0, 1856.0, 250.0], canvas, &[], 6.0);
        assert_eq!((dx, g[0]), (-3.2, Guide::V(1852.8)), "the right edge to the 93 % safe area's line (1920 · 0.965)");
        let ((_, dy), g) = snap_move([400.0, 497.0, 600.0, 597.0], canvas, &[[1000.0, 600.0, 1200.0, 700.0]], 6.0);
        assert_eq!((dy, g[0]), (3.0, Guide::H(600.0)), "the bottom edge to another text's top");
        let ((dx, dy), g) = snap_move([300.0, 300.0, 400.0, 400.0], canvas, &[], 6.0);
        assert!(dx == 0.0 && dy == 0.0 && g.is_empty(), "nothing within reach: no snap");
    }

    #[test]
    fn corner_drags_scale_about_the_centre_and_rotation_snaps_to_15_degrees() {
        let f = frame_of(&solid_layer(Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(500.0, 500.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 }), None, 24.0);
        let start = DragStart { grab: Grab::Corner(2), at: (600.0, 550.0), x: 0.0, y: 0.0, scale: 1.0, rotation: 0.0, frame: f };
        let (v, _) = drag_to(&start, (700.0, 600.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, "scale");
        assert!((v[0].1 - 2.0).abs() < 1e-9, "twice as far from the centre: twice the scale");
        let rot = DragStart { grab: Grab::Rotate, at: (500.0, 400.0), ..start };
        let (v, _) = drag_to(&rot, (600.0, 503.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v, [("rotation", 90.0)], "91.7° snaps to 90°");
        let (v, _) = drag_to(&rot, (600.0, 503.0), (1920.0, 1080.0), &[], 6.0, true);
        assert!((v[0].1 - 91.7184).abs() < 1e-3, "Shift: free, {}", v[0].1);
        assert_eq!(snap_angle(46.0), 45.0);
        assert_eq!(snap_angle(52.0), 52.0);
        // Across atan2's seam (left of the centre) a small turn stays small,
        // and the angle stays within the inspector's ±180°.
        let seam = DragStart { rotation: 170.0, at: (400.0, 501.0), ..rot };
        let (v, _) = drag_to(&seam, (400.0, 499.0), (1920.0, 1080.0), &[], 6.0, true);
        assert!((v[0].1 - 171.146).abs() < 1e-2, "1.15° on, not 358.85° back: {}", v[0].1);
        let (v, _) = drag_to(&DragStart { rotation: 179.0, ..seam }, (400.0, 499.0), (1920.0, 1080.0), &[], 6.0, true);
        assert!((v[0].1 - (-179.854)).abs() < 1e-2, "past 180° it reads −179.85°: {}", v[0].1);
        let (v, _) = drag_to(&DragStart { rotation: -170.0, ..rot }, (500.0, 400.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v, [("rotation", -170.0)], "−170° is not a 15° step and stays");
        let mv = DragStart { grab: Grab::Move, at: (500.0, 500.0), x: 10.0, y: 20.0, ..start };
        let (v, guides) = drag_to(&mv, (958.0, 500.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v, [("x", 470.0), ("y", 20.0)], "the box centre lands on the canvas centre (dx 458 + 2)");
        assert_eq!(guides, [Guide::V(960.0)]);
    }

    #[test]
    fn a_drag_is_one_entry_and_a_click_is_none() {
        let mut c = Clip::new_text(TextDoc::new("x"), Time::ZERO, Time::from_secs(5));
        let frame = Time::from_millis(40);
        assert!(transform_command(&c, &[("x", 0.0), ("y", 0.0)], Time::from_secs(1), frame, "m".into()).is_none(), "released where it started: nothing");
        let cmd = transform_command(&c, &[("x", 30.0), ("y", -5.0)], Time::from_secs(1), frame, "m".into()).unwrap();
        let EditCommand::Batch { commands, .. } = &cmd else { panic!() };
        assert_eq!(commands.len(), 1, "both static values in one SetClipProperty");
        c.animation.set_key(AnimParam::X, SourceTime(Time::ZERO), 0.0, None);
        let EditCommand::Batch { commands, .. } = transform_command(&c, &[("x", 30.0), ("y", -5.0)], Time::from_secs(1), frame, "m".into()).unwrap() else { panic!() };
        assert!(matches!(commands[0], EditCommand::SetKeyframe { .. }) && matches!(commands[1], EditCommand::SetClipProperty { .. }), "an animated x gets a keyframe at the playhead, y stays static — one entry");
        assert!(transform_command(&c, &[("x", 30.0)], Time::from_secs(9), frame, "m".into()).is_none(), "the playhead off the clip: no keyframe there");
    }
}
