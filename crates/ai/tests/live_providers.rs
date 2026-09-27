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
                    ("KEEP".to_string(), "good enough for the edit".into()),
                    ("DISCARD".to_string(), "technically unusable".into()),
                    ("REVIEW".to_string(), "unclear, a human should check".into()),
                ]),
            },
        ),
    ]);
    let state = "Shot 42, camera CAM_B, 6.1 s. Sharpness 0.08 (very blurred, lens out of focus). \
                 Shake 0.7 (strong). No faces visible. Audio: music, no speech.";
    let r = rt().block_on(jev.decide("jev-1.13.0", &serde_json::json!(state), &questions)).unwrap();
    eprintln!("jev: {:?}, tokens {}", r.answers, r.input_tokens);
    assert!(r.answers["usable"].noul.unwrap() < 0.5, "a blurred, shaky shot should not be usable");
    assert_ne!(r.answers["action"].choice.as_deref(), Some("KEEP"));
}

#[test]
#[ignore = "live network call (Jev)"]
fn jev_templates_through_the_service() {
    use kadr_ai::jev::prefs::Precedent;
    use kadr_ai::jev::templates::{camera_item, shot_item, AngleFeatures, CameraInterval};
    use kadr_ai::jev::JevDecisionService;
    use kadr_core::{CancelToken, Time, TimeRange};
    use kadr_project::{Gate, ShotSummary};
    use std::sync::Arc;

    let Some(key) = load_key("jev") else { return eprintln!("no jev key stored") };
    let settings = AiSettings::default();
    let cfg = settings.provider("jev").unwrap().clone();
    let pricing = cfg.pricing(&settings.jev_model);
    let svc = JevDecisionService::new(Arc::new(JevProvider::new(cfg, key)), settings.jev_model.clone(), pricing);

    let shot = |sharp: &str, shake: &str| ShotSummary {
        range: TimeRange::new(Time::ZERO, Time::from_secs(5)),
        sharpness: sharp.into(),
        exposure: "normal".into(),
        shake: shake.into(),
        black: false,
    };
    let angle = |l: &str, d: &str| AngleFeatures {
        label: l.into(),
        description: d.into(),
        sharpness: "sharp".into(),
        exposure: "normal".into(),
        shake: "none".into(),
        audio_level: "quiet".into(),
    };
    let iv = CameraInterval {
        subject: "solo".into(),
        time_label: "05:00-05:04".into(),
        angles: vec![angle("CAM1", "wide shot of the whole stage"), angle("CAM2", "medium shot of the singer"), angle("CAM3", "close-up of the guitarist's hands")],
        previous: Some(("CAM2".into(), Time::from_secs(12))),
        context: serde_json::json!({"music_section": "guitar solo", "singer_singing": false}),
    };
    let history: Vec<Precedent> = (0..5)
        .map(|i| Precedent { context: iv.context.clone(), ai_suggested: "CAM3".into(), editor_chose: "CAM1".into(), at_ms: i })
        .collect();
    let items = vec![
        shot_item("bad".into(), &shot("very blurry", "heavy"), "music"),
        shot_item("good".into(), &shot("sharp", "none"), "speech"),
        camera_item(&iv, &[]),
        camera_item(&CameraInterval { subject: "solo-personal".into(), ..iv.clone() }, &history),
    ];
    let est = svc.estimate(&items, &[]);
    let (d, input, _) = rt().block_on(svc.run(items, &[], CancelToken::new())).unwrap();
    for x in &d {
        eprintln!("{:>14}: {} p={:.2} m={:.2} {:?} | {:?}", x.stored.subject, x.stored.value, x.stored.p_max, x.stored.margin, x.stored.gate,
            x.answers.iter().map(|(k, a)| (k.clone(), a.choice.clone(), a.noul, a.score)).collect::<Vec<_>>());
    }
    eprintln!("estimated {} tokens / ${:.6}, billed {input} tokens", est.input_tokens, est.usd);
    assert_eq!(d[0].stored.value, "DISCARD");
    assert_ne!(d[0].stored.gate, Gate::Review);
    assert_eq!(d[1].stored.value, "KEEP");
    for x in &d {
        assert!((x.stored.probs.values().sum::<f32>() - 1.0).abs() < 0.05, "{:?}", x.stored.probs);
    }
    let personal = &d[3].answers["editor_pick"];
    assert_eq!(personal.choice.as_deref(), Some("CAM1"), "five solos where the editor chose the wide shot");
    assert!(d[3].answers["has_precedent"].noul.unwrap() > 0.5);
}
