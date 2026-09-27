use kadr_mcp::instance::{clean_stale, launch, launch_exit_message, liveness, pick, Liveness};
use kadr_mcp_bridge::{discovery, Discovery};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn d(pid: u32, started_ms: i64, headless: bool) -> Discovery {
    Discovery { port: 1, token: "t".into(), ui_port: 2, pid, started_ms, headless }
}

#[test]
fn prefers_the_users_window_then_the_newest() {
    let got = pick(vec![d(1, 10, false), d(2, 30, true), d(3, 20, false)], |_| true).unwrap();
    assert_eq!(got.pid, 3, "newest non-headless");
    let got = pick(vec![d(2, 30, true)], |_| true).unwrap();
    assert_eq!(got.pid, 2, "headless when it's all there is");
    assert!(pick(vec![d(1, 10, false)], |_| false).is_none(), "dead instances are never picked");
}

/// A pid that existed and has exited.
fn dead_pid() -> u32 {
    let mut c = Command::new(if cfg!(windows) { "cmd" } else { "true" });
    if cfg!(windows) {
        c.args(["/C", "exit 0"]);
    }
    let mut child = c.stdout(Stdio::null()).spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

/// A loopback port nobody listens on.
fn closed_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

#[test]
fn clean_stale_keeps_files_of_live_pids_even_when_they_do_not_answer() {
    let dir = tempfile::tempdir().unwrap();
    let me = std::process::id();
    let dead = dead_pid();
    let mut live = d(me, 1, false);
    live.port = closed_port();
    discovery::write(dir.path(), &live).unwrap();
    discovery::write(dir.path(), &d(dead, 2, false)).unwrap();
    clean_stale(dir.path());
    let left = discovery::list(dir.path());
    assert_eq!(left.iter().map(|x| x.pid).collect::<Vec<_>>(), vec![me], "live pid kept, dead pid removed");
}

fn held_dispatch() -> kadr_mcp_bridge::Dispatch {
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    std::sync::Arc::new(move |_m: String, _p: serde_json::Value, tx: crossbeam_channel::Sender<kadr_mcp_bridge::Reply>| {
        held.lock().unwrap().push(tx); // a UI thread that never gets to the call
    })
}

#[test]
fn liveness_is_ready_while_the_ui_is_blocked_and_gone_only_when_the_process_or_port_is() {
    let me = std::process::id();
    let b = kadr_mcp_bridge::Bridge::start(held_dispatch()).unwrap();
    let ready = Discovery { port: b.port, token: b.token.clone(), ui_port: 0, pid: me, started_ms: 1, headless: true };
    assert_eq!(liveness(&ready), Liveness::Ready, "a blocked UI thread still answers ping");
    let dead = Discovery { pid: dead_pid(), ..ready.clone() };
    assert_eq!(liveness(&dead), Liveness::Gone, "dead pid");
    let refused = Discovery { port: closed_port(), ..ready.clone() };
    assert_eq!(liveness(&refused), Liveness::Gone, "live pid but nothing listening");
    // Accepts connections but never answers: busy, not gone.
    let silent = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let busy = Discovery { port: silent.local_addr().unwrap().port(), ..ready };
    assert_eq!(liveness(&busy), Liveness::Busy);
}

#[cfg(windows)]
#[test]
fn launch_of_a_program_that_exits_immediately_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    // where.exe rejects `--headless` and exits at once.
    let exe = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or(r"C:\Windows".into())).join("System32").join("where.exe");
    let t = std::time::Instant::now();
    let e = launch(dir.path(), &exe).err().expect("launch must fail");
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "failed only after {:?}", t.elapsed());
    assert!(e.contains("exited"), "unexpected error: {e}");
}

#[test]
fn exit_code_3_means_mcp_is_disabled_in_settings() {
    assert!(launch_exit_message(1234, Some(kadr_mcp_bridge::EXIT_MCP_DISABLED)).contains("MCP control is disabled in Kadr settings"));
    assert!(launch_exit_message(1234, Some(1)).contains("exited"));
}

#[test]
fn stdio_server_answers_mcp_ping() {
    let data_dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kadr-mcp"))
        .env("KADR_DATA_DIR", data_dir.path())
        .env("KADR_EXE", data_dir.path().join("no-such-kadr.exe"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":7,"method":"ping"}}"#).unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let v: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["result"], serde_json::json!({}), "MCP ping gets an empty result: {v}");
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn stdio_server_reports_launch_failure_as_a_tool_error() {
    let data_dir = tempfile::tempdir().unwrap();
    let bad_exe = data_dir.path().join("no-such-kadr.exe");

    let mut child = Command::new(env!("CARGO_BIN_EXE_kadr-mcp"))
        .env("KADR_DATA_DIR", data_dir.path())
        .env("KADR_EXE", &bad_exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"get_state","arguments":{{}}}}}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/lsit","params":{{}}}}"#).unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();

    let line1 = lines.next().unwrap().unwrap();
    let v1: serde_json::Value = serde_json::from_str(&line1).unwrap();
    assert_eq!(v1["result"]["serverInfo"]["name"], "kadr");

    let line2 = lines.next().unwrap().unwrap();
    let v2: serde_json::Value = serde_json::from_str(&line2).unwrap();
    assert_eq!(v2["result"]["isError"], true);
    let text = v2["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("cannot start"), "unexpected text: {text}");

    let line3 = lines.next().unwrap().unwrap();
    let v3: serde_json::Value = serde_json::from_str(&line3).unwrap();
    assert!(v3.get("result").is_none(), "unknown method must not produce a `result`: {v3}");
    assert_eq!(v3["error"]["code"], -32601);
    let msg = v3["error"]["message"].as_str().unwrap();
    assert!(msg.contains("tools/lsit"), "unexpected error message: {msg}");

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn launch_passes_the_parent_pid_in_the_environment_not_as_an_argument() {
    // A Kadr from before `--parent-pid` would import an unknown argument as
    // a media path; an environment variable it doesn't know is ignored.
    let c = kadr_mcp::instance::launch_command(std::path::Path::new("kadr.exe"));
    let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args, ["--headless"]);
    let pid = c.get_envs().find(|(k, _)| *k == kadr_mcp_bridge::PARENT_PID_ENV).and_then(|(_, v)| v).map(|v| v.to_string_lossy().into_owned());
    assert_eq!(pid, Some(std::process::id().to_string()));
}

/// Sends `lines` to a fresh kadr-mcp and returns its first `n` replies.
fn replies(lines: &[&str], n: usize) -> Vec<serde_json::Value> {
    let data_dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kadr-mcp"))
        .env("KADR_DATA_DIR", data_dir.path())
        .env("KADR_EXE", data_dir.path().join("no-such-kadr.exe"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for l in lines {
        writeln!(stdin, "{l}").unwrap();
    }
    // Read on a thread: a reply that never comes must fail the test, not hang it.
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(stdout).lines().take(n) {
            let _ = tx.send(l.unwrap());
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut out = vec![];
    while out.len() < n {
        match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(l) => out.push(serde_json::from_str(&l).unwrap()),
            Err(_) => {
                let _ = child.kill();
                panic!("kadr-mcp gave {} of {n} replies: {out:?}", out.len());
            }
        }
    }
    drop(stdin);
    let _ = child.wait();
    out
}

#[test]
fn unparseable_and_invalid_messages_get_json_rpc_errors() {
    let r = replies(&["this is not json", r#"{"jsonrpc":"2.0","id":5}"#, "[1,2]", r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#], 4);
    assert_eq!(r[0]["error"]["code"], -32700, "{}", r[0]);
    assert!(r[0]["id"].is_null(), "the id is unknown: {}", r[0]);
    assert_eq!((r[1]["error"]["code"].as_i64(), r[1]["id"].as_i64()), (Some(-32600), Some(5)), "{}", r[1]);
    assert_eq!(r[2]["error"]["code"], -32600, "{}", r[2]);
    assert_eq!(r[3]["id"], 6, "the server keeps going: {}", r[3]);
}
