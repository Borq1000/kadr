//! Minimal HTTP/1.1: one request per connection, Content-Length bodies.

use crate::{BridgeError, MAX_BODY};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

const MAX_HEADER_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;

fn respond(s: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let _ = write!(s, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = s.flush();
}

pub fn serve(mut stream: TcpStream, token: &str, handle: &dyn Fn(String, Value) -> Result<Value, BridgeError>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut r = BufReader::new(&stream);
    let mut line = String::new();
    if r.read_line(&mut line).is_err() || line.len() > MAX_HEADER_LINE || !line.starts_with("POST ") {
        return respond(&mut stream, 400, "{}");
    }
    let (mut len, mut auth) = (None, false);
    let mut header_count = 0;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() {
            return respond(&mut stream, 400, "{}");
        }
        if h.len() > MAX_HEADER_LINE {
            return respond(&mut stream, 431, "{}");
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            return respond(&mut stream, 431, "{}");
        }
        let (k, v) = h.split_once(':').unwrap_or((h, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse::<usize>().ok(),
            "authorization" => auth = v.trim() == format!("Bearer {token}"),
            _ => {}
        }
    }
    if !auth {
        return respond(&mut stream, 401, &json!({"error": {"code": "unauthorized", "message": "bad or missing token"}}).to_string());
    }
    let Some(len) = len else { return respond(&mut stream, 411, "{}") };
    if len > MAX_BODY {
        return respond(&mut stream, 413, "{}");
    }
    let mut body = vec![0; len];
    if r.read_exact(&mut body).is_err() {
        return respond(&mut stream, 400, "{}");
    }
    let Ok(req) = serde_json::from_slice::<Value>(&body) else { return respond(&mut stream, 400, "{}") };
    let (Some(method), params) = (req.get("method").and_then(Value::as_str), req.get("params").cloned().unwrap_or(Value::Null)) else {
        return respond(&mut stream, 400, "{}");
    };
    let out = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(method.to_string(), params))) {
        Ok(Ok(v)) => json!({"jsonrpc": "2.0", "id": req.get("id"), "result": v}),
        Ok(Err(e)) => json!({"jsonrpc": "2.0", "id": req.get("id"), "error": e}),
        Err(p) => {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            json!({"jsonrpc": "2.0", "id": req.get("id"), "error": {"code": "panicked", "message": msg}})
        }
    };
    respond(&mut stream, 200, &out.to_string());
}

pub fn post(port: u16, path: &str, token: Option<&str>, body: &str, timeout: Duration) -> std::io::Result<Response> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(timeout))?;
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(s, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    let mut raw = String::new();
    s.read_to_string(&mut raw)?;
    let status = raw.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map(|x| x.1.to_string()).unwrap_or_default();
    Ok(Response { status, body })
}
