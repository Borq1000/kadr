//! Structured logging: JSON lines to `%LOCALAPPDATA%\Kadr\logs\kadr.YYYY-MM-DD`
//! plus human-readable console output. API keys never reach the log: they
//! are only held in `Secret`, whose Debug/Display are redacted.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

pub fn init(log_dir: &Path) -> WorkerGuard {
    let _ = std::fs::create_dir_all(log_dir);
    let file = tracing_appender::rolling::daily(log_dir, "kadr.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("KADR_LOG").unwrap_or_else(|_| EnvFilter::new("info,kadr=debug,wgpu=warn,naga=warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_writer(writer).with_current_span(false))
        .with(fmt::layer().with_target(false).compact())
        .init();

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "panic");
        prev(info);
    }));
    guard
}
