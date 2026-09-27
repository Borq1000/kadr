//! `kadr-mcp`: stdio MCP server that finds/launches Kadr and aggregates its
//! tools with Slint's embedded UI-automation tools (prefixed `ui_`).
//!
//! Protocol: newline-delimited JSON-RPC 2.0 on stdin/stdout. Never print
//! anything but JSON-RPC to stdout; all logging goes to stderr.

use kadr_cache::AppDirs;
use kadr_mcp::instance::{self, Instance};
use kadr_mcp::tools::kadr_tools;
use kadr_mcp_bridge::{call, discovery, post};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::time::Duration;

const PROTOCOL_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "\
Tools prefixed `ui_` come from Slint's embedded UI-automation server and drive \
real input events; call `ui_list_windows` first to get a window handle. Kadr's \
own tools read and edit the project. After sending input, call `wait_idle` \
before reading state or a frame, since edits and renders can be asynchronous. \
Input events do not go through the OS's normal input pipeline, so \
layout-dependent keyboard shortcuts may not translate the way they would on a \
physical keyboard; use `layout_text` when you need to type text in a specific \
keyboard layout.";

fn main() {
    let mut inst: Option<Instance> = None;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // EOF / IO error: drop `inst` on the way out and exit.
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("kadr-mcp: ignoring unparseable line: {e}");
                continue;
            }
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let id = req.get("id").cloned();
        if method == "notifications/initialized" {
            continue; // no reply
        }
        let Some(id) = id else {
            continue; // notification we don't handle
        };
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let reply = handle(method, params, &mut inst);
        let msg = json!({"jsonrpc": "2.0", "id": id, "result": reply}).to_string();
        if writeln!(stdout, "{msg}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
    // `inst` drops here, killing any Kadr we launched.
    drop(inst);
}

fn data_dir() -> std::path::PathBuf {
    AppDirs::new().data
}

/// Lazily connects (or reconnects) to a Kadr instance.
fn ensure_connected(inst: &mut Option<Instance>) -> Result<&Instance, String> {
    let stale = match inst {
        Some(i) => !instance::answers(&i.d),
        None => true,
    };
    if stale {
        *inst = Some(instance::connect(&data_dir())?);
    }
    Ok(inst.as_ref().unwrap())
}

fn handle(method: &str, params: Value, inst: &mut Option<Instance>) -> Value {
    match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kadr", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS,
        }),
        "tools/list" => tools_list(inst),
        "tools/call" => tools_call(params, inst),
        _ => json!({"isError": true, "content": [{"type": "text", "text": format!("unknown method: {method}")}]}),
    }
}

fn tools_list(inst: &mut Option<Instance>) -> Value {
    let mut tools = kadr_tools();
    match ensure_connected(inst) {
        Ok(i) => {
            let ui_port = i.d.ui_port;
            match fetch_ui_tools(ui_port) {
                Ok(mut ui) => tools.append(&mut ui),
                Err(e) => eprintln!("kadr-mcp: Slint UI tools unavailable: {e}"),
            }
        }
        Err(e) => eprintln!("kadr-mcp: no Kadr instance for tools/list: {e}"),
    }
    json!({"tools": tools})
}

fn fetch_ui_tools(ui_port: u16) -> Result<Vec<Value>, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string();
    let resp = post(ui_port, "/mcp", None, &body, Duration::from_secs(10)).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&resp.body).map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error") {
        return Err(err.to_string());
    }
    let list = v.get("result").and_then(|r| r.get("tools")).and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(list
        .into_iter()
        .map(|mut t| {
            if let Some(name) = t.get("name").and_then(Value::as_str).map(str::to_string) {
                t["name"] = json!(format!("ui_{name}"));
            }
            t
        })
        .collect())
}

fn tools_call(params: Value, inst: &mut Option<Instance>) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    if let Some(ui_name) = name.strip_prefix("ui_") {
        return call_ui(ui_name, arguments, inst);
    }

    match name.as_str() {
        "list_instances" => return list_instances(),
        "use_instance" => return use_instance(arguments, inst),
        "wait_idle" => return wait_idle(arguments, inst),
        "get_frame" => return get_frame(arguments, inst),
        _ => {}
    }

    let i = match ensure_connected(inst) {
        Ok(i) => i,
        Err(e) => return error_result("io", &e),
    };
    let timeout = if name == "export" || name == "export_status" { Duration::from_secs(30 * 60) } else { Duration::from_secs(30) };
    match call(i.d.port, &i.d.token, &name, arguments, timeout) {
        Ok(v) => text_result(&v),
        Err(e) => error_result(e.code, &e.message),
    }
}

fn call_ui(ui_name: &str, arguments: Value, inst: &mut Option<Instance>) -> Value {
    let i = match ensure_connected(inst) {
        Ok(i) => i,
        Err(e) => return error_result("io", &e),
    };
    let ui_port = i.d.ui_port;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": ui_name, "arguments": arguments}}).to_string();
    match post(ui_port, "/mcp", None, &body, Duration::from_secs(60)) {
        Ok(resp) => match serde_json::from_str::<Value>(&resp.body) {
            Ok(v) => {
                if let Some(err) = v.get("error") {
                    error_result("io", &err.to_string())
                } else {
                    v.get("result").cloned().unwrap_or(Value::Null)
                }
            }
            Err(e) => error_result("io", &format!("bad reply from Slint UI server: {e}")),
        },
        Err(e) => error_result("io", &format!("Slint UI server unreachable: {e}")),
    }
}

fn wait_idle(arguments: Value, inst: &mut Option<Instance>) -> Value {
    let timeout_ms = arguments.get("timeout_ms").and_then(Value::as_u64).unwrap_or(20_000);
    let i = match ensure_connected(inst) {
        Ok(i) => i,
        Err(e) => return error_result("io", &e),
    };
    let (port, token) = (i.d.port, i.d.token.clone());
    let start = std::time::Instant::now();
    let mut consecutive_idle = 0;
    loop {
        match call(port, &token, "idle", json!({}), Duration::from_secs(5)) {
            Ok(v) => {
                let idle = v.get("idle").and_then(Value::as_bool).unwrap_or(false);
                if idle {
                    consecutive_idle += 1;
                    if consecutive_idle >= 2 {
                        return text_result(&json!({"idle": true, "waited_ms": start.elapsed().as_millis()}));
                    }
                } else {
                    consecutive_idle = 0;
                }
            }
            Err(e) => return error_result(e.code, &e.message),
        }
        if start.elapsed().as_millis() as u64 >= timeout_ms {
            return text_result(&json!({"idle": false, "waited_ms": start.elapsed().as_millis()}));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn get_frame(arguments: Value, inst: &mut Option<Instance>) -> Value {
    let i = match ensure_connected(inst) {
        Ok(i) => i,
        Err(e) => return error_result("io", &e),
    };
    match call(i.d.port, &i.d.token, "get_frame", arguments, Duration::from_secs(30)) {
        Ok(v) => {
            let mime = v.get("mime").and_then(Value::as_str).unwrap_or("image/png").to_string();
            let data = v.get("data").and_then(Value::as_str).unwrap_or("").to_string();
            json!({"content": [{"type": "image", "data": data, "mimeType": mime}]})
        }
        Err(e) => error_result(e.code, &e.message),
    }
}

fn list_instances() -> Value {
    let dir = data_dir();
    let list = discovery::list(&dir);
    let out: Vec<Value> = list
        .into_iter()
        .map(|d| {
            let alive = instance::answers(&d);
            json!({"pid": d.pid, "headless": d.headless, "started_ms": d.started_ms, "alive": alive})
        })
        .collect();
    text_result(&json!({"instances": out}))
}

fn use_instance(arguments: Value, inst: &mut Option<Instance>) -> Value {
    let pid = match arguments.get("pid").and_then(Value::as_u64) {
        Some(p) => p as u32,
        None => return error_result("bad_params", "missing `pid`"),
    };
    let dir = data_dir();
    let found = discovery::list(&dir).into_iter().find(|d| d.pid == pid);
    match found {
        Some(d) if instance::answers(&d) => {
            *inst = Some(Instance { d, child: None });
            text_result(&json!({"ok": true}))
        }
        Some(_) => error_result("not_found", &format!("instance {pid} is not responding")),
        None => error_result("not_found", &format!("no instance with pid {pid}")),
    }
}

fn text_result(v: &Value) -> Value {
    let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    json!({"content": [{"type": "text", "text": text}]})
}

fn error_result(code: &str, message: &str) -> Value {
    json!({"isError": true, "content": [{"type": "text", "text": format!("{code}: {message}")}]})
}
