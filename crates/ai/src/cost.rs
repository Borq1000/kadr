//! Token/cost estimation, spend ledger and budget enforcement.

use serde::{Deserialize, Serialize};

/// USD per million tokens. Users can edit these in settings — prices
/// change, and we never want to silently under-report cost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl Pricing {
    pub const FREE: Pricing = Pricing { input_per_mtok: 0.0, output_per_mtok: 0.0 };

    pub fn cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 * self.input_per_mtok + output_tokens as f64 * self.output_per_mtok) / 1_000_000.0
    }
}

/// Conservative token estimate for text (≈ 3.5 chars/token; Cyrillic and
/// JSON tokenize worse than English prose, so err on the high side).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as f64 / 3.5).ceil() as u64 + 8
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostEstimate {
    pub input_tokens: u64,
    pub output_tokens_min: u64,
    pub output_tokens_max: u64,
    pub low_usd: f64,
    pub high_usd: f64,
}

impl CostEstimate {
    pub fn new(pricing: &Pricing, input_tokens: u64, out_min: u64, out_max: u64) -> Self {
        CostEstimate {
            input_tokens,
            output_tokens_min: out_min,
            output_tokens_max: out_max,
            low_usd: pricing.cost(input_tokens, out_min),
            high_usd: pricing.cost(input_tokens, out_max),
        }
    }
    pub const ZERO: CostEstimate =
        CostEstimate { input_tokens: 0, output_tokens_min: 0, output_tokens_max: 0, low_usd: 0.0, high_usd: 0.0 };
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub per_request_usd: Option<f64>,
    pub per_session_usd: Option<f64>,
    pub per_project_usd: Option<f64>,
    pub monthly_usd: Option<f64>,
}

/// Spend so far. Session is in-memory; project lives in the project file;
/// month is persisted in settings keyed by "YYYY-MM".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    pub session_usd: f64,
    pub project_usd: f64,
    pub month_usd: f64,
    pub session_input_tokens: u64,
    pub session_output_tokens: u64,
}

impl Ledger {
    pub fn record(&mut self, usd: f64, input_tokens: u64, output_tokens: u64) {
        self.session_usd += usd;
        self.project_usd += usd;
        self.month_usd += usd;
        self.session_input_tokens += input_tokens;
        self.session_output_tokens += output_tokens;
    }
}

/// One exceeded limit. `kind` is an i18n key (`budget.request`, …).
#[derive(Clone, Debug, PartialEq)]
pub struct LimitHit {
    pub kind: &'static str,
    pub spent: f64,
    pub estimate: f64,
    pub limit: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BudgetDecision {
    Allowed,
    /// Over a limit: requires explicit confirmation naming the limit.
    ExceedsLimit(Vec<LimitHit>),
}

pub fn check_budget(limits: &BudgetLimits, ledger: &Ledger, est: &CostEstimate) -> BudgetDecision {
    let worst = est.high_usd;
    let mut over = vec![];
    let mut chk = |kind: &'static str, limit: Option<f64>, spent: f64| {
        if let Some(l) = limit {
            if spent + worst > l + 1e-12 {
                over.push(LimitHit { kind, spent, estimate: worst, limit: l });
            }
        }
    };
    chk("budget.request", limits.per_request_usd, 0.0);
    chk("budget.session", limits.per_session_usd, ledger.session_usd);
    chk("budget.project", limits.per_project_usd, ledger.project_usd);
    chk("budget.monthly", limits.monthly_usd, ledger.month_usd);
    if over.is_empty() { BudgetDecision::Allowed } else { BudgetDecision::ExceedsLimit(over) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_checks_every_limit() {
        let limits = BudgetLimits { per_request_usd: Some(0.10), per_session_usd: Some(1.0), per_project_usd: None, monthly_usd: Some(5.0) };
        let mut ledger = Ledger::default();
        let p = Pricing { input_per_mtok: 3.0, output_per_mtok: 15.0 };
        let small = CostEstimate::new(&p, 10_000, 500, 2_000); // ≤ $0.06
        assert_eq!(check_budget(&limits, &ledger, &small), BudgetDecision::Allowed);
        let big = CostEstimate::new(&p, 100_000, 1_000, 8_000); // $0.42
        assert!(matches!(check_budget(&limits, &ledger, &big), BudgetDecision::ExceedsLimit(v) if v.len() == 1));
        ledger.record(0.98, 0, 0);
        assert!(matches!(check_budget(&limits, &ledger, &small), BudgetDecision::ExceedsLimit(v) if v[0].kind == "budget.session"));
    }

    #[test]
    fn cost_math() {
        let p = Pricing { input_per_mtok: 1.0, output_per_mtok: 4.0 };
        assert!((p.cost(1_000_000, 250_000) - 2.0).abs() < 1e-12);
        assert!(estimate_tokens("привет мир") >= 3);
    }
}
