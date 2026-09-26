//! Research harness: runs a list of raw Jev requests from a JSON file and
//! prints status, latency, request id and response body for each.
//! The key is loaded from the credential store and is never printed.
//!
//! Usage: cargo run -q -p kadr-ai --example jev_lab -- <cases.json>
//! cases.json: [{"name": "...", "method": "POST"|"GET", "path": "/v1/systemone",
//!               "auth": "real"|"bogus"|"none", "body": {...}, "save_to": "optional.json"}]
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("cases.json path");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let key = kadr_ai::credentials::load_key("jev").expect("jev key");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let http = reqwest::Client::new();
        for c in cases {
            let name = c["name"].as_str().unwrap_or("?");
            let method = c["method"].as_str().unwrap_or("POST");
            let url = format!("https://api.typesafe.ai{}", c["path"].as_str().unwrap_or("/v1/systemone"));
            let mut req = if method == "GET" { http.get(&url) } else { http.post(&url) };
            match c["auth"].as_str().unwrap_or("real") {
                "real" => req = req.bearer_auth(key.expose()),
                "bogus" => req = req.bearer_auth("ts-invalid-key-for-testing"),
                _ => {}
            }
            if method != "GET" {
                if let Some(raw) = c["raw_body"].as_str() {
                    req = req.header("content-type", "application/json").body(raw.to_string());
                } else {
                    req = req.json(&c["body"]);
                }
            }
            let t = Instant::now();
            let r = req.send().await;
            let ms = t.elapsed().as_millis();
            match r {
                Ok(resp) => {
                    let status = resp.status();
                    let rid = resp.headers().get("x-typesafe-request-id").and_then(|v| v.to_str().ok()).unwrap_or("-").to_string();
                    let ra = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).unwrap_or("-").to_string();
                    let body = resp.text().await.unwrap_or_default();
                    if let Some(f) = c["save_to"].as_str() {
                        std::fs::write(f, &body).unwrap();
                    }
                    let shown: String = body.chars().take(c["max_print"].as_u64().unwrap_or(6000) as usize).collect();
                    println!("=== {name} | HTTP {status} | {ms} ms | req-id {rid} | retry-after {ra}\n{shown}\n");
                }
                Err(e) => println!("=== {name} | NETWORK ERROR after {ms} ms: {e}\n"),
            }
        }
    });
}
