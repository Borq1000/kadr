//! Prints the raw Jev response for one tiny decision (key from credential store).
fn main() {
    let key = kadr_ai::credentials::load_key("jev").expect("jev key");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let body = serde_json::json!({
            "model": "jev-latest",
            "state": "Shot: 4.2 s, blur 0.1 (sharp), shake 0.05, 1 face large and centered, speech present.",
            "questions": {
                "keep": {"type": "noul", "instructions": "Is this shot usable in a final edit?"},
                "usability": {"type": "choice", "instructions": "Decide what to do with this shot.",
                    "criteria": {"KEEP": "good enough for the edit", "DISCARD": "technically unusable", "REVIEW": "unclear, human should check"}}
            }
        });
        let r = reqwest::Client::new()
            .post("https://api.typesafe.ai/v1/systemone")
            .bearer_auth(key.expose())
            .json(&body)
            .send()
            .await
            .unwrap();
        println!("HTTP {}", r.status());
        println!("{}", r.text().await.unwrap());
    });
}
