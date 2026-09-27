//! Minimal HTTP/1.1: one request per connection, Content-Length bodies.

use crate::{BridgeError, MAX_BODY};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

pub struct Response {
    pub status: u16,
    pub body: String,
}

pub(crate) const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
const MAX_HEADER_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;

fn respond(mut s: &TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    // One write: the status line must not arrive split from the headers.
    let msg = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = s.write_all(msg.as_bytes());
    let _ = s.flush();
}

/// Reads one LF-terminated line, never buffering more than `cap` bytes and
/// never reading past `deadline` (a trickling client can't extend it).
/// `Err` carries the HTTP status to answer with.
fn read_line_bounded(r: &mut BufReader<&TcpStream>, deadline: Instant, cap: usize, too_long: u16) -> Result<String, u16> {
    let mut line = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(408);
        }
        let _ = r.get_ref().set_read_timeout(Some(remaining));
        let avail = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => continue,
            Err(_) => return Err(400),
        };
        if avail.is_empty() {
            return Err(400); // EOF before the end of the line
        }
        let newline = avail.iter().position(|&b| b == b'\n');
        let take = newline.map_or(avail.len(), |i| i + 1).min(cap + 1 - line.len());
        line.extend_from_slice(&avail[..take]);
        r.consume(take);
        if line.last() == Some(&b'\n') {
            return String::from_utf8(line).map_err(|_| 400);
        }
        if line.len() > cap {
            return Err(too_long);
        }
    }
}

/// Fills `buf` completely before `deadline`.
fn read_exact_by(r: &mut BufReader<&TcpStream>, buf: &mut [u8], deadline: Instant) -> Result<(), u16> {
    let mut done = 0;
    while done < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(408);
        }
        let _ = r.get_ref().set_read_timeout(Some(remaining));
        match r.read(&mut buf[done..]) {
            Ok(0) => return Err(400),
            Ok(n) => done += n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => continue,
            Err(_) => return Err(400),
        }
    }
    Ok(())
}

/// Answers an error and closes gracefully: the unread rest of the request is
/// drained (bounded) first, because closing a socket with unread input sends
/// a reset that can destroy the response before the client reads it.
fn reject(mut s: &TcpStream, status: u16, body: &str) {
    respond(s, status, body);
    let _ = s.shutdown(Shutdown::Write);
    let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
    let until = Instant::now() + Duration::from_millis(500);
    let mut sink = [0u8; 16 * 1024];
    let mut drained = 0usize;
    while Instant::now() < until && drained < 1024 * 1024 {
        match s.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

pub fn serve(stream: TcpStream, token: &str, request_deadline: Duration, handle: &dyn Fn(String, Value) -> Result<Value, BridgeError>) {
    let deadline = Instant::now() + request_deadline;
    let mut r = BufReader::new(&stream);
    match read_line_bounded(&mut r, deadline, MAX_HEADER_LINE, 400) {
        Ok(l) if l.starts_with("POST ") => {}
        Ok(_) => return reject(&stream, 400, "{}"),
        Err(status) => return reject(&stream, status, "{}"),
    };
    let (mut len, mut auth) = (None, false);
    let mut header_count = 0;
    loop {
        let h = match read_line_bounded(&mut r, deadline, MAX_HEADER_LINE, 431) {
            Ok(h) => h,
            Err(status) => return reject(&stream, status, "{}"),
        };
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            return reject(&stream, 431, "{}");
        }
        let (k, v) = h.split_once(':').unwrap_or((h, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse::<usize>().ok(),
            "authorization" => auth = v.trim() == format!("Bearer {token}"),
            _ => {}
        }
    }
    if !auth {
        return reject(&stream, 401, &json!({"error": {"code": "unauthorized", "message": "bad or missing token"}}).to_string());
    }
    let Some(len) = len else { return reject(&stream, 411, "{}") };
    if len > MAX_BODY {
        return reject(&stream, 413, "{}");
    }
    let mut body = vec![0; len];
    if let Err(status) = read_exact_by(&mut r, &mut body, deadline) {
        return reject(&stream, status, "{}");
    }
    drop(r);
    let Ok(req) = serde_json::from_slice::<Value>(&body) else { return reject(&stream, 400, "{}") };
    let (Some(method), params) = (req.get("method").and_then(Value::as_str), req.get("params").cloned().unwrap_or(Value::Null)) else {
        return reject(&stream, 400, "{}");
    };
    let out = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(method.to_string(), params))) {
        Ok(Ok(v)) => json!({"jsonrpc": "2.0", "id": req.get("id"), "result": v}),
        Ok(Err(e)) => json!({"jsonrpc": "2.0", "id": req.get("id"), "error": e}),
        Err(p) => {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            json!({"jsonrpc": "2.0", "id": req.get("id"), "error": {"code": "panicked", "message": msg}})
        }
    };
    respond(&stream, 200, &out.to_string());
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
