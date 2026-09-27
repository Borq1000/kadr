//! User AI settings (persisted as JSON in the app data dir — never keys).

use crate::cost::BudgetLimits;
use crate::privacy::{DataPermissions, PrivacyMode};
use crate::providers::{ModelPrice, ProviderConfig, ProviderKind};
use crate::tier::Tier;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TierRoute {
    pub tier: Tier,
    pub provider_id: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiSettings {
    pub mode: PrivacyMode,
    pub providers: Vec<ProviderConfig>,
    pub routes: Vec<TierRoute>,
    pub limits: BudgetLimits,
    /// Tier used for free-form requests the local parser does not understand.
    pub default_tier: Tier,
    /// Jev model for typed decisions (Economy tier).
    #[serde(default = "default_jev_model")]
    pub jev_model: String,
    /// Send the editor's past corrections as precedents to Jev.
    #[serde(default = "yes")]
    pub jev_personalize: bool,
    /// Monthly spend tracking ("YYYY-MM" → USD).
    #[serde(default)]
    pub month_key: String,
    #[serde(default)]
    pub month_spent_usd: f64,
}

fn yes() -> bool {
    true
}

fn default_jev_model() -> String {
    crate::providers::jev::DEFAULT_MODEL.into()
}

fn price(model: &str, i: f64, o: f64) -> ModelPrice {
    ModelPrice { model: model.into(), input_per_mtok: i, output_per_mtok: o }
}

impl Default for AiSettings {
    fn default() -> Self {
        // Prices are editable estimates (USD / 1M tokens) — verify against the
        // provider's price list; unknown models are assumed expensive.
        let text_only = DataPermissions { allowed: false, text: true, ..Default::default() };
        AiSettings {
            mode: PrivacyMode::LocalOnly,
            providers: vec![
                ProviderConfig {
                    id: "openai".into(),
                    name: "OpenAI".into(),
                    kind: ProviderKind::OpenAi,
                    base_url: "https://api.openai.com/v1".into(),
                    permissions: text_only.clone(),
                    prices: vec![
                        price("gpt-5-nano", 0.05, 0.4),
                        price("gpt-5-mini", 0.25, 2.0),
                        price("gpt-5", 1.25, 10.0),
                        price("gpt-5-pro", 15.0, 120.0),
                        price("gpt-4.1-mini", 0.4, 1.6),
                    ],
                    timeout_secs: 90,
                },
                ProviderConfig {
                    id: "anthropic".into(),
                    name: "Anthropic".into(),
                    kind: ProviderKind::Anthropic,
                    base_url: "https://api.anthropic.com".into(),
                    permissions: text_only.clone(),
                    prices: vec![
                        price("claude-haiku-4-5", 1.0, 5.0),
                        price("claude-sonnet-5", 3.0, 15.0),
                        price("claude-opus-5-5", 5.0, 25.0),
                    ],
                    timeout_secs: 120,
                },
                ProviderConfig {
                    id: "jev".into(),
                    name: "Jev".into(),
                    kind: ProviderKind::Jev,
                    base_url: "https://api.typesafe.ai".into(),
                    permissions: text_only.clone(),
                    prices: vec![price("jev-latest", 0.042, 0.0), price("jev-1.13.0", 0.042, 0.0)],
                    timeout_secs: 60,
                },
                ProviderConfig {
                    id: "local".into(),
                    name: "Local (Ollama)".into(),
                    kind: ProviderKind::Local,
                    base_url: "http://localhost:11434/v1".into(),
                    permissions: DataPermissions { allowed: true, text: true, images: true, audio: true, video: true },
                    prices: vec![],
                    timeout_secs: 300,
                },
            ],
            routes: vec![
                TierRoute { tier: Tier::Economy, provider_id: "openai".into(), model: "gpt-5-mini".into() },
                TierRoute { tier: Tier::Smart, provider_id: "openai".into(), model: "gpt-5".into() },
                TierRoute { tier: Tier::Director, provider_id: "openai".into(), model: "gpt-5-pro".into() },
            ],
            limits: BudgetLimits {
                per_request_usd: Some(0.50),
                per_session_usd: Some(2.00),
                per_project_usd: Some(10.00),
                monthly_usd: Some(20.00),
            },
            default_tier: Tier::Smart,
            jev_model: default_jev_model(),
            jev_personalize: true,
            month_key: String::new(),
            month_spent_usd: 0.0,
        }
    }
}

impl AiSettings {
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn route(&self, tier: Tier) -> Option<(&ProviderConfig, &str)> {
        let r = self.routes.iter().find(|r| r.tier == tier)?;
        Some((self.provider(&r.provider_id)?, r.model.as_str()))
    }

    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).expect("settings"))?;
        std::fs::rename(tmp, path)
    }

    /// Adds `usd` to month `key`'s spend. The file is re-read first, so
    /// spend recorded meanwhile by another Kadr (a second window, a headless
    /// MCP instance) is kept. `write_all` saves all of `self`; otherwise only
    /// the spend fields change on disk (an instance that must not overwrite
    /// the user's settings).
    pub fn record_spend(&mut self, path: &std::path::Path, key: &str, usd: f64, write_all: bool) -> std::io::Result<()> {
        let disk: Option<AiSettings> = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let spent = |s: &AiSettings| if s.month_key == key { s.month_spent_usd } else { 0.0 };
        let base = disk.as_ref().map_or(0.0, spent).max(spent(self));
        self.month_key = key.to_string();
        self.month_spent_usd = base + usd;
        if write_all {
            return self.save(path);
        }
        let mut out = disk.unwrap_or_else(|| self.clone());
        out.month_key = self.month_key.clone();
        out.month_spent_usd = self.month_spent_usd;
        out.save(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_is_added_to_what_another_kadr_already_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-settings.json");
        let mut window = AiSettings { month_key: "2026-09".into(), month_spent_usd: 5.0, ..AiSettings::default() };
        window.save(&path).unwrap();
        // A headless instance loaded the same file, the user then changed a
        // setting in their window.
        let mut headless = AiSettings::load(&path);
        window.jev_personalize = false;
        window.save(&path).unwrap();
        headless.record_spend(&path, "2026-09", 2.0, false).unwrap();
        let disk = AiSettings::load(&path);
        assert_eq!(disk.month_spent_usd, 7.0);
        assert!(!disk.jev_personalize, "a spend-only write keeps the user's newer settings");
        window.record_spend(&path, "2026-09", 1.0, true).unwrap();
        assert_eq!(window.month_spent_usd, 8.0, "the headless spend is not lost");
        assert_eq!(AiSettings::load(&path).month_spent_usd, 8.0);
    }

    #[test]
    fn a_new_month_starts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-settings.json");
        let mut s = AiSettings { month_key: "2026-08".into(), month_spent_usd: 9.0, ..AiSettings::default() };
        s.save(&path).unwrap();
        s.record_spend(&path, "2026-09", 0.5, true).unwrap();
        assert_eq!((s.month_key.as_str(), s.month_spent_usd), ("2026-09", 0.5));
        let fresh = dir.path().join("missing.json");
        let mut s = AiSettings::default();
        s.record_spend(&fresh, "2026-09", 0.25, false).unwrap();
        assert_eq!(AiSettings::load(&fresh).month_spent_usd, 0.25);
    }
}
