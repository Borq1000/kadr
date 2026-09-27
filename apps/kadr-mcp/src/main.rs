//! `kadr-mcp`: stdio MCP server that finds/launches Kadr and aggregates its
//! tools with Slint's embedded UI-automation tools (prefixed `ui_`).
//!
//! Protocol: newline-delimited JSON-RPC 2.0 on stdin/stdout. Never print
//! anything but JSON-RPC to stdout; all logging goes to stderr.

use kadr_cache::AppDirs;
use kadr_mcp::instance::{self, Instance, Liveness};
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
        let msg = match parse_request(line) {
            Parsed::Request { id, method, params } => match handle(&method, params, &mut inst) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
                Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}).to_string(),
            },
            // Notifications (`notifications/initialized`, cancellations, …) get no reply.
            Parsed::Notification => continue,
            Parsed::Invalid { id, code, message } => {
                eprintln!("kadr-mcp: {message}");
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
            }
        };
        if writeln!(stdout, "{msg}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
    // `inst` drops here, killing any Kadr we launched.
    drop(inst);
}

/// One line of input, classified per JSON-RPC 2.0.
enum Parsed {
    Request { id: Value, method: String, params: Value },
    Notification,
    /// Answered with an error: `id` is null when it couldn't be read.
    Invalid { id: Value, code: i32, message: String },
}

fn parse_request(line: &str) -> Parsed {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Parsed::Invalid { id: Value::Null, code: -32700, message: format!("Parse error: {e}") },
    };
    let Some(obj) = req.as_object() else {
        return Parsed::Invalid { id: Value::Null, code: -32600, message: "Invalid Request: expected a JSON object (batches are not supported)".into() };
    };
    let id = obj.get("id").cloned();
    match (obj.get("method").and_then(Value::as_str), id) {
        (Some(method), Some(id)) => Parsed::Request { id, method: method.to_string(), params: obj.get("params").cloned().unwrap_or(json!({})) },
        (Some(_), None) => Parsed::Notification,
        // A reply from the client (we never send requests) or garbage.
        (None, Some(_)) if obj.contains_key("result") || obj.contains_key("error") => Parsed::Notification,
        (None, id) => Parsed::Invalid { id: id.unwrap_or(Value::Null), code: -32600, message: "Invalid Request: missing `method`".into() },
    }
}

fn data_dir() -> std::path::PathBuf {
    AppDirs::new().data
}

/// A failed connection, as (error code, message) for the tool result.
type ConnectError = (&'static str, String);

/// Lazily connects to a Kadr instance. An instance whose process is alive
/// is kept even when it is slow (calls then report `busy_timeout`); only
/// when its process has exited (or its bridge port refuses connections)
/// do we switch — and then the current call is NOT run, and the result
/// says `instance_gone`, so nothing lands in a different project silently.
fn ensure_connected(inst: &mut Option<Instance>) -> Result<&Instance, ConnectError> {
    match inst {
        None => *inst = Some(instance::connect(&data_dir()).map_err(|e| ("io", e))?),
        Some(i) => {
            if instance::liveness(&i.d) == Liveness::Gone {
                let old = i.d.pid;
                *inst = None;
                let new = instance::connect(&data_dir()).map_err(|e| ("instance_gone", format!("Kadr instance {old} exited; reconnecting failed: {e}")))?;
                let msg = format!(
                    "Kadr instance {old} exited; now connected to {} ({}). The call was not run: check `get_state` and repeat it if still intended.",
                    new.d.pid,
                    if new.d.headless { "headless" } else { "window" }
                );
                *inst = Some(new);
                return Err(("instance_gone", msg));
            }
        }
    }
    Ok(inst.as_ref().unwrap())
}

/// `Ok` becomes the JSON-RPC `result`; `Err` becomes the JSON-RPC `error`
/// object (used only for top-level protocol errors, e.g. an unknown
/// method). Tool-call failures are reported as `Ok` results with
/// `isError: true`, per MCP convention.
fn handle(method: &str, params: Value, inst: &mut Option<Instance>) -> Result<Value, Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kadr", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS,
        })),
        "tools/list" => Ok(tools_list(inst)),
        "tools/call" => Ok(tools_call(params, inst)),
        "ping" => Ok(json!({})),
        _ => Err(json!({"code": -32601, "message": format!("Method not found: {method}")})),
    }
}

fn tools_list(inst: &mut Option<Instance>) -> Value {
    let mut tools = kadr_tools();
    let connected = match ensure_connected(inst) {
        Err(("instance_gone", e)) => {
            eprintln!("kadr-mcp: {e}");
            inst.as_ref().ok_or(("io", e))
        }
        other => other,
    };
    match connected {
        Ok(i) => {
            let ui_port = i.d.ui_port;
            match fetch_ui_tools(ui_port) {
                Ok(mut ui) => tools.append(&mut ui),
                Err(e) => eprintln!("kadr-mcp: Slint UI tools unavailable: {e}"),
            }
        }
        Err((_, e)) => eprintln!("kadr-mcp: no Kadr instance for tools/list: {e}"),
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
        Err((code, e)) => return error_result(code, &e),
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
        Err((code, e)) => return error_result(code, &e),
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
        Err((code, e)) => return error_result(code, &e),
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
        Err((code, e)) => return error_result(code, &e),
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
            let state = instance::liveness(&d);
            json!({"pid": d.pid, "headless": d.headless, "started_ms": d.started_ms, "alive": state != Liveness::Gone, "responding": state == Liveness::Ready})
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
        Some(d) if instance::liveness(&d) == Liveness::Ready => {
            *inst = Some(Instance::attached(d));
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
