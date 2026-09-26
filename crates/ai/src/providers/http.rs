//! Shared HTTP behaviour: timeout, retry with backoff (honouring
//! Retry-After), cancellation and structured errors. Keys are sent in
//! headers only and never logged.

use super::AiError;
use std::time::Duration;

pub(super) fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("Kadr/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

const MAX_ATTEMPTS: u32 = 3;

/// Sends `build()` up to 3 times on transient failures.
pub(super) async fn send_json(
    provider: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<serde_json::Value, AiError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let started = std::time::Instant::now();
        let result = build().send().await;
        let err = match result {
            Ok(resp) => {
                let status = resp.status();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let body = resp.text().await.map_err(|e| AiError::Network(e.to_string()))?;
                tracing::info!(provider, status = status.as_u16(), ms = started.elapsed().as_millis() as u64, attempt, "ai response");
                if status.is_success() {
                    return serde_json::from_str(&body).map_err(|e| AiError::BadResponse(e.to_string()));
                }
                tracing::debug!(provider, body = %truncate(&body, 500), "ai error body");
                match status.as_u16() {
                    401 | 403 => return Err(AiError::Auth),
                    429 => AiError::RateLimited { retry_after },
                    s if s >= 500 => AiError::Http { status: s, body: truncate(&body, 300) },
                    s => return Err(AiError::Http { status: s, body: truncate(&body, 300) }),
                }
            }
            Err(e) if e.is_timeout() => AiError::Timeout,
            Err(e) => AiError::Network(e.to_string()),
        };
        if attempt >= MAX_ATTEMPTS {
            return Err(err);
        }
        let wait = match &err {
            AiError::RateLimited { retry_after: Some(d) } => (*d).min(Duration::from_secs(60)),
            _ => Duration::from_millis(800 * 2u64.pow(attempt - 1)),
        };
        tracing::warn!(provider, error = %err, ?wait, "ai request failed, retrying");
        tokio::time::sleep(wait).await;
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
