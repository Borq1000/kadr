//! MCP for effects (effects spec §7): `list_effects` and the `edit` op
//! `move_effect`. `add_effect` / `set_effect` / `remove_effect` are AI
//! commands (validated in kadr-ai), reached through `edit`'s fallback.
//! Pure: tested without a window.

use kadr_core::{ClipId, EffectId};
use kadr_i18n::{t_in, Lang};
use kadr_mcp_bridge::BridgeError;
use kadr_project::{Project, Sequence};
use kadr_timeline::EditCommand;
use serde_json::{json, Value};

fn bad(msg: impl Into<String>) -> BridgeError {
    BridgeError::new("edit_rejected", msg.into())
}

/// `list_effects`: the registry, each recipe named in both languages; a
/// recipe with variants (the Look's presets) lists them with their names.
pub fn list_effects() -> Value {
    let names = |key: &str| json!({"ru": t_in(Lang::Ru, key), "en": t_in(Lang::En, key)});
    let all: Vec<Value> = kadr_effects::registry()
        .iter()
        .map(|d| {
            let mut v = kadr_effects::json::recipe_json(d);
            v["name"] = names(&d.name_key());
            if let Some(key) = d.variants
                && !d.variant_options().is_empty()
            {
                v["variants"] = Value::Array(d.variant_options().iter().map(|o| json!({"key": o, "name": names(&d.option_name_key(key, o))})).collect());
            }
            v
        })
        .collect();
    json!({ "effects": all })
}

/// `move_effect {clip_id, effect_id, to}`; `None` for every other `type`.
/// An effect from a newer Kadr may be moved (spec §2: kept, moved, removed).
pub fn fx_op(_project: &Project, seq: &Sequence, op: &Value) -> Option<Result<Vec<EditCommand>, BridgeError>> {
    if op.get("type").and_then(Value::as_str)? != "move_effect" {
        return None;
    }
    Some((|| {
        if let Some(extra) = op.as_object().into_iter().flat_map(|o| o.keys()).find(|k| !matches!(k.as_str(), "type" | "reason" | "clip_id" | "effect_id" | "to")) {
            return Err(bad(format!("unknown field `{extra}` for move_effect")));
        }
        let clip = op.get("clip_id").and_then(Value::as_str).and_then(ClipId::parse).ok_or_else(|| bad("clip_id missing or malformed"))?;
        let c = seq.clip(clip).ok_or_else(|| bad(format!("unknown clip {clip}")))?;
        if let Some(t) = seq.locate_clip(clip).map(|(ti, _)| &seq.tracks[ti]).filter(|t| t.locked) {
            return Err(bad(format!("track {} is locked", t.name)));
        }
        let id = op.get("effect_id").and_then(Value::as_str).and_then(EffectId::parse).ok_or_else(|| bad("effect_id missing or malformed"))?;
        let from = c.effects.iter().position(|e| e.id == id).ok_or_else(|| bad(format!("the clip has no effect {id}")))?;
        let to = op
            .get("to")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("`to` must be a non-negative integer: the position in the clip's effect list, 0 = applied first"))?;
        let last = c.effects.len() - 1;
        if to > last as u64 {
            return Err(bad(format!("`to` {to} is past the last position ({last})")));
        }
        let to = to as usize;
        if to == from {
            return Err(bad("no change: the effect is already there"));
        }
        Ok(vec![EditCommand::MoveEffect { clip, id, to }])
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, Time, TimeRange};
    use kadr_project::{AppliedEffect, Clip};
    use serde_json::json;

    fn project() -> (Project, String, Vec<String>) {
        let mut p = Project::new("t");
        let mut c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::ZERO);
        let ids: Vec<String> = ["smooth_zoom", "handheld"]
            .iter()
            .map(|r| {
                let e = AppliedEffect::new(*r, 1, Default::default());
                let id = e.id.to_string();
                c.effects.push(e);
                id
            })
            .collect();
        let cid = c.id.to_string();
        p.sequence_mut().tracks[0].clips.push(c);
        (p, cid, ids)
    }

    fn op(p: &Project, v: Value) -> Result<Vec<EditCommand>, BridgeError> {
        fx_op(p, p.sequence(), &v).expect("owned op")
    }

    #[test]
    fn move_effect_names_its_reasons() {
        let (p, c, ids) = project();
        let ok = op(&p, json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": 1})).unwrap();
        assert!(matches!(ok[..], [EditCommand::MoveEffect { to: 1, .. }]));
        let err = |v: Value| op(&p, v).unwrap_err().message;
        assert!(err(json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": 0})).contains("no change"));
        assert!(err(json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": 2})).contains("past the last position (1)"));
        assert!(err(json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": -1})).contains("non-negative"));
        assert!(err(json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": 1, "speed": 2})).contains("unknown field `speed`"));
        assert!(err(json!({"type": "move_effect", "clip_id": c, "effect_id": kadr_core::EffectId::new().to_string(), "to": 1})).contains("no effect"));
        assert!(fx_op(&p, p.sequence(), &json!({"type": "split_clip"})).is_none(), "not ours");
    }

    #[test]
    fn move_effect_rejects_malformed_ids_a_missing_position_and_locked_tracks() {
        let (mut p, c, ids) = project();
        let err = |p: &Project, v: Value| op(p, v).unwrap_err().message;
        assert!(err(&p, json!({"type": "move_effect", "clip_id": "nope", "effect_id": ids[0], "to": 1})).contains("clip_id"));
        assert!(err(&p, json!({"type": "move_effect", "clip_id": kadr_core::ClipId::new().to_string(), "effect_id": ids[0], "to": 1})).contains("unknown clip"));
        assert!(err(&p, json!({"type": "move_effect", "clip_id": c, "effect_id": "nope", "to": 1})).contains("effect_id"));
        assert!(err(&p, json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0]})).contains("`to`"));
        assert!(err(&p, json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": 1.0})).contains("integer"));
        p.sequence_mut().tracks[0].locked = true;
        assert!(err(&p, json!({"type": "move_effect", "clip_id": c, "effect_id": ids[0], "to": 1})).contains("locked"));
    }

    /// Spec §2: an effect from a newer Kadr cannot be changed, but it can be moved.
    #[test]
    fn an_effect_from_a_newer_kadr_can_be_moved() {
        let (mut p, c, _) = project();
        let future = AppliedEffect::new("glitch_v2", 7, Default::default());
        let fid = future.id.to_string();
        p.sequence_mut().tracks[0].clips[0].effects.push(future);
        let ok = op(&p, json!({"type": "move_effect", "clip_id": c, "effect_id": fid, "to": 0})).unwrap();
        assert!(matches!(ok[..], [EditCommand::MoveEffect { to: 0, .. }]));
    }

    #[test]
    fn list_effects_names_each_recipe_in_both_languages() {
        let v = list_effects();
        let all = v["effects"].as_array().unwrap();
        assert_eq!(all.len(), kadr_effects::registry().len());
        assert_eq!(all[0]["name"], json!({"ru": "Плавный наезд", "en": "Smooth Zoom"}));
        assert_eq!(all[0]["params"][1]["key"], "strength");
        for e in all {
            let n = &e["name"];
            assert!(n["ru"].as_str().is_some_and(|s| !s.starts_with("fx.")) && n["en"].as_str().is_some_and(|s| !s.starts_with("fx.")), "{e}");
        }
    }

    /// Refinement 13: the Look's presets with their names, for MCP clients.
    #[test]
    fn list_effects_names_the_look_presets() {
        let v = list_effects();
        let all = v["effects"].as_array().unwrap();
        let look = all.iter().find(|e| e["id"] == "look").unwrap();
        assert_eq!(look["category"], "look");
        assert_eq!(look["variants"].as_array().unwrap().len(), kadr_effects::look::preset_keys().len());
        assert_eq!(look["variants"][0], json!({"key": "cinematic", "name": {"ru": "Кино", "en": "Cinematic"}}));
        assert_eq!(look["variants"][2], json!({"key": "soft_film", "name": {"ru": "Мягкая плёнка", "en": "Soft Film"}}));
        let vignette = all.iter().find(|e| e["id"] == "vignette").unwrap();
        assert_eq!((vignette["category"].as_str(), vignette["name"]["ru"].as_str()), (Some("light"), Some("Виньетка")));
        assert!(vignette.get("variants").is_none(), "one card, no variants");
    }
}
