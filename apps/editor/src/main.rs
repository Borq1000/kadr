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
mod jev_ui;
mod keys;
mod library;
mod mcp_env;
mod mcp_state;
mod mcp_api;
mod multicam_ui;
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
    // Context menus render inside the window (not modal Win32 menus): same
    // look as the rest of Kadr, visible in snapshots, driveable by input.
    // SAFETY: single-threaded here; nothing reads the environment yet.
    unsafe { std::env::set_var("SLINT_NO_MUDA", "1") };
    let dirs = kadr_cache::AppDirs::new();
    let settings = crate::app_settings::AppSettings::load(&dirs.settings_file());
    if settings.allow_mcp {
        if let Ok(port) = mcp_env::free_port() {
            // SAFETY: as above, before any thread or Slint init.
            unsafe { std::env::set_var("SLINT_MCP_PORT", port.to_string()) };
            let _ = mcp_env::UI_PORT.set(port);
        }
    }
    let _log = logging::init(&dirs.logs());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Kadr starting");
    if let Some(&port) = mcp_env::UI_PORT.get() {
        tracing::info!(ui_port = port, "Slint MCP enabled");
    }
    let (flags, args) = mcp_env::parse_flags(&std::env::args_os().skip(1).collect::<Vec<_>>());
    if let Err(e) = app::run(dirs, flags, args) {
        tracing::error!(error = %e, "fatal");
        eprintln!("Kadr failed to start: {e}");
    }
}

mod logging;
