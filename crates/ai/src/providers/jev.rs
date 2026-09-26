//! Jev decision model (TypeSafe). Not a chat model: it answers typed
//! questions about a text `state` — yes/no probabilities, choices among
//! named options, or levels on a scale. We only ever send locally extracted
//! *features* as text, never media.
//!
//! `POST {base}/v1/systemone` with `{model, state, questions}`.

use super::http::{client, send_json};
use super::{AiError, ProviderConfig};
use crate::credentials::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "jev-latest";
/// Conservative client-side cap (API: 64k tokens per request).
pub const MAX_REQUEST_TOKENS: u64 = 56_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JevQuestion {
    /// Yes/no → probability of "yes".
    Noul { instructions: String },
    /// One of named options (id → description).
    Choice { instructions: String, criteria: BTreeMap<String, String> },
    /// One of ordered levels.
    Score { instructions: String, criteria: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct JevAnswer {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub noul: Option<f64>,
    #[serde(default)]
    pub choice: Option<String>,
    /// Score answers are numeric (level value), not strings.
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub probabilities: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JevResponse {
    pub model: String,
    pub answers: BTreeMap<String, JevAnswer>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub struct JevProvider {
    config: ProviderConfig,
    key: Secret,
    http: reqwest::Client,
}

impl JevProvider {
    pub fn new(config: ProviderConfig, key: Secret) -> Self {
        let http = client(Duration::from_secs(config.timeout_secs));
        JevProvider { config, key, http }
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    /// Limits are in tokens (≈32k for state + longest question, 64k per
    /// request); the API answers 400 `max_tokens_exceeded` beyond that. We
    /// refuse oversized requests up front so callers split batches instead
    /// of silently losing context to truncation.
    pub async fn decide(&self, model: &str, state: &str, questions: &BTreeMap<String, JevQuestion>) -> Result<JevResponse, AiError> {
        let q_tokens: u64 = questions.values().map(|q| crate::cost::estimate_tokens(&serde_json::to_string(q).unwrap_or_default())).sum();
        let total = crate::cost::estimate_tokens(state) + q_tokens;
        if total > MAX_REQUEST_TOKENS {
            return Err(AiError::Blocked(format!("Jev request too large (~{total} tokens > {MAX_REQUEST_TOKENS}); split the batch")));
        }
        let body = json!({"model": model, "state": state, "questions": questions});
        let url = format!("{}/v1/systemone", self.config.base_url.trim_end_matches('/'));
        let v = send_json(&self.config.name, || self.http.post(&url).bearer_auth(self.key.expose()).json(&body)).await?;
        parse_response(&v)
    }
}

pub(crate) fn parse_response(v: &serde_json::Value) -> Result<JevResponse, AiError> {
    // Some gateways wrap as {code, message, data}; accept both.
    let v = if v.get("code").is_some() {
        if v["code"].as_i64().unwrap_or(0) != 0 {
            return Err(AiError::BadResponse(v["message"].as_str().unwrap_or("jev error").to_string()));
        }
        &v["data"]
    } else {
        v
    };
    let answers: BTreeMap<String, JevAnswer> = serde_json::from_value(v["answers"].clone())
        .map_err(|e| AiError::BadResponse(format!("jev answers: {e}")))?;
    Ok(JevResponse {
        model: v["model"].as_str().unwrap_or("").to_string(),
        answers,
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_serialization_matches_api() {
        let q = JevQuestion::Choice {
            instructions: "i".into(),
            criteria: BTreeMap::from([("KEEP".to_string(), "good".to_string())]),
        };
        assert_eq!(serde_json::to_value(&q).unwrap(), json!({"type":"choice","instructions":"i","criteria":{"KEEP":"good"}}));
    }

    #[test]
    fn parses_real_response_shape() {
        // Captured from a live call (2026-09-26).
        let v = json!({"model":"jev-1.13.0","answers":{
            "keep":{"type":"noul","noul":0.8},
            "usability":{"type":"choice","choice":"KEEP","confidence":0.94,"probabilities":{"KEEP":0.96,"DISCARD":0.01,"REVIEW":0.03}}},
            "usage":{"input_tokens":386,"output_tokens":58}});
        let r = parse_response(&v).unwrap();
        assert_eq!(r.answers["keep"].noul, Some(0.8));
        assert_eq!(r.answers["usability"].choice.as_deref(), Some("KEEP"));
        assert_eq!(r.input_tokens, 386);
        assert!(parse_response(&json!({"code":3,"message":"bad model"})).is_err());
        let v = json!({"model":"jev-1.13.0","answers":{"q":{"type":"score","score":3,"confidence":0.7,"probabilities":{"1":0.1,"3":0.8}}}});
        assert_eq!(parse_response(&v).unwrap().answers["q"].score, Some(3.0));
    }
}
