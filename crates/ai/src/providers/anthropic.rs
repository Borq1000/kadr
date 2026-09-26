use super::http::{client, send_json};
use super::{AiError, AiProvider, BoxFuture, ChatRequest, ChatResponse, ProviderConfig, Role};
use crate::credentials::Secret;
use serde_json::json;
use std::time::Duration;

/// Anthropic Messages API (`/v1/messages`).
pub struct AnthropicProvider {
    config: ProviderConfig,
    key: Option<Secret>,
    http: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(config: ProviderConfig, key: Option<Secret>) -> Self {
        let http = client(Duration::from_secs(config.timeout_secs));
        AnthropicProvider { config, key, http }
    }
}

pub(crate) fn body(req: &ChatRequest) -> serde_json::Value {
    let system: Vec<&str> = req.messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
    let messages: Vec<_> = req
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| json!({"role": if m.role == Role::User { "user" } else { "assistant" }, "content": m.content}))
        .collect();
    json!({
        "model": req.model,
        "max_tokens": req.max_output_tokens,
        "system": system.join("\n\n"),
        "messages": messages,
    })
}

pub(crate) fn parse_response(v: &serde_json::Value, model: &str) -> Result<ChatResponse, AiError> {
    let text: String = v["content"]
        .as_array()
        .ok_or_else(|| AiError::BadResponse("missing content".into()))?
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    Ok(ChatResponse {
        text,
        model: v["model"].as_str().unwrap_or(model).to_string(),
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
    })
}

impl AiProvider for AnthropicProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn complete<'a>(&'a self, req: ChatRequest) -> BoxFuture<'a, Result<ChatResponse, AiError>> {
        Box::pin(async move {
            let key = self.key.as_ref().ok_or_else(|| AiError::NoKey(self.config.name.clone()))?;
            let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
            let body = body(&req);
            let v = send_json(&self.config.name, || {
                self.http
                    .post(&url)
                    .header("x-api-key", key.expose())
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
            })
            .await?;
            parse_response(&v, &req.model)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ChatMessage;

    #[test]
    fn system_prompt_is_hoisted() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![
                ChatMessage { role: Role::System, content: "sys".into() },
                ChatMessage { role: Role::User, content: "hi".into() },
            ],
            max_output_tokens: 100,
            temperature: 0.0,
            json: true,
        };
        let b = body(&req);
        assert_eq!(b["system"], "sys");
        assert_eq!(b["messages"].as_array().unwrap().len(), 1);
        let r = parse_response(&json!({"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":5,"output_tokens":1}}), "m").unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens), ("ok", 5));
    }
}
