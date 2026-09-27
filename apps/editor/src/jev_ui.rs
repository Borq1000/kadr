//! Jev in the editor: consent/cost card (same rules as cloud chat), shot
//! grading with badges and human overrides, and personalised multicam
//! auto-cut plans. Decisions live in `project.jev_decisions`.

use crate::ai_ui::{OfferKind, OfferUi};
use crate::app::{post, App};
use crate::{ClipMark, ShotRow};
use crate::multicam_ui::{camera_subject, multicam_clip_at};
use kadr_ai::cost::{check_budget, BudgetDecision, CostEstimate, Pricing};
use kadr_ai::jev::decided::summarize;
use kadr_ai::jev::prefs::{self, Precedent};
use kadr_ai::jev::templates::{camera_item, shot_item, AngleFeatures, CameraInterval, UNSURE};
use kadr_ai::jev::{decide_gate, Decided, JevDecisionService, JevEstimate, JevFailure, JevItem};
use kadr_ai::privacy::{gate, DataKind, GateDecision};
use kadr_ai::providers::jev::{JevProvider, JevQuestion, JevResponse, JevTransport};
use kadr_ai::providers::{AiError, BoxFuture};
use kadr_ai::{Plan, PlanItem, Tier};
use kadr_core::{ActionId, AssetId, CancelToken, Time, TimeRange};
use kadr_i18n::{t, tf, tn};
use kadr_project::{AnalysisData, CorrectionKind, DecisionKind, EditorPreferenceEvent, Project, ShotSummary, StoredDecision, TrackKind};
use kadr_timeline::multicam::group_time_at;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Camera decisions cover this much timeline each.
const CAMERA_INTERVAL: Time = Time::from_secs(4);
/// No AI cut shorter than this.
const MIN_CAMERA_SHOT: Time = Time::from_secs(2);
const PRECEDENTS: usize = 16;

pub fn shot_subject(asset: AssetId, i: usize) -> String {
    format!("shot:{asset}:{i}")
}

#[cfg(test)]
pub fn parse_shot_subject(s: &str) -> Option<(AssetId, usize)> {
    let (a, i) = s.strip_prefix("shot:")?.rsplit_once(':')?;
    Some((AssetId::parse(a)?, i.parse().ok()?))
}

/// Click on a verdict chip: KEEP → REVIEW → DISCARD → KEEP.
pub fn next_verdict(v: &str) -> &'static str {
    match v {
        "KEEP" => "REVIEW",
        "REVIEW" => "DISCARD",
        _ => "KEEP",
    }
}

pub fn shots_of(project: &Project, asset: AssetId) -> Option<&Vec<ShotSummary>> {
    project.analysis.iter().rev().find_map(|a| match &a.data {
        AnalysisData::Shots { shots } if a.asset == asset => Some(shots),
        _ => None,
    })
}

/// Share of `range` (source time) covered by detected silence, 0..1.
fn silence_share(project: &Project, asset: AssetId, range: TimeRange) -> f64 {
    let Some(ranges) = project.analysis.iter().rev().find_map(|a| match &a.data {
        AnalysisData::Silence { ranges, .. } if a.asset == asset => Some(ranges),
        _ => None,
    }) else {
        return 0.0;
    };
    let silent: f64 = ranges.iter().filter_map(|r| r.intersect(&range)).map(|r| r.duration().as_secs_f64()).sum();
    silent / range.duration().as_secs_f64().max(1e-6)
}

/// One Jev item per analysed shot of `assets` (assets still being analysed
/// are skipped). Only bucket words are sent: no names, no paths.
pub fn shot_items(project: &Project, assets: &[AssetId]) -> Vec<JevItem> {
    let mut out = vec![];
    for &a in assets {
        let has_audio = project.asset(a).is_some_and(|x| x.has_audio());
        for (i, s) in shots_of(project, a).into_iter().flatten().enumerate() {
            let audio = if !has_audio || silence_share(project, a, s.range) > 0.8 { "silence" } else { "sound" };
            out.push(shot_item(shot_subject(a, i), s, audio));
        }
    }
    out
}

/// A camera choice over a timeline range.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub range: TimeRange,
    pub label: String,
    pub p: f32,
}

/// Merges equal neighbours, then folds picks shorter than `min` into the
/// stronger neighbour (the later one on a tie, so cuts come early).
pub fn merge_picks(picks: Vec<Pick>, min: Time) -> Vec<Pick> {
    fn merge_equal(v: Vec<Pick>) -> Vec<Pick> {
        let mut out: Vec<Pick> = vec![];
        for p in v {
            match out.last_mut() {
                Some(l) if l.label == p.label && l.range.end == p.range.start => {
                    l.range.end = p.range.end;
                    l.p = l.p.max(p.p);
                }
                _ => out.push(p),
            }
        }
        out
    }
    let mut v = merge_equal(picks);
    while let Some(i) = v.iter().position(|p| p.range.duration() < min) {
        if v.len() == 1 {
            break;
        }
        let prev = i.checked_sub(1).map(|j| v[j].p);
        let next = v.get(i + 1).map(|p| p.p);
        let short = v.remove(i);
        match (prev, next) {
            (Some(a), Some(b)) if a > b => v[i - 1].range.end = short.range.end,
            (_, Some(_)) => v[i].range.start = short.range.start,
            (Some(_), None) => v[i - 1].range.end = short.range.end,
            (None, None) => unreachable!(),
        }
        v = merge_equal(v);
    }
    v
}

/// Chat line for a camera plan with nothing to change. UNSURE everywhere
/// means Jev had nothing to go on, which is not agreement with the cut.
fn no_change_key(unsure: usize, total: usize) -> &'static str {
    match unsure {
        0 => "jev.cams.nothing_to_change",
        n if n >= total => "jev.cams.all_unsure",
        _ => "jev.cams.agree_some_unsure",
    }
}

/// The best other angle Jev rated at least "good" (score 2 of 0–3) for a slot.
fn good_alternative(answers: &BTreeMap<String, kadr_ai::providers::jev::JevAnswer>, labels: &[String], current: &str) -> Option<String> {
    labels
        .iter()
        .filter(|l| l.as_str() != current)
        .filter_map(|l| {
            let a = answers.get(&format!("fit_{l}"))?;
            let p: BTreeMap<String, f32> = a.probabilities.iter().map(|(k, v)| (k.clone(), *v as f32)).collect();
            let (top, p_top, _) = summarize(&p)?;
            let score: usize = top.parse().ok()?;
            (score >= 2).then(|| (score, p_top, l.clone()))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)))
        .map(|x| x.2)
}

/// Longest time one angle may hold while another is rated at least "good".
const MAX_HOLD: Time = Time::from_secs(20);

/// Per-slot picks with each slot's best other angle (fit ≥ "good"): a hold
/// longer than `max` cuts away to that angle for one slot.
pub fn cap_holds(picks: Vec<Pick>, alternatives: &[Option<String>], max: Time) -> Vec<Pick> {
    let mut out: Vec<Pick> = Vec::with_capacity(picks.len());
    let mut held_since = picks.first().map_or(Time::ZERO, |p| p.range.start);
    for (i, mut p) in picks.into_iter().enumerate() {
        if out.last().is_some_and(|l| l.label != p.label) {
            held_since = p.range.start;
        }
        if p.range.end - held_since > max {
            if let Some(a) = alternatives.get(i).cloned().flatten().filter(|a| *a != p.label) {
                p.label = a;
                held_since = p.range.start;
            }
        }
        out.push(p);
    }
    out
}

/// What a Jev card is for, with what's needed to use its answers.
pub enum JevPurpose {
    Grade,
    Cameras(Vec<CameraSlot>),
}

pub struct CameraSlot {
    /// Timeline range the pick applies to.
    pub range: TimeRange,
    pub labels: Vec<String>,
    /// Angle on screen now at the start of the range.
    pub current: String,
    pub history: Vec<Precedent>,
}

pub struct JevRequest {
    pub purpose: JevPurpose,
    pub title: String,
    pub items: Vec<JevItem>,
    pub estimate: JevEstimate,
    pub budget: BudgetDecision,
    pub gate: GateDecision,
    pub model: String,
    pub pricing: Pricing,
}

/// Transport used when no key is stored: estimates still work, calls fail.
struct NoKey;

impl JevTransport for NoKey {
    fn decide<'a>(&'a self, _: &'a str, _: &'a serde_json::Value, _: &'a BTreeMap<String, JevQuestion>) -> BoxFuture<'a, Result<JevResponse, AiError>> {
        Box::pin(async { Err(AiError::NoKey("Jev".into())) })
    }
}

impl App {
    fn jev_service(&self) -> Option<(JevDecisionService, bool, Pricing, GateDecision)> {
        let st = &self.ai.assistant.settings;
        let cfg = st.provider("jev")?.clone();
        let model = st.jev_model.clone();
        let pricing = cfg.pricing(&model);
        let key = kadr_ai::credentials::load_key("jev");
        let has_key = key.is_some();
        let privacy = gate(st.mode, cfg.is_local(), &cfg.permissions, &[DataKind::Text]);
        let transport: Arc<dyn JevTransport> = match key {
            Some(k) => Arc::new(JevProvider::new(cfg, k)),
            None => Arc::new(NoKey),
        };
        Some((JevDecisionService::new(transport, model, pricing.clone()), has_key, pricing, privacy))
    }

    /// Shows the cost/consent card; runs at once when policy and budget
    /// allow it without asking (or when everything is cached).
    fn open_jev_request(&mut self, purpose: JevPurpose, title: String, items: Vec<JevItem>) {
        let Some((svc, has_key, pricing, privacy)) = self.jev_service() else { return };
        self.sync_month_spend();
        let estimate = svc.estimate(&items, &self.project.jev_decisions);
        let cost = CostEstimate::new(&pricing, estimate.input_tokens, 0, 0);
        let ledger = self.ai.assistant.ledger.lock().unwrap().clone();
        let budget = if estimate.calls == 0 { BudgetDecision::Allowed } else { check_budget(&self.ai.assistant.settings.limits, &ledger, &cost) };
        let gate = match (estimate.calls, has_key) {
            (0, _) => GateDecision::Allow,
            (_, false) => GateDecision::Deny("jev.no_key".into()),
            _ => privacy,
        };
        let auto = gate == GateDecision::Allow && budget == BudgetDecision::Allowed;
        let model = svc.model().to_string();
        let req = JevRequest { purpose, title, items, estimate, budget, gate, model, pricing };
        self.ai.offers.push(OfferUi { kind: OfferKind::Jev(Box::new(req)), state: 0, cancel: CancelToken::new() });
        let i = self.ai.offers.len() - 1;
        self.ai.chat.push((3, String::new(), i as i32));
        if !self.ui().get_ai_open() {
            self.menu("toggle-ai");
        }
        if auto {
            self.jev_run(i);
        }
        self.refresh_ai();
    }

    pub fn jev_run(&mut self, i: usize) {
        let Some((svc, ..)) = self.jev_service() else { return };
        let Some(o) = self.ai.offers.get_mut(i) else { return };
        let OfferKind::Jev(req) = &o.kind else { return };
        let items = req.items.clone();
        let cancel = CancelToken::new();
        o.cancel = cancel.clone();
        o.state = 1;
        let cache = self.project.jev_decisions.clone();
        tracing::info!(items = items.len(), calls = req.estimate.calls, est_usd = req.estimate.usd, "Jev request");
        self.ai.assistant.runtime().spawn(async move {
            let r = svc.run(items, &cache, cancel).await;
            post(move |app| app.on_jev_result(i, r));
        });
        self.refresh_ai();
    }

    fn on_jev_result(&mut self, i: usize, r: Result<(Vec<Decided>, u64, u64), JevFailure>) {
        let Some(o) = self.ai.offers.get(i) else { return };
        let OfferKind::Jev(req) = &o.kind else { return };
        let cancelled = o.state == 3;
        let (decided, input, output, error) = match r {
            Ok((d, input, output)) => (d, input, output, None),
            Err(f) => (f.partial, f.input_tokens, f.output_tokens, Some(f.error)),
        };
        let cost = req.pricing.cost(input, output);
        let (model, purpose_is_grade) = (req.model.clone(), matches!(req.purpose, JevPurpose::Grade));
        // Calls that went through are paid for, whatever happens next.
        if input > 0 {
            self.ai.assistant.ledger.lock().unwrap().record(cost, input, output);
            self.project.ai_cost_usd += cost;
            self.record_month_spend(cost);
            self.meta_dirty = true;
        }
        if cancelled || error.is_some() {
            // Keep paid-for verdicts; a camera plan needs every interval.
            if purpose_is_grade && !decided.is_empty() {
                self.upsert_decisions(decided.into_iter().map(|d| d.stored).collect());
                self.refresh_library();
                self.refresh_timeline();
                self.refresh_inspector();
            }
            if let (false, Some(e)) = (cancelled, error) {
                self.ai.offers[i].state = 4;
                self.ai.say(4, tf("ai.msg.error", &[("error", &crate::ai_ui::ai_error_text(&e))]));
            }
            return self.refresh_ai();
        }
        self.ai.offers[i].state = 2;
        let cached = decided.iter().filter(|d| d.from_cache).count();
        tracing::info!(decisions = decided.len(), cached, input, cost, "Jev answered");
        if purpose_is_grade {
            let stored: Vec<StoredDecision> = decided.into_iter().map(|d| d.stored).collect();
            let (bad, review) = verdict_counts(stored.iter());
            self.upsert_decisions(stored);
            self.ai.say(1, tf("jev.grade.done", &[("bad", &bad.to_string()), ("review", &review.to_string())]));
            self.refresh_library();
            self.refresh_timeline();
            self.refresh_inspector();
        } else {
            let OfferKind::Jev(req) = std::mem::replace(&mut self.ai.offers[i].kind, OfferKind::Done { model: model.clone(), input, cost }) else { return };
            let JevPurpose::Cameras(slots) = req.purpose else { return };
            self.finish_camera_plan(slots, decided, model, cost);
        }
        self.meta_dirty = true;
        self.refresh_ai();
        self.refresh_status();
    }

    /// Latest decision per subject wins (re-grading replaces old verdicts).
    fn upsert_decisions(&mut self, new: Vec<StoredDecision>) {
        for d in new {
            self.project.jev_decisions.retain(|x| !(x.subject == d.subject && x.kind == d.kind));
            self.project.jev_decisions.push(d);
        }
        self.meta_dirty = true;
    }

    /// "Оцени кадры": the selected media, else what's on the timeline, else all.
    pub fn grade_shots(&mut self) {
        let on_timeline: Vec<AssetId> = {
            let mut v: Vec<AssetId> = self.project.sequence().tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.asset)).collect();
            v.sort();
            v.dedup();
            v
        };
        let assets: Vec<AssetId> = match self.selected_asset {
            Some(a) => vec![a],
            None if !on_timeline.is_empty() => on_timeline,
            None => self.project.assets.iter().map(|a| a.id).collect(),
        };
        let items = shot_items(&self.project, &assets);
        if items.is_empty() {
            self.ai.say(1, t("jev.grade.nothing"));
            return self.refresh_ai();
        }
        let title = tn("jev.grade.title", items.len() as i64, &[]);
        self.open_jev_request(JevPurpose::Grade, title, items);
    }

    pub fn shot_decision(&self, asset: AssetId, i: usize) -> Option<&StoredDecision> {
        let s = shot_subject(asset, i);
        self.project.jev_decisions.iter().rev().find(|d| d.kind == DecisionKind::ShotUsability && d.subject == s)
    }

    /// (DISCARD, REVIEW) counts of an asset's shots, human overrides included.
    pub fn asset_verdicts(&self, asset: AssetId) -> (usize, usize) {
        let prefix = format!("shot:{asset}:");
        verdict_counts(self.project.jev_decisions.iter().filter(|d| d.kind == DecisionKind::ShotUsability && d.subject.starts_with(&prefix)))
    }

    /// Inspector chip: the editor corrects Jev. Recorded as a precedent.
    pub fn cycle_shot_verdict(&mut self, asset: AssetId, i: usize) {
        let s = shot_subject(asset, i);
        let Some(d) = self.project.jev_decisions.iter_mut().rev().find(|d| d.kind == DecisionKind::ShotUsability && d.subject == s) else {
            return;
        };
        let new = next_verdict(d.effective()).to_string();
        d.human = (new != d.value).then(|| new.clone());
        let ev = EditorPreferenceEvent {
            at_ms: kadr_project::now_ms(),
            action: None,
            kind: if new == "DISCARD" { CorrectionKind::SegmentDeleted } else { CorrectionKind::SegmentRestored },
            context: d.features.clone(),
            features: serde_json::json!({"subject": d.subject}),
            ai_choice: d.value.clone(),
            ai_confidence: d.p_max,
            human_choice: new,
            decision: Some(d.id),
        };
        self.project.preference_events.push(ev);
        self.meta_dirty = true;
        self.refresh_library();
        self.refresh_timeline();
        self.refresh_inspector();
    }

    /// "Смонтируй камеры": Jev picks an angle every 4 s of the multicam clips.
    pub fn cut_cameras(&mut self) {
        let seq = self.project.sequence();
        let mut clips: Vec<kadr_project::Clip> = self
            .tl
            .selection
            .iter()
            .filter_map(|id| seq.clip(*id))
            .filter(|c| c.multicam.is_some() && seq.locate_clip(c.id).is_some_and(|(ti, _)| seq.tracks[ti].kind == TrackKind::Video))
            .cloned()
            .collect();
        if clips.is_empty() {
            // The whole run of multicam clips on the track under the playhead.
            if let Some(c) = multicam_clip_at(seq, self.playhead) {
                let (ti, _) = seq.locate_clip(c.id).unwrap();
                let group = c.multicam.as_ref().unwrap().group;
                clips = seq.tracks[ti].clips.iter().filter(|x| x.multicam.as_ref().is_some_and(|m| m.group == group)).cloned().collect();
            }
        }
        if clips.is_empty() {
            self.ai.say(1, t("jev.cams.no_multicam"));
            return self.refresh_ai();
        }
        let personalize = self.ai.assistant.settings.jev_personalize;
        let now = kadr_project::now_ms();
        let (mut items, mut slots) = (vec![], vec![]);
        for clip in &clips {
            let Some(group) = self.project.multicam_groups.iter().find(|g| Some(g.id) == clip.multicam.as_ref().map(|m| m.group)).cloned() else { continue };
            let mut t0 = clip.timeline_in;
            while t0 < clip.timeline_out {
                let mut t1 = (t0 + CAMERA_INTERVAL).min(clip.timeline_out);
                if clip.timeline_out - t1 < Time::from_secs(1) {
                    t1 = clip.timeline_out; // no tiny tail interval
                }
                let (Some(g0), Some(g1)) = (group_time_at(&group, clip, t0), group_time_at(&group, clip, t1)) else { break };
                let angles = self.angle_features(&group, TimeRange::new(g0, g1));
                if angles.len() >= 2 {
                    let loudest = angles.iter().filter_map(|a| a.1).fold(f32::MIN, f32::max);
                    let context = serde_json::json!({"audio_activity": if loudest > -30.0 { "loud" } else if loudest > -50.0 { "quiet" } else { "silent" }});
                    let features: Vec<AngleFeatures> = angles.into_iter().map(|a| a.0).collect();
                    let labels: Vec<String> = features.iter().map(|a| a.label.clone()).collect();
                    let iv = CameraInterval {
                        subject: camera_subject(group.id, TimeRange::new(g0, g1)),
                        time_label: format!("{}-{}", short_tc(t0), short_tc(t1)),
                        angles: features,
                        previous: None,
                        context,
                    };
                    let history = if personalize {
                        let current = serde_json::json!({"context": iv.context, "cameras": camera_ctx(&iv)});
                        prefs::precedents(&self.project.preference_events, &self.project.jev_decisions, DecisionKind::CameraPick, &current, PRECEDENTS, now)
                    } else {
                        vec![]
                    };
                    let current = clip.multicam.as_ref().and_then(|m| group.angles.get(m.angle as usize)).map(|a| a.label.clone()).unwrap_or_default();
                    items.push(camera_item(&iv, &history));
                    slots.push(CameraSlot { range: TimeRange::new(t0, t1), labels, current, history });
                }
                t0 = t1;
            }
        }
        if items.is_empty() {
            self.ai.say(1, t("jev.cams.no_angles"));
            return self.refresh_ai();
        }
        let title = tn("jev.cams.title", items.len() as i64, &[]);
        self.open_jev_request(JevPurpose::Cameras(slots), title, items);
    }

    /// Bucketed picture words and mean loudness of every angle that has
    /// media over group-time `g`.
    fn angle_features(&self, group: &kadr_project::MulticamGroup, g: TimeRange) -> Vec<(AngleFeatures, Option<f32>)> {
        let mut out = vec![];
        for a in &group.angles {
            let Some(asset) = self.project.asset(a.asset) else { continue };
            let src = TimeRange::new(g.start + a.sync_offset, g.end + a.sync_offset);
            if src.start < Time::ZERO || src.end > asset.duration() {
                continue;
            }
            let mid = src.start + (src.end - src.start).mul_ratio(1, 2);
            let shot = shots_of(&self.project, a.asset).and_then(|s| s.iter().find(|s| s.range.contains(mid)));
            let level = self.assets_rt.get(&a.asset).and_then(|r| r.overview.clone()).map(|ov| {
                let w = ov.level_window as f64 / ov.sample_rate as f64;
                let (i0, i1) = ((src.start.as_secs_f64() / w) as usize, (src.end.as_secs_f64() / w) as usize);
                let v = &ov.levels_db[i0.min(ov.levels_db.len())..i1.min(ov.levels_db.len())];
                if v.is_empty() { -100.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
            });
            let word = |f: fn(&ShotSummary) -> &String| shot.map(|s| f(s).clone()).unwrap_or_else(|| "unknown".into());
            out.push((
                AngleFeatures {
                    label: a.label.clone(),
                    description: a.description.clone(),
                    sharpness: word(|s| &s.sharpness),
                    exposure: word(|s| &s.exposure),
                    shake: word(|s| &s.shake),
                    audio_level: String::new(),
                },
                level,
            ));
        }
        let loudest = out.iter().filter_map(|a| a.1).fold(f32::MIN, f32::max);
        for (f, l) in &mut out {
            f.audio_level = match *l {
                None => "unknown",
                Some(l) if l < -55.0 => "silent",
                Some(l) if l >= loudest - 3.0 => "loudest",
                Some(_) => "quiet",
            }
            .into();
        }
        out
    }

    /// Turns camera decisions into a reviewable plan (Apply = one undo step).
    fn finish_camera_plan(&mut self, slots: Vec<CameraSlot>, decided: Vec<Decided>, model: String, cost: f64) {
        let mut picks = vec![];
        let mut stored = vec![];
        let mut personal_n = 0;
        let mut unsure_n = 0;
        let mut alternatives = vec![];
        for (slot, mut d) in slots.into_iter().zip(decided) {
            if !d.from_cache {
                // Personal pick when the history has precedents for this moment.
                let precedent = d.answers.get("has_precedent").and_then(|a| a.noul).unwrap_or(0.0);
                if let (Some(pick), true) = (d.answers.get("editor_pick"), precedent >= 0.5) {
                    let p_pick: BTreeMap<String, f32> = pick.probabilities.iter().map(|(k, v)| (k.clone(), *v as f32)).collect();
                    if let Some(p) = prefs::personal_pick(&p_pick, &slot.labels, &slot.history) {
                        if let Some((value, p_max, margin)) = summarize(&p) {
                            d.stored.gate = decide_gate(DecisionKind::CameraPick, &value, p_max, margin, false);
                            d.stored.value = value;
                            d.stored.probs = p;
                            d.stored.p_max = p_max;
                            d.stored.margin = margin;
                            d.stored.features["personal"] = serde_json::json!(true);
                        }
                    }
                }
            }
            let personal = d.stored.features.get("personal").is_some();
            personal_n += personal as usize;
            let usable = d.stored.value != UNSURE && d.stored.gate != kadr_project::Gate::Review && slot.labels.contains(&d.stored.value);
            unsure_n += !usable as usize;
            let label = if usable { d.stored.effective().to_string() } else { slot.current.clone() };
            alternatives.push(good_alternative(&d.answers, &slot.labels, &label));
            picks.push((Pick { range: slot.range, label, p: d.stored.p_max }, personal, slot.current));
            stored.push(d.stored);
        }
        self.upsert_decisions(stored);
        let personal_of = |r: TimeRange| picks.iter().any(|(p, personal, _)| *personal && p.range.intersect(&r).is_some());
        let current_of = |r: TimeRange| picks.iter().find(|(p, ..)| p.range.contains(r.start)).map(|x| x.2.clone()).unwrap_or_default();
        let merged = merge_picks(cap_holds(picks.iter().map(|x| x.0.clone()).collect(), &alternatives, MAX_HOLD), MIN_CAMERA_SHOT);
        let items: Vec<PlanItem> = merged
            .iter()
            .filter(|p| p.label != current_of(p.range))
            .map(|p| {
                let why = if personal_of(p.range) { t("jev.cams.why_personal") } else { t("jev.cams.why_general") };
                PlanItem {
                    label: format!("{} · {}–{} · p {:.2} · {why}", p.label, short_tc(p.range.start), short_tc(p.range.end), p.p),
                    start: p.range.start,
                    end: p.range.end,
                    enabled: true,
                    command: kadr_ai::command::AiCommand::SelectCamera {
                        start_ms: p.range.start.as_millis(),
                        end_ms: p.range.end.as_millis(),
                        angle: p.label.clone(),
                        reason: why,
                    },
                }
            })
            .collect();
        if items.is_empty() {
            self.ai.say(1, tf(no_change_key(unsure_n, picks.len()), &[("n", &unsure_n.to_string())]));
            return;
        }
        let mean_p = merged.iter().map(|p| p.p).sum::<f32>() / merged.len().max(1) as f32;
        let plan = Plan {
            id: ActionId::new(),
            prompt: t("jev.cams.prompt"),
            title: t("jev.cams.plan_title"),
            findings: vec![tf("jev.cams.findings", &[("n", &picks.len().to_string()), ("personal", &personal_n.to_string())])],
            proposal: tn("ai.plan.operations", items.len() as i64, &[]),
            items,
            tier: Tier::Economy,
            provider: "Jev".into(),
            model,
            cost_usd: cost,
            confidence: mean_p,
            before_duration: None,
            after_duration: None,
        };
        self.ai.plans.push(crate::ai_ui::PlanUi { plan, reviewing: true, state: 0 });
        let n = self.ai.plans.len() as i32 - 1;
        self.ai.chat.push((2, String::new(), n));
    }
}

/// Jev reads English bucket words; the UI shows them translated.
fn word(w: &str) -> String {
    t(&format!("jev.word.{}", w.replace(' ', "_")))
}

impl App {
    /// Rejected / to-check stripes along a video clip, in clip pixels.
    pub fn clip_marks(&self, c: &kadr_project::Clip, width_px: f64) -> Vec<ClipMark> {
        let Some(shots) = shots_of(&self.project, c.asset) else { return vec![] };
        let speed = c.speed().max(1e-6);
        let mut out = vec![];
        for (i, s) in shots.iter().enumerate() {
            let kind = match self.shot_decision(c.asset, i).map(|d| d.effective()) {
                Some("DISCARD") => 2,
                Some("REVIEW") => 1,
                _ => continue,
            };
            let Some(r) = s.range.intersect(&c.source_range()) else { continue };
            let x0 = (r.start - c.source_in).as_secs_f64() / speed * self.tl.pps;
            let x1 = ((r.end - c.source_in).as_secs_f64() / speed * self.tl.pps).min(width_px);
            if x1 > x0 {
                out.push(ClipMark { x: x0 as f32, w: (x1 - x0).max(2.0) as f32, kind });
            }
        }
        out
    }

    /// Shot rows of the inspected clip's media within the clip.
    pub fn inspector_shots(&self, c: &kadr_project::Clip) -> (Vec<ShotRow>, bool) {
        let Some(shots) = shots_of(&self.project, c.asset) else { return (vec![], false) };
        let mut graded = false;
        let rows = shots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.range.intersect(&c.source_range()).is_some())
            .map(|(i, s)| {
                let d = self.shot_decision(c.asset, i);
                graded |= d.is_some();
                let v = d.map(|d| d.effective().to_string());
                ShotRow {
                    index: i as i32,
                    time: short_tc(c.timeline_time_of(s.range.start.max(c.source_in))).into(),
                    info: format!("{} · {} · {}", word(&s.sharpness), word(&s.exposure), tf("jev.word.shake_is", &[("v", &word(&s.shake))])).into(),
                    verdict: match v.as_deref() {
                        Some("DISCARD") => t("jev.verdict.discard"),
                        Some("REVIEW") => t("jev.verdict.review"),
                        Some(_) => t("jev.verdict.keep"),
                        None => String::new(),
                    }
                    .into(),
                    kind: match v.as_deref() {
                        Some("DISCARD") => 2,
                        Some("REVIEW") => 1,
                        Some(_) => 0,
                        None => 3,
                    },
                    human: d.is_some_and(|d| d.human.is_some()),
                }
            })
            .collect();
        (rows, graded)
    }

    pub fn inspector_shot_clicked(&mut self, i: i32) {
        let asset = self.inspected().and_then(|id| self.project.sequence().clip(id)).map(|c| c.asset);
        if let Some(a) = asset {
            self.cycle_shot_verdict(a, i as usize);
        }
    }
}

fn verdict_counts<'a>(ds: impl Iterator<Item = &'a StoredDecision>) -> (usize, usize) {
    ds.fold((0, 0), |(b, r), d| match d.effective() {
        "DISCARD" => (b + 1, r),
        "REVIEW" => (b, r + 1),
        _ => (b, r),
    })
}

fn camera_ctx(iv: &CameraInterval) -> serde_json::Value {
    iv.angles
        .iter()
        .map(|a| (a.label.clone(), serde_json::json!({"shows": a.description, "sharpness": a.sharpness, "exposure": a.exposure, "shake": a.shake, "audio_level": a.audio_level})))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// "12:04" / "1:02:03".
fn short_tc(t: Time) -> String {
    let s = t.as_millis().max(0) / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, MediaInfo, MediaKind, Time, TimeRange};
    use kadr_project::{AnalysisData, AnalysisResult, MediaAsset, Project, ShotSummary};

    fn shot(a: i64, b: i64, sharp: &str) -> ShotSummary {
        ShotSummary {
            range: TimeRange::new(Time::from_secs(a), Time::from_secs(b)),
            sharpness: sharp.into(),
            exposure: "normal".into(),
            shake: "none".into(),
            black: false,
        }
    }

    fn project_with(shots: Vec<ShotSummary>, silence: Vec<TimeRange>) -> (Project, AssetId, AssetId) {
        let mut p = Project::new("t");
        let info = MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(20), container: "mp4".into(), size_bytes: 0, video: None, timecode: None,
            audio: Some(kadr_core::AudioInfo { sample_rate: 48_000, channels: 2, codec: "aac".into(), channel_layout: "stereo".into() }) };
        let a = MediaAsset::new(r"C:\secret\cam1.mp4", info.clone());
        let b = MediaAsset::new(r"C:\secret\cam2.mp4", info);
        let (ia, ib) = (a.id, b.id);
        p.assets.extend([a, b]);
        p.analysis.push(AnalysisResult { asset: ia, algo_version: 1, data: AnalysisData::Shots { shots } });
        p.analysis.push(AnalysisResult {
            asset: ia,
            algo_version: 1,
            data: AnalysisData::Silence { threshold_db: -40.0, min_duration: Time::from_millis(300), ranges: silence },
        });
        (p, ia, ib)
    }

    #[test]
    fn shot_items_cover_analysed_assets_only_and_leak_no_paths() {
        let (p, a, b) = project_with(
            vec![shot(0, 5, "sharp"), shot(5, 12, "very blurry")],
            vec![TimeRange::new(Time::from_secs(5), Time::from_secs(12))],
        );
        let items = shot_items(&p, &[a, b]);
        assert_eq!(items.len(), 2, "b has no shot analysis yet");
        assert_eq!(parse_shot_subject(&items[1].subject), Some((a, 1)));
        assert_eq!(items[0].state["audio"], "sound");
        assert_eq!(items[1].state["audio"], "silence");
        assert!(items[1].extra_ok, "very blurry: local detector agrees with DISCARD");
        for it in &items {
            assert!(!it.state.to_string().contains("secret"), "file paths must never reach Jev");
        }
    }

    #[test]
    fn verdict_cycles_through_all_three() {
        assert_eq!(next_verdict("KEEP"), "REVIEW");
        assert_eq!(next_verdict("REVIEW"), "DISCARD");
        assert_eq!(next_verdict("DISCARD"), "KEEP");
    }

    #[test]
    fn an_unsure_answer_is_not_reported_as_agreement() {
        assert_eq!(no_change_key(4, 4), "jev.cams.all_unsure");
        assert_eq!(no_change_key(1, 4), "jev.cams.agree_some_unsure");
        assert_eq!(no_change_key(0, 4), "jev.cams.nothing_to_change");
    }

    #[test]
    fn camera_picks_merge_and_respect_a_minimum_shot_length() {
        let s = |a: i64, b: i64, l: &str, p: f32| Pick { range: TimeRange::new(Time::from_millis(a), Time::from_millis(b)), label: l.into(), p };
        let got = merge_picks(
            vec![s(0, 4000, "CAM1", 0.9), s(4000, 8000, "CAM1", 0.8), s(8000, 9500, "CAM2", 0.6), s(9500, 13500, "CAM3", 0.9)],
            Time::from_secs(2),
        );
        // Equal neighbours merge; the 1.5 s CAM2 blip joins its stronger neighbour.
        let summary: Vec<_> = got.iter().map(|p| (p.range.start.as_millis(), p.range.end.as_millis(), p.label.as_str())).collect();
        assert_eq!(summary, vec![(0, 8000, "CAM1"), (8000, 13500, "CAM3")]);
    }

    #[test]
    fn a_long_hold_cuts_away_to_a_good_alternative() {
        // Seven 4 s slots all on CAM1; CAM2 fits "good" from slot 3 on.
        let slots: Vec<Pick> = (0..7).map(|i| Pick { range: TimeRange::new(Time::from_secs(4 * i), Time::from_secs(4 * i + 4)), label: "CAM1".into(), p: 0.9 }).collect();
        let alts: Vec<Option<String>> = (0..7).map(|i| (i >= 3).then(|| "CAM2".to_string())).collect();
        let got = cap_holds(slots, &alts, Time::from_secs(20));
        let labels: Vec<&str> = got.iter().map(|p| p.label.as_str()).collect();
        // 0–20 s is the longest allowed hold; the slot ending past 20 s cuts away.
        assert_eq!(labels, ["CAM1", "CAM1", "CAM1", "CAM1", "CAM1", "CAM2", "CAM1"]);
        // Without a good alternative the hold stays.
        let none = vec![None; 7];
        let slots2: Vec<Pick> = got.iter().map(|p| Pick { label: "CAM1".into(), ..p.clone() }).collect();
        assert!(cap_holds(slots2, &none, Time::from_secs(20)).iter().all(|p| p.label == "CAM1"));
    }

    #[test]
    fn grade_intent_is_recognised() {
        use kadr_ai::intent::{parse, Intent};
        for s in ["Оцени кадры", "найди брак", "grade shots", "оцени качество кадров"] {
            assert_eq!(parse(s), Intent::GradeShots, "{s}");
        }
    }
}
