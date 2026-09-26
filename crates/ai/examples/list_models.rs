//! Lists model ids available to the stored OpenAI key (ids only).
fn main() {
    let key = kadr_ai::credentials::load_key("openai").expect("openai key");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let r = reqwest::Client::new().get("https://api.openai.com/v1/models").bearer_auth(key.expose()).send().await.unwrap();
        println!("HTTP {}", r.status());
        let v: serde_json::Value = r.json().await.unwrap();
        let mut ids: Vec<String> = v["data"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default();
        ids.retain(|i| i.starts_with("gpt") || i.starts_with("o"));
        ids.sort();
        println!("{}", ids.join(" "));
    });
}
