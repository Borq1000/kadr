use super::http::{client, send_json};
use super::{AiError, AiProvider, BoxFuture, ChatRequest, ChatResponse, ProviderConfig, ProviderKind, Role};
use crate::credentials::Secret;
use serde_json::json;
use std::time::Duration;

/// `/chat/completions` — OpenAI, Jev, OpenRouter, Ollama, llama.cpp, LM Studio…
pub struct OpenAiCompatibleProvider {
    config: ProviderConfig,
    key: Option<Secret>,
    http: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: ProviderConfig, key: Option<Secret>) -> Self {
        let http = client(Duration::from_secs(config.timeout_secs));
        OpenAiCompatibleProvider { config, key, http }
    }

    pub(crate) fn body(&self, req: &ChatRequest) -> serde_json::Value {
        let messages: Vec<_> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                json!({"role": role, "content": m.content})
            })
            .collect();
        let mut body = json!({"model": req.model, "messages": messages});
        // Current OpenAI models take max_completion_tokens; compatible servers max_tokens.
        if self.config.kind == ProviderKind::OpenAi {
            body["max_completion_tokens"] = json!(req.max_output_tokens);
            // Reasoning models spend completion tokens on thinking; edit
            // planning needs little of it and the user pays for every token.
            if req.model.starts_with("gpt-5") || req.model.starts_with('o') {
                body["reasoning_effort"] = json!("low");
            }
        } else {
            body["max_tokens"] = json!(req.max_output_tokens);
            body["temperature"] = json!(req.temperature);
        }
        if req.json {
            body["response_format"] = json!({"type": "json_object"});
        }
        body
    }
}

pub(crate) fn parse_response(v: &serde_json::Value, model: &str) -> Result<ChatResponse, AiError> {
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| AiError::BadResponse("missing choices[0].message.content".into()))?;
    Ok(ChatResponse {
        text: text.to_string(),
        model: v["model"].as_str().unwrap_or(model).to_string(),
        input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    })
}

impl AiProvider for OpenAiCompatibleProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn complete<'a>(&'a self, req: ChatRequest) -> BoxFuture<'a, Result<ChatResponse, AiError>> {
        Box::pin(async move {
            let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));
            let body = self.body(&req);
            let v = send_json(&self.config.name, || {
                let mut b = self.http.post(&url).json(&body);
                if let Some(k) = &self.key {
                    b = b.bearer_auth(k.expose());
                }
                b
            })
            .await?;
            parse_response(&v, &req.model)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage() {
        let v = json!({"model":"m","choices":[{"message":{"content":"{}"}}],"usage":{"prompt_tokens":12,"completion_tokens":3}});
        let r = parse_response(&v, "x").unwrap();
        assert_eq!((r.input_tokens, r.output_tokens, r.text.as_str()), (12, 3, "{}"));
        assert!(parse_response(&json!({}), "x").is_err());
    }
}
