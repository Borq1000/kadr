//! JSON forms of effects for MCP and the AI assistant (effects spec §7):
//! the registry, an effect as `get_state` shows it, and parameters as
//! clients write them — numbers, choice strings, `{x, y}` points and
//! `{at_ms}` / `{source_ms}` moments (`at_ms` is timeline time inside the clip).

use crate::params::{ParamDef, ParamKind};
use crate::registry::{definition, registry, RecipeDef};
use kadr_core::Time;
use kadr_project::{AppliedEffect, Clip, ParamValue, SourceTime};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Largest |milliseconds| accepted, as MCP's other ops.
const MAX_MS: u64 = 10_000_000_000;

pub fn param_schema(d: &ParamDef) -> Value {
    match d.kind {
        ParamKind::Number { min, max, default } => json!({"key": d.key, "type": "number", "min": min, "max": max, "default": default, "animatable": true}),
        ParamKind::Choice { options, default } => json!({"key": d.key, "type": "choice", "options": options, "default": default}),
        ParamKind::Point => json!({"key": d.key, "type": "point", "default": {"x": 0.5, "y": 0.5}}),
        ParamKind::Moment => json!({"key": d.key, "type": "moment", "default": "the source frame where the effect is applied (MCP / AI: the clip's middle)"}),
    }
}

pub fn recipe_json(d: &RecipeDef) -> Value {
    json!({
        "id": d.id, "category": d.category.key(), "target": d.target.key(), "version": d.version,
        "params": d.params.iter().map(param_schema).collect::<Vec<_>>(),
    })
}

/// `list_effects`: every recipe in card order.
pub fn registry_json() -> Value {
    Value::Array(registry().iter().map(recipe_json).collect())
}

/// A stored value as clients read it; a moment in both time domains.
pub fn value_json(clip: &Clip, v: &ParamValue) -> Value {
    match v {
        ParamValue::Number(n) => json!(n),
        ParamValue::Choice(c) => json!(c),
        ParamValue::Point { x, y } => json!({"x": x, "y": y}),
        ParamValue::Color(c) => json!(c),
        ParamValue::Moment(m) => {
            // A clip showing no source time (degenerate file) maps nothing; never divide by it.
            let tl = if clip.source_out == clip.source_in { clip.timeline_in } else { clip.timeline_time_of(m.0) };
            json!({"at_ms": tl.as_millis(), "source_ms": m.0.as_millis(), "visible": tl >= clip.timeline_in && tl < clip.timeline_out})
        }
        ParamValue::Unknown(v) => v.clone(),
    }
}

/// An effect as `get_state` and the AI context show it; `known: false` for
/// a recipe or version from a newer Kadr.
pub fn effect_json(clip: &Clip, e: &AppliedEffect) -> Value {
    json!({
        "id": e.id.to_string(), "recipe": e.recipe, "version": e.version, "enabled": e.enabled,
        "known": definition(e).is_some(), "progress": [e.progress.0, e.progress.1],
        "params": e.params.iter().map(|(k, v)| (k.clone(), value_json(clip, v))).collect::<Map<_, _>>(),
    })
}

/// Client parameters for `def` on `clip`, each checked like a command
/// would; the error names the key and the reason.
pub fn params_from_json(def: &RecipeDef, clip: &Clip, json: &Map<String, Value>) -> Result<BTreeMap<String, ParamValue>, String> {
    params_from_json_in(def, clip, json, None, Time::ZERO)
}

/// [`params_from_json`] for a caller that knows more. `current` is the
/// effect's stored values when updating: a moment echoed back from
/// [`value_json`] (milliseconds are floored) resolves to the stored one,
/// so it is not a change. `slack` widens the snap of a timeline `at_ms`
/// just before the clip's first frame (the AI passes half a frame); it is
/// at least one millisecond, the flooring of an echoed start.
pub fn params_from_json_in(
    def: &RecipeDef,
    clip: &Clip,
    json: &Map<String, Value>,
    current: Option<&BTreeMap<String, ParamValue>>,
    slack: Time,
) -> Result<BTreeMap<String, ParamValue>, String> {
    let mut out = BTreeMap::new();
    for (k, v) in json {
        let d = def
            .param(k)
            .ok_or_else(|| format!("{} has no parameter {k:?} (it has {})", def.id, def.params.iter().map(|d| d.key).collect::<Vec<_>>().join(", ")))?;
        let value = match d.kind {
            ParamKind::Number { .. } => ParamValue::Number(v.as_f64().ok_or_else(|| format!("{k} must be a number"))?),
            ParamKind::Choice { .. } => ParamValue::Choice(v.as_str().ok_or_else(|| format!("{k} must be a string"))?.to_string()),
            ParamKind::Point => {
                let o = v.as_object().filter(|o| o.len() == 2).ok_or_else(|| format!("{k} must be {{\"x\": 0..1, \"y\": 0..1}}"))?;
                let get = |c: &str| o.get(c).and_then(Value::as_f64).ok_or_else(|| format!("{k} must be {{\"x\": 0..1, \"y\": 0..1}}"));
                ParamValue::Point { x: get("x")?, y: get("y")? }
            }
            ParamKind::Moment => {
                let stored = match current.and_then(|c| c.get(k.as_str())) {
                    Some(ParamValue::Moment(m)) => Some(*m),
                    _ => None,
                };
                ParamValue::Moment(moment_from_json(clip, k, v, stored, slack)?)
            }
        };
        d.check(&value)?;
        out.insert(k.clone(), value);
    }
    Ok(out)
}

/// `{at_ms}` (timeline, inside the clip), `{source_ms}`, or what
/// [`value_json`] writes, `{at_ms, source_ms, visible}` — then `source_ms`
/// wins, it is the exact one.
fn moment_from_json(clip: &Clip, k: &str, v: &Value, stored: Option<SourceTime>, slack: Time) -> Result<SourceTime, String> {
    let shape = || format!("{k} must be {{\"at_ms\": int}} (timeline, inside the clip) or {{\"source_ms\": int}}");
    let o = v
        .as_object()
        .filter(|o| o.keys().all(|key| matches!(key.as_str(), "at_ms" | "source_ms" | "visible")))
        .ok_or_else(shape)?;
    let ms = |key: &str| -> Result<Option<i64>, String> {
        o.get(key)
            .map(|x| x.as_i64().filter(|m| m.unsigned_abs() <= MAX_MS).ok_or_else(|| format!("{k}.{key} must be an integer within ±{MAX_MS}")))
            .transpose()
    };
    let (at, source) = (ms("at_ms")?, ms("source_ms")?);
    if let Some(ms) = source {
        // Milliseconds are floored when shown: the stored moment within that ms is the one named.
        return Ok(stored.filter(|m| m.0.as_millis() == ms).unwrap_or(SourceTime(Time::from_millis(ms))));
    }
    let ms = at.ok_or_else(shape)?;
    let degenerate = clip.source_out == clip.source_in;
    if let Some(m) = stored.filter(|m| !degenerate && clip.timeline_time_of(m.0).as_millis() == ms) {
        return Ok(m);
    }
    let mut t = Time::from_millis(ms);
    if t < clip.timeline_in && clip.timeline_in - t < slack.max(Time::from_millis(1)) {
        t = clip.timeline_in;
    }
    if t < clip.timeline_in || t >= clip.timeline_out {
        return Err(format!(
            "{k}.at_ms {ms} is outside the clip ({}..{} ms); use source_ms for a moment off the clip",
            clip.timeline_in.as_millis(),
            clip.timeline_out.as_millis()
        ));
    }
    Ok(SourceTime(clip.source_time_at(t)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::recipe;
    use kadr_core::{AssetId, TimeRange};
    use serde_json::json;

    /// Timeline 10..20 s showing source 30..40 s.
    fn clip() -> Clip {
        Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10))
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn the_registry_describes_types_ranges_options_and_defaults() {
        let all = registry_json();
        assert_eq!(all.as_array().unwrap().len(), 5);
        let zoom = &all[0];
        assert_eq!((zoom["id"].as_str(), zoom["category"].as_str(), zoom["target"].as_str()), (Some("smooth_zoom"), Some("motion"), Some("clip")));
        assert_eq!(zoom["params"][0], json!({"key": "direction", "type": "choice", "options": ["in", "out"], "default": "in"}));
        assert_eq!(zoom["params"][1], json!({"key": "strength", "type": "number", "min": 0.0, "max": 1.0, "default": 0.6, "animatable": true}));
        assert_eq!(zoom["params"][2]["type"], "point");
    }

    #[test]
    fn params_from_json_converts_and_checks_each_value() {
        let c = clip();
        let def = recipe("punch_in").unwrap();
        let p = params_from_json(def, &c, &obj(json!({"strength": 0.9, "point": {"x": 0.25, "y": 1}, "moment": {"at_ms": 12_000}}))).unwrap();
        assert_eq!(p["strength"], ParamValue::Number(0.9));
        assert_eq!(p["point"], ParamValue::Point { x: 0.25, y: 1.0 });
        assert_eq!(p["moment"], ParamValue::Moment(SourceTime(Time::from_secs(32))));
        let src = params_from_json(def, &c, &obj(json!({"moment": {"source_ms": 45_000}}))).unwrap();
        assert_eq!(src["moment"], ParamValue::Moment(SourceTime(Time::from_secs(45))), "source_ms may lie off the clip");
        let err = |j: Value| params_from_json(def, &c, &obj(j)).unwrap_err();
        assert!(err(json!({"strength": 1.4})).contains("strength must be 0..1"));
        assert!(err(json!({"strength": "high"})).contains("strength must be a number"));
        assert!(err(json!({"speed": 1})).contains("punch_in has no parameter \"speed\""));
        assert!(err(json!({"point": {"x": 0.5}})).contains("point must be"));
        assert!(err(json!({"moment": 12_000})).contains("moment must be"));
        assert!(err(json!({"moment": {"at_ms": i64::MIN}})).contains("within"));
    }

    /// Review focus 4: a timeline moment off the visible clip is refused.
    #[test]
    fn params_from_json_rejects_a_moment_off_the_clip() {
        let def = recipe("punch_in").unwrap();
        let msg = params_from_json(def, &clip(), &obj(json!({"moment": {"at_ms": 25_000}}))).unwrap_err();
        assert!(msg.contains("outside the clip (10000..20000 ms)") && msg.contains("source_ms"), "{msg}");
        assert!(params_from_json(def, &clip(), &obj(json!({"moment": {"at_ms": 20_000}}))).is_err(), "timeline_out is outside");
    }

    #[test]
    fn effects_are_shown_with_both_times_of_a_moment() {
        let c = clip();
        let mut e = crate::fresh(recipe("punch_in").unwrap(), &c, Time::from_secs(12));
        e.params.insert("moment".into(), ParamValue::Moment(SourceTime(Time::from_secs(45))));
        let j = effect_json(&c, &e);
        assert_eq!(j["recipe"], "punch_in");
        assert_eq!((j["known"].as_bool(), j["enabled"].as_bool()), (Some(true), Some(true)));
        assert_eq!(j["progress"], json!([0.0, 1.0]));
        assert_eq!(j["params"]["moment"], json!({"at_ms": 25_000, "source_ms": 45_000, "visible": false}));
        assert_eq!(j["params"]["strength"], json!(0.6));
        assert_eq!(effect_json(&c, &AppliedEffect::new("glitch_v2", 1, BTreeMap::new()))["known"], false);
    }

    /// R11(a): what `value_json` writes is accepted back, `source_ms` wins,
    /// and an echoed unchanged moment is that same moment (ms flooring).
    #[test]
    fn a_moment_echoed_from_get_state_is_accepted_and_unchanged() {
        let c = clip();
        let def = recipe("punch_in").unwrap();
        let stored = ParamValue::Moment(SourceTime(Time::from_secs(35) + Time(1234)));
        let cur = BTreeMap::from([("moment".to_string(), stored.clone())]);
        let echo = value_json(&c, &stored);
        let got = |j: Value, cur: Option<&BTreeMap<String, ParamValue>>| params_from_json_in(def, &c, &obj(json!({"moment": j})), cur, Time::ZERO).unwrap()["moment"].clone();
        assert_eq!(got(echo.clone(), Some(&cur)), stored, "the whole echo");
        assert_eq!(got(json!({"at_ms": echo["at_ms"]}), Some(&cur)), stored, "at_ms alone, floored");
        assert_eq!(got(json!({"source_ms": echo["source_ms"]}), Some(&cur)), stored, "source_ms alone, floored");
        assert_eq!(got(json!({"source_ms": 36_000}), Some(&cur)), ParamValue::Moment(SourceTime(Time::from_secs(36))), "a different moment is a change");
        assert_eq!(got(json!({"at_ms": 11_000, "source_ms": 45_000, "visible": true}), None), ParamValue::Moment(SourceTime(Time::from_secs(45))), "source_ms is preferred");
        let off = ParamValue::Moment(SourceTime(Time::from_secs(45) + Time(99)));
        let cur_off = BTreeMap::from([("moment".to_string(), off.clone())]);
        assert_eq!(got(value_json(&c, &off), Some(&cur_off)), off, "an off-clip moment echoes back too");
        assert!(params_from_json(def, &c, &obj(json!({"moment": {"visible": true}}))).unwrap_err().contains("moment must be"));
        assert!(params_from_json(def, &c, &obj(json!({"moment": {"at_ms": 12_000, "bogus": 1}}))).unwrap_err().contains("moment must be"));
    }

    /// R11(b): the floored millisecond of a clip that starts between two
    /// milliseconds names its first frame, not a frame before the clip.
    #[test]
    fn at_ms_snaps_to_the_clips_frame_exact_start() {
        let mut c = clip();
        c.move_to(Time::from_secs(10) + Time(1000));
        let def = recipe("punch_in").unwrap();
        let at = |ms: i64| params_from_json(def, &c, &obj(json!({"moment": {"at_ms": ms}})));
        assert_eq!(at(10_000).unwrap()["moment"], ParamValue::Moment(SourceTime(Time::from_secs(30))));
        assert!(at(9_999).is_err());
        // A caller-given slack (the AI's half frame) widens the snap.
        let wide = params_from_json_in(def, &c, &obj(json!({"moment": {"at_ms": 9_990}})), None, Time::from_millis(16)).unwrap();
        assert_eq!(wide["moment"], ParamValue::Moment(SourceTime(Time::from_secs(30))));
    }
}
