//! Provider-agnostic LLM interface with REST implementations. Nothing in the
//! editor depends on a specific vendor.

mod anthropic;
mod http;
pub mod jev;
mod openai;

pub use anthropic::AnthropicProvider;
pub use jev::{JevProvider, JevQuestion, JevResponse};
pub use openai::OpenAiCompatibleProvider;

use crate::credentials::Secret;
use crate::privacy::DataPermissions;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_output_tokens: u32,
    pub temperature: f32,
    /// Ask for a JSON object response where the API supports it.
    pub json: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatResponse {
    pub text: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum AiError {
    #[error("no API key configured for {0}")]
    NoKey(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("request timed out")]
    Timeout,
    #[error("rate limited by provider")]
    RateLimited { retry_after: Option<Duration> },
    #[error("authentication failed (check API key)")]
    Auth,
    #[error("provider returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("unexpected response: {0}")]
    BadResponse(String),
    #[error("cancelled")]
    Cancelled,
    #[error("blocked: {0}")]
    Blocked(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    /// Jev typed-decision model (`/v1/systemone`), text features only.
    Jev,
    OpenAiCompatible,
    /// OpenAI-compatible server on this machine (Ollama, llama.cpp, LM Studio).
    Local,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub permissions: DataPermissions,
    #[serde(default)]
    pub prices: Vec<ModelPrice>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    90
}

impl ProviderConfig {
    pub fn is_local(&self) -> bool {
        self.kind == ProviderKind::Local
            || self.base_url.contains("://localhost")
            || self.base_url.contains("://127.0.0.1")
            || self.base_url.contains("://[::1]")
    }
    pub fn needs_key(&self) -> bool {
        !self.is_local()
    }
    pub fn pricing(&self, model: &str) -> crate::cost::Pricing {
        if self.is_local() {
            return crate::cost::Pricing::FREE;
        }
        self.prices
            .iter()
            .find(|p| p.model == model)
            .map(|p| crate::cost::Pricing { input_per_mtok: p.input_per_mtok, output_per_mtok: p.output_per_mtok })
            // Unknown model: assume an expensive one rather than free.
            .unwrap_or(crate::cost::Pricing { input_per_mtok: 15.0, output_per_mtok: 75.0 })
    }
}

pub trait AiProvider: Send + Sync {
    fn config(&self) -> &ProviderConfig;
    fn complete<'a>(&'a self, req: ChatRequest) -> BoxFuture<'a, Result<ChatResponse, AiError>>;
}

/// Instantiates the REST client for a configuration.
pub fn build(config: ProviderConfig, key: Option<Secret>) -> Result<Box<dyn AiProvider>, AiError> {
    if config.needs_key() && key.is_none() {
        return Err(AiError::NoKey(config.name.clone()));
    }
    Ok(match config.kind {
        ProviderKind::Jev => return Err(AiError::Blocked("Jev is a decision model, not a chat model; use JevDecisionService".into())),
        ProviderKind::Anthropic => Box::new(AnthropicProvider::new(config, key)),
        _ => Box::new(OpenAiCompatibleProvider::new(config, key)),
    })
}
