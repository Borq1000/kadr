//! End-to-end numbers from the real editor: a headless Kadr driven through
//! kadr-mcp (stdio JSON-RPC), in a temporary data dir — never the user's.

use crate::report::Report;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(data: &Path) -> Result<Mcp, String> {
        let dir = std::env::current_exe().map_err(|e| e.to_string())?.parent().ok_or("no exe dir")?.to_path_buf();
        let exe = |n: &str| dir.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
        let mut child = Command::new(exe("kadr-mcp"))
            .env("KADR_DATA_DIR", data)
            .env("KADR_EXE", exe("kadr"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start kadr-mcp next to kadr-bench (build kadr-editor and kadr-mcp into the same target dir): {e}"))?;
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(Mcp { child, stdin, out, next: 0 })
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}).to_string();
        let w = self.stdin.as_mut().ok_or("session closed")?;
        writeln!(w, "{line}").and_then(|_| w.flush()).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        self.out.read_line(&mut reply).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&reply).map_err(|e| format!("bad reply {reply:?}: {e}"))?;
        match v.get("error") {
            Some(e) => Err(e.to_string()),
            None => Ok(v["result"].clone()),
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        tool_result(&self.rpc("tools/call", json!({"name": tool, "arguments": args}))?).map_err(|e| format!("{tool}: {e}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // Closing stdin ends the session; kadr-mcp stops the headless Kadr it launched.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// An MCP `tools/call` result: `isError` → Err(text); JSON text → parsed; other text → string.
pub fn tool_result(v: &Value) -> Result<Value, String> {
    let text = v["content"][0]["text"].as_str().unwrap_or_default();
    if v["isError"].as_bool() == Some(true) {
        return Err(text.to_string());
    }
    Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
}

/// Numeric leaves of `v` → rows `scenario = prefix`, `metric = dotted path`.
pub fn flatten(prefix: &str, case: &str, v: &Value, r: &mut Report) {
    fn walk(scenario: &str, case: &str, path: &str, v: &Value, r: &mut Report) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    walk(scenario, case, &p, x, r);
                }
            }
            Value::Number(n) => r.push(scenario, case, path, n.as_f64().unwrap_or(0.0), ""),
            _ => {}
        }
    }
    walk(prefix, case, "", v, r)
}

/// What the live run plays: one clip, or the three-layer project (`layers == 3`), at `quality`
/// (0 = Full, 1 = ½, 2 = ¼; the editor's `preview_quality`).
pub struct Options {
    pub layers: u32,
    pub quality: i32,
}

impl Options {
    pub fn case(&self) -> String {
        let q = match self.quality {
            0 => "full",
            1 => "half",
            _ => "quarter",
        };
        format!("{} layer{} {q}", self.layers, if self.layers == 1 { "" } else { "s" })
    }
}

/// `full` / `half` / `quarter` → the editor's preview quality index.
pub fn parse_quality(s: &str) -> Option<i32> {
    match s {
        "full" => Some(0),
        "half" => Some(1),
        "quarter" => Some(2),
        _ => None,
    }
}

pub fn run(clip: &Path, seconds: u64, opts: &Options) -> Result<Report, String> {
    let clip = clip.canonicalize().map_err(|e| format!("{}: {e}", clip.display()))?;
    let data = std::env::temp_dir().join(format!("kadr-bench-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let result = drive(&clip, &data, seconds, opts);
    let _ = std::fs::remove_dir_all(&data);
    result
}

/// The three-layer live project: `clip` on V1 (60 s), the 4K bench clip as a rotated picture-in-picture
/// on V2 (three 20 s segments end to end) and the alpha logo on V3.
fn three_layer_file(clip: &Path, data: &Path) -> Result<std::path::PathBuf, String> {
    use kadr_core::{FrameRate, Time, TimeRange};
    use kadr_project::{Clip, Project, Track, TrackKind};
    let ff = kadr_media::ffmpeg::FfmpegCli::locate().map_err(|e| e.to_string())?;
    let dir = crate::media::bench_dir();
    let uhd = crate::export_bench::probe(&ff, &dir.join("h264_2160p30.mp4"))?;
    let logo = crate::export_bench::probe(&ff, &crate::export_bench::ensure_logo(&dir)?)?;
    let hd = crate::export_bench::probe(&ff, clip)?;
    let mut p = Project::new("live3");
    let seq = p.sequence_mut();
    seq.width = 1920;
    seq.height = 1080;
    seq.frame_rate = FrameRate::FPS_30;
    let total = Time::from_secs(60);
    seq.tracks[0].clips.push(Clip::new(hd.id, "base", TimeRange::new(Time::ZERO, total), Time::ZERO));
    seq.tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    for i in 0..3 {
        let at = Time::from_secs(i * 20);
        let mut pip = Clip::new(uhd.id, format!("pip{i}"), TimeRange::new(Time::ZERO, Time::from_secs(20)), at);
        pip.transform.scale = 0.3;
        pip.transform.rotation_deg = 5.0;
        pip.transform.x = 560.0;
        pip.transform.y = -290.0;
        seq.tracks[1].clips.push(pip);
    }
    seq.tracks.insert(2, Track::new(TrackKind::Video, "V3"));
    let mut mark = Clip::new(logo.id, "logo", TimeRange::new(Time::ZERO, total), Time::ZERO);
    mark.transform.scale = 0.5;
    mark.transform.x = -700.0;
    mark.transform.y = 380.0;
    seq.tracks[2].clips.push(mark);
    p.assets.extend([hd, uhd, logo]);
    let path = data.join("live3.kadr");
    kadr_project::io::save(&mut p, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn drive(clip: &Path, data: &Path, seconds: u64, opts: &Options) -> Result<Report, String> {
    // The isolated data dir carries the preview quality (the editor reads settings.json at start).
    std::fs::write(data.join("settings.json"), format!("{{\"preview_quality\": {}}}", opts.quality)).map_err(|e| e.to_string())?;
    let case = opts.case();
    let project3 = if opts.layers == 3 { Some(three_layer_file(clip, data)?) } else { None };
    let mut m = Mcp::start(data)?;
    m.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "kadr-bench", "version": "1"}}))?;
    if let Some(p) = &project3 {
        m.call("project", json!({"action": "open", "path": p}))?;
    } else {
        m.call("import_media", json!({"paths": [clip]}))?;
        m.call("wait_idle", json!({"timeout_ms": 120_000}))?;
        let state = m.call("get_state", json!({}))?;
        let asset = state["project"]["assets"][0]["id"].as_str().ok_or("import produced no asset")?.to_string();
        m.call("place_media", json!({"id": asset, "at_ms": 0}))?;
    }
    m.call("wait_idle", json!({"timeout_ms": 120_000}))?;
    let duration_ms = m.call("get_state", json!({}))?["timeline"]["duration_ms"].as_i64().ok_or("empty timeline")?;

    let mut r = Report::new("m0-live");
    m.call("get_perf", json!({"reset": true}))?;
    m.call("playback", json!({"action": "play"}))?;
    std::thread::sleep(Duration::from_secs(seconds));
    m.call("playback", json!({"action": "pause"}))?;
    flatten("playback", &case, &m.call("get_perf", json!({"reset": true}))?, &mut r);

    for t in crate::baseline::seek_times((duration_ms / 1000) as u32, 10) {
        m.call("set_playhead", json!({"at_ms": t.as_millis()}))?;
        m.call("wait_idle", json!({"timeout_ms": 30_000}))?;
    }
    flatten("seek", &case, &m.call("get_perf", json!({"reset": true}))?, &mut r);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_results_parse_json_text_and_report_errors() {
        let ok = json!({"content": [{"type": "text", "text": "{\"idle\": true}"}]});
        assert_eq!(tool_result(&ok).unwrap(), json!({"idle": true}));
        let plain = json!({"content": [{"type": "text", "text": "done"}]});
        assert_eq!(tool_result(&plain).unwrap(), json!("done"));
        let err = json!({"isError": true, "content": [{"type": "text", "text": "not_found: x"}]});
        assert_eq!(tool_result(&err).unwrap_err(), "not_found: x");
    }

    #[test]
    fn nested_numbers_become_rows() {
        let mut r = Report::new("t");
        flatten("playback", "live", &json!({"frames": 10, "total_ms": {"p50": 12.5}, "note": "x"}), &mut r);
        let got: Vec<(String, f64)> = r.rows.iter().map(|x| (format!("{}:{}", x.scenario, x.metric), x.value)).collect();
        assert_eq!(got, vec![("playback:frames".to_string(), 10.0), ("playback:total_ms.p50".to_string(), 12.5)]);
    }

    #[test]
    fn options_name_their_case_and_quality_words_parse() {
        assert_eq!(Options { layers: 1, quality: 1 }.case(), "1 layer half");
        assert_eq!(Options { layers: 3, quality: 0 }.case(), "3 layers full");
        assert_eq!([parse_quality("full"), parse_quality("half"), parse_quality("quarter"), parse_quality("x")], [Some(0), Some(1), Some(2), None]);
    }
}
