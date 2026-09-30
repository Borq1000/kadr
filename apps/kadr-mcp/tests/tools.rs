use kadr_mcp::tools::kadr_tools;
use serde_json::{json, Value};

fn tool(name: &str) -> Value {
    kadr_tools().into_iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no tool {name}"))
}

fn prop(name: &str, p: &str) -> Value {
    tool(name)["inputSchema"]["properties"][p].clone()
}

#[test]
fn export_preset_and_resolution_are_integer_indices() {
    let preset = prop("export", "preset");
    assert_eq!(preset["type"], "integer");
    assert_eq!(preset["enum"], json!([0, 1, 2]));
    let res = prop("export", "resolution");
    assert_eq!(res["type"], "integer");
    assert_eq!(res["enum"], json!([0, 1, 2, 3]));
    let d = res["description"].as_str().unwrap();
    for word in ["sequence", "1080", "720", "2160"] {
        assert!(d.contains(word), "resolution description lacks {word}: {d}");
    }
    let d = preset["description"].as_str().unwrap();
    for word in ["High", "Balanced", "Draft"] {
        assert!(d.contains(word), "preset description lacks {word}: {d}");
    }
}

#[test]
fn closed_string_arguments_declare_their_enums() {
    assert_eq!(prop("playback", "action")["enum"], json!(["play", "pause", "stop", "step"]));
    assert_eq!(prop("place_media", "mode")["enum"], json!(["insert", "overwrite"]));
    assert_eq!(prop("layout_text", "layout")["enum"], json!(["ru", "en"]));
    assert_eq!(prop("project", "action")["enum"], json!(["new", "open", "save", "save_as"]));
}

#[test]
fn edit_description_documents_every_op() {
    let d = tool("edit")["description"].as_str().unwrap().to_string() + prop("edit", "ops")["description"].as_str().unwrap_or("");
    for op in [
        "split_clip", "delete_range", "delete_clip", "trim_clip", "move_clip", "set_transform", "change_speed", "set_audio_gain", "add_marker",
        "add_transition", "select_camera", "add_caption", "unlink", "link", "delete_part",
    ] {
        assert!(d.contains(&format!("\"{op}\"")), "edit docs lack op {op}");
    }
    for field in ["at_ms", "clip_id", "start_ms", "end_ms", "edge", "to_ms", "speed", "gain_db", "rotation_deg", "cross_dissolve", "angle", "part"] {
        assert!(d.contains(field), "edit docs lack field {field}");
    }
}

#[test]
fn wait_idle_and_layout_text_say_what_they_really_do() {
    let d = tool("wait_idle")["description"].as_str().unwrap().to_string();
    assert!(d.contains("background jobs finished and the preview frame loaded"), "{d}");
    let d = tool("layout_text")["description"].as_str().unwrap().to_string();
    assert!(!d.starts_with("Type"), "layout_text does not type anything: {d}");
    assert!(d.contains("other layout"), "{d}");
}

#[test]
fn pointer_tools_take_logical_window_coordinates() {
    let t = tool("click_at");
    assert_eq!(t["inputSchema"]["required"], json!(["x", "y"]));
    assert_eq!(prop("click_at", "button")["enum"], json!(["left", "right", "middle"]));
    assert_eq!(prop("click_at", "double")["type"], "boolean");
    let t = tool("drag_at");
    assert_eq!(t["inputSchema"]["required"], json!(["from", "to"]));
    assert_eq!(prop("drag_at", "from")["required"], json!(["x", "y"]));
    let d = tool("click_at")["description"].as_str().unwrap().to_string();
    assert!(d.contains("logical"), "{d}");
}

#[test]
fn get_perf_reports_and_can_reset() {
    let t = tool("get_perf");
    assert_eq!(prop("get_perf", "reset")["type"], "boolean");
    let d = t["description"].as_str().unwrap();
    for word in ["dropped", "seek", "reset"] {
        assert!(d.contains(word), "get_perf description lacks {word}: {d}");
    }
}
