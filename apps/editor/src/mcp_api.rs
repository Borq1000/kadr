//! Kadr's MCP tools, executed on the UI thread (see kadr-mcp-bridge).

use crate::app::App;
use crate::mcp_state;
use kadr_mcp_bridge::{BridgeError, Reply};
use serde_json::{json, Value};
use slint::ComponentHandle;

fn bad(msg: impl Into<String>) -> Reply {
    Reply::Now(Err(BridgeError::new("bad_params", msg)))
}

fn ms(p: &Value, key: &str) -> Option<kadr_core::Time> {
    p.get(key).and_then(Value::as_i64).map(kadr_core::Time::from_millis)
}

pub fn handle(app: &mut App, method: &str, p: Value) -> Reply {
    match method {
        "ping" => Reply::Now(Ok(json!({"pid": std::process::id()}))),
        "get_state" => Reply::Now(Ok(app.mcp_state())),
        "get_log" => {
            let n = p.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
            match log_level(&p) {
                Ok(level) => Reply::Now(Ok(json!(crate::logging::ring().tail(n, level)))),
                Err(e) => Reply::Now(Err(e)),
            }
        }
        "idle" => Reply::Now(Ok(json!({
            "idle": app.jobs.active_count() == 0 && !app.ui().get_preview_loading(),
            "jobs": app.jobs.active_count(),
        }))),
        "layout_text" => match (p.get("text").and_then(Value::as_str), p.get("layout").and_then(Value::as_str)) {
            (Some(t), Some(l @ ("ru" | "en"))) => Reply::Now(Ok(json!(mcp_state::layout_text(t, l)))),
            _ => bad("layout_text needs text and layout: ru|en"),
        },
        "click_at" | "drag_at" => {
            let ui = app.ui();
            let (size, scale) = (ui.window().size(), ui.window().scale_factor());
            let size = (size.width as f32 / scale, size.height as f32 / scale);
            let events = if method == "click_at" { crate::mcp_input::click_events(&p, size) } else { crate::mcp_input::drag_events(&p, size) };
            match events {
                Ok(events) => app.mcp_pointer(events),
                Err(e) => Reply::Now(Err(e)),
            }
        }
        "get_perf" => {
            let summary = if p.get("reset").and_then(Value::as_bool) == Some(true) {
                app.preview.perf.take_summary()
            } else {
                app.preview.perf.summary()
            };
            Reply::Now(Ok(crate::perf_view::summary_json(&summary)))
        }
        "get_frame" => match ms(&p, "at_ms") {
            Some(t) => {
                let dim = |k: &str| p.get(k).and_then(Value::as_u64).map(|v| v.clamp(2, 8192) as u32);
                app.mcp_frame(t, dim("max_w").unwrap_or(960), dim("max_h"))
            }
            None => bad("get_frame needs at_ms"),
        },
        _ => crate::mcp_api::actions(app, method, p),
    }
}

/// `get_log`'s `level`: the minimum level to return (default: everything).
fn log_level(p: &Value) -> Result<tracing::Level, BridgeError> {
    let bad = || BridgeError::new("bad_params", format!("`level` must be one of error|warn|info|debug|trace, got {}", p["level"]));
    match p.get("level") {
        None | Some(Value::Null) => Ok(tracing::Level::TRACE),
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "error" => Ok(tracing::Level::ERROR),
            "warn" => Ok(tracing::Level::WARN),
            "info" => Ok(tracing::Level::INFO),
            "debug" => Ok(tracing::Level::DEBUG),
            "trace" => Ok(tracing::Level::TRACE),
            _ => Err(bad()),
        },
        Some(_) => Err(bad()),
    }
}

use kadr_core::ClipId;
use kadr_project::{Project, Sequence, TrackKind};
use kadr_timeline::{commands, ClipProperty, EditCommand, InsertMode};

fn clip_ids(p: &Project, v: &Value) -> Result<Vec<ClipId>, BridgeError> {
    let arr = v.get("clips").and_then(Value::as_array).ok_or_else(|| BridgeError::new("bad_params", "`clips` must be an array of clip ids"))?;
    arr.iter()
        .map(|x| {
            let s = x.as_str().unwrap_or_default();
            ClipId::parse(s).filter(|id| p.sequence().clip(*id).is_some()).ok_or_else(|| BridgeError::new("not_found", format!("clip {s}")))
        })
        .collect()
}

/// Turns MCP `edit` ops (a mix of `AiCommand` JSON and the extra
/// link/unlink/delete_part ops) into engine `EditCommand`s. Pure and tested
/// without a window.
pub fn edit_commands(project: &Project, _selection: &[ClipId], ops: &Value) -> Result<Vec<EditCommand>, BridgeError> {
    let ops = ops.as_array().ok_or_else(|| BridgeError::new("bad_params", "`ops` must be an array"))?;
    let seq = project.sequence();
    let mut out = vec![];
    for (i, op) in ops.iter().enumerate() {
        let at = |e: BridgeError| BridgeError::new(e.code, format!("op #{i}: {}", e.message));
        out.extend(edit_op(project, seq, op).map_err(at)?);
    }
    Ok(out)
}

/// One `edit` op; `edit_commands` prefixes errors with its index.
fn edit_op(project: &Project, seq: &Sequence, op: &Value) -> Result<Vec<EditCommand>, BridgeError> {
    match op.get("type").and_then(Value::as_str) {
        Some("unlink") => {
            let ids = commands::with_links(seq, &clip_ids(project, op)?);
            Ok(ids.into_iter().map(|clip| EditCommand::SetClipProperty { clip, prop: ClipProperty::Link(None) }).collect())
        }
        Some("link") => commands::link_selection(seq, &clip_ids(project, op)?).ok_or_else(|| BridgeError::new("edit_rejected", "link needs at least two clips")),
        Some("delete_part") => {
            let kind = match op.get("part").and_then(Value::as_str) {
                Some("video") => TrackKind::Video,
                Some("audio") => TrackKind::Audio,
                _ => return Err(BridgeError::new("bad_params", "part must be video|audio")),
            };
            commands::delete_part(seq, &clip_ids(project, op)?, kind).ok_or_else(|| BridgeError::new("edit_rejected", "no clip of that kind in the selection"))
        }
        _ => {
            let cmd: kadr_ai::command::AiCommand =
                serde_json::from_value(op.clone()).map_err(|e| BridgeError::new("edit_rejected", format!("unknown or malformed op: {e}")))?;
            let perms = kadr_ai::command::Permissions { allow_destructive: true, max_commands: 500 };
            // Validated one at a time, so the validator's own "command 0:"
            // prefix is meaningless; the caller adds the real op index.
            kadr_ai::command::validate(&[cmd], project, &perms).map_err(|e| {
                let s = e.to_string();
                BridgeError::new("edit_rejected", s.strip_prefix("command 0: ").map(str::to_string).unwrap_or(s))
            })
        }
    }
}

/// Menu ids `App::menu` handles that never open a native (OS) dialog.
/// `new`/`open`/`save`/`save-as`/`import`/`export` are excluded even though
/// some of them are handled, because they can defer into `rfd::FileDialog`
/// (e.g. plain `"save"` with no current path opens the native "save as"
/// picker) — those go through the dedicated `project`/`import_media`/`export`
/// tools instead, which never touch a native dialog.
const UI_MENU_ALLOWED: &[&str] = &[
    "welcome",
    "settings",
    "settings-ai",
    "shortcuts",
    "undo",
    "redo",
    "split",
    "delete",
    "ripple-delete",
    "select-all",
    "deselect",
    "marker",
    "add-video-track",
    "add-audio-track",
    "toggle-ai",
    "toggle-jobs",
    "zoom-fit",
    "snap",
    "ripple",
    "safe",
    "fullscreen",
    "reset-layout",
    "lang-ru",
    "lang-en",
    "logs",
    "about",
];

/// Pure allow-list check for the `ui` tool's `menu` id. Rejects anything that
/// can open a native file dialog (project new/open/save/save-as, import,
/// export), anything that opens the in-app Confirm modal no MCP tool can
/// dismiss (`"exit"` -> `request_exit` -> `guard_unsaved`/`ask` when the
/// project is dirty), and anything `App::menu` doesn't actually handle.
fn ui_menu_allowed(id: &str) -> Result<(), BridgeError> {
    if UI_MENU_ALLOWED.contains(&id) {
        return Ok(());
    }
    match id {
        "new" | "open" | "save" | "save-as" | "import" | "export" => Err(BridgeError::new(
            "bad_params",
            format!("`{id}` opens a native dialog or isn't routed here: new/open/save/save_as -> use the `project` tool; import -> `import_media`; export -> `export`"),
        )),
        "exit" => Err(BridgeError::new(
            "bad_params",
            "`exit` can open an in-app confirmation dialog that no MCP tool can dismiss; not supported over MCP",
        )),
        _ => Err(BridgeError::new("bad_params", format!("unknown menu id `{id}`"))),
    }
}

/// As [`ui_menu_allowed`], plus: `fullscreen` would bring a headless
/// instance's off-screen window on screen, so it's rejected there.
fn ui_menu_allowed_in(id: &str, headless: bool) -> Result<(), BridgeError> {
    ui_menu_allowed(id)?;
    if headless && id == "fullscreen" {
        return Err(BridgeError::new("bad_params", "`fullscreen` is not available in a headless instance"));
    }
    if headless && id == "logs" {
        return Err(BridgeError::new("bad_params", "`logs` opens Explorer on the user's desktop; in a headless instance use `get_log`"));
    }
    Ok(())
}

/// `export`'s `preset` (0 High, 1 Balanced, 2 Draft) and `resolution`
/// (0 sequence, 1 1080p, 2 720p, 3 2160p) indices; both default to 0.
pub fn export_indices(p: &Value) -> Result<(i32, i32), BridgeError> {
    let index = |key: &str, max: i64, meaning: &str| -> Result<i32, BridgeError> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(0),
            Some(v) => v
                .as_i64()
                .filter(|i| (0..=max).contains(i))
                .map(|i| i as i32)
                .ok_or_else(|| BridgeError::new("bad_params", format!("`{key}` must be an integer 0..={max} ({meaning}), got {v}"))),
        }
    };
    Ok((index("preset", 2, "0 High, 1 Balanced, 2 Draft")?, index("resolution", 3, "0 sequence, 1 1080p, 2 720p, 3 2160p")?))
}

/// Removes later duplicates, keeping each id's first occurrence in place.
fn dedupe_keep_first(ids: &mut Vec<ClipId>) {
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
}

pub fn actions(app: &mut App, method: &str, p: Value) -> Reply {
    let ok = |v: Value| Reply::Now(Ok(v));
    match method {
        "edit" => {
            let label = format!("Claude: {}", p.get("label").and_then(Value::as_str).unwrap_or("edit"));
            match edit_commands(&app.project, &app.tl.selection, p.get("ops").unwrap_or(&Value::Null)) {
                Ok(commands) if commands.is_empty() => bad("no ops"),
                Ok(commands) => {
                    let applied = app.execute_as(EditCommand::Batch { label, commands }, kadr_project::EditSource::Ai, None);
                    let seq = app.project.sequence();
                    app.tl.selection.retain(|id| seq.clip(*id).is_some());
                    ok(json!({"applied": applied, "timeline": mcp_state::timeline_json(app.project.sequence(), &app.tl.selection, app.playhead)}))
                }
                Err(e) => Reply::Now(Err(e)),
            }
        }
        "undo" => {
            app.undo();
            ok(json!({}))
        }
        "redo" => {
            app.redo();
            ok(json!({}))
        }
        "select" => {
            let ids = if p.get("clips").is_some() {
                match clip_ids(&app.project, &p) {
                    Ok(v) => v,
                    Err(e) => return Reply::Now(Err(e)),
                }
            } else {
                vec![]
            };
            if p.get("add").and_then(Value::as_bool) == Some(true) {
                app.tl.selection.extend(ids)
            } else {
                app.tl.selection = ids
            }
            dedupe_keep_first(&mut app.tl.selection);
            app.refresh_timeline();
            app.refresh_inspector();
            app.refresh_status();
            ok(json!({"selection": app.tl.selection.iter().map(|c| c.to_string()).collect::<Vec<_>>()}))
        }
        "set_playhead" => match ms(&p, "at_ms") {
            Some(t) => {
                app.set_playhead(t);
                ok(json!({"playhead_ms": app.playhead.as_millis()}))
            }
            None => bad("at_ms"),
        },
        "playback" => {
            match p.get("action").and_then(Value::as_str) {
                Some("play") if !app.playing => app.toggle_playback(),
                Some("pause") if app.playing => app.toggle_playback(),
                Some("stop") => {
                    app.stop_playback();
                    app.transport("start")
                }
                Some("step") => app.step_frames(p.get("frames").and_then(Value::as_i64).unwrap_or(1)),
                Some(_) => {}
                None => return bad("action: play|pause|stop|step"),
            }
            ok(json!({"playing": app.playing, "playhead_ms": app.playhead.as_millis()}))
        }
        "import_media" => {
            let paths: Vec<std::path::PathBuf> =
                p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(Into::into)).collect()).unwrap_or_default();
            if paths.is_empty() {
                return bad("paths");
            }
            if let Some(missing) = paths.iter().find(|x| !x.exists()) {
                return Reply::Now(Err(BridgeError::new("not_found", missing.display().to_string())));
            }
            if let Some(dir) = paths.iter().find(|x| !x.is_file()) {
                return bad(format!("{} is a directory, not a file", dir.display()));
            }
            let before = app.toasts.seq();
            app.import_paths(paths);
            let messages = app.toasts.since(before);
            ok(json!({"note": "probing runs in the background; poll `idle`", "messages": messages}))
        }
        "place_media" => {
            let at = ms(&p, "at_ms").unwrap_or(app.playhead);
            let mode = match p.get("mode").and_then(Value::as_str) {
                Some("overwrite") => InsertMode::Overwrite,
                _ => InsertMode::Insert,
            };
            let id = p.get("id").and_then(Value::as_str).unwrap_or_default();
            let before_toasts = app.toasts.seq();
            let before_marker = app.engine.history_marker();
            if let Some(a) = kadr_core::AssetId::parse(id).filter(|a| app.project.asset(*a).is_some()) {
                app.place_asset(a, at, mode, None);
            } else if let Some(g) = kadr_core::MulticamId::parse(id).filter(|g| app.project.multicam_groups.iter().any(|x| x.id == *g)) {
                app.place_group(g, at, mode, None);
            } else {
                return Reply::Now(Err(BridgeError::new("not_found", format!("asset or group {id}"))));
            }
            let applied = app.engine.history_marker() != before_marker;
            let messages = app.toasts.since(before_toasts);
            ok(json!({"applied": applied, "messages": messages, "timeline": mcp_state::timeline_json(app.project.sequence(), &app.tl.selection, app.playhead)}))
        }
        "project" => {
            let before = app.toasts.seq();
            let reply = match (p.get("action").and_then(Value::as_str), p.get("path").and_then(Value::as_str)) {
                (Some("new"), _) => {
                    app.new_project();
                    ok(json!({}))
                }
                (Some("open"), Some(path)) => {
                    app.open_project(path.into());
                    ok(json!({}))
                }
                (Some("save"), _) if app.path.is_some() => {
                    let pth = app.path.clone().unwrap();
                    app.save_to(pth);
                    ok(json!({}))
                }
                (Some("save_as" | "save"), Some(path)) => {
                    app.save_to(path.into());
                    ok(json!({"path": path}))
                }
                _ => bad("action: new|open(path)|save|save_as(path)"),
            };
            let messages = app.toasts.since(before);
            match reply {
                Reply::Now(Ok(mut v)) => {
                    if let Value::Object(o) = &mut v {
                        o.insert("messages".into(), json!(messages));
                    }
                    Reply::Now(Ok(v))
                }
                other => other,
            }
        }
        "export" => {
            let Some(path) = p.get("path").and_then(Value::as_str) else { return bad("path") };
            let (preset, resolution) = match export_indices(&p) {
                Ok(x) => x,
                Err(e) => return Reply::Now(Err(e)),
            };
            app.export.path = Some(path.into());
            let before = app.toasts.seq();
            app.export_start(preset, resolution);
            let messages = app.toasts.since(before);
            let started = app.export.job.is_some();
            if started {
                ok(json!({"started": true, "messages": messages, "note": "poll export_status"}))
            } else {
                ok(json!({"started": false, "reason": messages}))
            }
        }
        "export_status" => ok(json!({"running": app.export.job.is_some() && !app.export.done && !app.export.failed, "done": app.export.done, "failed": app.export.failed, "status": app.export.status})),
        "ui" => match p.get("menu").and_then(Value::as_str) {
            Some(m) => match ui_menu_allowed_in(m, app.flags.headless) {
                Ok(()) => {
                    let before = app.toasts.seq();
                    app.menu(m);
                    let messages = app.toasts.since(before);
                    ok(json!({"messages": messages}))
                }
                Err(e) => Reply::Now(Err(e)),
            },
            None => bad("ui needs `menu`: a menu action id (undo, redo, split, delete, select-all, marker, settings, …)"),
        },
        "assistant" => match p.get("prompt").and_then(Value::as_str) {
            Some(text) => {
                app.ai_send(text);
                ok(json!({"transcript": app.ai.transcript()}))
            }
            None => bad("prompt"),
        },
        _ => Reply::Now(Err(BridgeError::new("unknown_method", method.to_string()))),
    }
}

impl App {
    pub fn mcp_state(&self) -> Value {
        let ui = self.ui();
        json!({
            "project": {"name": self.project.name, "path": self.path, "dirty": self.is_dirty(), "assets": self.project.assets.iter().map(|a| json!({"id": a.id.to_string(), "name": a.name, "kind": format!("{:?}", a.kind()), "duration_ms": a.duration().as_millis()})).collect::<Vec<_>>()},
            "timeline": mcp_state::timeline_json(self.project.sequence(), &self.tl.selection, self.playhead),
            "playing": self.playing,
            "open": {
                "settings": ui.get_settings_open(), "export": ui.get_export_open(), "confirm": ui.get_confirm_open(),
                "prompt": ui.get_prompt_open(), "multicam": ui.get_mc_open(), "welcome": ui.get_welcome_open(), "jobs": ui.get_jobs_open(),
            },
            "jobs": self.jobs.active_count(),
            "toasts": self.toasts.texts(),
            "assistant": self.ai.transcript(),
        })
    }

    /// Sends pointer events to the window one per ~10 ms, like a mouse, and
    /// replies once the last one is handled. They go out from a timer, after
    /// this MCP handler has returned: dispatched here, inside the `App`
    /// borrow, Kadr's own callbacks could not borrow `App` and would drop them.
    pub fn mcp_pointer(&self, events: Vec<slint::platform::WindowEvent>) -> Reply {
        type Queue = std::collections::VecDeque<slint::platform::WindowEvent>;
        /// Dispatches the next event; reports how many were delivered when
        /// the queue is empty or the window is gone.
        fn next(ui: slint::Weak<crate::AppWindow>, mut queue: Queue, sent: usize, done: std::sync::mpsc::Sender<usize>) {
            match (queue.pop_front(), ui.upgrade()) {
                (Some(ev), Some(w)) => {
                    w.window().dispatch_event(ev);
                    slint::Timer::single_shot(std::time::Duration::from_millis(10), move || next(ui, queue, sent + 1, done));
                }
                _ => {
                    let _ = done.send(sent);
                }
            }
        }
        let n = events.len();
        let (tx, rx) = std::sync::mpsc::channel();
        let ui = self.ui.clone();
        crate::app::defer(move || next(ui, events.into(), 0, tx));
        Reply::Later(Box::new(move || match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(sent) if sent == n => Ok(json!({"events": n})),
            Ok(sent) => Err(BridgeError::new("io", format!("the window closed after {sent} of {n} pointer events"))),
            Err(_) => Err(BridgeError::new("busy_timeout", format!("the {n} pointer events were not all delivered within 20 s (UI busy); the rest may still arrive — check `get_state` before retrying"))),
        }))
    }

    /// The preview frame at `t` as PNG (at most `max_w` wide, and `max_h`
    /// tall when given), decoded off the UI thread.
    pub fn mcp_frame(&self, t: kadr_core::Time, max_w: u32, max_h: Option<u32>) -> Reply {
        let Some(media) = self.media.clone() else { return Reply::Now(Err(BridgeError::new("io", "no FFmpeg"))) };
        let src = match mcp_state::frame_source(&self.project, t) {
            Ok(s) => s,
            Err(e) => return Reply::Now(Err(e)),
        };
        let (w, h) = mcp_state::frame_size(src.size, max_w, max_h);
        Reply::Later(Box::new(move || {
            let f = media.decode_frame(&src.path, src.source, w, h).map_err(|e| BridgeError::new("io", e.to_string()))?;
            let mut png = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut png, f.width, f.height);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().and_then(|mut w| w.write_image_data(&f.data)).map_err(|e| BridgeError::new("io", e.to_string()))?;
            }
            use base64_lite::encode;
            Ok(json!({"mime": "image/png", "data": encode(&png), "width": f.width, "height": f.height}))
        }))
    }
}

/// A tiny standard-alphabet base64 encoder (no padding-free variant, no
/// external crate — this is the only thing MCP frame replies need).
mod base64_lite {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
            out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHA[((n >> 6) & 0x3f) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHA[(n & 0x3f) as usize] as char } else { '=' });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::encode;

        #[test]
        fn encode_pads_per_rfc_4648_examples() {
            assert_eq!(encode(b"Man"), "TWFu");
            assert_eq!(encode(b"Ma"), "TWE=");
            assert_eq!(encode(b"M"), "TQ==");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{LinkId, MediaInfo, MediaKind, Time, TimeRange};
    use kadr_project::{Clip, MediaAsset, Project};
    use kadr_timeline::EditCommand;

    fn project_with_pair() -> (Project, kadr_core::ClipId, kadr_core::ClipId) {
        let mut p = Project::new("t");
        let a = MediaAsset::new("a.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(10), container: "mp4".into(), size_bytes: 0, video: None, audio: None, timecode: None });
        let link = LinkId::new();
        let mut v = Clip::new(a.id, "a", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::ZERO);
        v.link = Some(link);
        let mut au = v.clone();
        au.id = kadr_core::ClipId::new();
        let (vid, aid) = (v.id, au.id);
        p.sequence_mut().tracks[0].clips.push(v);
        p.sequence_mut().tracks[1].clips.push(au);
        p.assets.push(a);
        (p, vid, aid)
    }

    #[test]
    fn edit_accepts_ai_commands_and_extra_ops() {
        let (p, v, _) = project_with_pair();
        let ops = serde_json::json!([
            {"type": "split_clip", "at_ms": 4000, "reason": "mcp"},
            {"type": "unlink", "clips": [v.to_string()]},
        ]);
        let cmds = edit_commands(&p, &[], &ops).unwrap();
        assert!(matches!(cmds[0], EditCommand::Split { .. }));
        assert!(cmds[1..].iter().all(|c| matches!(c, EditCommand::SetClipProperty { .. })));
    }

    #[test]
    fn edit_rejects_unknown_clips_and_bad_ops_with_a_reason() {
        let (p, _, _) = project_with_pair();
        let e = edit_commands(&p, &[], &serde_json::json!([{"type": "unlink", "clips": ["nope"]}])).unwrap_err();
        assert_eq!(e.code, "not_found");
        let e = edit_commands(&p, &[], &serde_json::json!([{"type": "teleport"}])).unwrap_err();
        assert_eq!(e.code, "edit_rejected");
        let e = edit_commands(&p, &[], &serde_json::json!({"type": "split_clip"})).unwrap_err();
        assert_eq!(e.code, "bad_params", "ops must be an array");
    }

    #[test]
    fn edit_errors_name_the_real_op_index() {
        let (p, v, _) = project_with_pair();
        let ops = serde_json::json!([
            {"type": "split_clip", "at_ms": 4000},
            {"type": "unlink", "clips": [v.to_string()]},
            {"type": "split_clip", "at_ms": 999999},
        ]);
        let e = edit_commands(&p, &[], &ops).unwrap_err();
        assert_eq!(e.code, "edit_rejected");
        assert!(e.message.starts_with("op #2: "), "{}", e.message);
        assert!(!e.message.contains("command 0"), "the per-op validator index is meaningless here: {}", e.message);
        let e = edit_commands(&p, &[], &serde_json::json!([{"type": "split_clip", "at_ms": 1}, {"type": "teleport"}])).unwrap_err();
        assert!(e.message.starts_with("op #1: "), "{}", e.message);
        let e = edit_commands(&p, &[], &serde_json::json!([{"type": "split_clip", "at_ms": 1}, {"type": "unlink", "clips": ["nope"]}])).unwrap_err();
        assert_eq!(e.code, "not_found");
        assert!(e.message.starts_with("op #1: "), "{}", e.message);
    }

    #[test]
    fn export_indices_are_validated() {
        assert_eq!(export_indices(&serde_json::json!({})).unwrap(), (0, 0));
        assert_eq!(export_indices(&serde_json::json!({"preset": 2, "resolution": 3})).unwrap(), (2, 3));
        for bad in [
            serde_json::json!({"preset": 3}),
            serde_json::json!({"preset": -1}),
            serde_json::json!({"preset": "1"}),
            serde_json::json!({"preset": 1.5}),
            serde_json::json!({"resolution": 4}),
            serde_json::json!({"resolution": "1080p"}),
        ] {
            assert_eq!(export_indices(&bad).unwrap_err().code, "bad_params", "{bad}");
        }
    }

    #[test]
    fn log_level_is_validated_not_guessed() {
        assert_eq!(log_level(&serde_json::json!({})).unwrap(), tracing::Level::TRACE);
        assert_eq!(log_level(&serde_json::json!({"level": "warn"})).unwrap(), tracing::Level::WARN);
        assert_eq!(log_level(&serde_json::json!({"level": "ERROR"})).unwrap(), tracing::Level::ERROR);
        for bad in [serde_json::json!({"level": "warning"}), serde_json::json!({"level": "3"}), serde_json::json!({"level": 2})] {
            let e = log_level(&bad).unwrap_err();
            assert_eq!(e.code, "bad_params", "{bad}");
            assert!(e.message.contains("error|warn|info|debug|trace"), "{}", e.message);
        }
    }

    #[test]
    fn logs_menu_is_rejected_only_when_headless() {
        assert_eq!(ui_menu_allowed_in("logs", true).unwrap_err().code, "bad_params");
        assert!(ui_menu_allowed_in("logs", false).is_ok());
    }

    #[test]
    fn delete_part_audio_keeps_the_video() {
        let (p, v, _) = project_with_pair();
        let cmds = edit_commands(&p, &[], &serde_json::json!([{"type": "delete_part", "clips": [v.to_string()], "part": "audio"}])).unwrap();
        assert!(cmds.iter().any(|c| matches!(c, EditCommand::DeleteClips { .. })));
    }

    #[test]
    fn ui_menu_allowed_lets_dialog_free_ids_through() {
        for id in ["undo", "redo", "split", "delete", "select-all", "marker", "settings"] {
            assert!(ui_menu_allowed(id).is_ok(), "{id} should be allowed");
        }
    }

    #[test]
    fn ui_menu_allowed_rejects_dialog_opening_and_unknown_ids() {
        for id in ["save", "save-as", "open", "import", "export", "new", "exit", "totally-unknown"] {
            let e = ui_menu_allowed(id).unwrap_err();
            assert_eq!(e.code, "bad_params", "{id} should be rejected as bad_params");
        }
    }

    #[test]
    fn fullscreen_is_rejected_only_when_headless() {
        let e = ui_menu_allowed_in("fullscreen", true).unwrap_err();
        assert_eq!(e.code, "bad_params");
        assert!(ui_menu_allowed_in("fullscreen", false).is_ok());
    }

    #[test]
    fn other_ids_are_unaffected_by_headless() {
        for id in ["undo", "redo", "settings"] {
            assert!(ui_menu_allowed_in(id, true).is_ok(), "{id} should still be allowed headless");
            assert!(ui_menu_allowed_in(id, false).is_ok());
        }
        for id in ["save", "exit", "totally-unknown"] {
            assert_eq!(ui_menu_allowed_in(id, true).unwrap_err().code, "bad_params");
            assert_eq!(ui_menu_allowed_in(id, false).unwrap_err().code, "bad_params");
        }
    }
}
