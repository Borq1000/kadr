//! AI panel glue: chat, plans (Apply / Review / Cancel), cloud offers with
//! cost preview, AI action log and human-correction events.

use crate::app::{post, App};
use crate::util::{fmt_tokens, money, money_range};
use crate::{ChatMsg, OfferView, PlanItemView, PlanView};
use kadr_ai::command::{self};
use kadr_ai::cost::BudgetDecision;
use kadr_ai::privacy::GateDecision;
use kadr_ai::{Assistant, CloudOffer, EditorState, Plan, Reply};
use kadr_core::CancelToken;
use kadr_i18n::{duration, t, tf};
use kadr_project::{AIAction, AIActionStatus, CorrectionKind, EditSource, EditorPreferenceEvent, now_ms};
use kadr_timeline::{EditCommand, EditEngine};
use slint::{ModelRc, SharedString, VecModel};

pub struct PlanUi {
    pub plan: Plan,
    pub reviewing: bool,
    pub state: i32,
}

pub struct OfferUi {
    pub offer: CloudOffer,
    pub state: i32,
    pub cancel: CancelToken,
}

pub struct AiUi {
    pub assistant: Assistant,
    pub chat: Vec<(i32, String, i32)>,
    pub plans: Vec<PlanUi>,
    pub offers: Vec<OfferUi>,
}

impl AiUi {
    pub fn new(assistant: Assistant) -> Self {
        AiUi { assistant, chat: vec![], plans: vec![], offers: vec![] }
    }

    fn say(&mut self, kind: i32, text: impl Into<String>) {
        self.chat.push((kind, text.into(), -1));
    }

    /// Re-evaluates privacy gates of pending offers after a settings change,
    /// so a card never shows a stale "blocked" (or stale "allowed") state.
    pub fn refresh_offer_gates(&mut self) {
        let st = &self.assistant.settings;
        for o in self.offers.iter_mut().filter(|o| o.state == 0) {
            if let Some(p) = st.provider(&o.offer.provider_id) {
                o.offer.gate = kadr_ai::privacy::gate(st.mode, p.is_local(), &p.permissions, &[kadr_ai::privacy::DataKind::Text]);
            }
        }
    }

    /// Keeps plan cards in sync with Ctrl+Z / Ctrl+Shift+Z on AI edits.
    pub fn on_history_changed(&mut self, engine: &EditEngine) {
        let undone = engine.redo_top_action();
        let redone = engine.last_entry().and_then(|e| e.action);
        for p in &mut self.plans {
            if p.state == 1 && undone == Some(p.plan.id) {
                p.state = 3;
            } else if p.state == 3 && redone == Some(p.plan.id) {
                p.state = 1;
            }
        }
    }
}

impl App {
    fn editor_state(&self) -> EditorState {
        EditorState { playhead: self.playhead, selection: self.tl.selection.clone(), in_out: self.project.sequence().in_out }
    }

    pub fn ai_send(&mut self, text: &str) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.ui().set_ai_draft("".into());
        self.ai.chat.push((0, text.clone(), -1));
        let st = self.editor_state();
        let reply = self.ai.assistant.handle(&text, &self.project, &st);
        self.handle_reply(reply);
        self.refresh_ai();
    }

    fn handle_reply(&mut self, reply: Reply) {
        match reply {
            Reply::Plan(plan) => {
                if plan.is_empty() {
                    let msg = plan.findings.join("\n") + "\n" + &plan.proposal;
                    self.ai.say(1, msg);
                } else {
                    self.ai.plans.push(PlanUi { plan, reviewing: false, state: 0 });
                    let i = self.ai.plans.len() as i32 - 1;
                    self.ai.chat.push((2, String::new(), i));
                }
            }
            Reply::Execute { label, command, message } => {
                let cmd = EditCommand::Batch { label: tf("cmd.ai_prefix", &[("what", &crate::app::cmd_label(&label))]), commands: vec![command] };
                if self.execute_as(cmd, EditSource::Ai, None) {
                    self.ai.say(1, message);
                } else {
                    self.ai.say(4, t("ai.msg.nothing_here"));
                }
            }
            Reply::Undo => {
                self.undo();
                self.ai.say(1, t("ai.msg.undone"));
            }
            Reply::Redo => {
                self.redo();
                self.ai.say(1, t("ai.msg.redone"));
            }
            Reply::NeedsAnalysis { message, .. } => self.ai.say(1, message),
            Reply::Message(m) => self.ai.say(1, m),
            Reply::CloudOffer(offer) => {
                self.ai.offers.push(OfferUi { offer, state: 0, cancel: CancelToken::new() });
                let i = self.ai.offers.len() as i32 - 1;
                self.ai.chat.push((3, String::new(), i));
            }
        }
    }

    pub fn ai_plan_action(&mut self, i: usize, action: &str) {
        if i >= self.ai.plans.len() {
            return;
        }
        if let Some(n) = action.strip_prefix("goto:") {
            if let Some(item) = n.parse::<usize>().ok().and_then(|n| self.ai.plans[i].plan.items.get(n)) {
                let t = item.start;
                self.set_playhead(t);
            }
            return;
        }
        match action {
            "review" => self.ai.plans[i].reviewing = !self.ai.plans[i].reviewing,
            "cancel" => {
                self.ai.plans[i].state = 2;
                self.log_action(i, AIActionStatus::Rejected);
            }
            "apply" => self.apply_plan(i),
            "undo" => {
                let id = self.ai.plans[i].plan.id;
                if self.engine.undo_action(&mut self.project, id).is_some() {
                    self.ai.plans[i].state = 3;
                    self.log_action(i, AIActionStatus::Undone);
                    let p = &self.ai.plans[i].plan;
                    self.project.preference_events.push(EditorPreferenceEvent {
                        at_ms: now_ms(),
                        action: Some(id),
                        kind: CorrectionKind::ActionUndone,
                        context: serde_json::json!({"prompt": p.prompt, "title": p.title}),
                        features: serde_json::json!({"items": p.enabled_count(), "tier": p.tier.label()}),
                        ai_choice: p.proposal.clone(),
                        ai_confidence: p.confidence,
                        human_choice: "undo".into(),
                        decision: None,
                    });
                    self.meta_dirty = true;
                    self.after_edit();
                } else {
                    self.toast_warn(t("toast.newer_edits"));
                }
            }
            _ => {}
        }
        self.refresh_ai();
    }

    fn apply_plan(&mut self, i: usize) {
        let plan = self.ai.plans[i].plan.clone();
        let cmds = match command::validate(&plan.enabled_commands(), &self.project, &self.ai.assistant.permissions) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "plan failed validation");
                self.ai.say(4, tf("ai.msg.plan_stale", &[("error", &e.to_string())]));
                return;
            }
        };
        let batch = EditCommand::Batch { label: tf("cmd.ai_prefix", &[("what", &plan.title)]), commands: cmds };
        if self.execute_as(batch, EditSource::Ai, Some(plan.id)) {
            self.ai.plans[i].state = 1;
            self.log_action(i, AIActionStatus::Applied);
            // Record which items the human switched off in Review.
            let disabled: Vec<String> = plan.items.iter().filter(|x| !x.enabled).map(|x| x.label.clone()).collect();
            if !disabled.is_empty() {
                self.project.preference_events.push(EditorPreferenceEvent {
                    at_ms: now_ms(),
                    action: Some(plan.id),
                    kind: CorrectionKind::SegmentRestored,
                    context: serde_json::json!({"prompt": plan.prompt}),
                    features: serde_json::json!({"rejected_items": disabled}),
                    ai_choice: "delete".into(),
                    ai_confidence: plan.confidence,
                    human_choice: "keep".into(),
                    decision: None,
                });
            }
            let after = self.project.sequence().duration();
            self.ai.say(1, tf("ai.msg.applied", &[("d", &duration(after.as_secs_f64()))]));
        }
    }

    fn log_action(&mut self, i: usize, status: AIActionStatus) {
        let p = &self.ai.plans[i].plan;
        if let Some(a) = self.project.ai_actions.iter_mut().find(|a| a.id == p.id) {
            a.status = status;
        } else {
            self.project.ai_actions.push(AIAction {
                id: p.id,
                at_ms: now_ms(),
                prompt: p.prompt.clone(),
                tier: p.tier.label().to_lowercase(),
                provider: p.provider.clone(),
                model: p.model.clone(),
                summary: p.proposal.clone(),
                commands: serde_json::to_value(p.enabled_commands()).unwrap_or_default(),
                status,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: p.cost_usd,
            });
        }
        self.meta_dirty = true;
    }

    pub fn ai_clear(&mut self) {
        // Pending cloud requests are cancelled; applied edits stay (use Undo).
        for o in &self.ai.offers {
            o.cancel.cancel();
        }
        self.ai.chat.clear();
        self.ai.plans.clear();
        self.ai.offers.clear();
        self.refresh_ai();
    }

    pub fn ai_plan_toggle(&mut self, i: usize, j: usize) {
        if let Some(item) = self.ai.plans.get_mut(i).and_then(|p| p.plan.items.get_mut(j)) {
            item.enabled = !item.enabled;
        }
        self.refresh_ai();
    }

    pub fn ai_offer_action(&mut self, i: usize, action: &str) {
        if i >= self.ai.offers.len() {
            return;
        }
        match action {
            "run" => {
                let offer = self.ai.offers[i].offer.clone();
                let cancel = CancelToken::new();
                self.ai.offers[i].cancel = cancel.clone();
                self.ai.offers[i].state = 1;
                self.ai.assistant.run_cloud(offer, self.project.clone(), true, cancel, move |r| {
                    post(move |app| app.on_cloud_result(i, r));
                });
            }
            "abort" => {
                self.ai.offers[i].cancel.cancel();
                self.ai.offers[i].state = 3;
            }
            "cheaper" => {
                let o = self.ai.offers[i].offer.clone();
                self.ai.offers[i].state = 3;
                if let Some(t) = o.tier.cheaper().filter(|t| t.is_cloud()) {
                    let st = self.editor_state();
                    let r = self.ai.assistant.cloud_offer(&o.prompt, &self.project, &st, t);
                    self.handle_reply(r);
                }
            }
            "local" => {
                self.ai.offers[i].state = 3;
                self.ai.say(1, t("ai.msg.local_only_help"));
            }
            "settings" => self.open_settings(1),
            _ => {}
        }
        self.refresh_ai();
    }

    fn on_cloud_result(&mut self, i: usize, r: Result<(Plan, AIAction), kadr_ai::providers::AiError>) {
        let Some(o) = self.ai.offers.get_mut(i) else { return };
        if o.state == 3 {
            return; // aborted by the user
        }
        match r {
            Ok((plan, action)) => {
                o.state = 2;
                self.project.ai_cost_usd += action.cost_usd;
                self.record_month_spend(action.cost_usd);
                self.project.ai_actions.push(action);
                self.meta_dirty = true;
                if plan.is_empty() {
                    self.ai.say(1, plan.findings.join("\n"));
                } else {
                    self.ai.plans.push(PlanUi { plan, reviewing: false, state: 0 });
                    let n = self.ai.plans.len() as i32 - 1;
                    self.ai.chat.push((2, String::new(), n));
                }
            }
            Err(e) => {
                o.state = 4;
                self.ai.say(4, tf("ai.msg.error", &[("error", &ai_error_text(&e))]));
            }
        }
        self.refresh_ai();
        self.refresh_status();
    }

    pub fn record_month_spend(&mut self, usd: f64) {
        let key = month_key();
        let s = &mut self.ai.assistant.settings;
        if s.month_key != key {
            s.month_key = key;
            s.month_spent_usd = 0.0;
        }
        s.month_spent_usd += usd;
        self.ai.assistant.ledger.lock().unwrap().month_usd = s.month_spent_usd;
        let _ = s.save(&self.dirs.data.join("ai-settings.json"));
    }

    pub fn refresh_ai_status(&mut self) {
        let ui = self.ui();
        {
            let mut l = self.ai.assistant.ledger.lock().unwrap();
            l.project_usd = self.project.ai_cost_usd;
            l.month_usd = if self.ai.assistant.settings.month_key == month_key() { self.ai.assistant.settings.month_spent_usd } else { 0.0 };
        }
        let l = self.ai.assistant.ledger.lock().unwrap().clone();
        ui.set_ai_mode_label(self.ai.assistant.status_label().into());
        ui.set_ai_cloud(self.ai.assistant.cloud_enabled());
        ui.set_session_cost(format!("${:.3}", l.session_usd).into());
        ui.set_project_cost(money(l.project_usd).into());
        ui.set_ai_cost_label(
            tf("ai.cost_line", &[("session", &format!("${:.3}", l.session_usd)), ("project", &money(l.project_usd)), ("month", &money(l.month_usd))]).into(),
        );
    }

    pub fn refresh_ai(&mut self) {
        let ui = self.ui();
        let chat: Vec<ChatMsg> = self.ai.chat.iter().map(|(k, t, i)| ChatMsg { kind: *k, text: t.clone().into(), index: *i }).collect();
        let plans: Vec<PlanView> = self
            .ai
            .plans
            .iter()
            .map(|p| {
                let pl = &p.plan;
                let items: Vec<PlanItemView> = pl.items.iter().map(|x| PlanItemView { label: x.label.clone().into(), enabled: x.enabled }).collect();
                let findings: Vec<SharedString> = pl.findings.iter().map(|f| SharedString::from(f.as_str())).collect();
                PlanView {
                    title: pl.title.clone().into(),
                    findings: ModelRc::new(VecModel::from(findings)),
                    proposal: pl.proposal.clone().into(),
                    count: pl.enabled_count() as i32,
                    tier: pl.tier.label().into(),
                    source: format!("{} · {}", pl.provider, pl.model).into(),
                    cost: money(pl.cost_usd).into(),
                    confidence: format!("{:.0}%", pl.confidence * 100.0).into(),
                    duration_change: match (pl.before_duration, pl.after_duration) {
                        (Some(a), Some(b)) => tf("ai.plan.duration_change", &[("a", &duration(a.as_secs_f64())), ("b", &duration(b.as_secs_f64()))]).into(),
                        _ => "".into(),
                    },
                    items: ModelRc::new(VecModel::from(items)),
                    reviewing: p.reviewing,
                    state: p.state,
                }
            })
            .collect();
        let offers: Vec<OfferView> = self
            .ai
            .offers
            .iter()
            .map(|o| {
                let of = &o.offer;
                let warnings: Vec<SharedString> = match &of.budget {
                    BudgetDecision::ExceedsLimit(v) => v
                        .iter()
                        .map(|h| {
                            SharedString::from(tf(
                                "ai.offer.over_budget",
                                &[("limit", &t(h.kind)), ("spent", &money(h.spent)), ("est", &money(h.estimate)), ("max", &money(h.limit))],
                            ))
                        })
                        .collect(),
                    BudgetDecision::Allowed => vec![],
                };
                let blocked = match &of.gate {
                    GateDecision::Deny(r) => t(r),
                    _ => String::new(),
                };
                OfferView {
                    title: match of.tier {
                        kadr_ai::Tier::Director => t("ai.offer.title_director").into(),
                        _ => t("ai.offer.title").into(),
                    },
                    tier: of.tier.label().into(),
                    provider: of.provider_name.clone().into(),
                    model: of.model.clone().into(),
                    input: fmt_tokens(of.estimate.input_tokens).into(),
                    cost: money_range(of.estimate.low_usd, of.estimate.high_usd).into(),
                    warnings: ModelRc::new(VecModel::from(warnings)),
                    blocked: blocked.into(),
                    can_cheaper: of.tier.cheaper().is_some_and(|t| t.is_cloud()),
                    state: o.state,
                }
            })
            .collect();
        ui.set_chat(ModelRc::new(VecModel::from(chat)));
        ui.set_plans(ModelRc::new(VecModel::from(plans)));
        ui.set_offers(ModelRc::new(VecModel::from(offers)));
        ui.set_ai_busy(self.ai.offers.iter().any(|o| o.state == 1));
        self.refresh_ai_status();
    }
}

/// User-facing text for provider / network errors.
pub fn ai_error_text(e: &kadr_ai::providers::AiError) -> String {
    use kadr_ai::providers::AiError::*;
    match e {
        NoKey(p) => tf("err.ai.no_key_for", &[("provider", p)]),
        Network(d) => tf("err.ai.network", &[("detail", d)]),
        Timeout => t("err.ai.timeout"),
        RateLimited { .. } => t("err.ai.rate_limited"),
        Auth => t("err.ai.auth"),
        Http { status, .. } => tf("err.ai.http", &[("status", &status.to_string())]),
        BadResponse(d) => tf("err.ai.bad_response", &[("detail", d)]),
        Cancelled => t("err.ai.cancelled"),
        TooLarge => t("err.ai.too_large"),
        UnknownModel(m) => tf("err.ai.unknown_model", &[("model", m)]),
        Blocked(r) => {
            let k = if r.starts_with("privacy.") { t(r) } else { r.clone() };
            tf("err.ai.blocked", &[("reason", &k)])
        }
    }
}

pub fn month_key() -> String {
    // Days since epoch → civil date (Howard Hinnant's algorithm), UTC.
    let days = now_ms().div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}")
}
