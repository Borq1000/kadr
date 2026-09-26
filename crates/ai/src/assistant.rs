//! The AI assistant: routes a natural-language request to a local
//! operation or — only when policy, budget and the user allow — to an LLM,
//! and always returns something reviewable rather than editing directly.

use crate::command::{self, AiCommand, Permissions};
use crate::context::{project_context, EditorState};
use crate::cost::{check_budget, estimate_tokens, BudgetDecision, CostEstimate, Ledger};
use crate::intent::{self, Intent};
use crate::local_ops::{self, LocalOpError};
use crate::plan::{Plan, PlanItem};
use crate::privacy::{gate, DataKind, GateDecision};
use crate::providers::{self, AiError, ChatMessage, ChatRequest, Role};
use crate::settings::AiSettings;
use crate::tier::Tier;
use kadr_core::{ActionId, AssetId, CancelToken, Time, TimeRange};
use kadr_project::{AIAction, AIActionStatus, Project, now_ms};
use kadr_timeline::EditCommand;
use kadr_i18n::{duration, t, tf, tn};
use std::sync::{Arc, Mutex};

/// Padding kept around speech when removing pauses.
pub const PAUSE_PADDING: Time = Time::from_millis(150);

#[derive(Debug)]
pub enum Reply {
    /// Reviewable plan: Apply / Review / Cancel.
    Plan(Plan),
    /// Trivial, unambiguous edit executed immediately (still one undo step).
    Execute { label: String, command: EditCommand, message: String },
    Undo,
    Redo,
    /// Local analysis still running for these assets.
    NeedsAnalysis { message: String, assets: Vec<AssetId> },
    /// The request needs an LLM: show the cost preview first.
    CloudOffer(CloudOffer),
    Message(String),
    /// Jev shot grading (the editor gathers shots and shows the cost card).
    GradeShots,
    /// Jev multicam auto-cut for the multicam clip(s) in play.
    CutCameras,
}

#[derive(Clone, Debug)]
pub struct CloudOffer {
    pub prompt: String,
    pub tier: Tier,
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub estimate: CostEstimate,
    pub budget: BudgetDecision,
    pub gate: GateDecision,
    pub(crate) system: String,
    pub(crate) user: String,
}

impl CloudOffer {
    pub fn blocked_reason(&self) -> Option<String> {
        match &self.gate {
            GateDecision::Deny(r) => Some(r.clone()),
            _ => None,
        }
    }
}

pub struct Assistant {
    pub settings: AiSettings,
    pub ledger: Arc<Mutex<Ledger>>,
    pub permissions: Permissions,
    runtime: tokio::runtime::Runtime,
}

impl Assistant {
    pub fn new(settings: AiSettings) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("kadr-ai-net")
            .enable_all()
            .build()
            .expect("tokio runtime");
        Assistant { settings, ledger: Arc::new(Mutex::new(Ledger::default())), permissions: Permissions::default(), runtime }
    }

    /// Handle of the AI network runtime (for provider tests etc.).
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// True when free-form requests may reach a cloud provider.
    pub fn cloud_enabled(&self) -> bool {
        use crate::privacy::PrivacyMode::*;
        matches!(self.settings.mode, AskBeforeCloud | AllowSelected)
    }

    /// Status-bar label: "AI: LOCAL" or "AI: Anthropic / claude-sonnet-5".
    pub fn status_label(&self) -> String {
        use crate::privacy::PrivacyMode::*;
        match self.settings.mode {
            Off => "AI: OFF".into(),
            LocalOnly => "AI: LOCAL".into(),
            _ => match self.settings.route(self.settings.default_tier) {
                Some((p, m)) => format!("AI: {} / {m}", p.name),
                None => "AI: LOCAL".into(),
            },
        }
    }

    /// Handles a request synchronously. Never performs network I/O.
    pub fn handle(&self, text: &str, project: &Project, st: &EditorState) -> Reply {
        if self.settings.mode == crate::privacy::PrivacyMode::Off {
            return Reply::Message(t("ai.msg.off"));
        }
        match intent::parse(text) {
            Intent::RemovePauses { min } => match local_ops::plan_remove_pauses(project, text, min, PAUSE_PADDING) {
                Ok(plan) => Reply::Plan(plan),
                Err(LocalOpError::AnalysisPending(assets)) => Reply::NeedsAnalysis {
                    message: tn("ai.msg.analysis_pending", assets.len() as i64, &[]),
                    assets,
                },
                Err(LocalOpError::NothingToAnalyse) => Reply::Message(t("ai.msg.no_audio")),
            },
            Intent::SplitAtPlayhead => Reply::Execute {
                label: "Split".into(),
                command: EditCommand::Split { at: st.playhead, clips: (!st.selection.is_empty()).then(|| st.selection.clone()) },
                message: t("ai.msg.split_done"),
            },
            Intent::DeleteLast { duration } => {
                let end = project.sequence().duration();
                let start = (end - duration).max(Time::ZERO);
                self.range_plan(project, text, &t("ai.plan.delete_end"), TimeRange::new(start, end), "last seconds")
            }
            Intent::DeleteFirst { duration } => {
                let end = duration.min(project.sequence().duration());
                self.range_plan(project, text, &t("ai.plan.delete_start"), TimeRange::new(Time::ZERO, end), "first seconds")
            }
            Intent::Undo => Reply::Undo,
            Intent::Redo => Reply::Redo,
            Intent::AddMarker { name } => Reply::Execute {
                label: "Add marker".into(),
                command: EditCommand::AddMarker(kadr_project::Marker::new(
                    st.playhead,
                    if name.is_empty() { "Marker".to_string() } else { name },
                )),
                message: t("ai.msg.marker_added"),
            },
            Intent::GradeShots => Reply::GradeShots,
            Intent::CutCameras => Reply::CutCameras,
            Intent::Unknown => self.cloud_offer(text, project, st, self.settings.default_tier),
        }
    }

    fn range_plan(&self, project: &Project, prompt: &str, title: &str, r: TimeRange, reason: &str) -> Reply {
        if r.is_empty() {
            return Reply::Message(t("ai.msg.timeline_empty"));
        }
        let dur = project.sequence().duration();
        Reply::Plan(Plan {
            id: ActionId::new(),
            prompt: prompt.into(),
            title: title.into(),
            findings: vec![tf("ai.plan.range", &[("a", &ms_label(r.start)), ("b", &ms_label(r.end))])],
            proposal: tf("ai.plan.range_proposal", &[("d", &duration(r.duration().as_secs_f64()))]),
            items: vec![PlanItem {
                label: format!("{} – {}", ms_label(r.start), ms_label(r.end)),
                start: r.start,
                end: r.end,
                enabled: true,
                command: AiCommand::DeleteRange {
                    sequence_id: None,
                    start_ms: r.start.as_millis(),
                    end_ms: r.end.as_millis(),
                    ripple: true,
                    reason: reason.into(),
                },
            }],
            tier: Tier::Local,
            provider: "Local".into(),
            model: "intent-parser v1".into(),
            cost_usd: 0.0,
            confidence: 1.0,
            before_duration: Some(dur),
            after_duration: Some(dur - r.duration()),
        })
    }

    /// Builds the cost preview for an LLM request (no network yet).
    pub fn cloud_offer(&self, text: &str, project: &Project, st: &EditorState, tier: Tier) -> Reply {
        let Some((provider, model)) = self.settings.route(tier) else {
            return Reply::Message(tf("ai.msg.no_provider", &[("tier", tier.label())]));
        };
        let system = format!(
            "You are the edit planner inside Kadr, a professional non-linear video editor. \
             You never edit media directly: you propose edit commands that a deterministic engine validates \
             and the user reviews. Prefer few, precise commands. Answer the summary in the user's language.\n\n{}",
            command::schema_for_prompt()
        );
        let ctx = project_context(project, st);
        let user = format!("Project state (JSON):\n{ctx}\n\nRequest: {text}");
        let input = estimate_tokens(&system) + estimate_tokens(&user);
        let pricing = provider.pricing(model);
        let estimate = CostEstimate::new(&pricing, input, 200, 4000);
        let ledger = self.ledger.lock().unwrap().clone();
        Reply::CloudOffer(CloudOffer {
            prompt: text.into(),
            tier,
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            model: model.into(),
            estimate,
            budget: check_budget(&self.settings.limits, &ledger, &estimate),
            gate: gate(self.settings.mode, provider.is_local(), &provider.permissions, &[DataKind::Text]),
            system,
            user,
        })
    }

    /// Runs a confirmed cloud offer in the background. `done` is called on
    /// the network thread with the plan (validated) or an error.
    pub fn run_cloud(
        &self,
        offer: CloudOffer,
        project: Project,
        confirmed_by_user: bool,
        cancel: CancelToken,
        done: impl FnOnce(Result<(Plan, AIAction), AiError>) + Send + 'static,
    ) {
        // Deterministic safeguards — re-checked here, not just in the UI.
        let blocked = match (&offer.gate, &offer.budget) {
            (GateDecision::Deny(r), _) => Some(r.clone()),
            (GateDecision::Ask, _) if !confirmed_by_user => Some("cloud request requires confirmation".into()),
            (_, BudgetDecision::ExceedsLimit(_)) if !confirmed_by_user => Some("budget exceeded".into()),
            _ => None,
        };
        if let Some(r) = blocked {
            done(Err(AiError::Blocked(r)));
            return;
        }
        let Some(cfg) = self.settings.provider(&offer.provider_id).cloned() else {
            done(Err(AiError::Blocked("provider not configured".into())));
            return;
        };
        let key = if cfg.needs_key() { crate::credentials::load_key(&cfg.id) } else { None };
        let provider = match providers::build(cfg.clone(), key) {
            Ok(p) => p,
            Err(e) => {
                done(Err(e));
                return;
            }
        };
        let ledger = self.ledger.clone();
        let perms = self.permissions.clone();
        tracing::info!(provider = %cfg.name, model = %offer.model, est_usd = offer.estimate.high_usd, "cloud AI request");
        self.runtime.spawn(async move {
            let req = ChatRequest {
                model: offer.model.clone(),
                messages: vec![
                    ChatMessage { role: Role::System, content: offer.system.clone() },
                    ChatMessage { role: Role::User, content: offer.user.clone() },
                ],
                max_output_tokens: offer.estimate.output_tokens_max as u32,
                temperature: 0.2,
                json: true,
            };
            let fut = provider.complete(req);
            let cancelled = async {
                while !cancel.is_cancelled() {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            };
            let resp = tokio::select! {
                r = fut => r,
                _ = cancelled => Err(AiError::Cancelled),
            };
            let result = resp.and_then(|r| {
                let cost = cfg.pricing(&offer.model).cost(r.input_tokens, r.output_tokens);
                ledger.lock().unwrap().record(cost, r.input_tokens, r.output_tokens);
                plan_from_llm(&r.text, &project, &offer, cost, r.input_tokens, r.output_tokens, &perms)
            });
            done(result);
        });
    }
}

fn ms_label(t: Time) -> String {
    let ms = t.as_millis().max(0);
    format!("{:02}:{:02}.{:01}", ms / 60_000, (ms / 1000) % 60, (ms % 1000) / 100)
}

/// Parses and validates an LLM answer into a plan.
pub fn plan_from_llm(
    text: &str,
    project: &Project,
    offer: &CloudOffer,
    cost: f64,
    input_tokens: u64,
    output_tokens: u64,
    perms: &Permissions,
) -> Result<(Plan, AIAction), AiError> {
    // Tolerate code fences around the JSON.
    let json = text.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| AiError::BadResponse(format!("not JSON: {e}")))?;
    let summary = v["summary"].as_str().unwrap_or("").to_string();
    let confidence = v["confidence"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0) as f32;
    let cmds = command::parse_commands(&v["commands"].to_string()).map_err(|e| AiError::BadResponse(e.to_string()))?;
    // Validate now so the user never reviews an impossible plan.
    command::validate(&cmds, project, perms).map_err(|e| {
        tracing::warn!(error = %e, "LLM proposed invalid EditCommands");
        AiError::BadResponse(format!("invalid edit command: {e}"))
    })?;
    let items: Vec<PlanItem> = cmds
        .iter()
        .map(|c| {
            let (s, e) = match c {
                AiCommand::DeleteRange { start_ms, end_ms, .. } => (*start_ms, *end_ms),
                AiCommand::SplitClip { at_ms, .. } | AiCommand::AddMarker { at_ms, .. } | AiCommand::AddTransition { at_ms, .. } => {
                    (*at_ms, *at_ms)
                }
                _ => (0, 0),
            };
            let kind = serde_json::to_value(c).ok().and_then(|v| v["type"].as_str().map(str::to_string)).unwrap_or_default();
            PlanItem {
                label: if c.reason().is_empty() { kind } else { format!("{kind}: {}", c.reason()) },
                start: Time::from_millis(s),
                end: Time::from_millis(e),
                enabled: true,
                command: c.clone(),
            }
        })
        .collect();
    let plan = Plan {
        id: ActionId::new(),
        prompt: offer.prompt.clone(),
        title: tf("ai.plan.cloud_title", &[("tier", offer.tier.label())]),
        findings: vec![summary.clone()],
        proposal: tn("ai.plan.operations", items.len() as i64, &[]),
        items,
        tier: offer.tier,
        provider: offer.provider_name.clone(),
        model: offer.model.clone(),
        cost_usd: cost,
        confidence,
        before_duration: None,
        after_duration: None,
    };
    let action = AIAction {
        id: plan.id,
        at_ms: now_ms(),
        prompt: offer.prompt.clone(),
        tier: offer.tier.label().to_lowercase(),
        provider: offer.provider_name.clone(),
        model: offer.model.clone(),
        summary,
        commands: serde_json::to_value(&cmds).unwrap_or_default(),
        status: AIActionStatus::Proposed,
        input_tokens,
        output_tokens,
        cost_usd: cost,
    };
    Ok((plan, action))
}
