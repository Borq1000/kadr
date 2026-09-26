//! Per-user application settings (`%LOCALAPPDATA%\Kadr\settings.json`):
//! language, recent projects, panel layout, autosave, preview quality.

use kadr_i18n::Lang;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_RECENT: usize = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// "ru" / "en"; empty = follow the system language.
    pub language: String,
    pub recent: Vec<RecentProject>,
    pub layout: Layout,
    /// Autosave interval in seconds (0 = off).
    pub autosave_secs: u64,
    pub preview_quality: i32,
    pub snapping: bool,
    pub settings_tab: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    pub name: String,
    pub opened_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub library_width: f32,
    pub inspector_width: f32,
    pub ai_width: f32,
    pub timeline_height: f32,
    pub ai_open: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Layout { library_width: 300.0, inspector_width: 300.0, ai_width: 350.0, timeline_height: 340.0, ai_open: true }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            language: String::new(),
            recent: vec![],
            layout: Layout::default(),
            autosave_secs: 60,
            preview_quality: 1,
            snapping: true,
            settings_tab: 0,
        }
    }
}

impl AppSettings {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = path.with_extension("tmp");
        let ok = std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default()).and_then(|_| std::fs::rename(&tmp, path));
        if let Err(e) = ok {
            tracing::warn!(error = %e, "could not save app settings");
        }
    }

    pub fn lang(&self) -> Lang {
        Lang::from_code(&self.language).unwrap_or_else(kadr_i18n::system_lang)
    }

    pub fn push_recent(&mut self, path: &Path, name: &str) {
        self.recent.retain(|r| r.path != path);
        self.recent.insert(0, RecentProject { path: path.to_path_buf(), name: name.to_string(), opened_ms: kadr_project::now_ms() });
        self.recent.truncate(MAX_RECENT);
    }
}
