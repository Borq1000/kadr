//! Live calls against real providers. Ignored by default (network + cost).
//! Run: cargo test -p kadr-ai --test live_providers -- --ignored --nocapture
//! Keys are read from the OS credential store (service "Kadr AI").

use kadr_ai::credentials::load_key;
use kadr_ai::providers::{self, ChatMessage, ChatRequest, JevProvider, JevQuestion, Role};
use kadr_ai::settings::AiSettings;
use std::collections::BTreeMap;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

#[test]
#[ignore = "live network call (OpenAI)"]
fn openai_chat_returns_json_and_usage() {
    let Some(key) = load_key("openai") else { return eprintln!("no openai key stored") };
    let settings = AiSettings::default();
    let cfg = settings.provider("openai").unwrap().clone();
    let p = providers::build(cfg.clone(), Some(key)).unwrap();
    let r = rt()
        .block_on(p.complete(ChatRequest {
            model: "gpt-5-mini".into(),
            messages: vec![
                ChatMessage { role: Role::System, content: "Reply with a JSON object only.".into() },
                ChatMessage { role: Role::User, content: "Return {\"ok\": true}".into() },
            ],
            max_output_tokens: 400,
            temperature: 0.0,
            json: true,
        }))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&r.text).unwrap();
    assert_eq!(v["ok"], true);
    assert!(r.input_tokens > 0);
    let cost = cfg.pricing("gpt-5-mini").cost(r.input_tokens, r.output_tokens);
    eprintln!("openai: {} in / {} out tokens, ${cost:.6}", r.input_tokens, r.output_tokens);
}

#[test]
#[ignore = "live network call (Jev)"]
fn jev_typed_decisions() {
    let Some(key) = load_key("jev") else { return eprintln!("no jev key stored") };
    let cfg = AiSettings::default().provider("jev").unwrap().clone();
    let jev = JevProvider::new(cfg, key);
    let questions = BTreeMap::from([
        ("usable".to_string(), JevQuestion::Noul { instructions: "Is this shot usable in a concert edit?".into() }),
        (
            "action".to_string(),
            JevQuestion::Choice {
                instructions: "What should the editor do with this shot?".into(),
                criteria: BTreeMap::from([
                    ("KEEP".to_string(), "good enough for the edit".to_string()),
                    ("DISCARD".to_string(), "technically unusable".to_string()),
                    ("REVIEW".to_string(), "unclear, a human should check".to_string()),
                ]),
            },
        ),
    ]);
    let state = "Shot 42, camera CAM_B, 6.1 s. Sharpness 0.08 (very blurred, lens out of focus). \
                 Shake 0.7 (strong). No faces visible. Audio: music, no speech.";
    let r = rt().block_on(jev.decide("jev-latest", state, &questions)).unwrap();
    eprintln!("jev: {:?}, tokens {}", r.answers, r.input_tokens);
    assert!(r.answers["usable"].noul.unwrap() < 0.5, "a blurred, shaky shot should not be usable");
    assert_ne!(r.answers["action"].choice.as_deref(), Some("KEEP"));
}
