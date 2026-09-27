//! Structured logging: JSON lines to `%LOCALAPPDATA%\Kadr\logs\kadr.YYYY-MM-DD`
//! plus human-readable console output. API keys never reach the log: they
//! are only held in `Secret`, whose Debug/Display are redacted.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub ms: i64,
    pub level: String,
    pub target: String,
    pub message: String,
    pub panic: bool,
}

#[derive(Clone)]
pub struct LogRing {
    lines: Arc<Mutex<VecDeque<LogLine>>>,
    cap: usize,
}

static RING: OnceLock<LogRing> = OnceLock::new();

/// The process-wide ring installed by `init` (empty before).
pub fn ring() -> LogRing {
    RING.get_or_init(|| LogRing::with_capacity(500)).clone()
}

impl LogRing {
    pub fn with_capacity(cap: usize) -> Self {
        LogRing { lines: Arc::new(Mutex::new(VecDeque::with_capacity(cap))), cap }
    }

    pub fn layer(&self) -> RingLayer {
        RingLayer(self.clone())
    }

    pub fn tail(&self, n: usize, min_level: tracing::Level) -> Vec<LogLine> {
        let lines = self.lines.lock().unwrap();
        let keep: Vec<LogLine> = lines
            .iter()
            .filter(|l| l.level.parse::<tracing::Level>().map_or(true, |lv| lv <= min_level))
            .cloned()
            .collect();
        keep[keep.len().saturating_sub(n)..].to_vec()
    }
}

pub struct RingLayer(LogRing);

#[derive(Default)]
struct Fields {
    message: String,
    rest: Vec<String>,
    panic: bool,
}

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        match f.name() {
            "message" => self.message = format!("{v:?}").trim_matches('"').to_string(),
            "panic" => {
                self.panic = true;
            }
            name => self.rest.push(format!("{name}={v:?}")),
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RingLayer {
    fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut f = Fields::default();
        e.record(&mut f);
        let message = if f.rest.is_empty() { f.message } else { format!("{} {}", f.message, f.rest.join(" ")) };
        let line = LogLine {
            ms: kadr_project::now_ms(),
            level: e.metadata().level().to_string(),
            target: e.metadata().target().to_string(),
            message,
            panic: f.panic,
        };
        let mut lines = self.0.lines.lock().unwrap();
        if lines.len() == self.0.cap {
            lines.pop_front();
        }
        lines.push_back(line);
    }
}

pub fn init(log_dir: &Path) -> WorkerGuard {
    let _ = std::fs::create_dir_all(log_dir);
    let file = tracing_appender::rolling::daily(log_dir, "kadr.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("KADR_LOG").unwrap_or_else(|_| EnvFilter::new("info,kadr=debug,wgpu=warn,naga=warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_writer(writer).with_current_span(false))
        .with(fmt::layer().with_target(false).compact())
        .with(ring().layer())
        .init();

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "panic");
        prev(info);
    }));
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn ring_keeps_the_newest_lines_and_filters_by_level() {
        let ring = LogRing::with_capacity(3);
        let sub = tracing_subscriber::registry().with(ring.layer());
        tracing::subscriber::with_default(sub, || {
            tracing::debug!("d0");
            for i in 1..=4 {
                tracing::info!(n = i, "line {i}");
            }
            tracing::error!(panic = "boom", "panic");
        });
        let all = ring.tail(10, tracing::Level::TRACE);
        assert_eq!(all.len(), 3, "capacity");
        assert_eq!(all.last().unwrap().message, "panic");
        assert!(all.last().unwrap().panic);
        let errors = ring.tail(10, tracing::Level::ERROR);
        assert_eq!(errors.len(), 1);
    }
}
