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

const EDIT_DESCRIPTION: &str = "Apply an undoable batch of timeline edit ops (undo label \"Claude: <label>\"). Times are timeline milliseconds; clip ids come from `get_state`. `reason` (string) is optional on every op except add_caption. Ops by `type`:
- \"split_clip\": at_ms, clip_id? (default: every clip under at_ms)
- \"delete_range\": start_ms, end_ms, ripple? (default true), sequence_id?
- \"delete_clip\": clip_id, ripple? (default false)
- \"trim_clip\": clip_id, edge (\"start\"|\"end\"), to_ms, ripple? (default false)
- \"move_clip\": clip_id, to_ms
- \"set_transform\": clip_id, scale?, x?, y?, rotation_deg?, opacity? (numbers, static values; a given property that has keyframes is rejected as \"<param> is animated\": use set_keyframe or clear_animation for it)
- \"change_speed\": clip_id, speed (number, 1.0 = normal)
- \"set_audio_gain\": clip_id, gain_db (number)
- \"add_marker\": at_ms, name
- \"add_transition\": at_ms, kind (\"cross_dissolve\"|\"dip_to_black\"|\"wipe\"), duration_ms (at_ms snaps to the cut within half a frame of it, if exactly one; else stays as given)
- \"select_camera\": start_ms, end_ms, angle (multicam angle label, e.g. \"CAM2\")
- \"add_caption\": start_ms, end_ms, text (not supported yet: rejected)
- \"unlink\": clips (array of clip ids; their linked partners are unlinked too)
- \"link\": clips (array of at least two clip ids)
- \"delete_part\": clips (array of clip ids), part (\"video\"|\"audio\"): deletes only that half of linked clips
- \"set_keyframe\": clip_id, param, value, at_ms | source_ms, ease? (video clips on unlocked video tracks only; a keyframe of that param within half a frame of the time is updated instead of adding a near-duplicate; repeating the same value and ease is rejected as no change). Params and value ranges (effect numbers: param \"fx:<effect-id>:<key>\", the range from `list_effects`, e.g. strength 0..1; a number of an effect from a newer Kadr cannot be animated): x, y (sequence pixels, +-20000); scale (multiplier 0.01..16, 1 = 100%); rotation (degrees, +-3600); opacity 0..1; crop_left crop_right crop_top crop_bottom (fraction of the frame 0..1); exposure (EV, -2..2); contrast, saturation (multiplier 0..2, 1 = unchanged); temperature (-1..1). ease: ease_in_out (default for new animation) linear ease_in ease_out hold. at_ms is timeline time inside the clip, snapped to a frame when a key is created; source_ms addresses source time, e.g. hidden keyframes
- \"remove_keyframe\": clip_id, param, at_ms | source_ms (finds the keyframe within half a frame; removing a property's last keyframe keeps its value as the static value; an effect number's static is kept in its range)
- \"set_keyframe_ease\": clip_id, param, at_ms | source_ms, ease (the same ease is rejected as no change)
- \"move_keyframes\": clip_id, from_ms | from_source_ms, to_ms (timeline time inside the clip; moving onto the same moment or onto another keyframe of the property is rejected), param? (all properties and effect numbers at that moment when omitted or null; keyframes of an effect from a newer Kadr stay where they are)
- \"clear_animation\": clip_id, params? (default: every animated property and effect number; static properties are never touched, a clip or list without animation is rejected), keep_ms? (static values = the picture at that time, an effect number's static is kept in its range; default: property and effect defaults)
- \"set_video_fade\": clip_id, fade_in_ms?, fade_out_ms? (picture fades from the clip's edges on a video clip, not an audio one; frame-snapped, so a tiny value becomes 0; negative rejected; fade_in + fade_out <= clip length; an unchanged result is rejected as no change; set both in one op)
- \"add_effect\": clip_id, recipe (an id from `list_effects`), params? (object: numbers; choice strings; point {\"x\": 0..1, \"y\": 0..1} as fractions of the clip's picture; moment {\"at_ms\": timeline ms inside the clip} or {\"source_ms\": int}, the frame the effect lands on (Punch In is fully in at this frame, its 0.12 s snap runs just before it); omitted ones get defaults: strength 0.6, the picture's centre, the clip's middle). One effect per recipe per clip: adding a recipe the clip has replaces its parameters (same effect id, its keyframes dropped); repeating it unchanged is rejected as no change. Motion effects go on video clips only
- \"set_effect\": clip_id, effect_id, params?, enabled? (merges the given params; a param with keyframes is rejected as \"<key> is animated\": use set_keyframe / clear_animation with param \"fx:<effect-id>:<key>\"; an effect from a newer Kadr (known: false in get_state) cannot be changed; no change is rejected, and a moment echoed from get_state is no change)
- \"remove_effect\": clip_id, effect_id (its keyframes go with it)
- \"move_effect\": clip_id, effect_id, to (position in the clip's effect list, 0 = applied first; the same position is rejected as no change)
Millisecond arguments are integers within +-10000000000. Keyframe and fade ops are resolved against the project as it was before the call: a keyframe created by one op cannot be addressed by a later op of the same call (pass `ease` on set_keyframe instead), geometry edits (move, speed, trim) earlier in the same call are not seen by them, and two set_video_fade ops on one clip in one call both start from the old fades. `get_state` shows each clip's `video_fade`, `animation` (per property: at_ms, source_ms, value, ease, visible; effect numbers under \"fx:<effect-id>:<key>\") and `effects` (id, recipe, version, enabled, known, progress [from, to], params; a moment as {at_ms, source_ms, visible}).
Unknown fields are rejected.";

/// One JSON Schema tool entry per Kadr tool.
pub fn kadr_tools() -> Vec<Value> {
    vec![
        tool("get_state", "Get the current project/timeline/selection state.", json!({}), &[]),
        tool(
            "list_effects",
            "The effect library: each recipe's id, names (ru/en), category, target (`clip` = video clips) and parameters with type, range or options and default. A moment parameter (Punch In) is the frame the effect lands on: fully in at this frame, its 0.12 s snap runs just before it. Apply with the `edit` op add_effect; animate a number with set_keyframe and param \"fx:<effect-id>:<key>\".",
            json!({}),
            &[],
        ),
        tool(
            "get_log",
            "Read recent editor log lines.",
            json!({
                "lines": {"type": "integer", "description": "Max number of lines to return."},
                "level": {"type": "string", "enum": ["error", "warn", "info", "debug", "trace"], "description": "Minimum log level (default trace: everything)."},
            }),
            &[],
        ),
        tool(
            "get_frame",
            "Render the frame at a timeline position exactly as the preview shows it (all layers, transitions, crop/rotation/opacity/colour, bypass). A gap or audio-only time is a black frame (`layers: 0`); an empty timeline or a time past the end is an error.",
            json!({
                "at_ms": {"type": "integer", "description": "Timeline position in milliseconds."},
                "max_w": {"type": "integer", "description": "Max width in pixels (default 960); the frame keeps its aspect and is never upscaled."},
                "max_h": {"type": "integer", "description": "Optional max height in pixels."},
            }),
            &["at_ms"],
        ),
        tool(
            "get_perf",
            "Preview performance over the last frames (up to 600): percentiles (ms) of frame time, decode, composite and present; dropped frames (late or superseded) in the window, plus `total_frames`/`total_dropped`/`dropped_pct` since the last reset; seek latency from a playhead change to the frame shown; frame-sized allocations and copies per frame; `renderer` (always `cpu`). Present is the hand-off of the finished buffer to the UI thread (no copy) and a copy means the display still held a ring buffer. `reset: true` reads and clears the window and the totals at once, so the next read measures only what happens afterwards.",
            json!({"reset": {"type": "boolean", "description": "Clear the window and the totals after reading."}}),
            &[],
        ),
        tool(
            "layout_text",
            "Map text to the characters the same physical keys produce on the other layout (ЙЦУКЕН ↔ QWERTY). Returns the mapped string; it does not type anything — send the result with `ui_dispatch_key_event` to test layout-independent shortcuts.",
            json!({
                "text": {"type": "string", "description": "Text as typed on the source layout."},
                "layout": {"type": "string", "enum": ["ru", "en"], "description": "Target layout: `ru` maps QWERTY keys to Russian characters, `en` maps Russian characters to QWERTY."},
            }),
            &["text", "layout"],
        ),
        tool(
            "wait_idle",
            "Wait until background jobs finished and the preview frame loaded (polls Kadr until both hold twice in a row). Call after edits, imports or input before reading state or a frame.",
            json!({
                "timeout_ms": {"type": "integer", "description": "Max time to wait; defaults to 20000."},
            }),
            &[],
        ),
        tool(
            "edit",
            EDIT_DESCRIPTION,
            json!({
                "ops": {"type": "array", "items": {"type": "object"}, "description": "Edit operations, each an object with a `type` (see the tool description). Applied as one undoable batch; any invalid op rejects the whole batch with `edit_rejected: op #<index>: …`."},
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
                "action": {"type": "string", "enum": ["play", "pause", "stop", "step"], "description": "`stop` also returns the playhead to the start; `step` moves by `frames`."},
                "frames": {"type": "integer", "description": "Frames to step for `step` (negative = backwards); default 1."},
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
                "mode": {"type": "string", "enum": ["insert", "overwrite"], "description": "`insert` (default) pushes later clips right; `overwrite` replaces what is under the new clip."},
            }),
            &["id"],
        ),
        tool(
            "project",
            "New/open/save the project. Use this instead of the `ui` tool for anything that would open an OS file dialog.",
            json!({
                "action": {"type": "string", "enum": ["new", "open", "save", "save_as"], "description": "`open` and `save_as` need `path`; `save` needs `path` when the project was never saved."},
                "path": {"type": "string", "description": "Project file path, for open/save_as."},
            }),
            &["action"],
        ),
        tool(
            "export",
            "Start exporting the project to a file. Use `export_status` to poll progress.",
            json!({
                "path": {"type": "string", "description": "Output file path."},
                "preset": {"type": "integer", "enum": [0, 1, 2], "description": "Quality: 0 = High (CRF 18, default), 1 = Balanced (CRF 21), 2 = Draft (CRF 26, fast)."},
                "resolution": {"type": "integer", "enum": [0, 1, 2, 3], "description": "0 = same as sequence (default), 1 = 1080p, 2 = 720p, 3 = 2160p (4K); scaled to fit, keeping the aspect ratio."},
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
        tool(
            "click_at",
            "Click at a point of the Kadr window, in logical window pixels (the coordinates of `ui_take_screenshot` divided by the window's scale factor, and of element geometry in `ui_get_element_tree`). Real pointer events through Slint's routing, like a mouse; unlike `ui_click_element` it needs no element handle. For modifier clicks, hold the key with `ui_dispatch_key_event` (Press … Release) around it.",
            json!({
                "x": {"type": "number"},
                "y": {"type": "number"},
                "button": {"type": "string", "enum": ["left", "right", "middle"], "description": "Default left."},
                "double": {"type": "boolean", "description": "Double-click."},
            }),
            &["x", "y"],
        ),
        tool(
            "drag_at",
            "Press at `from`, move to `to` in steps (~10 ms apart), release: a real pointer drag in logical window pixels (see `click_at`).",
            json!({
                "from": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]},
                "to": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]},
                "button": {"type": "string", "enum": ["left", "right", "middle"], "description": "Default left."},
                "steps": {"type": "integer", "description": "Intermediate moves (default 12, max 200)."},
            }),
            &["from", "to"],
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
