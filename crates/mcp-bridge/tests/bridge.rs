use kadr_mcp_bridge::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

fn echo() -> Dispatch {
    Arc::new(|method: String, params: Value, tx: crossbeam_channel::Sender<Reply>| {
        let r = match method.as_str() {
            "echo" => Reply::Now(Ok(params)),
            "later" => Reply::Later(Box::new(|| Ok(json!("done")))),
            "never" => return, // simulates a blocked UI thread: no reply
            _ => Reply::Now(Err(BridgeError::new("unknown_method", method))),
        };
        let _ = tx.send(r);
    })
}

fn raw(port: u16, request: &[u8]) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(request).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn round_trip_now_and_later() {
    let b = Bridge::start(echo()).unwrap();
    let v = call(b.port, &b.token, "echo", json!({"x": 1}), Duration::from_secs(5)).unwrap();
    assert_eq!(v, json!({"x": 1}));
    assert_eq!(call(b.port, &b.token, "later", json!({}), Duration::from_secs(5)).unwrap(), json!("done"));
    let e = call(b.port, &b.token, "nope", json!({}), Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.code, "unknown_method");
}

#[test]
fn wrong_token_is_rejected() {
    let b = Bridge::start(echo()).unwrap();
    let e = call(b.port, "bad", "echo", json!({}), Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.code, "unauthorized");
}

#[test]
fn handler_that_never_replies_times_out() {
    let b = Bridge::start(echo()).unwrap();
    let t = std::time::Instant::now();
    let e = call(b.port, &b.token, "never", json!({"timeout_ms": 300}), Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.code, "busy_timeout");
    assert!(t.elapsed() < Duration::from_secs(3));
    // The server is still alive.
    assert_eq!(call(b.port, &b.token, "echo", json!(2), Duration::from_secs(5)).unwrap(), json!(2));
}

#[test]
fn malformed_requests_get_4xx_and_server_survives() {
    let b = Bridge::start(echo()).unwrap();
    let auth = format!("Authorization: Bearer {}\r\n", b.token);
    assert!(raw(b.port, b"GARBAGE\r\n\r\n").starts_with("HTTP/1.1 400"));
    assert!(raw(b.port, format!("POST /rpc HTTP/1.1\r\n{auth}\r\n").as_bytes()).starts_with("HTTP/1.1 411"));
    assert!(raw(b.port, format!("POST /rpc HTTP/1.1\r\n{auth}Content-Length: 9\r\n\r\nnot json!").as_bytes()).starts_with("HTTP/1.1 400"));
    assert!(raw(b.port, format!("POST /rpc HTTP/1.1\r\n{auth}Content-Length: 99999999\r\n\r\n").as_bytes()).starts_with("HTTP/1.1 413"));
    assert!(raw(b.port, b"POST /rpc HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}").starts_with("HTTP/1.1 401"));
    assert_eq!(call(b.port, &b.token, "echo", json!(3), Duration::from_secs(5)).unwrap(), json!(3));
}

#[test]
fn discovery_files_are_per_instance() {
    let dir = tempfile::tempdir().unwrap();
    let d = |pid| Discovery { port: 1, token: "t".into(), ui_port: 2, pid, started_ms: pid as i64, headless: pid == 2 };
    discovery::write(dir.path(), &d(1)).unwrap();
    discovery::write(dir.path(), &d(2)).unwrap();
    assert_eq!(discovery::list(dir.path()).len(), 2);
    discovery::remove(dir.path(), 2);
    let left = discovery::list(dir.path());
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].pid, 1);
}

#[test]
fn disconnected_handler_has_different_message_than_timeout() {
    let b = Bridge::start(echo()).unwrap();

    // Handler that drops tx without replying (disconnected case)
    let e = call(b.port, &b.token, "never", json!({}), Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.code, "busy_timeout");
    assert!(!e.message.contains("within"), "disconnected message should not claim a wait: {}", e.message);
    assert!(e.message.contains("without replying") || e.message.contains("gave no reply"),
        "disconnected message should mention no reply: {}", e.message);

    // Handler that holds tx alive but doesn't send (real timeout case)
    let b2 = Bridge::start({
        Arc::new(|method: String, _params: Value, tx: crossbeam_channel::Sender<Reply>| {
            match method.as_str() {
                "slow" => {
                    // Move tx into a thread that sleeps forever
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(60));
                        let _ = tx.send(Reply::Now(Ok(json!("done"))));
                    });
                }
                "echo" => {
                    let _ = tx.send(Reply::Now(Ok(json!("ok"))));
                }
                _ => {
                    let _ = tx.send(Reply::Now(Err(BridgeError::new("unknown_method", method))));
                }
            }
        })
    }).unwrap();

    let e = call(b2.port, &b2.token, "slow", json!({"timeout_ms": 200}), Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.code, "busy_timeout");
    assert!(e.message.contains("within 200 ms"), "real timeout message should mention time: {}", e.message);
}

#[test]
fn oversized_header_line_is_rejected() {
    let b = Bridge::start(echo()).unwrap();
    let auth = format!("Authorization: Bearer {}\r\n", b.token);
    let huge_header = format!("X-Huge: {}\r\n", "x".repeat(10000));
    let response = raw(b.port, format!("POST /rpc HTTP/1.1\r\n{auth}{huge_header}\r\n").as_bytes());
    assert!(response.starts_with("HTTP/1.1 431"), "oversized header should get 431");
    // Server should survive
    assert_eq!(call(b.port, &b.token, "echo", json!(1), Duration::from_secs(5)).unwrap(), json!(1));
}
