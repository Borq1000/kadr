//! The «Эффекты» tab (effects spec §6.1): categories, search, cards.
//! Hovering a card shows its effect on the selected clips without touching
//! the project (`PreviewOverride`, applied to the preview's snapshot only);
//! a click applies it to them, a drag to the one clip under the pointer.
//! Pure parts are free functions, tested without a window.

use crate::app::App;
use crate::timeline_ui::{rows, Row};
use crate::{FxCardView, FxCategoryView};
use kadr_core::{ClipId, Time};
use kadr_effects::{Category, RecipeDef};
use kadr_i18n::{t, t_in, tf, Lang};
use kadr_project::{AppliedEffect, Sequence};
use kadr_timeline::EditCommand;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline_ui::rows;
    use kadr_core::{AssetId, FrameRate, MediaInfo, MediaKind, TimeRange, VideoInfo};
    use kadr_playback::SceneSource;
    use kadr_project::{Clip, MediaAsset, Project, Track, TrackKind};
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
