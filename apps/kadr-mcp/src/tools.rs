//! Kadr's own MCP tool definitions (§4.3-4.4 of the spec). Slint's UI tools
//! are discovered at runtime and prefixed `ui_` by `main.rs`.

use serde_json::{json, Value};

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

/// One JSON Schema tool entry per Kadr tool.
pub fn kadr_tools() -> Vec<Value> {
    vec![
        tool("get_state", "Get the current project/timeline/selection state.", json!({}), &[]),
        tool(
            "get_log",
            "Read recent editor log lines.",
            json!({
                "lines": {"type": "integer", "description": "Max number of lines to return."},
                "level": {"type": "string", "description": "Minimum log level."},
            }),
            &[],
        ),
        tool(
            "get_frame",
            "Render a preview frame at a given timeline position.",
            json!({
                "at_ms": {"type": "integer", "description": "Timeline position in milliseconds."},
                "max_w": {"type": "integer", "description": "Max width in pixels; scaled down if larger."},
            }),
            &["at_ms"],
        ),
        tool(
            "layout_text",
            "Type text respecting a specific keyboard layout, for layout-dependent shortcuts.",
            json!({
                "text": {"type": "string"},
                "layout": {"type": "string", "description": "Keyboard layout identifier."},
            }),
            &["text", "layout"],
        ),
        tool(
            "wait_idle",
            "Wait until Kadr's UI has settled (no pending renders/animations) after input.",
            json!({
                "timeout_ms": {"type": "integer", "description": "Max time to wait; defaults to 20000."},
            }),
            &[],
        ),
        tool(
            "edit",
            "Apply an undoable batch of timeline edit operations.",
            json!({
                "ops": {"type": "array", "description": "Edit operations to apply."},
                "label": {"type": "string", "description": "Undo history label."},
            }),
            &["ops"],
        ),
        tool("undo", "Undo the last edit.", json!({}), &[]),
        tool("redo", "Redo the last undone edit.", json!({}), &[]),
        tool(
            "select",
            "Select clips on the timeline.",
            json!({
                "clips": {"type": "array", "description": "Clip ids to select."},
                "add": {"type": "boolean", "description": "Add to the current selection instead of replacing it."},
            }),
            &[],
        ),
        tool(
            "set_playhead",
            "Move the playhead to a timeline position.",
            json!({"at_ms": {"type": "integer", "description": "Timeline position in milliseconds."}}),
            &["at_ms"],
        ),
        tool(
            "playback",
            "Control playback (play/pause/step).",
            json!({
                "action": {"type": "string", "description": "One of the supported playback actions."},
                "frames": {"type": "integer", "description": "Number of frames to step, if applicable."},
            }),
            &["action"],
        ),
        tool(
            "import_media",
            "Import media files into the project.",
            json!({"paths": {"type": "array", "items": {"type": "string"}, "description": "Absolute file paths to import."}}),
            &["paths"],
        ),
        tool(
            "place_media",
            "Place an imported media item onto the timeline.",
            json!({
                "id": {"type": "string", "description": "Media id to place."},
                "at_ms": {"type": "integer", "description": "Timeline position in milliseconds."},
                "mode": {"type": "string", "description": "Placement mode."},
            }),
            &["id"],
        ),
        tool(
            "project",
            "New/open/save the project. Use this instead of the `ui` tool for anything that would open an OS file dialog.",
            json!({
                "action": {"type": "string", "description": "One of: new, open, save, save_as."},
                "path": {"type": "string", "description": "Project file path, for open/save_as."},
            }),
            &["action"],
        ),
        tool(
            "export",
            "Start exporting the project to a file. Use `export_status` to poll progress.",
            json!({
                "path": {"type": "string", "description": "Output file path."},
                "preset": {"type": "string", "description": "Export preset name."},
                "resolution": {"type": "string", "description": "Output resolution."},
            }),
            &["path"],
        ),
        tool("export_status", "Poll the status of an in-progress export.", json!({}), &[]),
        tool(
            "ui",
            "Trigger a menu command by id. Dialog-opening ids (new, open, save, save_as, import, export, exit) are rejected — use `project`, `import_media` or `export` instead.",
            json!({"menu": {"type": "string", "description": "Menu command id."}}),
            &["menu"],
        ),
        tool(
            "assistant",
            "Ask Kadr's built-in AI assistant to perform an editing task in natural language.",
            json!({"prompt": {"type": "string"}}),
            &["prompt"],
        ),
        tool("list_instances", "List all discoverable Kadr instances and whether they are alive.", json!({}), &[]),
        tool(
            "use_instance",
            "Switch which Kadr instance kadr-mcp is connected to.",
            json!({"pid": {"type": "integer", "description": "Process id of the instance to use."}}),
            &["pid"],
        ),
    ]
}
