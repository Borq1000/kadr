//! Imports API keys from a local text file into the Windows Credential
//! Manager, then the file can be deleted. Never prints the keys.
//!
//! Usage: cargo run -p kadr-ai --example import_keys -- <file>
//! File format: a line "<Provider> api key:" followed by the key line.

use kadr_ai::credentials::{load_key, store_key, Secret};

fn main() {
    let path = std::env::args().nth(1).expect("usage: import_keys <file>");
    let text = std::fs::read_to_string(&path).expect("read key file");
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut stored = 0;
    for (i, l) in lines.iter().enumerate() {
        let lower = l.to_lowercase();
        let id = if lower.starts_with("openai") {
            "openai"
        } else if lower.starts_with("jev") {
            "jev"
        } else if lower.starts_with("anthropic") {
            "anthropic"
        } else {
            continue;
        };
        let Some(key) = lines[i + 1..].iter().find(|k| !k.is_empty()) else { continue };
        store_key(id, &Secret::new(*key)).expect("store key");
        let back = load_key(id).expect("read back");
        assert_eq!(back.expose(), *key, "credential store round-trip");
        let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        println!("stored {id}: …{tail} ({} chars)", key.len());
        stored += 1;
    }
    println!("{stored} key(s) stored in Windows Credential Manager (service \"Kadr AI\")");
}
