//! Kadr's local control bridge: JSON-RPC over loopback HTTP with a bearer
//! token. The editor supplies a `Dispatch` that runs each call on its UI
//! thread; `kadr-mcp` talks to it with `call`.

pub mod discovery;
mod http;

use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub use discovery::Discovery;
pub use http::{post, Response};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BridgeError {
    pub code: &'static str,
    pub message: String,
}

impl BridgeError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        BridgeError { code, message: message.into() }
    }
}

pub enum Reply {
    Now(Result<Value, BridgeError>),
    Later(Box<dyn FnOnce() -> Result<Value, BridgeError> + Send>),
}

pub type Dispatch = Arc<dyn Fn(String, Value, crossbeam_channel::Sender<Reply>) + Send + Sync>;

pub const MAX_BODY: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub struct Bridge {
    pub port: u16,
    pub token: String,
}

impl Bridge {
    pub fn start(dispatch: Dispatch) -> std::io::Result<Bridge> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let tok = token.clone();
        std::thread::Builder::new().name("mcp-bridge".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let tok_clone = tok.clone();
                let dispatch_clone = dispatch.clone();
                // Spawn each connection on its own thread so slow clients don't block accept loop
                std::thread::spawn(move || {
                    http::serve(stream, &tok_clone, &|method, params| run(&dispatch_clone, method, params));
                });
            }
        })?;
        Ok(Bridge { port, token })
    }
}

fn run(dispatch: &Dispatch, method: String, params: Value) -> Result<Value, BridgeError> {
    let timeout = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
        .min(MAX_TIMEOUT);
    let (tx, rx) = crossbeam_channel::bounded(1);
    dispatch(method.clone(), params, tx);
    match rx.recv_timeout(timeout) {
        Ok(Reply::Now(r)) => r,
        Ok(Reply::Later(f)) => f(),
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
            Err(BridgeError::new("busy_timeout", format!("Kadr did not answer `{method}` within {} ms (UI busy or a modal is open)", timeout.as_millis())))
        }
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
            Err(BridgeError::new("busy_timeout", format!("Kadr's `{method}` handler finished without replying (UI busy, a modal is open, or the handler gave no reply)")))
        }
    }
}

/// Blocking client (used by `kadr-mcp`).
pub fn call(port: u16, token: &str, method: &str, params: Value, timeout: Duration) -> Result<Value, BridgeError> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let resp = http::post(port, "/rpc", Some(token), &body, timeout).map_err(|e| BridgeError::new("io", e.to_string()))?;
    let v: Value = serde_json::from_str(&resp.body).map_err(|e| BridgeError::new("io", format!("bad reply ({}): {e}", resp.status)))?;
    if let Some(err) = v.get("error") {
        let code = err.get("code").and_then(Value::as_str).unwrap_or("io");
        let code: &'static str = ["unauthorized", "unknown_method", "bad_params", "not_found", "busy_timeout", "panicked", "edit_rejected", "io"]
            .into_iter()
            .find(|c| *c == code)
            .unwrap_or("io");
        return Err(BridgeError::new(code, err.get("message").and_then(Value::as_str).unwrap_or("").to_string()));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}
