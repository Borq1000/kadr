//! Settings dialog: General (language, autosave, preview, storage),
//! AI & Privacy (mode, keys in the credential store, permissions, routes,
//! budgets) and Keyboard shortcuts.

use crate::app::{post, App, Confirm};
use crate::util::money;
use crate::{ProviderRow, SettingsState};
use kadr_ai::credentials::{delete_key, load_key, store_key, Secret};
use kadr_ai::privacy::PrivacyMode;
use kadr_ai::providers::{self, ChatMessage, ChatRequest, JevProvider, JevQuestion, ProviderKind, Role};
use kadr_ai::Tier;
use kadr_i18n::{t, tf};
use slint::{ModelRc, VecModel};
use std::collections::BTreeMap;

const AUTOSAVE_STEPS: [u64; 4] = [0, 60, 120, 300];

impl App {
    fn save_ai_settings(&mut self) {
        if let Err(e) = self.ai.assistant.settings.save(&self.dirs.data.join("ai-settings.json")) {
            self.toast_error(tf("err.settings.save", &[("error", &e.to_string())]));
        }
    }

    pub fn refresh_settings(&mut self) {
        let s = &self.ai.assistant.settings;
        let providers: Vec<ProviderRow> = s
            .providers
            .iter()
            .map(|p| ProviderRow {
                id: p.id.clone().into(),
                name: p.name.clone().into(),
                kind: format!("{:?}", p.kind).to_uppercase().into(),
                base_url: p.base_url.clone().into(),
                has_key: p.is_local() || load_key(&p.id).is_some(),
                allowed: p.permissions.allowed,
                local: p.is_local(),
            })
            .collect();
        let route = |tier: Tier| s.route(tier).map(|(p, m)| format!("{m}  ({})", p.name)).unwrap_or_default();
        let lim = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_default();
        let l = self.ai.assistant.ledger.lock().unwrap().clone();
        let autosave = AUTOSAVE_STEPS.iter().position(|&v| v == self.settings.autosave_secs).unwrap_or(1) as i32;
        let st = SettingsState {
            tab: self.settings.settings_tab,
            language: crate::app::lang_index(),
            autosave,
            quality: self.settings.preview_quality,
            mode: match s.mode {
                PrivacyMode::Off => 0,
                PrivacyMode::LocalOnly => 1,
                PrivacyMode::AskBeforeCloud => 2,
                PrivacyMode::AllowSelected => 3,
            },
            providers: ModelRc::new(VecModel::from(providers)),
            economy: route(Tier::Economy).into(),
            smart: route(Tier::Smart).into(),
            director: route(Tier::Director).into(),
            jev_model: s.jev_model.clone().into(),
            per_request: lim(s.limits.per_request_usd).into(),
            per_session: lim(s.limits.per_session_usd).into(),
            per_project: lim(s.limits.per_project_usd).into(),
            monthly: lim(s.limits.monthly_usd).into(),
            spent: tf(
                "settings.spent",
                &[
                    ("session", &format!("${:.4}", l.session_usd)),
                    ("tin", &l.session_input_tokens.to_string()),
                    ("tout", &l.session_output_tokens.to_string()),
                    ("project", &money(self.project.ai_cost_usd)),
                    ("month", &money(s.month_spent_usd)),
                ],
            )
            .into(),
            cache_size: tf("settings.cache_size", &[("size", &crate::util::fmt_bytes(self.cache.size_bytes())), ("path", &self.cache.root().display().to_string())]).into(),
            data_dir: self.dirs.data.display().to_string().into(),
        };
        let ui = self.ui();
        ui.set_settings_state(st);
        ui.set_shortcuts(ModelRc::new(VecModel::from(crate::keys::shortcut_rows())));
    }

    pub fn open_settings(&mut self, tab: i32) {
        self.settings.settings_tab = tab;
        self.refresh_settings();
        self.ui().set_settings_open(true);
    }

    pub fn settings_tab(&mut self, tab: i32) {
        self.settings.settings_tab = tab;
        self.settings.save(&self.dirs.settings_file());
        self.refresh_settings();
    }

    pub fn settings_autosave(&mut self, i: i32) {
        self.settings.autosave_secs = AUTOSAVE_STEPS.get(i as usize).copied().unwrap_or(60);
        self.settings.save(&self.dirs.settings_file());
        self.refresh_settings();
    }

    pub fn settings_clear_cache(&mut self) {
        if self.jobs.active_count() > 0 {
            return self.toast_warn(t("toast.cache_busy"));
        }
        self.ask(&t("dlg.clear_cache.title"), &t("dlg.clear_cache.body"), &t("dlg.clear_cache.ok"), "", true, Confirm::ClearCache);
    }

    pub fn clear_cache_now(&mut self) {
        let root = self.cache.root().to_path_buf();
        let before = self.cache.size_bytes();
        let result = std::fs::remove_dir_all(&root);
        match result {
            _ if !root.exists() => {
                self.toast_ok(tf("toast.cache_cleared", &[("size", &crate::util::fmt_bytes(before))]));
                // Rebuild what the open project needs.
                let ids: Vec<_> = self.project.assets.iter().map(|a| a.id).collect();
                self.assets_rt.clear();
                self.waves.clear();
                for id in ids {
                    self.process_asset(id);
                }
                self.refresh_library();
            }
            Err(e) => self.toast_error(tf("err.cache.clear", &[("error", &e.to_string())])),
            Ok(()) => {}
        }
        self.refresh_settings();
    }

    pub fn settings_mode(&mut self, m: i32) {
        self.ai.assistant.settings.mode = match m {
            0 => PrivacyMode::Off,
            1 => PrivacyMode::LocalOnly,
            2 => PrivacyMode::AskBeforeCloud,
            _ => PrivacyMode::AllowSelected,
        };
        tracing::info!(mode = ?self.ai.assistant.settings.mode, "privacy mode changed");
        self.save_ai_settings();
        self.refresh_settings();
        self.refresh_ai_status();
        // Offers created under the old mode are no longer accurate.
        self.ai.refresh_offer_gates();
        self.refresh_ai();
    }

    pub fn settings_save_key(&mut self, id: &str, key: &str) {
        match store_key(id, &Secret::new(key.trim())) {
            Ok(()) => self.toast_ok(tf("toast.key_stored", &[("provider", id)])),
            Err(e) => self.toast_error(tf("err.key_store", &[("error", &e)])),
        }
        self.refresh_settings();
    }

    pub fn settings_delete_key(&mut self, id: &str) {
        match delete_key(id) {
            Ok(()) => self.toast(tf("toast.key_removed", &[("provider", id)])),
            Err(e) => self.toast_error(tf("err.key_store", &[("error", &e)])),
        }
        self.refresh_settings();
    }

    pub fn settings_toggle_allowed(&mut self, id: &str) {
        if let Some(p) = self.ai.assistant.settings.providers.iter_mut().find(|p| p.id == id) {
            p.permissions.allowed = !p.permissions.allowed;
            // "Allowed" means text context may be sent; media kinds stay off in V0.1.
            p.permissions.text = true;
        }
        self.save_ai_settings();
        self.refresh_settings();
        self.ai.refresh_offer_gates();
        self.refresh_ai();
    }

    pub fn settings_route(&mut self, tier: &str, model: &str) {
        let model = model.split("  (").next().unwrap_or(model).trim().to_string();
        if model.is_empty() {
            return;
        }
        let s = &mut self.ai.assistant.settings;
        if tier == "jev" {
            s.jev_model = model;
        } else {
            let tr = match tier {
                "economy" => Tier::Economy,
                "director" => Tier::Director,
                _ => Tier::Smart,
            };
            match s.routes.iter_mut().find(|r| r.tier == tr) {
                Some(r) => r.model = model,
                None => s.routes.push(kadr_ai::settings::TierRoute { tier: tr, provider_id: "openai".into(), model }),
            }
        }
        self.save_ai_settings();
    }

    pub fn settings_limit(&mut self, which: &str, v: &str) {
        let val = v.trim().trim_start_matches('$').replace(',', ".").parse::<f64>().ok().filter(|x| *x >= 0.0);
        if !v.trim().is_empty() && val.is_none() {
            return; // keep typing
        }
        let l = &mut self.ai.assistant.settings.limits;
        match which {
            "request" => l.per_request_usd = val,
            "session" => l.per_session_usd = val,
            "project" => l.per_project_usd = val,
            _ => l.monthly_usd = val,
        }
        self.save_ai_settings();
    }

    /// Sends one tiny request to verify the key (costs a fraction of a cent).
    pub fn settings_test(&mut self, id: &str) {
        let Some(cfg) = self.ai.assistant.settings.provider(id).cloned() else { return };
        let key = load_key(id);
        let rt = self.ai.assistant.runtime();
        self.toast(tf("toast.testing", &[("provider", &cfg.name)]));
        let chat_model = self.ai.assistant.settings.route(Tier::Economy).map(|(_, m)| m.to_string()).unwrap_or_else(|| "gpt-5-mini".into());
        let jev_model = self.ai.assistant.settings.jev_model.clone();
        rt.spawn(async move {
            let name = cfg.name.clone();
            let result: Result<String, String> = if cfg.kind == ProviderKind::Jev {
                match key {
                    None => Err(t("err.ai.no_key")),
                    Some(k) => {
                        let jev = JevProvider::new(cfg, k);
                        let q = BTreeMap::from([("ok".to_string(), JevQuestion::Noul { instructions: "Is the sky usually blue on a clear day?".into() })]);
                        jev.decide(&jev_model, &serde_json::json!("Connectivity test."), &q)
                            .await
                            .map(|r| tf("toast.test_ok_jev", &[("model", &r.model), ("p", &format!("{:.2}", r.answers["ok"].noul.unwrap_or(0.0)))]))
                            .map_err(|e| crate::ai_ui::ai_error_text(&e))
                    }
                }
            } else {
                match providers::build(cfg, key) {
                    Err(e) => Err(crate::ai_ui::ai_error_text(&e)),
                    Ok(p) => p
                        .complete(ChatRequest {
                            model: chat_model,
                            messages: vec![ChatMessage { role: Role::User, content: "Reply with the single word: ok".into() }],
                            max_output_tokens: 200,
                            temperature: 0.0,
                            json: false,
                        })
                        .await
                        .map(|r| kadr_i18n::tn("toast.test_ok", (r.input_tokens + r.output_tokens) as i64, &[("model", &r.model)]))
                        .map_err(|e| crate::ai_ui::ai_error_text(&e)),
                }
            };
            post(move |app| match result {
                Ok(m) => app.toast_ok(format!("{name}: {m}")),
                Err(e) => app.toast_error(format!("{name}: {e}")),
            });
        });
    }
}
