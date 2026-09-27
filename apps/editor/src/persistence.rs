//! New / open / save, recent projects, welcome screen, autosave, crash
//! recovery, confirmations, rename prompts, exit, and menu dispatch.
//! Native dialogs are opened *outside* the app borrow.

use crate::app::{defer, with_app, App, Confirm, Prompt};
use crate::RecentView;
use kadr_i18n::{t, tf};
use kadr_project::io;
use kadr_project::Project;
use kadr_timeline::EditCommand;
use slint::{ModelRc, VecModel};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub fn menu(m: &str) {
    match m {
        "open" => {
            let asked = with_app(|app| app.guard_unsaved(Box::new(|_| defer(open_dialog))));
            if asked == Some(false) {
                open_dialog();
            }
        }
        "save-as" => save_as(),
        "import" => crate::import::import_dialog(),
        _ => {
            with_app(|app| app.menu(m));
        }
    }
}

pub fn welcome_action(a: &str) {
    with_app(|app| app.ui().set_welcome_open(false));
    match a {
        "import" => defer(crate::import::import_dialog),
        "open" => defer(|| menu("open")),
        _ => {
            with_app(|app| app.ui().invoke_focus_editor());
        }
    }
}

fn open_dialog() {
    if let Some(p) = rfd::FileDialog::new().set_title(t("dlg.open_project")).add_filter(t("dlg.filter.project"), &[io::EXTENSION]).pick_file() {
        with_app(|app| app.open_project(p));
    }
}

fn save_as() {
    let name = with_app(|app| format!("{}.{}", app.display_name(), io::EXTENSION)).unwrap_or_default();
    if let Some(p) = rfd::FileDialog::new()
        .set_title(t("dlg.save_project"))
        .set_file_name(name)
        .add_filter(t("dlg.filter.project"), &[io::EXTENSION])
        .save_file()
    {
        with_app(|app| app.save_to(p));
    }
}

impl App {
    pub fn menu(&mut self, m: &str) {
        match m {
            "new" => {
                if !self.guard_unsaved(Box::new(|app| app.new_project())) {
                    self.new_project();
                }
            }
            "welcome" => self.show_welcome(),
            "save" => match self.path.clone() {
                Some(p) => self.save_to(p),
                None => defer(save_as),
            },
            "export" => self.open_export(),
            "settings" => self.open_settings(0),
            "settings-ai" => self.open_settings(1),
            "shortcuts" => self.open_settings(2),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "split" => self.split_at_playhead(),
            "delete" => self.delete_selection(self.tl.ripple),
            "ripple-delete" => self.delete_selection(true),
            "select-all" => self.select_all(),
            "deselect" => {
                self.tl.selection.clear();
                self.refresh_timeline();
                self.refresh_inspector();
                self.refresh_status();
            }
            "marker" => self.add_marker(),
            "add-video-track" => {
                self.execute(EditCommand::AddTrack { kind: kadr_project::TrackKind::Video });
            }
            "add-audio-track" => {
                self.execute(EditCommand::AddTrack { kind: kadr_project::TrackKind::Audio });
            }
            "toggle-ai" => {
                let ui = self.ui();
                ui.set_ai_open(!ui.get_ai_open());
                self.layout_changed();
            }
            "toggle-jobs" => {
                let ui = self.ui();
                ui.set_jobs_open(!ui.get_jobs_open());
                self.refresh_jobs();
            }
            "zoom-fit" => self.zoom_fit(),
            "snap" | "ripple" => self.tl_tool(m),
            "safe" | "fullscreen" => self.transport(m),
            "reset-layout" => self.reset_layout(),
            "lang-ru" => self.set_language(0),
            "lang-en" => self.set_language(1),
            "logs" => crate::util::open_folder(&self.dirs.logs()),
            "about" => self.toast(tf("toast.about", &[("version", env!("CARGO_PKG_VERSION"))])),
            "exit" => {
                if !self.request_exit() {
                    let _ = slint::quit_event_loop();
                }
            }
            _ => {}
        }
    }

    /// If there are unsaved changes, asks first and returns true (caller
    /// should stop; `then` runs after "Discard").
    pub fn guard_unsaved(&mut self, then: Box<dyn FnOnce(&mut App)>) -> bool {
        if !self.is_dirty() {
            return false;
        }
        self.ask(&t("dlg.unsaved.title"), &t("dlg.unsaved.body"), &t("dlg.unsaved.discard"), "", true, Confirm::DiscardThen(then));
        true
    }

    pub fn ask(&mut self, title: &str, msg: &str, ok: &str, alt: &str, danger: bool, c: Confirm) {
        let ui = self.ui();
        ui.set_confirm_title(title.into());
        ui.set_confirm_message(msg.into());
        ui.set_confirm_ok(ok.into());
        ui.set_confirm_alt(alt.into());
        ui.set_confirm_danger(danger);
        ui.set_confirm_open(true);
        self.confirm = Some(c);
    }

    pub fn confirm_answer(&mut self, ok: bool) {
        self.ui().set_confirm_open(false);
        let Some(c) = self.confirm.take() else { return };
        match (c, ok) {
            (Confirm::DiscardThen(f), true) => {
                self.meta_dirty = false;
                self.saved_rev = self.engine.history_marker();
                f(self);
            }
            (Confirm::Recover(p), true) => self.recover_from(p),
            (Confirm::Recover(p), false) => {
                let _ = std::fs::remove_file(p);
                self.show_welcome();
            }
            (Confirm::Exit, true) => {
                self.meta_dirty = false;
                self.saved_rev = self.engine.history_marker();
                let _ = slint::quit_event_loop();
            }
            (Confirm::ClearCache, true) => self.clear_cache_now(),
            (Confirm::RemoveAsset(id), true) => self.remove_asset_now(id),
            (Confirm::DeleteBin(id), true) => self.delete_bin_now(id),
            _ => {}
        }
        self.ui().invoke_focus_editor();
    }

    /// "Save" in the exit dialog.
    pub fn confirm_alt(&mut self) {
        self.ui().set_confirm_open(false);
        if let Some(Confirm::Exit) = self.confirm.take() {
            if let Some(p) = self.path.clone() {
                self.save_to(p);
                if !self.is_dirty() {
                    let _ = slint::quit_event_loop();
                }
            } else {
                defer(|| {
                    save_as();
                    with_app(|app| {
                        if !app.is_dirty() {
                            let _ = slint::quit_event_loop();
                        }
                    });
                });
            }
        }
    }

    /// Returns true when the window should stay open (asking the user).
    pub fn request_exit(&mut self) -> bool {
        if self.export.job.is_some() {
            self.toast_warn(t("toast.export_running"));
            return true;
        }
        if !self.is_dirty() {
            return false;
        }
        self.ask(&t("dlg.exit.title"), &t("dlg.exit.body"), &t("dlg.exit.dont_save"), &t("dlg.exit.save"), true, Confirm::Exit);
        true
    }

    // ------------------------------------------------------------ prompt

    pub fn prompt(&mut self, title: &str, label: &str, value: &str, p: Prompt) {
        let ui = self.ui();
        ui.set_prompt_title(title.into());
        ui.set_prompt_label(label.into());
        ui.set_prompt_value(value.into());
        ui.set_prompt_open(true);
        self.prompt = Some(p);
    }

    pub fn prompt_dismiss(&mut self) {
        self.ui().set_prompt_open(false);
        self.prompt = None;
    }

    pub fn prompt_submit(&mut self, text: &str) {
        self.ui().set_prompt_open(false);
        let text = text.trim();
        let Some(p) = self.prompt.take() else { return };
        if text.is_empty() {
            return;
        }
        match p {
            Prompt::RenameClip(id) => {
                self.execute(EditCommand::SetClipProperty { clip: id, prop: kadr_timeline::ClipProperty::Name(text.into()) });
            }
            Prompt::RenameMarker(id) => {
                if let Some(mut m) = self.project.sequence().markers.iter().find(|m| m.id == id).cloned() {
                    m.name = text.into();
                    self.execute(EditCommand::UpdateMarker(m));
                }
            }
            Prompt::RenameSequence => {
                self.project.sequence_mut().name = text.into();
                self.meta_dirty = true;
                self.refresh_inspector();
                self.refresh_status();
            }
        }
    }

    // ------------------------------------------------------------ projects

    pub fn new_project(&mut self) {
        self.stop_playback();
        self.cancel_all_asset_jobs();
        self.set_project(crate::app::new_localized_project(), None);
        self.last_save_ms = None;
        self.toast(t("toast.new_project"));
    }

    fn set_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.project = project;
        self.path = path;
        self.engine.clear();
        self.saved_rev = self.engine.history_marker();
        self.meta_dirty = false;
        self.assets_rt.clear();
        self.waves.clear();
        self.tl.selection.clear();
        self.tl.scroll = 0.0;
        self.playhead = kadr_core::Time::ZERO;
        self.selected_asset = None;
        self.bin_filter = None;
        self.media_search.clear();
        self.ui().set_media_search("".into());
        self.ai.plans.clear();
        self.ai.offers.clear();
        self.ai.chat.clear();
        let ids: Vec<_> = self.project.assets.iter().map(|a| a.id).collect();
        for id in ids {
            self.process_asset(id); // cache hits make this cheap
        }
        self.refresh_all();
        if self.project.sequence().duration() > kadr_core::Time::ZERO {
            slint::Timer::single_shot(Duration::from_millis(80), || {
                with_app(|app| app.zoom_fit());
            });
        }
    }

    pub fn open_project(&mut self, path: PathBuf) {
        self.ui().set_welcome_open(false);
        // Prefer a newer autosave (crash recovery) if present.
        if let Some(auto) = io::recovery_candidate(&path) {
            match io::load(&auto) {
                Ok(p) => {
                    self.set_project(p, Some(path.clone()));
                    self.meta_dirty = true;
                    self.remember_recent(&path);
                    self.toast_warn(t("toast.recovered_autosave"));
                    return;
                }
                Err(e) => tracing::warn!(error = %e, "autosave unreadable"),
            }
        }
        match io::load(&path) {
            Ok(p) => {
                let missing = p.assets.iter().filter(|a| !a.exists()).count();
                self.set_project(p, Some(path.clone()));
                self.remember_recent(&path);
                self.last_save_ms = None;
                if missing > 0 {
                    self.toast_warn(kadr_i18n::tn("toast.media_offline", missing as i64, &[]));
                } else {
                    self.toast_ok(t("toast.project_opened"));
                }
            }
            Err(e) => {
                let key = match e {
                    io::ProjectIoError::TooNew { .. } => "err.project.too_new",
                    io::ProjectIoError::Parse { .. } => "err.project.corrupt",
                    io::ProjectIoError::Io { .. } => "err.project.io",
                };
                self.toast_error(tf(key, &[("path", &path.display().to_string())]));
                self.settings.recent.retain(|r| r.path != path);
                self.settings.save(&self.dirs.settings_file());
                self.refresh_welcome();
            }
        }
    }

    pub fn save_to(&mut self, path: PathBuf) {
        let path = if path.extension().is_none() { path.with_extension(io::EXTENSION) } else { path };
        if self.path.is_none() {
            if let Some(stem) = path.file_stem() {
                self.project.name = stem.to_string_lossy().into_owned();
            }
        }
        match io::save(&mut self.project, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(self.untitled_autosave_path());
                self.path = Some(path.clone());
                self.saved_rev = self.engine.history_marker();
                self.meta_dirty = false;
                self.last_save_ms = Some(kadr_project::now_ms());
                self.remember_recent(&path);
                self.toast_ok(t("toast.saved"));
            }
            Err(e) => self.toast_error(tf("err.project.save", &[("error", &e.to_string())])),
        }
        self.refresh_status();
    }

    fn remember_recent(&mut self, path: &std::path::Path) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.settings.push_recent(path, &name);
        self.settings.save(&self.dirs.settings_file());
    }

    pub fn open_recent(&mut self, i: usize) {
        let Some(r) = self.settings.recent.get(i).cloned() else { return };
        self.ui().set_welcome_open(false);
        if !self.guard_unsaved(Box::new(move |app| app.open_project(r.path.clone()))) {
            let p = self.settings.recent[i].path.clone();
            self.open_project(p);
        }
    }

    pub fn show_welcome(&mut self) {
        self.refresh_welcome();
        self.ui().set_welcome_open(true);
    }

    pub fn refresh_welcome(&mut self) {
        let v: Vec<RecentView> = self
            .settings
            .recent
            .iter()
            .enumerate()
            .map(|(i, r)| RecentView {
                index: i as i32,
                name: r.name.clone().into(),
                path: r.path.display().to_string().into(),
                when: crate::util::relative_time(r.opened_ms).into(),
                exists: r.path.exists(),
            })
            .collect();
        self.ui().set_recent(ModelRc::new(VecModel::from(v)));
    }

    // ------------------------------------------------------------ autosave

    fn untitled_autosave_path(&self) -> PathBuf {
        crate::mcp_env::recovery_dir(&self.dirs.data, self.flags.headless)
            .join(format!("{}.{}.autosave", self.project.id, io::EXTENSION))
    }

    pub fn tick_autosave(&mut self) {
        let every = self.settings.autosave_secs;
        if every == 0 || !self.is_dirty() || self.last_autosave.elapsed() < Duration::from_secs(every) {
            return;
        }
        self.last_autosave = Instant::now();
        let target = match &self.path {
            Some(p) => io::autosave_path(p),
            None => self.untitled_autosave_path(),
        };
        // Serialize on the UI thread (fast), write in the background.
        let bytes = io::to_json(&self.project);
        std::thread::spawn(move || match io::write_atomic(&target, &bytes, false) {
            Ok(()) => tracing::debug!(path = %target.display(), "autosaved"),
            Err(e) => tracing::warn!(error = %e, "autosave failed"),
        });
    }

    /// On startup: offer to restore the newest untitled autosave.
    pub fn check_recovery(&mut self) -> bool {
        let dir = crate::mcp_env::recovery_dir(&self.dirs.data, self.flags.headless);
        let newest = std::fs::read_dir(&dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .max_by_key(|(t, _)| *t);
        match newest {
            Some((_, p)) => {
                self.ask(&t("dlg.recover.title"), &t("dlg.recover.body"), &t("dlg.recover.restore"), "", false, Confirm::Recover(p));
                true
            }
            None => false,
        }
    }

    fn recover_from(&mut self, p: PathBuf) {
        match io::load(&p) {
            Ok(project) => {
                self.set_project(project, None);
                self.meta_dirty = true;
                let _ = std::fs::remove_file(&p);
                self.toast_warn(t("toast.restored"));
            }
            Err(e) => self.toast_error(tf("err.project.recover", &[("error", &e.to_string())])),
        }
    }
}
