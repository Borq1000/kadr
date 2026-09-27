//! Kadr's MCP tools, executed on the UI thread (see kadr-mcp-bridge).

use crate::app::App;
use crate::mcp_state;
use kadr_mcp_bridge::{BridgeError, Reply};
use serde_json::{json, Value};

fn bad(msg: impl Into<String>) -> Reply {
    Reply::Now(Err(BridgeError::new("bad_params", msg)))
}

fn ms(p: &Value, key: &str) -> Option<kadr_core::Time> {
    p.get(key).and_then(Value::as_i64).map(kadr_core::Time::from_millis)
}

pub fn handle(app: &mut App, method: &str, p: Value) -> Reply {
    match method {
        "ping" => Reply::Now(Ok(json!({"pid": std::process::id()}))),
        "get_state" => Reply::Now(Ok(app.mcp_state())),
        "get_log" => {
            let n = p.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
            let level = p.get("level").and_then(Value::as_str).unwrap_or("trace").parse().unwrap_or(tracing::Level::TRACE);
            Reply::Now(Ok(json!(crate::logging::ring().tail(n, level))))
        }
        "idle" => Reply::Now(Ok(json!({
            "idle": app.jobs.active_count() == 0 && !app.ui().get_preview_loading(),
            "jobs": app.jobs.active_count(),
        }))),
        "layout_text" => match (p.get("text").and_then(Value::as_str), p.get("layout").and_then(Value::as_str)) {
            (Some(t), Some(l @ ("ru" | "en"))) => Reply::Now(Ok(json!(mcp_state::layout_text(t, l)))),
            _ => bad("layout_text needs text and layout: ru|en"),
        },
        "get_frame" => match ms(&p, "at_ms") {
            Some(t) => app.mcp_frame(t, p.get("max_w").and_then(Value::as_u64).unwrap_or(960) as u32),
            None => bad("get_frame needs at_ms"),
        },
        _ => crate::mcp_api::actions(app, method, p),
    }
}

/// Filled in by Task 5.
pub fn actions(_app: &mut App, method: &str, _p: Value) -> Reply {
    Reply::Now(Err(BridgeError::new("unknown_method", method.to_string())))
}

impl App {
    pub fn mcp_state(&self) -> Value {
        let ui = self.ui();
        json!({
            "project": {"name": self.project.name, "path": self.path, "dirty": self.is_dirty(), "assets": self.project.assets.iter().map(|a| json!({"id": a.id.to_string(), "name": a.name, "kind": format!("{:?}", a.kind()), "duration_ms": a.duration().as_millis()})).collect::<Vec<_>>()},
            "timeline": mcp_state::timeline_json(self.project.sequence(), &self.tl.selection, self.playhead),
            "playing": self.playing,
            "open": {
                "settings": ui.get_settings_open(), "export": ui.get_export_open(), "confirm": ui.get_confirm_open(),
                "prompt": ui.get_prompt_open(), "multicam": ui.get_mc_open(), "welcome": ui.get_welcome_open(), "jobs": ui.get_jobs_open(),
            },
            "jobs": self.jobs.active_count(),
            "toasts": self.toasts.texts(),
            "assistant": self.ai.transcript(),
        })
    }

    /// The composited preview frame at `t` as PNG, decoded off the UI thread.
    pub fn mcp_frame(&self, t: kadr_core::Time, max_w: u32) -> Reply {
        let Some(media) = self.media.clone() else { return Reply::Now(Err(BridgeError::new("io", "no FFmpeg"))) };
        let seq = self.project.sequence();
        let Some(v) = kadr_timeline::composition::video_at(seq, t) else { return Reply::Now(Err(BridgeError::new("not_found", "no video at this time"))) };
        let Some(path) = self.project.asset(v.asset).map(|a| a.path.clone()) else { return Reply::Now(Err(BridgeError::new("not_found", "asset missing"))) };
        let src = v.source_start;
        Reply::Later(Box::new(move || {
            let f = media.decode_frame(&path, src, max_w, max_w).map_err(|e| BridgeError::new("io", e.to_string()))?;
            let mut png = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut png, f.width, f.height);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().and_then(|mut w| w.write_image_data(&f.data)).map_err(|e| BridgeError::new("io", e.to_string()))?;
            }
            use base64_lite::encode;
            Ok(json!({"mime": "image/png", "data": encode(&png), "width": f.width, "height": f.height}))
        }))
    }
}

/// A tiny standard-alphabet base64 encoder (no padding-free variant, no
/// external crate — this is the only thing MCP frame replies need).
mod base64_lite {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
            out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHA[((n >> 6) & 0x3f) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHA[(n & 0x3f) as usize] as char } else { '=' });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::encode;

        #[test]
        fn encode_pads_per_rfc_4648_examples() {
            assert_eq!(encode(b"Man"), "TWFu");
            assert_eq!(encode(b"Ma"), "TWE=");
            assert_eq!(encode(b"M"), "TQ==");
        }
    }
}
