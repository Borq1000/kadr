//! The «Эффекты» tab (effects spec §6.1): categories, search, cards.
//! Hovering a card shows its effect on the selected clips without touching
//! the project (`PreviewOverride`, applied to the preview's snapshot only);
//! a click applies it to them, a drag to the one clip under the pointer.
//! Also the inspector's «Эффекты клипа» (§6.2): one block per effect.
//! Pure parts are free functions, tested without a window.

use crate::anim_ui;
use crate::app::App;
use crate::timeline_ui::{rows, Row};
use crate::{FxCardView, FxCategoryView, FxParamRow, FxRow, KeyState};
use kadr_core::{ClipId, EffectId, FrameRate, Time, Timecode};
use kadr_effects::{Category, ParamDef, ParamKind, RecipeDef};
use kadr_i18n::{t, t_in, tf, Lang};
use kadr_project::{AppliedEffect, Clip, FadeEdge, ParamValue, Sequence, SourceTime, TrackKey, TrackKind};
use kadr_scene::{FrameScene, Layer, LayerContent, LayerId, SizeU};
use kadr_timeline::EditCommand;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// A press that moves less than this (window pixels) is a click.
const DRAG_PX: f64 = 5.0;
/// Hover this long before the preview shows the card's effect (spec §6.1).
pub const HOVER_DELAY: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct FxUi {
    /// The category chip (`""` = all).
    pub category: String,
    pub search: String,
    /// The card under the pointer and the generation of that hover: a newer
    /// hover makes an older delayed preview stale.
    pub hover: Option<(&'static str, u64)>,
    hover_gen: u64,
    /// The hovered card's effect on the selected clips — app state only.
    pub preview: Option<PreviewOverride>,
    pub drag: Option<FxDrag>,
    /// The cards' pictures (spec §6.1): cache, the batch in flight, the hover loop.
    pub thumbs: crate::fx_thumbs::Thumbs,
    /// The effect point being picked on the preview.
    pub picking: Option<Pick>,
    /// The effect last used in the inspector: its point shows on the preview
    /// (refinement 10).
    pub focus: Option<EffectId>,
    /// The clip whose badge tooltip shows.
    pub tip_clip: Option<ClipId>,
}

pub struct FxDrag {
    recipe: &'static str,
    start: (f64, f64),
    active: bool,
}

/// An effect shown on clips by the preview only (spec §6.1).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewOverride {
    pub recipe: &'static str,
    /// The playhead the instances were made at (a «Момент» is its frame).
    pub at: Time,
    pub effects: Vec<(ClipId, AppliedEffect)>,
}

/// `def` at «Обычно» on `clips`, made at `at` as a click there would make
/// it; `None` on none. A clip `keep` shows the recipe on already keeps that
/// instance's id: Handheld's shake is seeded by it, so the picture does not
/// jump when the override is made again.
pub fn make_override(seq: &Sequence, def: &'static RecipeDef, clips: &[ClipId], at: Time, keep: Option<&PreviewOverride>) -> Option<PreviewOverride> {
    let kept = |id: ClipId| keep.filter(|o| o.recipe == def.id).and_then(|o| o.effects.iter().find(|(c, _)| *c == id)).map(|(_, e)| e.id);
    let effects: Vec<(ClipId, AppliedEffect)> = clips
        .iter()
        .filter_map(|id| {
            let mut e = kadr_effects::fresh(def, seq.clip(*id)?, at);
            if let Some(same) = kept(*id) {
                e.id = same;
            }
            Some((*id, e))
        })
        .collect();
    (!effects.is_empty()).then_some(PreviewOverride { recipe: def.id, at, effects })
}

impl PreviewOverride {
    /// Whether it no longer shows what a click would apply: the clips it
    /// goes on are others now, or the playhead moved and `def` has a
    /// «Момент» (made at the old playhead, a punch would land there).
    pub fn outdated(&self, def: &RecipeDef, targets: &[ClipId], playhead: Time) -> bool {
        self.clips() != targets || (self.at != playhead && def.params.iter().any(|p| p.kind == ParamKind::Moment))
    }

    /// The effects put on a copy of the sequence (the preview's snapshot),
    /// as applying them would.
    pub fn apply(&self, seq: &mut Sequence) {
        for (id, e) in &self.effects {
            if let Some((ti, ci)) = seq.locate_clip(*id) {
                seq.tracks[ti].clips[ci].put_effect(e.clone());
            }
        }
    }

    pub fn clips(&self) -> Vec<ClipId> {
        self.effects.iter().map(|(c, _)| *c).collect()
    }
}

/// The UI name of an effect: its recipe's name, or the raw recipe id of an
/// effect from a newer Kadr.
pub fn fx_name(e: &AppliedEffect) -> String {
    kadr_effects::recipe(&e.recipe).map_or_else(|| e.recipe.clone(), |d| t(&d.name_key()))
}

/// Width of the `fx N` badge in a clip's name bar (timeline.slint draws it this wide).
pub const BADGE_W: f64 = 30.0;
/// Room the name keeps left of the badge, with the gap to it (the link icon
/// and the speed label, if any, are extra).
const NAME_MIN_W: f64 = 28.0;
/// A linked clip's icon with its spacing.
const LINK_W: f64 = 15.0;
/// The speed label ("2.00×") of a speed-changed clip, with its spacing: it
/// never elides, so the name bar keeps its room generously.
const SPEED_W: f64 = 40.0;

/// The badge of a clip (spec §6.3): `fx N` for N effects, `""` without.
pub fn fx_badge(c: &Clip) -> String {
    if c.effects.is_empty() { String::new() } else { format!("fx {}", c.effects.len()) }
}

/// The badge's tooltip: the effects in order of application, a switched-off
/// one marked, one this version cannot run (a newer Kadr's) marked too.
pub fn fx_tip(c: &Clip) -> String {
    let names: Vec<String> = c
        .effects
        .iter()
        .map(|e| match (kadr_effects::definition(e), e.enabled) {
            (None, _) => tf("fx.tip.unavailable", &[("name", &fx_name(e))]),
            (Some(_), false) => tf("fx.tip.off", &[("name", &fx_name(e))]),
            (Some(_), true) => fx_name(e),
        })
        .collect();
    tf("fx.tip", &[("list", &names.join(", "))])
}

/// Where the badge is in a clip `width` px wide (clip x range): right-aligned
/// in the name bar, left of the fade-out handle wherever that handle can
/// appear (`out_can_appear`: also on a mere hover, ruling R16 — so the badge
/// never slides).
pub fn badge_span(width: f64, fade_out_px: f64, out_can_appear: bool) -> (f64, f64) {
    let pad = if out_can_appear { (width - anim_ui::handle_x(0.0, width, fade_out_px, FadeEdge::Out) + 7.0).max(6.0) } else { 6.0 };
    (width - pad - BADGE_W, width - pad)
}

/// Where the badge of video clip `c`, `width` px wide at `pps` px/s, is drawn
/// (clip x range) — `None` without effects, or where the name bar has no room
/// for it. Rust positions the badge (`ClipView.fx-x`) and hit-tests it with
/// this, so drawing and tooltip cannot disagree. The fade-out handle's room is
/// kept whenever that handle can appear (also on a mere hover), so the badge
/// never slides; the name's room counts the link icon and the speed label.
/// `selected` only matters for the fade-in side, which pads the name when its
/// handle stays on screen.
pub fn badge_place(c: &Clip, width: f64, pps: f64, selected: bool, locked: bool, blade: bool) -> Option<(f64, f64)> {
    if c.effects.is_empty() {
        return None;
    }
    let (fi, fo) = (c.video_fade.fade_in, c.video_fade.fade_out);
    let (can_in, can_out) = if locked || blade { (false, false) } else { anim_ui::shown_handles(width, true, fi, fo) };
    let pinned_in = can_in && (fi > Time::ZERO || selected);
    let pad_left = if pinned_in { (anim_ui::handle_x(0.0, width, fi.as_secs_f64() * pps, FadeEdge::In) + 7.0).max(6.0) } else { 6.0 };
    let (a, b) = badge_span(width, fo.as_secs_f64() * pps, can_out);
    let link = if c.link.is_some() { LINK_W } else { 0.0 };
    let speed = if (c.speed() - 1.0).abs() > 0.001 { SPEED_W } else { 0.0 };
    (a - pad_left >= NAME_MIN_W + link + speed).then_some((a, b))
}

/// The categories that have recipes, in tab order (refinement 8).
pub fn categories() -> Vec<Category> {
    Category::ALL.into_iter().filter(|c| kadr_effects::registry().iter().any(|d| d.category == *c)).collect()
}

/// Recipes listed for `category` (`""` = all) whose Russian or English name
/// contains `query`, any case (spec §6.1: search by name, RU/EN).
pub fn visible_recipes(category: &str, query: &str) -> Vec<&'static RecipeDef> {
    let q = query.trim().to_lowercase();
    kadr_effects::registry()
        .iter()
        .filter(|d| category.is_empty() || d.category.key() == category)
        .filter(|d| q.is_empty() || Lang::ALL.iter().any(|l| t_in(*l, &d.name_key()).to_lowercase().contains(&q)))
        .collect()
}

/// Whether the hovered card (`hover`, a recipe id) is not among the
/// `listed` cards any more: a card filtered out under a resting pointer is
/// destroyed without a pointer leave, so its hover must end without one.
pub fn hover_gone(hover: Option<&str>, listed: &[&RecipeDef]) -> bool {
    hover.is_some_and(|r| !listed.iter().any(|d| d.id == r))
}

/// The selected clips an effect of `def` goes on: those it fits, on unlocked tracks.
pub fn targets(seq: &Sequence, selection: &[ClipId], def: &RecipeDef) -> Vec<ClipId> {
    selection
        .iter()
        .copied()
        .filter(|id| seq.locate_clip(*id).is_some_and(|(ti, _)| !seq.tracks[ti].locked && kadr_effects::fits(def.target, seq.tracks[ti].kind)))
        .collect()
}

/// Why a card is inactive (an i18n key): a selection it fits none of, or
/// whose clips it fits are all on locked tracks. `None` without a
/// selection: the card can still be dragged onto a clip.
pub fn card_hint(seq: &Sequence, selection: &[ClipId], def: &RecipeDef) -> Option<&'static str> {
    if selection.is_empty() || !targets(seq, selection, def).is_empty() {
        return None;
    }
    let fitting = selection.iter().any(|id| seq.locate_clip(*id).is_some_and(|(ti, _)| kadr_effects::fits(def.target, seq.tracks[ti].kind)));
    Some(if fitting { "fx.drop.locked" } else { "fx.hint.unfit" })
}

/// What a card dropped on the lanes at time `t`, lane y `y` hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropHit {
    Clip(ClipId),
    Unfit,
    Locked,
    Nothing,
}

/// Only a clip really under the pointer on a real row (review focus 3: no
/// fallback to the top row as the media drop has), that the effect fits.
pub fn drop_hit(seq: &Sequence, rows: &[Row], t: Time, y: f64, def: &RecipeDef) -> DropHit {
    let Some(r) = rows.iter().find(|r| y >= r.y && y < r.y + r.h) else { return DropHit::Nothing };
    let Some(track) = seq.tracks.get(r.track) else { return DropHit::Nothing };
    let Some(c) = track.clip_at(t) else { return DropHit::Nothing };
    if !kadr_effects::fits(def.target, track.kind) {
        DropHit::Unfit
    } else if track.locked {
        DropHit::Locked
    } else {
        DropHit::Clip(c.id)
    }
}

/// The edit applying `def` at «Обычно» to `clips` (spec §6.1: one undo entry
/// «Эффект: …»), applied at timeline time `at`; only clips it changes —
/// `None` when it changes none.
pub fn apply_batch(seq: &Sequence, def: &RecipeDef, clips: &[ClipId], at: Time) -> Option<EditCommand> {
    let effects = clips.iter().filter_map(|id| Some((*id, kadr_effects::fresh(def, seq.clip(*id)?, at)))).collect();
    batch_of(seq, def, effects)
}

/// [`apply_batch`] with the effects given — the hovered preview's, so what
/// was shown is what is applied (Handheld's noise is seeded by the id).
fn batch_of(seq: &Sequence, def: &RecipeDef, effects: Vec<(ClipId, AppliedEffect)>) -> Option<EditCommand> {
    let commands: Vec<EditCommand> = effects
        .into_iter()
        .filter(|(id, effect)| seq.clip(*id).is_some_and(|c| c.clone().put_effect(effect.clone())))
        .map(|(clip, effect)| EditCommand::AddEffect { clip, effect })
        .collect();
    (!commands.is_empty()).then(|| EditCommand::Batch { label: tf("cmd.effect_named", &[("name", &t(&def.name_key()))]), commands })
}

/// One parameter row of an effect block at the playhead (spec §6.2): a
/// number with ◀ ◆ ▶ (its value at the playhead, or at the nearest edge),
/// a choice, the point (pick on the preview) or the moment (to the playhead).
pub fn param_row(c: &Clip, e: &AppliedEffect, p: &ParamDef, playhead: Time, rate: FrameRate, picking: bool) -> FxParamRow {
    let frame = rate.frame_duration();
    let on_clip = anim_ui::edit_time(c, playhead, frame).is_some();
    let key = TrackKey::fx(e.id, p.key);
    let mut row = FxParamRow { key: p.key.into(), prop: key.key_string().into(), label: t(&p.label_key()).into(), ..Default::default() };
    match p.kind {
        ParamKind::Number { min, max, default } => {
            row.kind = 0;
            row.value = c.track_value(&key, anim_ui::shown_time(c, playhead, frame)).unwrap_or(default) as f32;
            (row.minimum, row.maximum, row.default_value) = (min as f32, max as f32, default as f32);
            let info = anim_ui::key_info(c, &key, playhead, frame);
            let ease = anim_ui::key_here(c, &key, playhead, frame).map_or("", |k| anim_ui::ease_key(&k.ease));
            row.k = KeyState { state: info.state, prev: info.prev.is_some(), next: info.next.is_some(), enabled: on_clip, ease: ease.into() };
            row.strength = p.key == "strength";
        }
        ParamKind::Choice { options, .. } => {
            row.kind = 1;
            let current = match e.params.get(p.key) {
                Some(ParamValue::Choice(v)) => v.as_str(),
                _ => "",
            };
            row.current = options.iter().position(|o| *o == current).map_or(-1, |i| i as i32);
            row.options = ModelRc::new(VecModel::from(options.iter().map(|o| SharedString::from(*o)).collect::<Vec<_>>()));
            row.tips = ModelRc::new(VecModel::from(options.iter().map(|o| SharedString::from(t(&format!("fx.choice.{o}")))).collect::<Vec<_>>()));
        }
        ParamKind::Point => {
            row.kind = 2;
            let (x, y) = match e.params.get(p.key) {
                Some(ParamValue::Point { x, y }) => (*x, *y),
                _ => (0.5, 0.5),
            };
            // As the sliders write a percentage: no space before the sign.
            row.text = format!("{:.0}%, {:.0}%", x * 100.0, y * 100.0).into();
            row.can_act = on_clip;
            row.active = picking;
            row.hint = if picking { t("fx.pick.hint") } else if on_clip { String::new() } else { t("fx.pick.off_clip") }.into();
        }
        ParamKind::Moment => {
            row.kind = 3;
            let current = match e.params.get(p.key) {
                Some(ParamValue::Moment(m)) => Some(*m),
                _ => None,
            };
            if let Some(m) = current {
                let tl = c.timeline_time_of(m.0);
                row.text = Timecode::from_time(tl.max(Time::ZERO), rate).to_string().into();
                if tl < c.timeline_in || tl >= c.timeline_out {
                    row.hint = t("fx.moment.outside").into();
                }
            }
            // Enabled exactly when the click would move it (`action_commands`).
            row.can_act = moment_target(c, playhead, rate).is_some_and(|to| current != Some(to));
        }
    }
    row
}

/// Where «к плейхеду» puts a moment: the source frame under the playhead,
/// only while the playhead is on the clip. The row's enabled state and the
/// action both use it, so an enabled button always does something (R14).
fn moment_target(c: &Clip, playhead: Time, rate: FrameRate) -> Option<SourceTime> {
    anim_ui::edit_time(c, playhead, rate.frame_duration()).map(|tt| SourceTime(c.source_time_at(tt)))
}

/// Whether the effects section repeats «Поставьте плейхед на клип…» (M6):
/// the playhead is off the clip and a number of one of its effects has
/// keyframes — that row is locked there, as the Transform rows are.
pub fn fx_outside_hint(c: &Clip, playhead: Time, frame: Time) -> bool {
    anim_ui::edit_time(c, playhead, frame).is_none()
        && c.effects.iter().any(|e| kadr_effects::definition(e).is_some_and(|d| d.params.iter().any(|p| c.animation.is_animated(TrackKey::fx(e.id, p.key)))))
}

/// Whether a block of `c` shows a moment: its «к плейхеду» depends on the
/// playhead's frame, so the inspector follows the playhead (as it does for
/// keyframes) even on a clip without animation.
pub fn has_moment(c: &Clip) -> bool {
    c.effects.iter().filter_map(kadr_effects::definition).any(|d| d.params.iter().any(|p| p.kind == ParamKind::Moment))
}

/// «Сбросить» of one effect: its settings back to the defaults and no
/// keyframes on its numbers — `None` when it is so already. Every moment
/// stays where it was put (ruling R13: a reset never re-times a punch); one
/// the effect lacks gets its default (the frame at `at`).
pub fn reset_commands(c: &Clip, e: &AppliedEffect, def: &RecipeDef, at: Time) -> Option<Vec<EditCommand>> {
    let mut defaults = kadr_effects::fresh(def, c, at).params;
    for p in def.params.iter().filter(|p| p.kind == ParamKind::Moment) {
        if let Some(m @ ParamValue::Moment(_)) = e.params.get(p.key) {
            defaults.insert(p.key.to_string(), m.clone());
        }
    }
    let animated: Vec<(TrackKey, f64)> = def
        .params
        .iter()
        .filter_map(|p| {
            let k = TrackKey::fx(e.id, p.key);
            let v = defaults.get(p.key)?.as_number()?;
            c.animation.is_animated(&k).then_some((k, v))
        })
        .collect();
    if animated.is_empty() && defaults.iter().all(|(k, v)| e.params.get(k) == Some(v)) {
        return None;
    }
    let mut commands = vec![];
    if !animated.is_empty() {
        commands.push(EditCommand::ClearAnimation { clip: c.id, statics: animated });
    }
    commands.push(EditCommand::UpdateEffect { clip: c.id, id: e.id, params: defaults, enabled: None });
    Some(commands)
}

/// Whether the ⋯ menu's «Сбросить» would change `e` (with the playhead at
/// `at`): greyed out otherwise (R14). A newer Kadr's effect never resets.
pub fn can_reset(c: &Clip, e: &AppliedEffect, at: Time) -> bool {
    kadr_effects::definition(e).and_then(|d| reset_commands(c, e, d, at)).is_some()
}

/// The effect point being picked on the preview (spec §6.2 «Точка»).
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub clip: ClipId,
    pub effect: EffectId,
    pub key: String,
}

/// The layer of `clip` in a scene's layers, also inside a transition.
pub fn layer_of(layers: &[Layer], clip: ClipId) -> Option<&Layer> {
    let id = LayerId::from(clip);
    layers.iter().find_map(|l| match &l.content {
        LayerContent::Transition(t) => layer_of(&t.from, clip).or_else(|| layer_of(&t.to, clip)),
        _ => (l.id == id).then_some(l),
    })
}

/// Why a pick finds no picture of clip `c` in the frame shown (an i18n
/// key): the playhead is off the clip, or it is on it and the clip is not
/// seen there — under a full-frame opaque layer, or switched off (M2).
pub fn unseen_key(c: Option<&Clip>, playhead: Time, frame: Time) -> &'static str {
    match c {
        Some(c) if anim_ui::edit_time(c, playhead, frame).is_some() => "fx.pick.hidden",
        _ => "fx.pick.off_clip",
    }
}

/// The point of `layer`'s picture under a click at canvas fraction (`fx`,
/// `fy`) — through the placement as shown (zoomed, panned, turned) — as
/// fractions of the picture (the content rectangle before crop,
/// refinement 6); `None` off its visible part (review focus 4).
pub fn pick_point(layer: &Layer, canvas: SizeU, fx: f64, fy: f64) -> Option<(f64, f64)> {
    let inv = layer.placement.to_canvas().inverse()?;
    let (x, y) = inv.apply(fx * canvas.w as f64, fy * canvas.h as f64);
    let c = layer.crop;
    let inside = x >= c.x0 as f64 && x <= c.x1 as f64 && y >= c.y0 as f64 && y <= c.y1 as f64;
    let (w, h) = (layer.placement.size.x as f64, layer.placement.size.y as f64);
    (inside && w > 0.0 && h > 0.0).then(|| ((x / w).clamp(0.0, 1.0), (y / h).clamp(0.0, 1.0)))
}

/// Where the point `(px, py)` of `layer`'s picture shows, as canvas fractions.
pub fn point_on_canvas(layer: &Layer, canvas: SizeU, (px, py): (f64, f64)) -> (f64, f64) {
    let (x, y) = layer.placement.to_canvas().apply(px * layer.placement.size.x as f64, py * layer.placement.size.y as f64);
    (x / canvas.w.max(1) as f64, y / canvas.h.max(1) as f64)
}

/// Where the preview marks an effect's point (ruling R15): the point a zoom
/// really holds still — `point` clamped into the clip's visible crop window
/// as motion recipes do (ruling R6, `CompileCtx::window`; the layer's crop
/// is that window in local pixels) — on the canvas as shown.
pub fn marker_on_canvas(layer: &Layer, canvas: SizeU, (px, py): (f64, f64)) -> (f64, f64) {
    let (w, h) = (layer.placement.size.x as f64, layer.placement.size.y as f64);
    let c = layer.crop;
    let (x0, y0, x1, y1) = (c.x0 as f64 / w, c.y0 as f64 / h, c.x1 as f64 / w, c.y1 as f64 / h);
    // NaN fails both comparisons: no usable window, no clamp.
    let point = if x0 < x1 && y0 < y1 { (px.clamp(x0, x1), py.clamp(y0, y1)) } else { (px, py) };
    point_on_canvas(layer, canvas, point)
}

/// A row's nested list in the model on screen (an empty one stays as it is).
fn same_list(old: ModelRc<SharedString>, new: ModelRc<SharedString>) -> ModelRc<SharedString> {
    if old.row_count() == 0 && new.row_count() == 0 {
        return old;
    }
    crate::util::reuse_rows(Some(old), new.iter().collect())
}

/// `rows` into the model on screen, each row keeping its nested option
/// lists (a fresh `ModelRc` would make every row look changed and rebuild
/// its elements on each refresh).
fn reuse_param_rows(old: Option<ModelRc<FxParamRow>>, mut rows: Vec<FxParamRow>) -> ModelRc<FxParamRow> {
    if let Some(old) = &old {
        for (i, r) in rows.iter_mut().enumerate() {
            if let Some(o) = old.row_data(i) {
                r.options = same_list(o.options, r.options.clone());
                r.tips = same_list(o.tips, r.tips.clone());
            }
        }
    }
    crate::util::reuse_rows(old, rows)
}

impl App {
    /// The badge under lane point (`x`, `y`) and its tooltip text.
    fn fx_badge_at(&self, x: f64, y: f64) -> Option<(ClipId, String)> {
        let seq = self.project.sequence();
        let r = rows(seq, self.tl.scroll_y).into_iter().find(|r| y >= r.y && y < r.y + anim_ui::HEADER_H)?;
        if r.kind != TrackKind::Video {
            return None;
        }
        let track = &seq.tracks[r.track];
        let c = track.clip_at(self.x_to_time(x)).filter(|c| !c.effects.is_empty())?;
        let x0 = (c.timeline_in.as_secs_f64() - self.tl.scroll) * self.tl.pps;
        let width = c.duration().as_secs_f64() * self.tl.pps;
        let (a, b) = badge_place(c, width, self.tl.pps, self.tl.selection.contains(&c.id), track.locked, self.tl.blade)?;
        (x - x0 >= a && x - x0 <= b).then(|| (c.id, fx_tip(c)))
    }

    /// The badge tooltip (spec §6.3) while the pointer rests on a badge
    /// (`None`: the pointer left the lanes); a drag's own tips are untouched.
    pub fn fx_badge_hover(&mut self, at: Option<(f64, f64)>) {
        // Not over a fade handle or a trim edge: they show their own cursor.
        let at = at.filter(|&(x, y)| self.over_clip_body(x, y));
        match at.and_then(|(x, y)| self.fx_badge_at(x, y).map(|hit| (hit, x, y))) {
            Some(((id, text), x, y)) => {
                self.fx.tip_clip = Some(id);
                self.show_tip(text, x, y);
            }
            None => {
                if self.fx.tip_clip.take().is_some() {
                    self.ui().set_tl_tip_text("".into());
                }
            }
        }
    }

    /// The left panel switched tabs (0 media, 1 effects). Hidden, the tab
    /// renders no thumbnails: the batch in flight stops. The user moved on:
    /// a point being picked is not any more.
    pub fn fx_tab_changed(&mut self, tab: i32) {
        self.fx_clear_hover();
        self.fx_cancel_pick();
        if tab == 1 {
            self.refresh_effects();
        } else {
            self.fx.thumbs.cancel();
        }
    }

    /// Switches the left panel to «Медиа» (an import lands there, «Показать
    /// в медиатеке» points there). The hidden tab's search field must not
    /// keep the keyboard: the editor takes it, as after a tab click.
    pub fn show_media_tab(&mut self) {
        let ui = self.ui();
        if ui.get_left_tab() != 0 {
            ui.set_left_tab(0);
            ui.invoke_focus_editor();
            self.fx_tab_changed(0);
        }
    }

    /// A project was opened or created: nothing hovered, dragged or picked;
    /// the thumbnail work stops and the old project's pictures are freed.
    pub fn fx_reset(&mut self) {
        self.fx.hover = None;
        self.fx.preview = None;
        self.fx.drag = None;
        self.fx.thumbs.clear();
        self.fx.picking = None;
        self.fx.focus = None;
    }

    /// Chips, cards and the hint of the tab — only while it shows. Rows keep
    /// their elements (`sync_rows_by_key`): a press in flight survives, a
    /// card's picture changes in place. Ends by asking for the pictures the
    /// cards lack (rendered in the background, `fx_thumbs`).
    pub fn refresh_effects(&mut self) {
        let ui = self.ui();
        if ui.get_left_tab() != 1 {
            return;
        }
        let listed = visible_recipes(&self.fx.category, &self.fx.search);
        // The selected clip's frame under the playhead, or the sample; a
        // card whose new picture is not rendered yet keeps its last one.
        let base = crate::fx_thumbs::thumb_base(&self.project, self.inspected(), self.playhead);
        let thumbs: Vec<Option<slint::Image>> = listed
            .iter()
            .map(|d| {
                let now = self.fx_thumb(d, &base);
                self.fx.thumbs.shown(d.id, now)
            })
            .collect();
        let seq = self.project.sequence();
        let mut chips = vec![FxCategoryView { key: "".into(), label: t("fx.cat.all").into() }];
        chips.extend(categories().into_iter().map(|c| FxCategoryView { key: c.key().into(), label: t(&c.label_key()).into() }));
        crate::util::sync_rows_by_key(ui.get_fx_categories(), chips, |c| c.key.clone(), |m| ui.set_fx_categories(m));
        ui.set_fx_category(self.fx.category.clone().into());
        let sel = &self.tl.selection;
        let cards: Vec<FxCardView> = listed
            .iter()
            .zip(thumbs)
            .map(|(d, thumb)| {
                let hint = card_hint(seq, sel, d);
                FxCardView {
                    id: d.id.into(),
                    name: t(&d.name_key()).into(),
                    category: d.category.key().into(),
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    enabled: hint.is_none(),
                    hint: hint.map(t).unwrap_or_default().into(),
                }
            })
            .collect();
        crate::util::sync_rows_by_key(ui.get_fx_cards(), cards, |c| c.id.clone(), |m| ui.set_fx_cards(m));
        ui.set_fx_hint(t(if sel.is_empty() { "fx.hint.select_clip" } else { "fx.hint.apply" }).into());
        // A hovered card filtered out (search, category) ends its hover; a
        // shown preview follows the selection — the hovered card's effect on
        // the clips it now fits, or none — and, while paused, a moment the
        // playhead (playing, the punch plays where it was shown).
        let gone = hover_gone(self.fx.hover.map(|(r, _)| r), &listed);
        let stale = self.fx.preview.as_ref().is_some_and(|o| {
            let playhead = if self.playing { o.at } else { self.playhead };
            kadr_effects::recipe(o.recipe).is_none_or(|d| o.outdated(d, &targets(seq, sel, d), playhead))
        });
        if gone {
            self.fx_clear_hover();
        } else if stale {
            let next = self.fx.hover.and_then(|(r, _)| kadr_effects::recipe(r)).and_then(|d| self.fx_override(d));
            self.fx_set_preview(next);
        }
        self.fx_request_thumbs(base);
    }

    pub fn fx_category(&mut self, key: &str) {
        self.fx.category = key.to_string();
        self.refresh_effects();
    }

    pub fn fx_search(&mut self, text: &str) {
        self.fx.search = text.to_string();
        self.refresh_effects();
    }

    /// The pointer entered or left a card. Entering starts the 150 ms delay;
    /// leaving restores the preview at once.
    pub fn fx_card_hover(&mut self, id: &str, on: bool) {
        let Some(def) = kadr_effects::recipe(id) else { return };
        if on {
            // Straight from another card (no gap crossed, or its leave
            // reported after this enter): that preview ends now, not when
            // this card's delay is over — this card may show none at all.
            if self.fx.preview.as_ref().is_some_and(|o| o.recipe != def.id) {
                self.fx_clear_preview();
            }
            self.fx.hover_gen += 1;
            let generation = self.fx.hover_gen;
            self.fx.hover = Some((def.id, generation));
            slint::Timer::single_shot(HOVER_DELAY, move || {
                crate::app::with_app(|app| app.fx_hover_fire(generation));
            });
            // Another card's loop stops now (its card shows its still).
            if self.fx.thumbs.stop_loop() {
                self.refresh_effects();
            }
        } else if self.fx.hover.is_some_and(|(r, _)| r == def.id) {
            self.fx_clear_hover();
        }
    }

    /// The delayed hover: the card's effect on the selected clips, in the
    /// frame under the playhead (spec §6.1), and a motion card's short loop
    /// of its motion — started here, not on entering: a pointer crossing
    /// cards renders no loops (M4).
    fn fx_hover_fire(&mut self, generation: u64) {
        let Some((recipe, g)) = self.fx.hover else { return };
        if g != generation || self.fx.drag.as_ref().is_some_and(|d| d.active) {
            return;
        }
        let Some(def) = kadr_effects::recipe(recipe) else { return };
        let next = self.fx_override(def);
        self.fx_set_preview(next);
        if def.category == Category::Motion {
            self.fx.thumbs.start_loop(def.id, || {
                crate::app::with_app(|app| app.fx_loop_tick());
            });
            self.refresh_effects();
        }
    }

    /// `def` at «Обычно» on the selected clips it goes on, made at the
    /// playhead as a click would; `None` when it goes on none. The
    /// instances shown already keep their ids ([`make_override`]).
    fn fx_override(&self, def: &'static RecipeDef) -> Option<PreviewOverride> {
        let seq = self.project.sequence();
        make_override(seq, def, &targets(seq, &self.tl.selection, def), self.playhead, self.fx.preview.as_ref())
    }

    /// Shows `next` on the preview (a new snapshot) — when there is a
    /// change: none before and none after needs no new frame.
    fn fx_set_preview(&mut self, next: Option<PreviewOverride>) {
        if self.fx.preview.is_none() && next.is_none() {
            return;
        }
        self.fx.preview = next;
        self.preview.invalidate();
        self.request_frame();
    }

    /// Nothing hovered any more: the preview shows the project again, the
    /// card's loop stops and it shows its still.
    pub fn fx_clear_hover(&mut self) {
        self.fx.hover = None;
        self.fx_clear_preview();
        if self.fx.thumbs.stop_loop() {
            self.refresh_effects();
        }
    }

    fn fx_clear_preview(&mut self) {
        self.fx_set_preview(None);
    }

    pub fn fx_card_pressed(&mut self, id: &str, x: f32, y: f32) {
        let Some(def) = kadr_effects::recipe(id) else { return };
        self.fx.drag = Some(FxDrag { recipe: def.id, start: (x as f64, y as f64), active: false });
    }

    /// What a drop at window point (`wx`, `wy`) hits, and the timeline time there.
    fn fx_drop_at(&self, wx: f64, wy: f64, def: &RecipeDef) -> (DropHit, Time) {
        let ui = self.ui();
        let (x, y) = (wx - ui.get_lanes_abs_x() as f64, wy - ui.get_lanes_abs_y() as f64);
        if x < 0.0 || y < 0.0 || x > ui.get_lanes_width() as f64 || y > ui.get_lanes_height() as f64 {
            return (DropHit::Nothing, Time::ZERO);
        }
        let seq = self.project.sequence();
        let at = self.x_to_time(x);
        (drop_hit(seq, &rows(seq, self.tl.scroll_y), at, y, def), at)
    }

    /// A card drag: past `DRAG_PX` the label follows the pointer and the
    /// clip it would land on is outlined (the timeline's ghost rectangle).
    pub fn fx_card_moved(&mut self, _id: &str, x: f32, y: f32) {
        let (x, y) = (x as f64, y as f64);
        let Some(d) = self.fx.drag.as_mut() else { return };
        if !d.active && ((x - d.start.0).abs() > DRAG_PX || (y - d.start.1).abs() > DRAG_PX) {
            d.active = true;
        }
        if !d.active {
            return;
        }
        let Some(def) = kadr_effects::recipe(d.recipe) else { return };
        self.fx_clear_preview();
        let name = t(&def.name_key());
        let seq = self.project.sequence();
        let (hit, _) = self.fx_drop_at(x, y, def);
        let (label, ghost) = match hit {
            DropHit::Clip(id) => match (seq.clip(id), seq.locate_clip(id)) {
                (Some(c), Some((ti, _))) => {
                    let row = rows(seq, self.tl.scroll_y).into_iter().find(|r| r.track == ti);
                    let x0 = (c.timeline_in.as_secs_f64() - self.tl.scroll) * self.tl.pps;
                    let w = c.duration().as_secs_f64() * self.tl.pps;
                    (tf("fx.drop.on", &[("name", &name), ("clip", &c.name)]), row.map(|r| (x0, r.y, w, r.h)))
                }
                _ => (tf("fx.drag_hint", &[("name", &name)]), None),
            },
            DropHit::Unfit => (t("fx.hint.unfit"), None),
            DropHit::Locked => (t("fx.drop.locked"), None),
            DropHit::Nothing => (tf("fx.drag_hint", &[("name", &name)]), None),
        };
        let ui = self.ui();
        ui.set_drag_active(true);
        ui.set_drag_refused(matches!(hit, DropHit::Unfit | DropHit::Locked));
        ui.set_drag_x(x as f32);
        ui.set_drag_y(y as f32);
        ui.set_drag_label(label.into());
        self.tl.ghost = ghost;
        self.refresh_timeline();
    }

    /// The end of a card press: a drag applies to the clip under the pointer
    /// (and selects it); a click applies to the selected clips.
    pub fn fx_card_released(&mut self, _id: &str, x: f32, y: f32) {
        let ui = self.ui();
        ui.set_drag_active(false);
        ui.set_drag_refused(false);
        self.tl.ghost = None;
        let Some(d) = self.fx.drag.take() else { return };
        let Some(def) = kadr_effects::recipe(d.recipe) else { return };
        if d.active {
            match self.fx_drop_at(x as f64, y as f64, def) {
                (DropHit::Clip(id), at) => {
                    // «Момент» defaults to the frame dropped on (spec §5).
                    let at = self.project.sequence().frame_rate.snap(at);
                    // (on the clip now, just applied)
                    let (on, new) = match apply_batch(self.project.sequence(), def, &[id], at) {
                        Some(cmd) => {
                            let done = self.execute(cmd);
                            (done, done)
                        }
                        None => {
                            self.toast(tf("fx.already", &[("name", &t(&def.name_key()))]));
                            (true, false)
                        }
                    };
                    if on {
                        self.tl.selection = vec![id];
                        self.refresh_inspector();
                    }
                    if new {
                        self.fx_reveal(def);
                    }
                }
                (DropHit::Unfit, _) => self.toast_warn(t("fx.hint.unfit")),
                (DropHit::Locked, _) => self.toast_warn(t("fx.drop.locked")),
                (DropHit::Nothing, _) => {}
            }
            self.refresh_timeline();
            return;
        }
        let seq = self.project.sequence();
        if self.tl.selection.is_empty() {
            return self.toast(t("fx.hint.select_clip"));
        }
        let clips = targets(seq, &self.tl.selection, def);
        if clips.is_empty() {
            let why = card_hint(seq, &self.tl.selection, def).unwrap_or("fx.hint.unfit");
            return self.toast_warn(t(why));
        }
        // The previewed instances when the hover showed this card on these
        // clips: what was shown is what is applied.
        let cmd = match &self.fx.preview {
            Some(o) if o.recipe == def.id && o.clips() == clips => batch_of(seq, def, o.effects.clone()),
            _ => apply_batch(seq, def, &clips, self.playhead),
        };
        match cmd {
            Some(cmd) => {
                if self.execute(cmd) && self.inspected().is_some_and(|id| clips.contains(&id)) {
                    self.fx_reveal(def);
                }
            }
            None => self.toast(tf("fx.already", &[("name", &t(&def.name_key()))])),
        }
    }

    /// `def` was just applied to the inspected clip (A3): its block takes
    /// the focus — its point shows on the preview — and the inspector
    /// scrolls to it, top first. The scroll waits a turn of the event loop:
    /// by then a new block exists and is laid out (Slint instantiates it
    /// right after the pointer event), so its `changed` handler runs.
    fn fx_reveal(&mut self, def: &RecipeDef) {
        let Some(id) = self.inspected() else { return };
        let Some(e) = self.project.sequence().clip(id).and_then(|c| c.effects.iter().find(|e| e.recipe == def.id)).map(|e| e.id) else { return };
        self.fx.focus = Some(e);
        self.fx_update_marker();
        let ui = self.ui();
        ui.set_inspector_reveal_fx(e.to_string().into());
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_inspector_reveal_seq(ui.get_inspector_reveal_seq().wrapping_add(1));
            }
        });
    }

    /// The «Эффекты клипа» rows of `c` (spec §6.2, mockup A), updating the
    /// models on screen in place: the inspector refreshes ~10 times a second
    /// during playback, and a rebuilt row would lose a press in flight.
    pub fn fx_rows(&self, c: &Clip, old: ModelRc<FxRow>) -> ModelRc<FxRow> {
        let rate = self.project.sequence().frame_rate;
        let old_params: HashMap<SharedString, ModelRc<FxParamRow>> = old.iter().map(|r| (r.id, r.params)).collect();
        let rows: Vec<FxRow> = c
            .effects
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let def = kadr_effects::definition(e);
                let picking = |key: &str| self.fx.picking.as_ref().is_some_and(|pk| pk.clip == c.id && pk.effect == e.id && pk.key == key);
                let params: Vec<FxParamRow> = def.map_or_else(Vec::new, |d| d.params.iter().map(|p| param_row(c, e, p, self.playhead, rate, picking(p.key))).collect());
                let id: SharedString = e.id.to_string().into();
                let params = reuse_param_rows(old_params.get(&id).cloned(), params);
                FxRow { id, name: fx_name(e).into(), enabled: e.enabled, known: def.is_some(), resettable: can_reset(c, e, self.playhead), first: i == 0, last: i + 1 == c.effects.len(), params }
            })
            .collect();
        crate::util::reuse_rows(Some(old), rows)
    }

    /// An effect block's action (spec §6.2, `action_commands`): one undo
    /// entry, nothing sent when nothing would change.
    pub fn fx_action(&mut self, effect: &str, action: &str) {
        let (Some(id), Some(eid)) = (self.inspected(), EffectId::parse(effect)) else { return };
        self.fx.focus = Some(eid);
        if let Some(key) = action.strip_prefix("pick:") {
            return self.fx_toggle_pick(Pick { clip: id, effect: eid, key: key.to_string() });
        }
        let seq = self.project.sequence();
        let Some(c) = seq.clip(id) else { return };
        if let Some((label, commands)) = action_commands(c, eid, action, self.playhead, seq.frame_rate)
            && self.execute(EditCommand::Batch { label, commands })
        {
            return; // the refresh after the edit moved the marker
        }
        // Nothing sent: the focus moved all the same, the marker follows it.
        self.fx_update_marker();
    }

    /// «Сбросить» of the section: every effect of the clip, after asking (spec §6.2).
    pub fn fx_ask_clear(&mut self, id: ClipId) {
        let Some(n) = self.project.sequence().clip(id).map(|c| c.effects.len()).filter(|n| *n > 0) else { return };
        let body = kadr_i18n::tn("dlg.clear_effects.body", n as i64, &[]);
        self.ask(&t("dlg.clear_effects.title"), &body, &t("dlg.clear_effects.ok"), "", true, crate::app::Confirm::ClearEffects(id));
    }

    /// The confirmed section reset: one undo step removes every effect (and its keyframes).
    pub fn fx_clear_now(&mut self, id: ClipId) {
        if let Some(cmd) = self.project.sequence().clip(id).and_then(clear_batch) {
            self.execute(cmd);
        }
    }

    /// An inspector control of `prop` was used: an effect's number puts that
    /// effect in focus (its point shows on the preview, refinement 10).
    /// `true` when the focus moved.
    pub fn fx_focus_prop(&mut self, prop: &str) -> bool {
        let Some(TrackKey::Fx(f)) = anim_ui::track_of(prop) else { return false };
        let moved = self.fx.focus != Some(f.effect);
        self.fx.focus = Some(f.effect);
        moved
    }

    /// «Выбрать на превью»: pick mode for the effect's point (crosshair over
    /// the preview, Esc cancels), or its end when it is on already. Only a
    /// point of a known recipe is picked.
    fn fx_toggle_pick(&mut self, pick: Pick) {
        if self.fx.picking.as_ref().is_some_and(|p| *p == pick) {
            return self.fx_cancel_pick();
        }
        let is_point = self.project.sequence().clip(pick.clip).and_then(|c| c.effect(pick.effect)).and_then(kadr_effects::definition).and_then(|d| d.param(&pick.key)).is_some_and(|p| p.kind == ParamKind::Point);
        if !is_point {
            return;
        }
        self.fx.picking = Some(pick);
        // The inspector shows the row active and sets `fx-picking`; it ends with the marker.
        self.refresh_inspector();
    }

    /// Pick mode ends without a pick (Esc, the button again, another tab,
    /// playback).
    pub fn fx_cancel_pick(&mut self) {
        if self.fx.picking.take().is_some() {
            self.refresh_inspector();
        }
    }

    /// The clip's layer in the frame the preview shows — the preview's own
    /// snapshot, so «До» and the hover preview count — with the canvas size.
    fn fx_shown_layer(&mut self, clip: ClipId) -> Option<(Layer, SizeU)> {
        let scenes = self.preview_scenes();
        let seq = self.project.sequence();
        let t = self.playhead.min((seq.duration() - seq.frame_rate.frame_duration()).max(Time::ZERO));
        let scene = kadr_playback::SceneSource::scene_at(&*scenes, t, &self.preview.output(&self.project));
        layer_of(&scene.layers, clip).cloned().map(|l| (l, scene.canvas))
    }

    /// A click on the preview in pick mode, at canvas fraction (`fx`, `fy`):
    /// the point of the picture under it becomes the effect's point (one
    /// undo entry, none when it is the point already). Off the clip's
    /// visible picture pick mode stays and says why (review focus 4).
    pub fn fx_point_picked(&mut self, fx: f32, fy: f32) {
        let Some(pick) = self.fx.picking.clone() else { return };
        let Some((layer, canvas)) = self.fx_shown_layer(pick.clip) else {
            let seq = self.project.sequence();
            let why = unseen_key(seq.clip(pick.clip), self.playhead, seq.frame_rate.frame_duration());
            return self.toast_warn(t(why));
        };
        let Some((x, y)) = pick_point(&layer, canvas, fx as f64, fy as f64) else {
            return self.toast_warn(t("fx.pick.miss"));
        };
        self.fx_cancel_pick();
        let round = |v: f64| (v * 10_000.0).round() / 10_000.0;
        let v = ParamValue::Point { x: round(x), y: round(y) };
        let Some(e) = self.project.sequence().clip(pick.clip).and_then(|c| c.effect(pick.effect)) else { return };
        if e.params.get(&pick.key) == Some(&v) {
            return; // the point already: no undo entry
        }
        let label = tf("cmd.update_effect_named", &[("name", &fx_name(e))]);
        let set = EditCommand::UpdateEffect { clip: pick.clip, id: pick.effect, params: BTreeMap::from([(pick.key.clone(), v)]), enabled: None };
        self.execute(EditCommand::Batch { label, commands: vec![set] });
    }

    /// The inspected clip's picked (or else focused) effect with a point, and
    /// that point as its recipe sees it (`Params`: clamped into 0..1, the
    /// centre when missing or broken).
    fn fx_marker_target(&self) -> Option<(ClipId, (f64, f64))> {
        let id = self.inspected()?;
        let picking = self.fx.picking.as_ref().filter(|p| p.clip == id);
        let effect = picking.map(|p| p.effect).or(self.fx.focus)?;
        let e = self.project.sequence().clip(id)?.effect(effect)?;
        let def = kadr_effects::definition(e)?;
        let key = match picking {
            Some(p) => def.param(&p.key).filter(|d| d.kind == ParamKind::Point)?.key,
            None => def.params.iter().find(|p| p.kind == ParamKind::Point)?.key,
        };
        Some((id, kadr_effects::Params::resolve(def.params, &e.params, |_| None).point(key)))
    }

    /// The point marker on the preview (spec §6.2, ruling R15): where the
    /// focused effect's zoom holds still in the frame shown — while paused.
    pub fn fx_update_marker(&mut self) {
        self.fx_update_marker_in(None);
    }

    /// [`Self::fx_update_marker`] given the frame shown when the caller has
    /// evaluated it already (`request_frame`): a paused frame is evaluated once.
    pub fn fx_update_marker_in(&mut self, frame: Option<&FrameScene>) {
        let shown = match self.fx_marker_target() {
            Some((clip, point)) if !self.playing => match frame {
                Some(scene) => layer_of(&scene.layers, clip).map(|l| marker_on_canvas(l, scene.canvas, point)),
                None => self.fx_shown_layer(clip).map(|(l, canvas)| marker_on_canvas(&l, canvas, point)),
            },
            _ => None,
        };
        let ui = self.ui();
        ui.set_fx_point_shown(shown.is_some());
        if let Some((x, y)) = shown {
            ui.set_fx_point_x(x as f32);
            ui.set_fx_point_y(y as f32);
        }
    }
}

/// What an effect block's action does (spec §6.2) with the playhead at
/// `playhead`: `toggle`, `reset`, `remove`, `up`, `down`,
/// `choice:<key>:<index>`, `moment:<key>` (to the playhead) — the history
/// label and the commands of one undo entry; `None` when nothing would
/// change (the engine records an entry for every success). An effect from
/// a newer Kadr is only moved or removed (§7).
pub fn action_commands(c: &Clip, eid: EffectId, action: &str, playhead: Time, rate: FrameRate) -> Option<(String, Vec<EditCommand>)> {
    let e = c.effect(eid)?;
    let name = fx_name(e);
    let def = kadr_effects::definition(e);
    let pos = c.effects.iter().position(|x| x.id == eid)?;
    let update = |params: BTreeMap<String, ParamValue>, enabled: Option<bool>| EditCommand::UpdateEffect { clip: c.id, id: eid, params, enabled };
    Some(match action {
        "toggle" if def.is_some() => {
            let key = if e.enabled { "cmd.effect_off_named" } else { "cmd.effect_on_named" };
            (tf(key, &[("name", &name)]), vec![update(BTreeMap::new(), Some(!e.enabled))])
        }
        "remove" => (tf("cmd.remove_effect_named", &[("name", &name)]), vec![EditCommand::RemoveEffect { clip: c.id, id: eid }]),
        "up" if pos > 0 => (t("cmd.move_effect"), vec![EditCommand::MoveEffect { clip: c.id, id: eid, to: pos - 1 }]),
        "down" if pos + 1 < c.effects.len() => (t("cmd.move_effect"), vec![EditCommand::MoveEffect { clip: c.id, id: eid, to: pos + 1 }]),
        "reset" => (tf("cmd.reset_effect_named", &[("name", &name)]), reset_commands(c, e, def?, playhead)?),
        a if a.starts_with("choice:") => {
            let (key, index) = a["choice:".len()..].rsplit_once(':')?;
            let ParamKind::Choice { options, .. } = def?.param(key)?.kind else { return None };
            let v = ParamValue::Choice(options.get(index.parse::<usize>().ok()?)?.to_string());
            if e.params.get(key) == Some(&v) {
                return None;
            }
            (tf("cmd.update_effect_named", &[("name", &name)]), vec![update(BTreeMap::from([(key.to_string(), v)]), None)])
        }
        a if a.starts_with("moment:") => {
            let key = &a["moment:".len()..];
            if def?.param(key)?.kind != ParamKind::Moment {
                return None;
            }
            let v = ParamValue::Moment(moment_target(c, playhead, rate)?);
            if e.params.get(key) == Some(&v) {
                return None;
            }
            (tf("cmd.update_effect_named", &[("name", &name)]), vec![update(BTreeMap::from([(key.to_string(), v)]), None)])
        }
        _ => return None,
    })
}

/// The section's confirmed «Сбросить»: every effect of `c` (and with it its
/// keyframes) in one undo step; `None` without effects.
pub fn clear_batch(c: &Clip) -> Option<EditCommand> {
    let commands: Vec<EditCommand> = c.effects.iter().map(|e| EditCommand::RemoveEffect { clip: c.id, id: e.id }).collect();
    (!commands.is_empty()).then(|| EditCommand::Batch { label: t("cmd.clear_effects"), commands })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline_ui::rows;
    use kadr_core::{AssetId, FrameRate, MediaInfo, MediaKind, TimeRange, VideoInfo};
    use kadr_playback::SceneSource;
    use kadr_project::{Clip, MediaAsset, ParamValue, Project, SourceTime, Track, TrackKey, TrackKind};
    use kadr_effects::PlacementDelta;
    use kadr_scene::{BlendMode, Layer, LayerContent, LayerId, OutputSpec, Placement, RectF, RenderQuality, Rgba, SizeU, TransitionLayer, TransitionOp, Vec2};

    /// V2 (locked) above V1, and A1: V1 0..10 s, V2 2..4 s, A1 0..10 s.
    fn seq() -> (Sequence, [ClipId; 3]) {
        let mut s = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.locked = true;
        s.tracks.insert(1, v2);
        let clip = |from: i64, to: i64| Clip::new(AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(to - from)), Time::from_secs(from));
        let (c1, c2, c3) = (clip(0, 10), clip(2, 4), clip(0, 10));
        let ids = [c1.id, c2.id, c3.id];
        s.tracks[0].clips.push(c1);
        s.tracks[1].clips.push(c2);
        s.tracks[2].clips.push(c3);
        (s, ids)
    }

    #[test]
    fn categories_hide_empty_ones_and_search_matches_either_language() {
        assert_eq!(categories(), vec![Category::Motion], "E1: motion only");
        assert_eq!(visible_recipes("", "").len(), 5);
        assert!(visible_recipes("look", "").is_empty());
        let ids = |v: Vec<&'static RecipeDef>| v.into_iter().map(|d| d.id).collect::<Vec<_>>();
        assert_eq!(ids(visible_recipes("", "наезд")), ["smooth_zoom", "cinematic_push", "punch_in"]);
        assert_eq!(ids(visible_recipes("motion", "HAND")), ["handheld"], "English name, any case");
        assert_eq!(ids(visible_recipes("", "  Ручная ")), ["handheld"], "trimmed");
    }

    /// Review focus 1: a card filtered out under a resting pointer is
    /// destroyed without a pointer leave — its hover (and preview) ends here.
    #[test]
    fn a_hovered_card_filtered_out_ends_its_hover() {
        let listed = visible_recipes("", "наезд");
        assert!(!hover_gone(Some("smooth_zoom"), &listed));
        assert!(hover_gone(Some("handheld"), &listed), "searched away");
        assert!(hover_gone(Some("handheld"), &[]), "no cards at all");
        assert!(!hover_gone(None, &[]), "nothing hovered");
    }

    /// Review focus 3: a drop lands only on the clip under the pointer.
    #[test]
    fn a_drop_hits_only_the_clip_under_the_pointer_that_fits() {
        let (s, [v1, _, _]) = seq();
        let rs = rows(&s, 0.0);
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let y_of = |track: usize| rs.iter().find(|r| r.track == track).map(|r| r.y + 5.0).unwrap();
        assert_eq!(drop_hit(&s, &rs, Time::from_secs(5), y_of(0), def), DropHit::Clip(v1));
        assert_eq!(drop_hit(&s, &rs, Time::from_secs(3), y_of(1), def), DropHit::Locked);
        assert_eq!(drop_hit(&s, &rs, Time::from_secs(8), y_of(1), def), DropHit::Nothing, "a gap on V2 is not the clip below it");
        assert_eq!(drop_hit(&s, &rs, Time::from_secs(5), y_of(2), def), DropHit::Unfit, "a sound clip");
        let below = rs.last().map(|r| r.y + r.h + 10.0).unwrap();
        assert_eq!(drop_hit(&s, &rs, Time::from_secs(5), below, def), DropHit::Nothing, "below the last track: no row — never the top one");
        assert_eq!(drop_hit(&s, &rs, Time::from_secs(5), -3.0, def), DropHit::Nothing);
    }

    #[test]
    fn a_click_applies_to_fitting_selected_clips_in_one_entry_only_when_it_changes_something() {
        let (s, [v1, v2, a1]) = seq();
        let def = kadr_effects::recipe("handheld").unwrap();
        assert_eq!(targets(&s, &[v1, v2, a1], def), vec![v1], "V2 is locked, A1 is sound");
        assert_eq!(card_hint(&s, &[a1], def), Some("fx.hint.unfit"));
        assert_eq!(card_hint(&s, &[], def), None, "nothing selected: the card stays usable (drag)");
        let Some(EditCommand::Batch { label, commands }) = apply_batch(&s, def, &[v1], Time::from_secs(1)) else { panic!("a batch") };
        assert_eq!(commands.len(), 1);
        assert_eq!(label, tf("cmd.effect_named", &[("name", &t("fx.handheld"))]));
        let mut applied = s.clone();
        let (ti, ci) = applied.locate_clip(v1).unwrap();
        let c = applied.tracks[ti].clips[ci].clone();
        applied.tracks[ti].clips[ci].put_effect(kadr_effects::fresh(def, &c, Time::from_secs(1)));
        assert!(apply_batch(&applied, def, &[v1], Time::from_secs(1)).is_none(), "already there: no command, no empty history entry");
    }

    /// A card over a selection on a locked track says why it is inactive
    /// (not "for video clips"); a click after a hover applies the instance
    /// the preview showed — Handheld's noise is seeded by its id.
    #[test]
    fn a_locked_selection_says_so_and_a_click_applies_what_the_hover_showed() {
        let (s, [v1, v2, a1]) = seq();
        let def = kadr_effects::recipe("handheld").unwrap();
        assert_eq!(card_hint(&s, &[v2], def), Some("fx.drop.locked"));
        assert_eq!(card_hint(&s, &[v2, a1], def), Some("fx.drop.locked"), "the video clip it fits is locked");
        assert_eq!(card_hint(&s, &[v1, a1], def), None, "V1 takes it");
        let shown = kadr_effects::fresh(def, s.clip(v1).unwrap(), Time::from_secs(1));
        let Some(EditCommand::Batch { commands, .. }) = batch_of(&s, def, vec![(v1, shown.clone())]) else { panic!("a batch") };
        assert_eq!(commands, vec![EditCommand::AddEffect { clip: v1, effect: shown.clone() }]);
        let mut applied = s.clone();
        let o = PreviewOverride { recipe: def.id, at: Time::from_secs(1), effects: vec![(v1, shown)] };
        o.apply(&mut applied);
        assert!(batch_of(&applied, def, o.effects.clone()).is_none(), "applied already: nothing to send");
    }

    /// A hover follows the playhead (M1): a punch's moment is made again at
    /// the new playhead with the same instance, so a click applies the punch
    /// the preview shows now; an effect without a moment is not made again,
    /// and when it is (another selection) its id — Handheld's seed — stays.
    #[test]
    fn a_hover_follows_the_playhead_with_the_same_instances() {
        let (s, [v1, _, _]) = seq();
        let at = Time::from_secs;
        let punch = kadr_effects::recipe("punch_in").unwrap();
        let o = make_override(&s, punch, &[v1], at(1), None).unwrap();
        assert!(!o.outdated(punch, &[v1], at(1)));
        assert!(o.outdated(punch, &[v1], at(3)), "its moment is the frame at 1 s");
        assert!(o.outdated(punch, &[], at(1)), "nothing selected any more");
        let moved = make_override(&s, punch, &[v1], at(3), Some(&o)).unwrap();
        assert_eq!(moved.at, at(3));
        assert_eq!(moved.effects[0].1.id, o.effects[0].1.id, "the same instance");
        assert_eq!(moved.effects[0].1.params["moment"], ParamValue::Moment(SourceTime(at(3))), "the punch lands at the new playhead");
        let Some(EditCommand::Batch { commands, .. }) = batch_of(&s, punch, moved.effects.clone()) else { panic!("a batch") };
        assert_eq!(commands, vec![EditCommand::AddEffect { clip: v1, effect: moved.effects[0].1.clone() }], "a click applies what is shown now");

        let hand = kadr_effects::recipe("handheld").unwrap();
        let h = make_override(&s, hand, &[v1], at(1), None).unwrap();
        assert!(!h.outdated(hand, &[v1], at(3)), "no moment: the playhead changes nothing");
        assert_eq!(make_override(&s, hand, &[v1], at(3), Some(&h)), Some(PreviewOverride { at: at(3), ..h.clone() }), "made again: the same shake");
        assert_ne!(make_override(&s, hand, &[v1], at(1), Some(&o)).unwrap().effects[0].1.id, o.effects[0].1.id, "another recipe's instance is not reused");
        assert!(make_override(&s, hand, &[], at(1), Some(&h)).is_none(), "on no clip: none");
    }

    fn project_with_clip() -> (Project, ClipId) {
        let mut p = Project::new("t");
        let video = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
        let a = MediaAsset::new("a.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None });
        let c = Clip::new(a.id, "c", TimeRange::new(Time::ZERO, Time::from_secs(8)), Time::ZERO);
        let id = c.id;
        p.assets.push(a);
        p.sequence_mut().tracks[0].clips.push(c);
        (p, id)
    }

    /// Review focus 1: the hover preview changes the preview's snapshot
    /// only — the project (and so history, "modified", autosave) stays.
    #[test]
    fn the_hover_override_lives_in_the_snapshot_only() {
        let (p, id) = project_with_clip();
        let before = p.clone();
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let o = PreviewOverride { recipe: def.id, at: Time::ZERO, effects: vec![(id, kadr_effects::fresh(def, p.sequence().clip(id).unwrap(), Time::ZERO))] };
        let out = OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewFast);
        let hovered = crate::scene_source::ProjectScenes::new_with(&p, false, |seq| o.apply(seq));
        assert!(hovered.scene_at(Time::from_secs(4), &out).layers[0].placement.scale.x > 1.0, "the snapshot shows it");
        assert_eq!(p, before, "the project does not");
        assert_eq!(crate::scene_source::ProjectScenes::new(&p, false).scene_at(Time::from_secs(4), &out).layers[0].placement.scale.x, 1.0, "nor does the next snapshot");
        assert_eq!(o.clips(), vec![id]);
    }

    /// Timeline 10..20 s showing source 30..40 s.
    fn clip_10_20() -> Clip {
        Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10))
    }

    #[test]
    fn parameter_rows_follow_the_playhead_and_the_effect() {
        let mut c = clip_10_20();
        let def = kadr_effects::recipe("punch_in").unwrap();
        let mut e = kadr_effects::fresh(def, &c, Time::from_secs(12));
        e.params.insert("strength".into(), ParamValue::Number(0.9));
        c.effects.push(e.clone());
        let row = |key: &str, playhead: i64| param_row(&c, &e, def.param(key).unwrap(), Time::from_secs(playhead), FrameRate::FPS_30, false);
        let s = row("strength", 12);
        assert_eq!((s.kind, s.value, s.strength, s.prop.to_string()), (0, 0.9, true, format!("fx:{}:strength", e.id)));
        assert_eq!((s.minimum, s.maximum, s.default_value), (0.0, 1.0, 0.6));
        assert_eq!((s.k.state, s.k.enabled), (0, true));
        assert!(!row("strength", 25).k.enabled, "off the clip: ◆ is disabled");
        let m = row("moment", 12);
        assert_eq!((m.kind, m.can_act, m.hint.as_str()), (3, false, ""), "already the playhead's frame: nothing to do");
        assert!(row("moment", 15).can_act);
        assert!(!row("moment", 25).can_act, "off the clip");
        let p = row("point", 12);
        assert_eq!((p.kind, p.text.as_str(), p.can_act, p.active), (2, "50%, 50%", true, false), "written as the sliders write a percentage");
        assert!(!row("point", 25).can_act);
        let zoom = kadr_effects::recipe("smooth_zoom").unwrap();
        let z = kadr_effects::fresh(zoom, &c, Time::from_secs(12));
        let d = param_row(&c, &z, zoom.param("direction").unwrap(), Time::from_secs(12), FrameRate::FPS_30, false);
        assert_eq!((d.kind, d.current), (1, 0));
        assert_eq!(d.options.iter().collect::<Vec<_>>(), vec![slint::SharedString::from("in"), slint::SharedString::from("out")]);
    }

    /// «К плейхеду» is enabled only off the moment's frame: a playhead move
    /// must refresh the block even on a clip without keyframes.
    #[test]
    fn a_moment_row_follows_the_playhead() {
        let mut c = clip_10_20();
        assert!(!has_moment(&c));
        c.effects.push(kadr_effects::fresh(kadr_effects::recipe("smooth_zoom").unwrap(), &c, Time::from_secs(12)));
        assert!(!has_moment(&c), "no moment: nothing depends on the playhead's frame");
        c.effects.push(AppliedEffect::new("punch_in", 9, BTreeMap::new()));
        assert!(!has_moment(&c), "a newer Kadr's version shows no rows");
        c.effects.push(kadr_effects::fresh(kadr_effects::recipe("punch_in").unwrap(), &c, Time::from_secs(12)));
        assert!(has_moment(&c));
    }

    /// M6: off the clip the effects section says why its keyframed numbers
    /// are locked — only when one is (Transform's own hint covers the rest).
    #[test]
    fn the_effects_section_repeats_the_outside_hint_when_its_rows_are_locked() {
        let mut c = clip_10_20();
        let frame = FrameRate::FPS_30.frame_duration();
        let z = kadr_effects::fresh(kadr_effects::recipe("smooth_zoom").unwrap(), &c, Time::from_secs(12));
        c.effects.push(z.clone());
        c.animation.set_key(kadr_project::AnimParam::Scale, SourceTime(Time::from_secs(31)), 0.5, None);
        assert!(!fx_outside_hint(&c, Time::from_secs(25), frame), "only the scale has keyframes");
        c.animation.set_key(TrackKey::fx(z.id, "strength"), SourceTime(Time::from_secs(31)), 0.3, None);
        assert!(fx_outside_hint(&c, Time::from_secs(25), frame));
        assert!(fx_outside_hint(&c, Time::from_secs(5), frame), "before the clip too");
        assert!(!fx_outside_hint(&c, Time::from_secs(12), frame), "on the clip: the rows work");
    }

    /// Review focus 4: a moment the trim left outside the visible clip says so.
    #[test]
    fn a_moment_off_the_visible_clip_is_flagged() {
        let c = clip_10_20();
        let def = kadr_effects::recipe("punch_in").unwrap();
        let mut e = kadr_effects::fresh(def, &c, Time::from_secs(12));
        e.params.insert("moment".into(), ParamValue::Moment(SourceTime(Time::from_secs(45))));
        let m = param_row(&c, &e, def.param("moment").unwrap(), Time::from_secs(12), FrameRate::FPS_30, false);
        assert_eq!(m.hint.to_string(), t("fx.moment.outside"));
        assert!(m.can_act, "«к плейхеду» brings it back");
    }

    #[test]
    fn resetting_an_effect_restores_defaults_and_drops_its_keyframes() {
        let mut c = clip_10_20();
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let e = kadr_effects::fresh(def, &c, Time::from_secs(12));
        c.effects.push(e.clone());
        assert!(reset_commands(&c, &e, def, Time::from_secs(12)).is_none(), "already the defaults: no command");
        let mut out = e.clone();
        out.params.insert("direction".into(), ParamValue::Choice("out".into()));
        c.effects[0] = out.clone();
        assert!(matches!(reset_commands(&c, &out, def, Time::from_secs(12)).as_deref(), Some([EditCommand::UpdateEffect { .. }])));
        let k = TrackKey::fx(e.id, "strength");
        c.animation.set_key(k.clone(), SourceTime(Time::from_secs(31)), 0.1, None);
        let cmds = reset_commands(&c, &out, def, Time::from_secs(12)).unwrap();
        assert!(matches!(&cmds[..], [EditCommand::ClearAnimation { statics, .. }, EditCommand::UpdateEffect { .. }] if *statics == vec![(k.clone(), 0.6)]));

        // Ruling R13: a reset keeps the moment where it was put — a hand-placed
        // punch is not re-timed to the playhead, and an untouched Punch In
        // stays untouched wherever the playhead is.
        let punch = kadr_effects::recipe("punch_in").unwrap();
        let mut p = kadr_effects::fresh(punch, &c, Time::from_secs(12));
        c.effects.push(p.clone());
        assert!(reset_commands(&c, &p, punch, Time::from_secs(18)).is_none(), "untouched: the playhead elsewhere changes nothing");
        p.params.insert("strength".into(), ParamValue::Number(0.9));
        p.params.insert("moment".into(), ParamValue::Moment(SourceTime(Time::from_secs(37))));
        c.effects[1] = p.clone();
        let cmds = reset_commands(&c, &p, punch, Time::from_secs(18));
        let Some([EditCommand::UpdateEffect { params, .. }]) = cmds.as_deref() else { panic!("one UpdateEffect") };
        assert_eq!(params["strength"], ParamValue::Number(0.6), "the settings go back");
        assert_eq!(params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(37))), "the moment stays");
        assert_eq!(params["point"], ParamValue::Point { x: 0.5, y: 0.5 });
    }

    /// R14: «Сбросить» in the ⋯ menu is greyed out exactly when it would do nothing.
    #[test]
    fn the_reset_item_greys_out_when_nothing_would_change() {
        let mut c = clip_10_20();
        let punch = kadr_effects::recipe("punch_in").unwrap();
        let mut p = kadr_effects::fresh(punch, &c, Time::from_secs(12));
        let future = AppliedEffect::new("glitch_v2", 3, BTreeMap::new());
        c.effects = vec![p.clone(), future.clone()];
        assert!(!can_reset(&c, &p, Time::from_secs(18)), "defaults, the moment where it was put");
        assert!(!can_reset(&c, &future, Time::from_secs(12)), "a newer Kadr's effect is not changed");
        c.animation.set_key(TrackKey::fx(p.id, "strength"), SourceTime(Time::from_secs(33)), 0.6, None);
        assert!(can_reset(&c, &p, Time::from_secs(18)), "keyframes to drop");
        c.animation = Default::default();
        p.params.insert("point".into(), ParamValue::Point { x: 0.2, y: 0.5 });
        c.effects[0] = p.clone();
        assert!(can_reset(&c, &p, Time::from_secs(18)), "a setting to restore");
    }

    /// R14: «к плейхеду» is enabled exactly when a click does something —
    /// the row and the action derive the target frame the same way.
    #[test]
    fn to_the_playhead_is_enabled_exactly_when_it_does_something() {
        let mut c = clip_10_20();
        c.source_out = Time::from_secs(50); // 2×: timeline 10..20 s = source 30..50 s
        let punch = kadr_effects::recipe("punch_in").unwrap();
        let mut e = kadr_effects::fresh(punch, &c, Time::from_millis(12_010));
        c.effects.push(e.clone());
        let def = punch.param("moment").unwrap();
        let check = |c: &Clip, e: &AppliedEffect| {
            for ms in [5_000, 9_999, 10_000, 12_000, 12_010, 12_011, 15_000, 19_999, 20_000, 25_000] {
                let at = Time::from_millis(ms);
                let row = param_row(c, e, def, at, FrameRate::FPS_30, false);
                assert_eq!(row.can_act, action_commands(c, e.id, "moment:moment", at, FrameRate::FPS_30).is_some(), "{ms} ms");
            }
        };
        check(&c, &e);
        assert!(!param_row(&c, &e, def, Time::from_millis(12_010), FrameRate::FPS_30, false).can_act, "already there");
        e.params.insert("moment".into(), ParamValue::Moment(SourceTime(Time::from_secs(55))));
        c.effects[0] = e.clone();
        check(&c, &e);
        e.params.remove("moment");
        c.effects[0] = e.clone();
        check(&c, &e);
    }

    /// One command per click, none when nothing would change (the engine
    /// records an entry for every Ok); an effect from a newer Kadr is only
    /// moved or removed (spec §7, review focus 5).
    #[test]
    fn block_actions_send_one_command_or_none_when_nothing_changes() {
        let mut c = clip_10_20();
        let (zoom, punch) = (kadr_effects::recipe("smooth_zoom").unwrap(), kadr_effects::recipe("punch_in").unwrap());
        let at = Time::from_secs(12);
        let (z, p) = (kadr_effects::fresh(zoom, &c, at), kadr_effects::fresh(punch, &c, at));
        let future = AppliedEffect::new("glitch_v2", 3, [("amount".to_string(), ParamValue::Number(0.5))].into());
        c.effects = vec![z.clone(), p.clone(), future.clone()];
        let act = |id, a: &str, playhead: i64| action_commands(&c, id, a, Time::from_secs(playhead), FrameRate::FPS_30);
        let update = |id, params: BTreeMap<String, ParamValue>, enabled| vec![EditCommand::UpdateEffect { clip: c.id, id, params, enabled }];
        let named = |key: &str| tf(key, &[("name", &t("fx.smooth_zoom"))]);

        assert_eq!(act(z.id, "toggle", 12), Some((named("cmd.effect_off_named"), update(z.id, BTreeMap::new(), Some(false)))));
        assert_eq!(act(z.id, "remove", 12), Some((named("cmd.remove_effect_named"), vec![EditCommand::RemoveEffect { clip: c.id, id: z.id }])));
        assert_eq!(act(z.id, "up", 12), None, "the first has nothing above");
        assert_eq!(act(z.id, "down", 12).map(|a| a.1), Some(vec![EditCommand::MoveEffect { clip: c.id, id: z.id, to: 1 }]));
        assert_eq!(act(z.id, "reset", 12), None, "already the defaults");

        assert_eq!(act(z.id, "choice:direction:0", 12), None, "already «in»");
        let out = BTreeMap::from([("direction".to_string(), ParamValue::Choice("out".into()))]);
        assert_eq!(act(z.id, "choice:direction:1", 12), Some((named("cmd.update_effect_named"), update(z.id, out, None))), "a change of a setting, not applying the effect");
        for bad in ["choice:direction:2", "choice:direction:x", "choice:strength:0", "choice:nope:0", "pick:point", "nonsense"] {
            assert_eq!(act(z.id, bad, 12), None, "{bad}");
        }

        assert_eq!(act(p.id, "moment:moment", 12), None, "already the playhead's frame");
        assert_eq!(act(p.id, "moment:moment", 25), None, "the playhead is off the clip");
        assert_eq!(act(p.id, "moment:strength", 15), None, "not a moment");
        let at_15 = BTreeMap::from([("moment".to_string(), ParamValue::Moment(SourceTime(Time::from_secs(35))))]);
        assert_eq!(act(p.id, "moment:moment", 15).map(|a| a.1), Some(update(p.id, at_15, None)), "source-bound: 15 s on the timeline is 35 s of the source");

        for locked in ["toggle", "reset", "choice:direction:0", "moment:moment"] {
            assert_eq!(act(future.id, locked, 15), None, "{locked}: a newer Kadr's effect is not changed");
        }
        assert!(act(future.id, "remove", 12).is_some());
        assert_eq!(act(future.id, "up", 12).map(|a| a.1), Some(vec![EditCommand::MoveEffect { clip: c.id, id: future.id, to: 1 }]));
        assert_eq!(act(future.id, "down", 12), None, "the last has nothing below");
        assert_eq!(act(kadr_core::EffectId::new(), "remove", 12), None, "not on the clip");
    }

    /// The effect's reset and the section's reset are one undo step each,
    /// keyframes included (spec §6.2, keyframes spec §5).
    #[test]
    fn resets_are_one_undo_step_with_their_keyframes() {
        use kadr_timeline::EditEngine;
        let (mut p, id) = project_with_clip();
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let (ti, ci) = p.sequence().locate_clip(id).unwrap();
        let c = &mut p.sequence_mut().tracks[ti].clips[ci];
        let mut e = kadr_effects::fresh(def, c, Time::from_secs(1));
        e.params.insert("direction".into(), ParamValue::Choice("out".into()));
        let k = TrackKey::fx(e.id, "strength");
        c.effects.push(e.clone());
        c.effects.push(kadr_effects::fresh(kadr_effects::recipe("handheld").unwrap(), &c.clone(), Time::from_secs(1)));
        c.animation.set_key(&k, SourceTime(Time::from_secs(1)), 0.2, None);
        c.animation.set_key(&k, SourceTime(Time::from_secs(3)), 0.9, None);
        let before = p.sequence().clone();
        let clip = |p: &Project| p.sequence().clip(id).unwrap().clone();
        let mut engine = EditEngine::new();

        let (label, commands) = action_commands(&clip(&p), e.id, "reset", Time::from_secs(1), FrameRate::FPS_30).unwrap();
        engine.execute(&mut p, EditCommand::Batch { label, commands }).unwrap();
        let after = clip(&p);
        assert_eq!(after.effects[0].params, kadr_effects::fresh(def, &after, Time::from_secs(1)).params, "the defaults");
        assert!(!after.animation.is_animated(&k), "and no keyframes");
        assert!(action_commands(&after, e.id, "reset", Time::from_secs(1), FrameRate::FPS_30).is_none(), "a second reset has nothing to do");
        engine.undo(&mut p);
        assert_eq!(*p.sequence(), before, "one undo brings back values and keyframes");

        let clear = clear_batch(&clip(&p)).unwrap();
        engine.execute(&mut p, clear).unwrap();
        let cleared = clip(&p);
        assert!(cleared.effects.is_empty() && !cleared.animation.has_effect_tracks(e.id), "every effect and its tracks");
        assert!(clear_batch(&cleared).is_none(), "nothing left: no command");
        engine.undo(&mut p);
        assert_eq!(*p.sequence(), before, "one undo step");
    }

    /// The inspector refreshes ~10 times a second during playback: rows
    /// that did not change keep their models (a new one would rebuild the
    /// elements and lose a press in flight).
    #[test]
    fn a_refresh_keeps_the_parameter_models_on_screen() {
        let c = clip_10_20();
        let zoom = kadr_effects::recipe("smooth_zoom").unwrap();
        let z = kadr_effects::fresh(zoom, &c, Time::from_secs(12));
        let rows = |playhead: i64| zoom.params.iter().map(|p| param_row(&c, &z, p, Time::from_secs(playhead), FrameRate::FPS_30, false)).collect::<Vec<_>>();
        let first = reuse_param_rows(None, rows(12));
        let (options, tips, empty) = (first.row_data(0).unwrap().options, first.row_data(0).unwrap().tips, first.row_data(1).unwrap().options);
        let again = reuse_param_rows(Some(first.clone()), rows(12));
        assert!(again == first, "the same parameter model");
        assert!(again.row_data(0).unwrap().options == options && again.row_data(0).unwrap().tips == tips, "the same option lists");
        assert!(again.row_data(1).unwrap().options == empty, "an empty list stays as it is");
        let off = reuse_param_rows(Some(first.clone()), rows(25));
        assert!(off == first, "a changed row is updated in place");
        assert!(!off.row_data(1).unwrap().k.enabled);
    }

    fn layer_with(placement: Placement, crop: Option<RectF>) -> Layer {
        Layer { id: LayerId(1), content: LayerContent::Solid(Rgba::BLACK), crop: crop.unwrap_or(placement.full_crop()), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
    }

    #[test]
    fn a_click_picks_the_point_of_the_picture_as_shown() {
        let canvas = SizeU::new(1920, 1080);
        let base = Placement::fill(canvas);
        assert_eq!(pick_point(&layer_with(base, None), canvas, 0.25, 0.75), Some((0.25, 0.75)));
        // Zoomed ×2 about the centre: a click a quarter across shows the picture's 3/8.
        let zoomed = PlacementDelta { scale: 2.0, ..PlacementDelta::IDENTITY }.apply(&base);
        let (x, y) = pick_point(&layer_with(zoomed, None), canvas, 0.25, 0.5).unwrap();
        assert!((x - 0.375).abs() < 1e-6 && (y - 0.5).abs() < 1e-6, "{x} {y}");
        let back = point_on_canvas(&layer_with(zoomed, None), canvas, (0.375, 0.5));
        assert!((back.0 - 0.25).abs() < 1e-6 && (back.1 - 0.5).abs() < 1e-6, "the marker lands where the click was");
    }

    /// Review focus 4: outside the clip's visible picture nothing is picked.
    #[test]
    fn a_click_off_the_picture_picks_nothing() {
        let canvas = SizeU::new(1920, 1080);
        // A vertical video pillarboxed in a 16:9 frame: 607.5 px wide in the middle.
        let p = Placement { size: Vec2::new(607.5, 1080.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(960.0, 540.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 };
        assert!(pick_point(&layer_with(p, None), canvas, 0.1, 0.5).is_none(), "the black bar");
        assert!(pick_point(&layer_with(p, None), canvas, 0.5, 0.5).is_some());
        let left_cropped = RectF::new(303.75, 0.0, 607.5, 1080.0);
        assert!(pick_point(&layer_with(p, Some(left_cropped)), canvas, 0.45, 0.5).is_none(), "a cropped-away part");
        assert!(pick_point(&layer_with(p, Some(left_cropped)), canvas, 0.55, 0.5).is_some());
        // M5: the whole viewport takes the click; the margin round the frame
        // comes as fractions outside 0..1 — a miss, even for a full-frame clip.
        let full = layer_with(Placement::fill(canvas), None);
        for (x, y) in [(-0.02, 0.5), (1.03, 0.5), (0.5, -0.1), (0.5, 1.2), (f64::NAN, 0.5)] {
            assert!(pick_point(&full, canvas, x, y).is_none(), "{x} {y}");
        }
    }

    /// M2: a pick that finds no picture of its clip says why — the playhead
    /// is off the clip, or the clip is not seen in this frame (under a
    /// full-frame opaque layer, switched off).
    #[test]
    fn a_pick_without_the_clips_picture_says_why() {
        let c = clip_10_20();
        let frame = FrameRate::FPS_30.frame_duration();
        assert_eq!(unseen_key(Some(&c), Time::from_secs(12), frame), "fx.pick.hidden");
        assert_eq!(unseen_key(Some(&c), Time::from_secs(25), frame), "fx.pick.off_clip");
        assert_eq!(unseen_key(Some(&c), Time::from_secs(20), frame), "fx.pick.off_clip", "its out point is past its last frame");
        assert_eq!(unseen_key(None, Time::from_secs(12), frame), "fx.pick.off_clip", "the clip is gone");
        for lang in Lang::ALL {
            let keys = kadr_i18n::all_keys(lang);
            for key in ["fx.pick.hidden", "fx.pick.off_clip", "fx.pick.miss"] {
                assert!(keys.contains(&key), "{lang:?} lacks {key}");
            }
        }
    }

    #[test]
    fn the_layer_of_a_clip_is_found_inside_a_transition_too() {
        let clip = ClipId::new();
        let mut inner = layer_with(Placement::fill(SizeU::new(100, 100)), None);
        inner.id = LayerId::from(clip);
        let tr = Layer {
            id: LayerId(9),
            content: LayerContent::Transition(Box::new(TransitionLayer { op: TransitionOp::Dissolve, progress: 0.5, from: vec![], to: vec![inner.clone()] })),
            ..layer_with(Placement::fill(SizeU::new(100, 100)), None)
        };
        assert_eq!(layer_of(std::slice::from_ref(&tr), clip), Some(&inner));
        assert_eq!(layer_of(&[], clip), None);
    }

    /// Ruling R15: the marker shows the effective anchor — the stored point
    /// clamped into the clip's visible crop window (ruling R6) — where the
    /// zoom really holds still, not a cropped-away spot.
    #[test]
    fn the_marker_shows_the_point_clamped_into_the_crop_window() {
        let canvas = SizeU::new(1920, 1080);
        let base = Placement::fill(canvas);
        let left_quarter_cropped = RectF::new(480.0, 0.0, 1920.0, 1080.0);
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6;
        let m = marker_on_canvas(&layer_with(base, Some(left_quarter_cropped)), canvas, (0.1, 0.5));
        assert!(close(m, (0.25, 0.5)), "cropped away: drawn at the window's edge, {m:?}");
        let m = marker_on_canvas(&layer_with(base, Some(left_quarter_cropped)), canvas, (0.6, 0.3));
        assert!(close(m, (0.6, 0.3)), "inside the window: the point itself, {m:?}");
        let m = marker_on_canvas(&layer_with(base, None), canvas, (0.1, 0.5));
        assert!(close(m, (0.1, 0.5)), "no crop: the point itself, {m:?}");

        // The real thing: Smooth Zoom about (0.1, 0.5) of a clip whose left
        // quarter is cropped away. The marker stays on one spot of the
        // screen while the picture grows around it; the stored point drifts.
        let (mut p, id) = project_with_clip();
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let (ti, ci) = p.sequence().locate_clip(id).unwrap();
        let c = &mut p.sequence_mut().tracks[ti].clips[ci];
        c.transform.crop_left = 0.25;
        let mut e = kadr_effects::fresh(def, c, Time::ZERO);
        e.params.insert("point".into(), ParamValue::Point { x: 0.1, y: 0.5 });
        c.effects.push(e);
        let out = OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewFast);
        let scenes = crate::scene_source::ProjectScenes::new(&p, false);
        let at = |secs: i64| {
            let scene = scenes.scene_at(Time::from_secs(secs), &out);
            (layer_of(&scene.layers, id).unwrap().clone(), scene.canvas)
        };
        let ((start, canvas), (later, _)) = (at(0), at(6));
        assert!(later.placement.scale.x > 1.05, "zoomed in by 6 s");
        let (m0, m6) = (marker_on_canvas(&start, canvas, (0.1, 0.5)), marker_on_canvas(&later, canvas, (0.1, 0.5)));
        assert!(close(m0, (0.25, 0.5)) && close(m0, m6), "the marker is where the zoom holds still: {m0:?} {m6:?}");
        let raw = point_on_canvas(&later, canvas, (0.1, 0.5));
        assert!((raw.0 - 0.1).abs() > 0.005, "the cropped-away point itself moves: {raw:?}");
    }

    #[test]
    fn the_badge_counts_effects_and_its_tip_names_them_in_order() {
        let mut c = clip_10_20();
        assert_eq!(fx_badge(&c), "");
        let mut off = AppliedEffect::new("handheld", 1, Default::default());
        off.enabled = false;
        c.effects = vec![AppliedEffect::new("smooth_zoom", 1, Default::default()), off, AppliedEffect::new("glitch_v2", 1, Default::default())];
        assert_eq!(fx_badge(&c), "fx 3");
        let list = [t("fx.smooth_zoom"), tf("fx.tip.off", &[("name", &t("fx.handheld"))]), tf("fx.tip.unavailable", &[("name", "glitch_v2")])].join(", ");
        assert_eq!(fx_tip(&c), tf("fx.tip", &[("list", &list)]));
        // A known recipe at a newer version cannot run here either.
        c.effects = vec![AppliedEffect::new("smooth_zoom", 99, Default::default())];
        assert_eq!(fx_tip(&c), tf("fx.tip", &[("list", &tf("fx.tip.unavailable", &[("name", &t("fx.smooth_zoom"))]))]));
    }

    /// The badge's span — where Rust draws it (`ClipView.fx-x`) and where its
    /// tooltip answers (R16): right-aligned in the name bar, left of the
    /// fade-out handle when that handle can appear.
    #[test]
    fn the_badge_sits_left_of_the_fade_out_handle_where_it_can_appear() {
        assert_eq!(badge_span(200.0, 0.0, false), (164.0, 194.0));
        let handle = anim_ui::handle_x(0.0, 200.0, 50.0, kadr_project::FadeEdge::Out);
        let (a, b) = badge_span(200.0, 50.0, true);
        assert_eq!(b, 200.0 - (200.0 - handle + 7.0));
        assert_eq!(b - a, BADGE_W);
    }

    /// The badge is drawn (and hit) only where the name bar has room for it
    /// beside the link icon, the speed label and the fade-in handle that
    /// stays on screen; the fade-out handle's room is always kept.
    #[test]
    fn the_badge_hides_where_the_name_bar_has_no_room() {
        let mut c = clip_10_20();
        c.effects = vec![AppliedEffect::new("smooth_zoom", 1, Default::default())];
        let place = |c: &Clip, w: f64, selected: bool| badge_place(c, w, 40.0, selected, false, false);
        assert_eq!(place(&c, 200.0, false), Some((151.0, 181.0)), "clear of the fade-out handle that hover shows");
        assert_eq!(place(&c, 82.0, false), None, "too narrow for the name and the badge");
        assert!(place(&c, 83.0, false).is_some());
        assert_eq!(place(&c, 95.0, true), None, "selected: the fade-in handle takes its room too");
        assert!(place(&c, 96.0, true).is_some());
        c.link = Some(kadr_core::LinkId::new());
        assert_eq!(place(&c, 97.0, false), None, "the link icon takes room too");
        assert!(place(&c, 98.0, false).is_some());
        // Locked track or blade mode: no handles at all, so no room is kept for them.
        c.link = None;
        for (locked, blade) in [(true, false), (false, true)] {
            assert_eq!(badge_place(&c, 69.0, 40.0, true, locked, blade), None);
            assert_eq!(badge_place(&c, 70.0, 40.0, true, locked, blade), Some((34.0, 64.0)));
        }
        c.effects.clear();
        assert_eq!(place(&c, 200.0, false), None, "no effects, no badge");
    }

    /// Hovering a clip shows both fade handles, a zero fade's too: the badge
    /// and the handle's grab zone never meet, at any width, and the badge
    /// does not slide between rest, hover and selection.
    #[test]
    fn a_handle_shown_on_hover_never_meets_the_badge() {
        let mut c = clip_10_20();
        c.effects = vec![AppliedEffect::new("smooth_zoom", 1, Default::default())];
        for w in 40..400 {
            let w = w as f64;
            let rest = badge_place(&c, w, 40.0, false, false, false);
            if let (Some(rest), Some(selected)) = (rest, badge_place(&c, w, 40.0, true, false, false)) {
                assert_eq!(rest, selected, "{w}: the badge stays put on selection");
            }
            let Some((a, b)) = rest else { continue };
            let handle = anim_ui::handle_x(0.0, w, 0.0, kadr_project::FadeEdge::Out);
            assert!(b + anim_ui::FADE_R < handle, "{w}: grab zone ({}..) vs badge ..{b}", handle - anim_ui::FADE_R);
            assert!(a >= 0.0 && b <= w);
        }
    }

    /// The speed label never elides: where the badge is shown, the row of
    /// link icon, name, speed label and spacing fits left of it.
    #[test]
    fn a_speed_changed_clip_keeps_its_badge_in_place() {
        let mut c = clip_10_20();
        c.effects = vec![AppliedEffect::new("smooth_zoom", 1, Default::default())];
        c.link = Some(kadr_core::LinkId::new());
        c.timeline_out = c.timeline_in + Time::from_secs(5); // 10 s of source in 5 s
        assert!((c.speed() - 2.0).abs() < 1e-9);
        const NAME_MIN: f64 = 24.0;
        const LINK: f64 = 11.0 + 4.0;
        const SPEED_TEXT: f64 = 30.0; // "2.00×" at the timeline's small font, a generous measure
        for selected in [false, true] {
            for w in 40..500 {
                let w = w as f64;
                let Some((a, b)) = badge_place(&c, w, 40.0, selected, false, false) else { continue };
                let pad_left = if selected { 19.0 } else { 6.0 };
                let row = pad_left + LINK + NAME_MIN + 4.0 + SPEED_TEXT;
                assert!(row + 4.0 <= a, "{w} selected={selected}: the row ({row}) spills under the badge ({a}..{b})");
                assert_eq!(b - a, BADGE_W);
            }
        }
    }

    #[test]
    fn a_fade_out_keeps_the_badge_clear_of_the_handle_zone() {
        let mut c = clip_10_20();
        c.effects = vec![AppliedEffect::new("smooth_zoom", 1, Default::default())];
        c.video_fade.fade_out = Time::from_secs(2);
        let (_, b) = badge_place(&c, 400.0, 40.0, false, false, false).unwrap();
        let handle = anim_ui::handle_x(0.0, 400.0, 80.0, kadr_project::FadeEdge::Out);
        assert!(b + anim_ui::FADE_R < handle, "the grab zone ({}..) starts right of the badge ({b})", handle - anim_ui::FADE_R);
    }

    #[test]
    fn every_library_string_exists_in_both_catalogs() {
        for lang in Lang::ALL {
            let have: std::collections::HashSet<&str> = kadr_i18n::all_keys(lang).into_iter().collect();
            let mut need: Vec<String> = vec!["fx.cat.all".into()];
            need.extend(Category::ALL.map(|c| c.label_key()));
            for d in kadr_effects::registry() {
                need.push(d.name_key());
                for p in d.params {
                    need.push(p.label_key());
                    if let kadr_effects::ParamKind::Choice { options, .. } = p.kind {
                        need.extend(options.iter().map(|o| format!("fx.choice.{o}")));
                    }
                }
            }
            let missing: Vec<&String> = need.iter().filter(|k| !have.contains(k.as_str())).collect();
            assert!(missing.is_empty(), "{lang:?} lacks {missing:?}");
        }
    }
}
