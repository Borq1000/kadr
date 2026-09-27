//! Kadr's local control bridge: JSON-RPC over loopback HTTP with a bearer
//! token. The editor supplies a `Dispatch` that runs each call on its UI
//! thread; `kadr-mcp` talks to it with `call`.

pub mod discovery;
mod http;
mod process;

use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;

pub use discovery::Discovery;
pub use http::{post, Response};
pub use process::pid_alive;

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

/// `kadr --headless` exits with this code, before creating a window, when
/// "Allow control via MCP" is off in Kadr's settings.
pub const EXIT_MCP_DISABLED: i32 = 3;

pub const MAX_BODY: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_CONNECTIONS: usize = 16;

pub struct Bridge {
    pub port: u16,
    pub token: String,
}

impl Bridge {
    pub fn start(dispatch: Dispatch) -> std::io::Result<Bridge> {
        Self::start_with_deadline(dispatch, http::REQUEST_DEADLINE)
    }

    /// As [`Bridge::start`], with a custom per-request read deadline (tests).
    #[doc(hidden)]
    pub fn start_with_deadline(dispatch: Dispatch, deadline: Duration) -> std::io::Result<Bridge> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let tok = token.clone();
        let conn_count = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new().name("mcp-bridge".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                // Check connection cap
                let current = conn_count.fetch_add(1, Ordering::SeqCst);
                if current >= MAX_CONNECTIONS {
                    // Decrement since we're not spawning a thread
                    conn_count.fetch_sub(1, Ordering::SeqCst);
                    // Send 503 on the accept thread (no thread spawned)
                    let mut s = stream;
                    let _ = std::io::Write::write_all(&mut s, b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
                    continue;
                }

                let tok_clone = tok.clone();
                let dispatch_clone = dispatch.clone();
                let conn_count_clone = conn_count.clone();
                // Spawn each connection on its own thread so slow clients don't block accept loop
                std::thread::spawn(move || {
                    // Drop guard to decrement count when thread ends
                    let _guard = ConnectionGuard(conn_count_clone);
                    http::serve(stream, &tok_clone, deadline, &|method, params| run(&dispatch_clone, method, params));
                });
            }
        })?;
        Ok(Bridge { port, token })
    }
}

// Drop guard to decrement connection count
struct ConnectionGuard(Arc<AtomicUsize>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn run(dispatch: &Dispatch, method: String, params: Value) -> Result<Value, BridgeError> {
    // Liveness is answered here, on the bridge thread: a busy UI thread (a
    // native dialog, long work) must not make a live Kadr look dead.
    if method == "ping" {
        return Ok(json!({"pid": std::process::id()}));
    }
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
            Err(BridgeError::new("busy_timeout", format!("Kadr did not answer `{method}` within {} ms (UI busy or a modal is open); the request may still run once Kadr is free — check `get_state` before retrying", timeout.as_millis())))
        }
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
            Err(BridgeError::new("busy_timeout", format!("Kadr's `{method}` handler finished without replying (UI busy, a modal is open, or the handler gave no reply)")))
        }
    }
}

/// Blocking client (used by `kadr-mcp`).
pub fn call(port: u16, token: &str, method: &str, params: Value, timeout: Duration) -> Result<Value, BridgeError> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let resp = http::post(port, "/rpc", Some(token), &body, timeout).map_err(|e| {
        // `refused`: nothing listens on the port any more (the instance is
        // gone), as opposed to a slow or busy one.
        BridgeError::new(if e.kind() == std::io::ErrorKind::ConnectionRefused { "refused" } else { "io" }, e.to_string())
    })?;
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
