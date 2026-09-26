//! Jev decision model (TypeSafe). Not a chat model: it answers typed
//! questions about a text `state` — yes/no probabilities, choices among
//! named options, or levels on a scale. We only ever send locally extracted
//! *features* as text, never media.
//!
//! `POST {base}/v1/systemone` with `{model, state, questions}`.

use super::http::{client, send_json};
use super::{AiError, BoxFuture, ProviderConfig};
use crate::credentials::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;

/// Pinned: the `jev-latest` alias moves with releases and answers can
/// shift under calibrated thresholds (research §1.2).
pub const DEFAULT_MODEL: &str = "jev-1.13.0";
/// Conservative client-side cap (API: 64k tokens per request).
pub const MAX_REQUEST_TOKENS: u64 = 56_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JevQuestion {
    /// Yes/no → probability of "yes".
    Noul { instructions: String },
    /// One of named options (id → description: string or `{what, not_for, examples}`).
    Choice { instructions: String, criteria: BTreeMap<String, serde_json::Value> },
    /// One of ordered levels, low to high.
    Score { instructions: String, criteria: Vec<serde_json::Value> },
}

impl JevQuestion {
    /// API limits checked locally: the API silently ignores unknown fields
    /// but rejects these with a 400 after a round trip.
    pub fn validate(&self) -> Result<(), AiError> {
        let bad = |m: &str| Err(AiError::Blocked(format!("invalid Jev question: {m}")));
        match self {
            JevQuestion::Noul { .. } => Ok(()),
            JevQuestion::Choice { criteria, .. } if criteria.is_empty() || criteria.len() > 255 => bad("choice needs 1..=255 options"),
            JevQuestion::Score { criteria, .. } if !(2..=10).contains(&criteria.len()) => bad("score needs 2..=10 levels"),
            _ => Ok(()),
        }
    }

    pub fn instructions(&self) -> &str {
        match self {
            JevQuestion::Noul { instructions } | JevQuestion::Choice { instructions, .. } | JevQuestion::Score { instructions, .. } => {
                instructions
            }
        }
    }
}

/// Anything that answers Jev requests: the REST client, or a fake in tests.
pub trait JevTransport: Send + Sync {
    fn decide<'a>(
        &'a self,
        model: &'a str,
        state: &'a serde_json::Value,
        questions: &'a BTreeMap<String, JevQuestion>,
    ) -> BoxFuture<'a, Result<JevResponse, AiError>>;
}

/// Documented 400 bodies that deserve their own error (research §2.5).
pub(crate) fn map_400(body: &str) -> Option<AiError> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let d = &v["detail"];
    match d["error_type"].as_str() {
        Some("max_tokens_exceeded") => Some(AiError::TooLarge),
        Some(_) if d["message"].as_str().is_some_and(|m| m.starts_with("Unknown model")) => {
            Some(AiError::UnknownModel(d["message"].as_str().unwrap_or_default().to_string()))
        }
        _ => None,
    }
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
    pub async fn decide(&self, model: &str, state: &serde_json::Value, questions: &BTreeMap<String, JevQuestion>) -> Result<JevResponse, AiError> {
        for q in questions.values() {
            q.validate()?;
        }
        let q_tokens: u64 = questions.values().map(|q| crate::cost::estimate_tokens(&serde_json::to_string(q).unwrap_or_default())).sum();
        let total = crate::cost::estimate_tokens(&state.to_string()) + q_tokens;
        if total > MAX_REQUEST_TOKENS {
            return Err(AiError::TooLarge);
        }
        let body = json!({"model": model, "state": state, "questions": questions});
        let url = format!("{}/v1/systemone", self.config.base_url.trim_end_matches('/'));
        let v = send_json(&self.config.name, || self.http.post(&url).bearer_auth(self.key.expose()).json(&body))
            .await
            .map_err(|e| match e {
                AiError::Http { status: 400, ref body } => map_400(body).unwrap_or(e),
                e => e,
            })?;
        parse_response(&v)
    }
}

impl JevTransport for JevProvider {
    fn decide<'a>(
        &'a self,
        model: &'a str,
        state: &'a serde_json::Value,
        questions: &'a BTreeMap<String, JevQuestion>,
    ) -> BoxFuture<'a, Result<JevResponse, AiError>> {
        Box::pin(JevProvider::decide(self, model, state, questions))
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
            criteria: BTreeMap::from([("KEEP".to_string(), json!("good"))]),
        };
        assert_eq!(serde_json::to_value(&q).unwrap(), json!({"type":"choice","instructions":"i","criteria":{"KEEP":"good"}}));
    }

    #[test]
    fn local_limits_are_enforced() {
        let many: BTreeMap<String, serde_json::Value> = (0..256).map(|i| (format!("O{i}"), json!(null))).collect();
        assert!(JevQuestion::Choice { instructions: "i".into(), criteria: many }.validate().is_err());
        assert!(JevQuestion::Choice { instructions: "i".into(), criteria: BTreeMap::new() }.validate().is_err());
        assert!(JevQuestion::Score { instructions: "i".into(), criteria: vec![json!("only")] }.validate().is_err());
        assert!(JevQuestion::Score { instructions: "i".into(), criteria: vec![json!("a"), json!({"what": "b"})] }.validate().is_ok());
        assert!(JevQuestion::Noul { instructions: "i".into() }.validate().is_ok());
    }

    #[test]
    fn maps_documented_400s() {
        assert_eq!(map_400(r#"{"detail":{"error_type":"max_tokens_exceeded"}}"#), Some(AiError::TooLarge));
        assert!(matches!(
            map_400(r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: jev-9"}}"#),
            Some(AiError::UnknownModel(_))
        ));
        assert_eq!(map_400("{}"), None);
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
