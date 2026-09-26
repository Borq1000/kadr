//! Application controller: owns the project, edit engine, media services
//! and the UI handle. All mutation happens on the UI thread; background
//! work reports back through [`post`].

use crate::ai_ui::AiUi;
use crate::app_settings::AppSettings;
use crate::export_ui::ExportUi;
use crate::library::LibDrag;
use crate::preview::PreviewController;
use crate::timeline_ui::TimelineUi;
use crate::toasts::Toasts;
use crate::waveform::WaveCache;
use crate::{AppWindow, Tr};
use kadr_ai::{AiSettings, Assistant};
use kadr_analysis::AudioOverview;
use kadr_audio::AudioEngine;
use kadr_cache::{AppDirs, Cache};
use kadr_core::{AssetId, BinId, ClipId, Time};
use kadr_i18n::{t, tf};
use kadr_jobs::JobSystem;
use kadr_media::MediaBackend;
use kadr_project::{Clip, EditSource, Project};
use kadr_timeline::{EditCommand, EditEngine, EditError};
use slint::{ComponentHandle, Global};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum AssetStatus {
    Queued,
    Working,
    Ready,
    Missing,
    Error(String),
}

pub struct AssetRuntime {
    pub thumb: Option<slint::Image>,
    pub overview: Option<Arc<AudioOverview>>,
    pub pcm: Option<PathBuf>,
    pub status: AssetStatus,
    pub progress: f32,
    pub job: Option<kadr_jobs::JobId>,
}

impl Default for AssetRuntime {
    fn default() -> Self {
        AssetRuntime { thumb: None, overview: None, pcm: None, status: AssetStatus::Queued, progress: 0.0, job: None }
    }
}

/// Actions awaiting an answer in the confirm dialog.
pub enum Confirm {
    DiscardThen(Box<dyn FnOnce(&mut App)>),
    Recover(PathBuf),
    Exit,
    ClearCache,
    RemoveAsset(AssetId),
    DeleteBin(BinId),
}

/// What the text prompt dialog is editing.
pub enum Prompt {
    RenameClip(ClipId),
    RenameMarker(kadr_core::MarkerId),
    RenameSequence,
    RenameBin(BinId),
}

pub struct App {
    pub ui: slint::Weak<AppWindow>,
    pub dirs: AppDirs,
    pub settings: AppSettings,
    pub project: Project,
    pub path: Option<PathBuf>,
    pub engine: EditEngine,
    pub saved_rev: u64,
    pub meta_dirty: bool,
    pub media: Option<Arc<dyn MediaBackend>>,
    pub jobs: JobSystem,
    pub cache: Cache,
    pub assets_rt: HashMap<AssetId, AssetRuntime>,
    pub bin_filter: Option<BinId>,
    pub renaming_bin: Option<BinId>,
    pub media_search: String,
    pub selected_asset: Option<AssetId>,
    pub tl: TimelineUi,
    pub playhead: Time,
    pub playing: bool,
    pub audio: AudioEngine,
    pub preview: PreviewController,
    pub ai: AiUi,
    pub export: ExportUi,
    pub inspector_snapshot: Option<(ClipId, Clip)>,
    pub library_drag: Option<LibDrag>,
    pub mc: crate::multicam_ui::McState,
    pub waves: WaveCache,
    pub confirm: Option<Confirm>,
    pub prompt: Option<Prompt>,
    pub toasts: Toasts,
    pub last_autosave: Instant,
    pub last_save_ms: Option<i64>,
    pub fullscreen: bool,
    pub last_window_width: f32,
    pub last_window_height: f32,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

/// Runs `f` with the app if it is not already borrowed (re-entrancy from
/// nested event loops, e.g. native dialogs, is silently skipped).
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let rc = APP.with(|a| a.borrow().clone())?;
    let mut app = rc.try_borrow_mut().ok()?;
    Some(f(&mut app))
}

/// Delivers background results to the UI thread. If the app is busy (a
/// modal dialog pumping messages), retries shortly instead of dropping.
pub fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || run_or_retry(Box::new(f)));
}

fn run_or_retry(f: Box<dyn FnOnce(&mut App)>) {
    let Some(rc) = APP.with(|a| a.borrow().clone()) else { return };
    let mut f = Some(f);
    if let Ok(mut app) = rc.try_borrow_mut() {
        (f.take().unwrap())(&mut app);
    }
    if let Some(f) = f {
        slint::Timer::single_shot(Duration::from_millis(40), move || run_or_retry(f));
    }
}

/// Runs `f` on the next event-loop turn, outside the current borrow
/// (for native dialogs opened from menu handlers).
pub fn defer(f: impl FnOnce() + 'static) {
    slint::Timer::single_shot(Duration::from_millis(1), f);
}

pub fn run(dirs: AppDirs) -> Result<(), slint::PlatformError> {
    let settings = AppSettings::load(&dirs.settings_file());
    kadr_i18n::set_lang(settings.lang());

    let ui = AppWindow::new()?;
    wire_translations(&ui);

    let media: Option<Arc<dyn MediaBackend>> = match kadr_media::ffmpeg::FfmpegCli::locate() {
        Ok(b) => Some(Arc::new(b)),
        Err(e) => {
            tracing::error!(error = %e, "no media backend");
            None
        }
    };
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6) - 1;
    let audio = AudioEngine::new();
    let clock = audio.clock();
    let ai_settings = AiSettings::load(&dirs.data.join("ai-settings.json"));
    let cache = Cache::new(dirs.cache());
    let mut preview = PreviewController::new(media.clone(), clock);
    preview.quality = settings.preview_quality;
    let mut tl = TimelineUi::default();
    tl.snapping = settings.snapping;
    let app = App {
        ui: ui.as_weak(),
        project: new_localized_project(),
        path: None,
        engine: EditEngine::new(),
        saved_rev: 0,
        meta_dirty: false,
        preview,
        media,
        jobs: JobSystem::new(workers.max(1)),
        cache,
        assets_rt: HashMap::new(),
        bin_filter: None,
        renaming_bin: None,
        media_search: String::new(),
        selected_asset: None,
        tl,
        playhead: Time::ZERO,
        playing: false,
        audio,
        ai: AiUi::new(Assistant::new(ai_settings)),
        export: ExportUi::default(),
        inspector_snapshot: None,
        library_drag: None,
        mc: Default::default(),
        waves: WaveCache::default(),
        confirm: None,
        prompt: None,
        toasts: Toasts::default(),
        last_autosave: Instant::now(),
        last_save_ms: None,
        fullscreen: false,
        last_window_width: 0.0,
        last_window_height: 0.0,
        settings,
        dirs,
    };
    let rc = Rc::new(RefCell::new(app));
    APP.with(|a| *a.borrow_mut() = Some(rc.clone()));

    wire_callbacks(&ui);
    crate::file_drop::install(&ui);
    start_timers();

    with_app(|app| {
        app.apply_layout();
        app.ui().set_language(lang_index());
        if app.media.is_none() {
            app.toast_error(t("err.ffmpeg_missing"));
        }
        app.refresh_all();
        // `kadr project.kadr` opens a project; media paths are imported.
        let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        let (projects, media): (Vec<_>, Vec<_>) =
            args.into_iter().partition(|p| p.extension().is_some_and(|e| e == kadr_project::io::EXTENSION));
        if let Some(p) = projects.into_iter().next() {
            app.open_project(p);
        } else if media.is_empty() && !app.check_recovery() {
            app.show_welcome();
        }
        if !media.is_empty() {
            app.import_paths(media);
        }
    });

    ui.window().on_close_requested(|| {
        let keep = with_app(|app| app.request_exit()).unwrap_or(false);
        if keep { slint::CloseRequestResponse::KeepWindowShown } else { slint::CloseRequestResponse::HideWindow }
    });

    // Editors live full-screen: maximize once the window is actually shown
    // (maximizing before show is overridden by the initial preferred size).
    slint::Timer::single_shot(Duration::from_millis(30), || {
        with_app(|app| {
            app.ui().window().set_maximized(true);
        });
        crate::winutil::dark_title_bar();
    });
    ui.invoke_focus_editor();
    let r = ui.run();
    with_app(|app| app.shutdown());
    r
}

pub fn lang_index() -> i32 {
    kadr_i18n::Lang::ALL.iter().position(|l| *l == kadr_i18n::lang()).unwrap_or(0) as i32
}

fn wire_translations(ui: &AppWindow) {
    let tr = Tr::get(ui);
    tr.on_lookup(|_, k| t(&k).into());
    tr.on_lookup1(|_, k, a| kadr_i18n::t_pos(&k, &[&a]).into());
    tr.on_lookup2(|_, k, a, b| kadr_i18n::t_pos(&k, &[&a, &b]).into());
}

fn start_timers() {
    // Playback clock → playhead (60 Hz).
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(16), || {
        with_app(|app| app.tick_playback());
    });
    std::mem::forget(t);
    // Jobs / status / toasts (7 Hz).
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(150), || {
        with_app(|app| app.tick_status());
    });
    std::mem::forget(t);
    // Autosave check.
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(5), || {
        with_app(|app| app.tick_autosave());
    });
    std::mem::forget(t);
}

macro_rules! cb {
    ($ui:ident . $on:ident, |$($arg:ident),*| $body:expr) => {
        $ui.$on(move |$($arg),*| { with_app(|app| { let _ = &app; $body(app) }); });
    };
}

fn wire_callbacks(ui: &AppWindow) {
    // Menus and dialogs that open native pickers run the picker outside the
    // app borrow (see persistence.rs / import.rs).
    ui.on_menu(|m| crate::persistence::menu(m.as_str()));
    ui.on_key(|text, ctrl, shift, alt| {
        // Ctrl shortcuts that open native dialogs must not hold the borrow.
        if ctrl && !alt {
            let k = text.to_lowercase();
            let id = match (k.as_str(), shift) {
                ("o", false) => Some("open"),
                ("s", true) => Some("save-as"),
                ("i", false) => Some("import"),
                _ => None,
            };
            if let Some(id) = id {
                crate::persistence::menu(id);
                return true;
            }
        }
        with_app(|app| app.on_key(text.as_str(), ctrl, shift, alt)).unwrap_or(false)
    });
    ui.on_import_media(crate::import::import_dialog);

    cb!(ui.on_new_bin, | | |app: &mut App| app.new_bin());
    cb!(ui.on_select_bin, |id| |app: &mut App| app.select_bin(&id));
    cb!(ui.on_bin_action, |id, a| |app: &mut App| app.bin_action(&id, &a));
    cb!(ui.on_rename_bin, |id, n| |app: &mut App| app.rename_bin(&id, &n));
    cb!(ui.on_media_search_changed, |s| |app: &mut App| app.media_search_changed(&s));
    cb!(ui.on_asset_pressed, |id, x, y| |app: &mut App| app.asset_pressed(&id, x, y));
    cb!(ui.on_asset_moved, |id, x, y| |app: &mut App| app.asset_moved(&id, x, y));
    cb!(ui.on_asset_released, |id, x, y| |app: &mut App| app.asset_released(&id, x, y));
    cb!(ui.on_asset_activated, |id| |app: &mut App| app.asset_activated(&id));
    cb!(ui.on_asset_action, |id, a| |app: &mut App| app.asset_action(&id, &a));
    cb!(ui.on_angle_clicked, |i| |app: &mut App| {
        app.cut_to_angle(i as u32);
    });
    cb!(ui.on_mc_toggle, |i| |app: &mut App| app.mc_toggle(i));
    cb!(ui.on_mc_set_label, |i, s| |app: &mut App| app.mc_set_label(i, &s));
    cb!(ui.on_mc_set_description, |i, s| |app: &mut App| app.mc_set_description(i, &s));
    cb!(ui.on_mc_confirm, |n| |app: &mut App| app.mc_confirm(&n));
    cb!(ui.on_mc_dismiss, | | |app: &mut App| app.mc_dismiss());

    cb!(ui.on_transport, |a| |app: &mut App| app.transport(&a));
    cb!(ui.on_set_quality, |q| |app: &mut App| app.set_quality(q));
    cb!(ui.on_set_zoom, |z| |app: &mut App| app.set_preview_zoom(z));
    cb!(ui.on_scrub, |f| |app: &mut App| app.scrub(f));

    cb!(ui.on_tl_pointer, |k, x, y, b, s, c, a| |app: &mut App| app.tl_pointer(k, x, y, b, s, c, a));
    cb!(ui.on_tl_scroll, |dx, dy, c, s| |app: &mut App| app.tl_scroll(dx, dy, c, s));
    cb!(ui.on_tl_ruler, |k, x| |app: &mut App| app.tl_ruler(k, x));
    cb!(ui.on_tl_track_toggle, |i, w| |app: &mut App| app.tl_track_toggle(i, &w));
    cb!(ui.on_tl_track_action, |i, a| |app: &mut App| app.tl_track_action(i, &a));
    cb!(ui.on_tl_track_rename, |i, n| |app: &mut App| app.tl_track_rename(i, &n));
    cb!(ui.on_tl_context, |a| |app: &mut App| app.tl_context(&a));
    cb!(ui.on_tl_tool, |t| |app: &mut App| app.tl_tool(&t));
    cb!(ui.on_tl_scrollbar, |f| |app: &mut App| app.tl_scrollbar(f));
    cb!(ui.on_layout_changed, | | |app: &mut App| app.layout_changed());

    cb!(ui.on_inspector_live, |p, v| |app: &mut App| app.inspector_live(&p, v));
    cb!(ui.on_inspector_commit, |p, v| |app: &mut App| app.inspector_commit(&p, v));
    cb!(ui.on_inspector_toggle_enabled, | | |app: &mut App| app.inspector_toggle_enabled());
    cb!(ui.on_inspector_reset, |s| |app: &mut App| app.inspector_reset(&s));
    cb!(ui.on_inspector_marker, |i, a| |app: &mut App| app.marker_action(i as usize, &a));
    cb!(ui.on_inspector_sequence, |a| |app: &mut App| app.sequence_action(&a));

    cb!(ui.on_ai_send, |t| |app: &mut App| app.ai_send(&t));
    cb!(ui.on_ai_plan_action, |i, a| |app: &mut App| app.ai_plan_action(i as usize, &a));
    cb!(ui.on_ai_plan_toggle, |i, j| |app: &mut App| app.ai_plan_toggle(i as usize, j as usize));
    cb!(ui.on_ai_offer_action, |i, a| |app: &mut App| app.ai_offer_action(i as usize, &a));
    cb!(ui.on_ai_clear, | | |app: &mut App| app.ai_clear());

    ui.on_export_choose_path(crate::export_ui::choose_path);
    cb!(ui.on_export_start, |p, r| |app: &mut App| app.export_start(p, r));
    cb!(ui.on_export_cancel, | | |app: &mut App| app.export_cancel());
    cb!(ui.on_export_reveal, | | |app: &mut App| app.export_reveal());
    cb!(ui.on_export_open_file, | | |app: &mut App| app.export_open_file());
    cb!(ui.on_export_changed, |p, r| |app: &mut App| app.export_changed(p, r));

    cb!(ui.on_settings_tab, |t| |app: &mut App| app.settings_tab(t));
    cb!(ui.on_settings_language, |l| |app: &mut App| app.set_language(l));
    cb!(ui.on_settings_autosave, |a| |app: &mut App| app.settings_autosave(a));
    cb!(ui.on_settings_quality, |q| |app: &mut App| app.set_quality(q));
    cb!(ui.on_settings_clear_cache, | | |app: &mut App| app.settings_clear_cache());
    cb!(ui.on_settings_open_data, | | |app: &mut App| crate::util::open_folder(&app.dirs.data));
    cb!(ui.on_settings_mode, |m| |app: &mut App| app.settings_mode(m));
    cb!(ui.on_settings_save_key, |id, k| |app: &mut App| app.settings_save_key(&id, &k));
    cb!(ui.on_settings_delete_key, |id| |app: &mut App| app.settings_delete_key(&id));
    cb!(ui.on_settings_toggle_allowed, |id| |app: &mut App| app.settings_toggle_allowed(&id));
    cb!(ui.on_settings_route, |t, m| |app: &mut App| app.settings_route(&t, &m));
    cb!(ui.on_settings_limit, |k, v| |app: &mut App| app.settings_limit(&k, &v));
    cb!(ui.on_settings_test, |id| |app: &mut App| app.settings_test(&id));

    cb!(ui.on_confirm_ok_clicked, | | |app: &mut App| app.confirm_answer(true));
    cb!(ui.on_confirm_alt_clicked, | | |app: &mut App| app.confirm_alt());
    cb!(ui.on_confirm_dismissed, | | |app: &mut App| app.confirm_answer(false));
    cb!(ui.on_prompt_submit, |t| |app: &mut App| app.prompt_submit(&t));
    cb!(ui.on_prompt_dismissed, | | |app: &mut App| app.prompt_dismiss());

    cb!(ui.on_job_cancel, |id| |app: &mut App| app.jobs.cancel(id as u64));
    cb!(ui.on_job_retry, |id| |app: &mut App| { app.jobs.retry(id as u64); });
    cb!(ui.on_jobs_clear, | | |app: &mut App| app.jobs.clear_finished());
    cb!(ui.on_toast_dismiss, |id| |app: &mut App| app.dismiss_toast(id));
    ui.on_welcome_action(|a| crate::persistence::welcome_action(a.as_str()));
    cb!(ui.on_open_recent, |i| |app: &mut App| app.open_recent(i as usize));
}

/// A new project with localized default names.
pub fn new_localized_project() -> Project {
    let mut p = Project::new(t("project.untitled"));
    p.sequence_mut().name = tf("project.sequence_default", &[("n", "1")]);
    p
}

/// Translates a history label (i18n key or literal batch label).
pub fn cmd_label(label: &str) -> String {
    if label.starts_with("cmd.") { t(label) } else { label.to_string() }
}

impl App {
    pub fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("ui alive")
    }

    pub fn is_dirty(&self) -> bool {
        self.meta_dirty || self.engine.history_marker() != self.saved_rev
    }

    /// Executes a user edit, refreshing everything it can affect.
    pub fn execute(&mut self, cmd: EditCommand) -> bool {
        self.execute_as(cmd, EditSource::User, None)
    }

    pub fn execute_as(&mut self, cmd: EditCommand, source: EditSource, action: Option<kadr_core::ActionId>) -> bool {
        let label = cmd_label(&cmd.label());
        match self.engine.execute_as(&mut self.project, cmd, source, action) {
            Ok(()) => {
                self.after_edit();
                true
            }
            Err(EditError::NoOp) => false,
            Err(e) => {
                let mut msg = t(EditCommand::error_key(&e));
                if let EditError::TrackLocked(name) = &e {
                    msg = tf("err.edit.track_locked", &[("track", name)]);
                }
                self.toast_warn(format!("{label}: {msg}"));
                false
            }
        }
    }

    pub fn after_edit(&mut self) {
        let seq = self.project.sequence();
        self.tl.selection.retain(|id| seq.clip(*id).is_some());
        let dur = seq.duration();
        if self.playhead > dur {
            self.playhead = dur;
        }
        if self.playing {
            self.restart_playback();
        }
        self.refresh_timeline();
        self.refresh_inspector();
        self.refresh_status();
        self.refresh_library();
        self.request_frame();
    }

    pub fn undo(&mut self) {
        match self.engine.undo(&mut self.project) {
            Some(l) => {
                self.ai.on_history_changed(&self.engine);
                self.after_edit();
                self.refresh_ai();
                self.toast(tf("toast.undo", &[("what", &cmd_label(&l))]));
            }
            None => self.toast(t("toast.nothing_to_undo")),
        }
    }

    pub fn redo(&mut self) {
        match self.engine.redo(&mut self.project) {
            Some(l) => {
                self.ai.on_history_changed(&self.engine);
                self.after_edit();
                self.refresh_ai();
                self.toast(tf("toast.redo", &[("what", &cmd_label(&l))]));
            }
            None => self.toast(t("toast.nothing_to_redo")),
        }
    }

    pub fn refresh_all(&mut self) {
        self.refresh_library();
        self.refresh_timeline();
        self.refresh_inspector();
        self.refresh_status();
        self.refresh_ai();
        self.update_preview_info();
        self.request_frame();
    }

    pub fn display_name(&self) -> String {
        match &self.path {
            Some(p) => p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            None => self.project.name.clone(),
        }
    }

    pub fn refresh_status(&mut self) {
        let ui = self.ui();
        ui.set_project_name(self.display_name().into());
        ui.set_dirty(self.is_dirty());
        let seq = self.project.sequence();
        ui.set_seq_info(
            format!("{}×{} · {} fps · {}", seq.width, seq.height, seq.frame_rate, kadr_i18n::duration(seq.duration().as_secs_f64())).into(),
        );
        ui.set_timeline_empty(seq.duration() == Time::ZERO);
        ui.set_has_selection(!self.tl.selection.is_empty());
        ui.set_can_undo(self.engine.can_undo());
        ui.set_can_redo(self.engine.can_redo());
        ui.set_undo_label(self.engine.undo_label().map(cmd_label).unwrap_or_default().into());
        ui.set_redo_label(self.engine.redo_label().map(cmd_label).unwrap_or_default().into());
        self.refresh_save_state();
        self.refresh_ai_status();
    }

    pub fn refresh_save_state(&mut self) {
        let s = if self.is_dirty() {
            if self.path.is_none() { t("status.not_saved") } else { t("status.unsaved") }
        } else {
            match self.last_save_ms {
                Some(ms) => tf("status.saved_at", &[("time", &crate::util::clock_time(ms))]),
                None => String::new(),
            }
        };
        self.ui().set_save_state(s.into());
    }

    pub fn tick_status(&mut self) {
        self.fit_layout_to_window();
        self.expire_toasts();
        self.refresh_jobs();
        self.export_poll();
        self.tick_angles();
    }

    /// Applies a new UI language everywhere, instantly.
    pub fn set_language(&mut self, idx: i32) {
        let lang = kadr_i18n::Lang::ALL.get(idx as usize).copied().unwrap_or(kadr_i18n::Lang::En);
        if lang == kadr_i18n::lang() {
            return;
        }
        kadr_i18n::set_lang(lang);
        self.settings.language = lang.code().into();
        self.settings.save(&self.dirs.settings_file());
        let ui = self.ui();
        let tr = Tr::get(&ui);
        tr.set_rev(tr.get_rev() + 1);
        ui.set_language(lang_index());
        self.refresh_all();
        self.refresh_settings();
        self.refresh_export();
        self.refresh_welcome();
        self.toast_ok(t("toast.language_changed"));
    }

    // ------------------------------------------------------------ layout

    pub fn apply_layout(&mut self) {
        let ui = self.ui();
        let l = &self.settings.layout;
        ui.set_library_width(l.library_width);
        ui.set_inspector_width(l.inspector_width);
        ui.set_ai_width(l.ai_width);
        ui.set_timeline_height(l.timeline_height);
        ui.set_ai_open(l.ai_open);
        ui.set_preview_quality(self.settings.preview_quality);
    }

    pub fn layout_changed(&mut self) {
        let ui = self.ui();
        let l = &mut self.settings.layout;
        l.library_width = ui.get_library_width();
        l.inspector_width = ui.get_inspector_width();
        l.ai_width = ui.get_ai_width();
        l.timeline_height = ui.get_timeline_height();
        l.ai_open = ui.get_ai_open();
        self.settings.save(&self.dirs.settings_file());
        // Lanes changed size → re-virtualize.
        slint::Timer::single_shot(Duration::from_millis(30), || {
            with_app(|app| app.refresh_timeline());
        });
    }

    /// Keeps the preview usable on small screens: when the side panels
    /// would squeeze it below ~480 px, they shrink toward their minimums.
    pub fn fit_layout_to_window(&mut self) {
        let ui = self.ui();
        let scale = ui.window().scale_factor();
        let w = ui.window().size().width as f32 / scale;
        let h = ui.window().size().height as f32 / scale;
        // Narrow preview hides secondary transport buttons (they stay in the
        // View menu). Splitter drags are picked up on the next tick.
        let ai_w = if ui.get_ai_open() { ui.get_ai_width() } else { 36.0 };
        let compact = w - ui.get_library_width() - ui.get_inspector_width() - ai_w - 20.0 < 640.0;
        if ui.get_preview_compact() != compact {
            ui.set_preview_compact(compact);
        }
        if (w - self.last_window_width).abs() < 1.0 && (h - self.last_window_height).abs() < 1.0 {
            return;
        }
        self.last_window_width = w;
        self.last_window_height = h;
        // The timeline may take at most ~45% of the height (preview needs room).
        let max_tl = (h * 0.45).max(200.0);
        if ui.get_timeline_height() > max_tl {
            ui.set_timeline_height(max_tl);
        }
        const PREVIEW_MIN: f32 = 480.0;
        let ai = if ui.get_ai_open() { ui.get_ai_width() } else { 36.0 };
        let mut lib = ui.get_library_width();
        let mut insp = ui.get_inspector_width();
        let mut ai_w = ai;
        let mut over = lib + insp + ai_w + 20.0 + PREVIEW_MIN - w;
        for (v, min) in [(&mut ai_w, 290.0f32), (&mut insp, 250.0), (&mut lib, 220.0)] {
            if over <= 0.0 {
                break;
            }
            let take = (*v - min).max(0.0).min(over);
            *v -= take;
            over -= take;
        }
        ui.set_library_width(lib);
        ui.set_inspector_width(insp);
        if ui.get_ai_open() {
            ui.set_ai_width(ai_w);
        }
        slint::Timer::single_shot(Duration::from_millis(30), || {
            with_app(|app| app.refresh_timeline());
        });
    }

    pub fn reset_layout(&mut self) {
        self.settings.layout = crate::app_settings::Layout::default();
        self.apply_layout();
        self.last_window_width = 0.0;
        self.fit_layout_to_window();
        self.layout_changed();
    }

    pub fn refresh_jobs(&mut self) {
        use kadr_jobs::JobState;
        let changed = self.sync_asset_jobs();
        let jobs = self.jobs.snapshot();
        let active: Vec<_> = jobs.iter().filter(|j| !j.state.is_finished()).collect();
        let ui = self.ui();
        if active.is_empty() {
            let failed = jobs.iter().filter(|j| matches!(j.state, JobState::Failed(_))).count();
            ui.set_jobs_label(if failed > 0 { kadr_i18n::tn("jobs.failed_n", failed as i64, &[]) } else { t("jobs.ready") }.into());
            ui.set_jobs_active(false);
        } else {
            let running = active.iter().find(|j| j.state == JobState::Running).unwrap_or(&active[0]);
            let p = active.iter().map(|j| j.progress).sum::<f32>() / active.len() as f32;
            ui.set_jobs_label(kadr_i18n::tn("jobs.active_n", active.len() as i64, &[("title", &running.title)]).into());
            ui.set_jobs_progress(p);
            ui.set_jobs_active(true);
        }
        if ui.get_jobs_open() {
            let views: Vec<crate::JobView> = jobs
                .iter()
                .take(60)
                .map(|j| crate::JobView {
                    id: j.id as i32,
                    title: j.title.clone().into(),
                    category: t(&format!("jobs.cat.{}", j.category)).into(),
                    state: match &j.state {
                        JobState::Queued => t("jobs.state.queued"),
                        JobState::Running => tf("jobs.state.running", &[("pct", &format!("{:.0}", j.progress * 100.0))]),
                        JobState::Done => t("jobs.state.done"),
                        JobState::Failed(e) => tf("jobs.state.failed", &[("error", e)]),
                        JobState::Cancelled => t("jobs.state.cancelled"),
                    }
                    .into(),
                    progress: j.progress,
                    failed: matches!(j.state, JobState::Failed(_)),
                    running: j.state == JobState::Running,
                    finished: j.state.is_finished(),
                })
                .collect();
            ui.set_jobs(slint::ModelRc::new(slint::VecModel::from(views)));
        }
        if changed {
            self.refresh_library();
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_playback();
        self.preview.shutdown();
        self.layout_changed();
        if let Err(e) = self.ai.assistant.settings.save(&self.dirs.data.join("ai-settings.json")) {
            tracing::warn!(error = %e, "could not save AI settings");
        }
        tracing::info!("Kadr exiting");
    }
}
