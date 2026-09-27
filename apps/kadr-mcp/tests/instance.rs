use kadr_mcp::instance::{pick, clean_stale};
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

#[test]
fn stale_discovery_file_is_ignored_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    discovery::write(dir.path(), &d(424242, 1, false)).unwrap();
    discovery::write(dir.path(), &d(7, 2, false)).unwrap();
    clean_stale(dir.path(), |x| x.pid == 7);
    let left = discovery::list(dir.path());
    assert_eq!(left.iter().map(|x| x.pid).collect::<Vec<_>>(), vec![7]);
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
