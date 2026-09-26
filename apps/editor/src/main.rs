// Release builds are GUI-subsystem apps (no console window).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod ai_ui;
mod app;
mod app_settings;
mod export_ui;
mod file_drop;
mod import;
mod inspector;
mod keys;
mod library;
mod persistence;
mod preview;
mod settings_ui;
mod timeline_ui;
mod toasts;
mod util;
mod video_analysis;
mod waveform;
mod winutil;

fn main() {
    let dirs = kadr_cache::AppDirs::new();
    let _log = logging::init(&dirs.logs());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Kadr starting");
    if let Err(e) = app::run(dirs) {
        tracing::error!(error = %e, "fatal");
        eprintln!("Kadr failed to start: {e}");
    }
}

mod logging;
