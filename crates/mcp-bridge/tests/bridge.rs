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

#[test]
fn slow_client_does_not_block_other_clients() {
    let b = Bridge::start(echo()).unwrap();
    let port = b.port;
    let token = b.token.clone();

    // Spawn a thread that opens a slow connection (trickles header bytes every ~300 ms)
    let port_clone = port;
    std::thread::spawn(move || {
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port_clone)) {
            let _ = s.write_all(b"POST /rpc HTTP/1.1\r\n");
            std::thread::sleep(Duration::from_millis(300));
            let _ = s.write_all(b"X");
            std::thread::sleep(Duration::from_millis(300));
            let _ = s.write_all(b"-H");
            std::thread::sleep(Duration::from_millis(300));
            let _ = s.write_all(b"eader");
            std::thread::sleep(Duration::from_millis(300));
            // Never send the full header; trickling forever

            // After deadline expires, server should send 408
            let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
            let mut buf = [0u8; 1024];
            let mut response = String::new();
            if let Ok(n) = s.read(&mut buf) {
                response = String::from_utf8_lossy(&buf[..n]).to_string();
            }
            assert!(response.contains("408"), "slow connection should get 408 Request Timeout");
        }
    });

    // Give slow connection time to establish and start trickling
    std::thread::sleep(Duration::from_millis(100));

    // Normal client should be served quickly even though slow client is trickling
    // (within 2s, not blocked by slow client's 10s timeout)
    let start = std::time::Instant::now();
    let v = call(port, &token, "echo", json!(42), Duration::from_secs(5)).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(v, json!(42));
    assert!(elapsed < Duration::from_secs(2), "normal client was blocked by slow client for {:?}", elapsed);
}

#[test]
fn connection_cap_prevents_exhaustion() {
    let b = Bridge::start(echo()).unwrap();
    let port = b.port;

    // Open 16 idle connections (the cap)
    let mut conns = vec![];
    for _ in 0..16 {
        if let Ok(s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            conns.push(s);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert_eq!(conns.len(), 16, "should be able to open 16 connections");

    // 17th connection should get 503 Service Unavailable
    if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
        let mut buf = [0u8; 1024];
        let mut response = String::new();
        if let Ok(n) = s.read(&mut buf) {
            response = String::from_utf8_lossy(&buf[..n]).to_string();
        }
        assert!(response.contains("503"), "17th connection should get 503, got: {}", &response[..80.min(response.len())]);
    } else {
        panic!("17th connection should be rejected with 503");
    }

    // Drop the 16 connections: their threads notice the hang-up and free
    // their slots, how soon depends on the scheduler — wait for it, bounded.
    drop(conns);
    let t = std::time::Instant::now();
    let v = loop {
        match call(b.port, &b.token, "echo", json!("ok"), Duration::from_secs(5)) {
            Ok(v) => break v,
            Err(_) if t.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("slots never freed: {e:?}"),
        }
    };
    assert_eq!(v, json!("ok"));
}

/// A dispatch whose handler never runs (a UI thread that is blocked forever):
/// it keeps every reply sender alive and never sends.
fn blocked_ui() -> Dispatch {
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    Arc::new(move |_method: String, _params: Value, tx: crossbeam_channel::Sender<Reply>| {
        held.lock().unwrap().push(tx);
    })
}

#[test]
fn ping_is_answered_on_the_bridge_thread_even_when_the_ui_never_replies() {
    let b = Bridge::start(blocked_ui()).unwrap();
    let t = std::time::Instant::now();
    let v = call(b.port, &b.token, "ping", json!({}), Duration::from_secs(5)).expect("ping must not need the UI thread");
    assert_eq!(v["pid"], json!(std::process::id()));
    assert!(t.elapsed() < Duration::from_secs(1), "ping took {:?}", t.elapsed());
}

#[test]
fn request_line_without_newline_is_rejected_promptly() {
    let b = Bridge::start(echo()).unwrap();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", b.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    let t = std::time::Instant::now();
    // 64 KiB, no newline, and the connection stays open.
    let _ = s.write_all(&vec![b'P'; 64 * 1024]);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let out = String::from_utf8_lossy(&out);
    assert!(out.starts_with("HTTP/1.1 414"), "expected 414, got {:?}", &out[..out.len().min(60)]);
    assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
    // The server keeps serving.
    assert_eq!(call(b.port, &b.token, "echo", json!(5), Duration::from_secs(5)).unwrap(), json!(5));
}

#[test]
fn trickling_client_gets_408_by_the_deadline() {
    let b = Bridge::start_with_deadline(echo(), Duration::from_millis(800)).unwrap();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", b.port)).unwrap();
    let mut w = s.try_clone().unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let trickler = std::thread::spawn(move || {
        let _ = w.write_all(b"POST /rpc HTTP/1.1\r\n");
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            if w.write_all(b"X").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    s.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
    let t = std::time::Instant::now();
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    }
    let elapsed = t.elapsed();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = trickler.join();
    let out = String::from_utf8_lossy(&got).to_string();
    assert!(out.starts_with("HTTP/1.1 408"), "expected 408, got {out:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "408 came after {elapsed:?}");
}

#[test]
fn pid_liveness_is_checked_with_the_os() {
    assert!(pid_alive(std::process::id()));
    let mut c = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" });
    if cfg!(windows) {
        c.args(["/C", "exit 0"]);
    }
    let mut child = c.stdout(std::process::Stdio::null()).spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    drop(child); // releases our handle, so the process object can go away
    assert!(!pid_alive(pid), "an exited process is not alive");
}

#[test]
fn only_the_rpc_path_is_served() {
    let b = Bridge::start(echo()).unwrap();
    let auth = format!("Authorization: Bearer {}\r\n", b.token);
    let body = r#"{"method":"echo","params":1}"#;
    for path in ["/", "/mcp", "/rpc/x", "/rpcx", "/rpc?x=1"] {
        let out = raw(b.port, format!("POST {path} HTTP/1.1\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
        assert!(out.starts_with("HTTP/1.1 404"), "{path}: {:?}", &out[..out.len().min(40)]);
    }
    let out = raw(b.port, format!("POST /rpc HTTP/1.1\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
}

#[test]
fn a_client_that_hangs_up_mid_headers_gets_no_reply() {
    let b = Bridge::start(echo()).unwrap();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", b.port)).unwrap();
    s.write_all(b"POST /rpc HTTP/1.1\r\nContent-Le").unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    assert!(out.is_empty(), "nobody is left to read a status: {:?}", String::from_utf8_lossy(&out));
    assert_eq!(call(b.port, &b.token, "echo", json!(7), Duration::from_secs(5)).unwrap(), json!(7));
}

#[test]
fn an_overlong_request_line_gets_414() {
    let b = Bridge::start(echo()).unwrap();
    let out = raw(b.port, format!("POST /{} HTTP/1.1\r\n\r\n", "a".repeat(10_000)).as_bytes());
    assert!(out.starts_with("HTTP/1.1 414"), "{:?}", &out[..out.len().min(40)]);
}
