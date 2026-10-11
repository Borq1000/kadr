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
use kadr_project::{AppliedEffect, Clip, ParamValue, Sequence, SourceTime, TrackKey};
use kadr_timeline::EditCommand;
use slint::{Model, ModelRc, SharedString, VecModel};
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
    pub effects: Vec<(ClipId, AppliedEffect)>,
}

impl PreviewOverride {
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
            row.text = format!("{:.0} %, {:.0} %", x * 100.0, y * 100.0).into();
            row.can_act = on_clip;
            row.active = picking;
            row.hint = if picking { t("fx.pick.hint") } else if on_clip { String::new() } else { t("fx.pick.off_clip") }.into();
        }
        ParamKind::Moment => {
            row.kind = 3;
            row.can_act = on_clip;
            if let Some(ParamValue::Moment(m)) = e.params.get(p.key) {
                let tl = c.timeline_time_of(m.0);
                row.text = Timecode::from_time(tl.max(Time::ZERO), rate).to_string().into();
                if tl < c.timeline_in || tl >= c.timeline_out {
                    row.hint = t("fx.moment.outside").into();
                }
                row.can_act = on_clip && SourceTime(c.source_time_at(playhead)) != *m;
            }
        }
    }
    row
}

/// Whether a block of `c` shows a moment: its «к плейхеду» depends on the
/// playhead's frame, so the inspector follows the playhead (as it does for
/// keyframes) even on a clip without animation.
pub fn has_moment(c: &Clip) -> bool {
    c.effects.iter().filter_map(kadr_effects::definition).any(|d| d.params.iter().any(|p| p.kind == ParamKind::Moment))
}

/// «Сбросить» of one effect: its defaults (the moment: the frame at `at`)
/// and no keyframes on its numbers — `None` when it is so already.
pub fn reset_commands(c: &Clip, e: &AppliedEffect, def: &RecipeDef, at: Time) -> Option<Vec<EditCommand>> {
    let defaults = kadr_effects::fresh(def, c, at).params;
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
    /// The left panel switched tabs (0 media, 1 effects). Hidden, the tab
    /// renders no thumbnails: the batch in flight stops.
    pub fn fx_tab_changed(&mut self, tab: i32) {
        self.fx_clear_hover();
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
        // shown preview follows the selection: the hovered card's effect on
        // the clips it now fits, or none.
        let gone = hover_gone(self.fx.hover.map(|(r, _)| r), &listed);
        let stale = self.fx.preview.as_ref().is_some_and(|o| kadr_effects::recipe(o.recipe).is_none_or(|d| targets(seq, sel, d) != o.clips()));
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
            // A motion card plays its motion in a short loop while hovered
            // (spec §6.1); another card's loop stops with its timer.
            self.fx.thumbs.loop_frame = 0;
            self.fx.thumbs.timer = (def.category == Category::Motion).then(|| {
                let timer = slint::Timer::default();
                timer.start(slint::TimerMode::Repeated, crate::fx_thumbs::LOOP_TICK, || {
                    crate::app::with_app(|app| app.fx_loop_tick());
                });
                timer
            });
            self.refresh_effects();
        } else if self.fx.hover.is_some_and(|(r, _)| r == def.id) {
            self.fx_clear_hover();
        }
    }

    /// The delayed hover: the card's effect on the selected clips, in the
    /// frame under the playhead (spec §6.1).
    fn fx_hover_fire(&mut self, generation: u64) {
        let Some((recipe, g)) = self.fx.hover else { return };
        if g != generation || self.fx.drag.as_ref().is_some_and(|d| d.active) {
            return;
        }
        let Some(def) = kadr_effects::recipe(recipe) else { return };
        let next = self.fx_override(def);
        self.fx_set_preview(next);
    }

    /// `def` at «Обычно» on the selected clips it goes on, made at the
    /// playhead as a click would; `None` when it goes on none.
    fn fx_override(&self, def: &'static RecipeDef) -> Option<PreviewOverride> {
        let seq = self.project.sequence();
        let effects: Vec<(ClipId, AppliedEffect)> =
            targets(seq, &self.tl.selection, def).into_iter().filter_map(|id| Some((id, kadr_effects::fresh(def, seq.clip(id)?, self.playhead)))).collect();
        (!effects.is_empty()).then_some(PreviewOverride { recipe: def.id, effects })
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
        self.fx.thumbs.loop_frame = 0;
        if self.fx.thumbs.timer.take().is_some() {
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
                    let applied = match apply_batch(self.project.sequence(), def, &[id], at) {
                        Some(cmd) => self.execute(cmd),
                        None => {
                            self.toast(tf("fx.already", &[("name", &t(&def.name_key()))]));
                            true
                        }
                    };
                    if applied {
                        self.tl.selection = vec![id];
                        self.refresh_inspector();
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
                self.execute(cmd);
            }
            None => self.toast(tf("fx.already", &[("name", &t(&def.name_key()))])),
        }
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
                let params: Vec<FxParamRow> = def.map_or_else(Vec::new, |d| d.params.iter().map(|p| param_row(c, e, p, self.playhead, rate, false)).collect());
                let id: SharedString = e.id.to_string().into();
                let params = reuse_param_rows(old_params.get(&id).cloned(), params);
                FxRow { id, name: fx_name(e).into(), enabled: e.enabled, known: def.is_some(), first: i == 0, last: i + 1 == c.effects.len(), params }
            })
            .collect();
        crate::util::reuse_rows(Some(old), rows)
    }

    /// An effect block's action (spec §6.2, `action_commands`): one undo
    /// entry, nothing sent when nothing would change.
    pub fn fx_action(&mut self, effect: &str, action: &str) {
        let (Some(id), Some(eid)) = (self.inspected(), EffectId::parse(effect)) else { return };
        let seq = self.project.sequence();
        let Some(c) = seq.clip(id) else { return };
        if let Some((label, commands)) = action_commands(c, eid, action, self.playhead, seq.frame_rate) {
            self.execute(EditCommand::Batch { label, commands });
        }
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
            (tf("cmd.effect_named", &[("name", &name)]), vec![update(BTreeMap::from([(key.to_string(), v)]), None)])
        }
        a if a.starts_with("moment:") => {
            let key = &a["moment:".len()..];
            if def?.param(key)?.kind != ParamKind::Moment {
                return None;
            }
            let tt = anim_ui::edit_time(c, playhead, rate.frame_duration())?;
            let v = ParamValue::Moment(SourceTime(c.source_time_at(tt)));
            if e.params.get(key) == Some(&v) {
                return None;
            }
            (tf("cmd.effect_named", &[("name", &name)]), vec![update(BTreeMap::from([(key.to_string(), v)]), None)])
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
    use kadr_scene::{OutputSpec, RenderQuality, SizeU};

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
        let o = PreviewOverride { recipe: def.id, effects: vec![(v1, shown)] };
        o.apply(&mut applied);
        assert!(batch_of(&applied, def, o.effects.clone()).is_none(), "applied already: nothing to send");
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
        let o = PreviewOverride { recipe: def.id, effects: vec![(id, kadr_effects::fresh(def, p.sequence().clip(id).unwrap(), Time::ZERO))] };
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
        assert_eq!((p.kind, p.text.as_str(), p.can_act, p.active), (2, "50 %, 50 %", true, false));
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
        assert_eq!(act(z.id, "choice:direction:1", 12), Some((named("cmd.effect_named"), update(z.id, out, None))));
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
