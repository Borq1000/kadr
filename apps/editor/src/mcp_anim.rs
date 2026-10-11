//! MCP `edit` ops for keyframes and clip fades (keyframes spec §7). Times
//! arrive as milliseconds and are turned into exact `Time`/`SourceTime`
//! at once; nothing below this module sees milliseconds. A keyframe op's
//! `param` is a clip property or `fx:<effect-id>:<key>`, a number of an
//! effect on the clip (effects spec §7).
//!
//! Like every `edit` op these are resolved against the project as it was
//! before the call, so a later op in the same batch does not see keyframes
//! an earlier one created.

use crate::anim_ui;
use kadr_core::{ClipId, Time};
use kadr_effects::ParamKind;
use kadr_mcp_bridge::BridgeError;
use kadr_project::{AnimParam, Clip, EdgeFades, Ease, Project, Sequence, SourceTime, TrackKey, TrackKind};
use kadr_timeline::{ClipProperty, EditCommand, EditError};
use serde_json::Value;

/// Largest accepted |milliseconds| (about 116 days): far beyond any
/// project, far below where `Time::from_millis` would overflow an `i64`.
const MAX_MS: i64 = 10_000_000_000;

fn bad(msg: impl Into<String>) -> BridgeError {
    BridgeError::new("edit_rejected", msg.into())
}

/// The fields each owned op accepts (besides `type` and `reason`).
fn fields(ty: &str) -> Option<&'static [&'static str]> {
    Some(match ty {
        "set_keyframe" => &["clip_id", "param", "value", "at_ms", "source_ms", "ease"],
        "remove_keyframe" => &["clip_id", "param", "at_ms", "source_ms"],
        "set_keyframe_ease" => &["clip_id", "param", "at_ms", "source_ms", "ease"],
        "move_keyframes" => &["clip_id", "param", "from_ms", "from_source_ms", "to_ms"],
        "clear_animation" => &["clip_id", "params", "keep_ms"],
        "set_video_fade" => &["clip_id", "fade_in_ms", "fade_out_ms"],
        _ => return None,
    })
}

/// A millisecond argument: absent -> `None`; present -> an integer within
/// `MAX_MS`, so no later `Time::from_millis` can overflow.
fn ms_arg(op: &Value, key: &str) -> Result<Option<i64>, BridgeError> {
    let Some(v) = op.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
    let ms = v.as_i64().ok_or_else(|| bad(format!("`{key}` must be an integer (milliseconds), got {v}")))?;
    if !(-MAX_MS..=MAX_MS).contains(&ms) {
        return Err(bad(format!("`{key}` {ms} is out of range (at most {MAX_MS} ms either way)")));
    }
    Ok(Some(ms))
}

fn clip<'a>(seq: &'a Sequence, op: &Value) -> Result<(ClipId, &'a Clip), BridgeError> {
    let id = op.get("clip_id").and_then(Value::as_str).and_then(ClipId::parse).ok_or_else(|| bad("clip_id missing or malformed"))?;
    Ok((id, seq.clip(id).ok_or_else(|| bad(format!("unknown clip {id}")))?))
}

/// A keyframe op's `param`: a clip property, or `fx:<effect-id>:<key>` — a
/// number of an effect on this clip that this version runs (the engine's
/// `track_def` rule, ruling R4), each refusal with its reason.
fn param_named(c: &Clip, k: &str) -> Result<TrackKey, BridgeError> {
    if k.starts_with("fx:") {
        let Some(key @ TrackKey::Fx(_)) = TrackKey::parse(k) else {
            return Err(bad(format!("malformed effect param {k:?} (fx:<effect-id>:<key>, the effect id as get_state shows it)")));
        };
        let effect = key.as_fx().map(|f| f.effect).expect("an fx key");
        return match kadr_timeline::track_def(c, &key) {
            Ok(_) => Ok(key),
            Err(EditError::EffectNotFound) => Err(bad(format!("the clip has no effect {effect}"))),
            Err(EditError::UnknownRecipe(r)) => Err(bad(format!("effect {effect} ({r}) was made by a newer Kadr and cannot be animated"))),
            Err(EditError::InvalidParam(m)) => Err(bad(m)),
            Err(e) => Err(bad(e.to_string())),
        };
    }
    AnimParam::from_key(k)
        .map(TrackKey::Clip)
        .ok_or_else(|| bad(format!("unknown param {k:?} (one of {}, or fx:<effect-id>:<key>)", AnimParam::ALL.map(|p| p.key()).join(", "))))
}

fn param(c: &Clip, op: &Value) -> Result<TrackKey, BridgeError> {
    param_named(c, op.get("param").and_then(Value::as_str).unwrap_or_default())
}

fn ease(op: &Value) -> Result<Option<Ease>, BridgeError> {
    match op.get("ease").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => {
            let k = v.as_str().unwrap_or_default();
            Ease::from_key(k).map(Some).ok_or_else(|| bad(format!("unknown ease {v} (one of ease_in_out, linear, ease_in, ease_out, hold)")))
        }
    }
}

fn outside(c: &Clip, ms: i64) -> BridgeError {
    bad(format!("{ms} ms is outside the clip ({}..{} ms); use source_ms for hidden keyframes", c.timeline_in.as_millis(), c.timeline_out.as_millis()))
}

/// A timeline position inside the clip, frame-snapped.
fn at_timeline(seq: &Sequence, c: &Clip, ms: i64) -> Result<Time, BridgeError> {
    let t = seq.frame_rate.snap(Time::from_millis(ms));
    if t < c.timeline_in || t >= c.timeline_out {
        return Err(outside(c, ms));
    }
    Ok(t)
}

/// A moment addressed in timeline or in source milliseconds.
enum When {
    Timeline(i64),
    Source(i64),
}

fn when(op: &Value, at: &str, src: &str) -> Result<When, BridgeError> {
    match (ms_arg(op, at)?, ms_arg(op, src)?) {
        (Some(ms), None) => Ok(When::Timeline(ms)),
        (None, Some(ms)) => Ok(When::Source(ms)),
        _ => Err(bad(format!("give exactly one of {at} or {src}"))),
    }
}

/// Where a new keyframe goes: `at_ms` (timeline) or `source_ms` (source).
fn new_time(seq: &Sequence, c: &Clip, op: &Value, at: &str, src: &str) -> Result<SourceTime, BridgeError> {
    Ok(match when(op, at, src)? {
        When::Timeline(ms) => SourceTime(c.source_time_at(at_timeline(seq, c, ms)?)),
        When::Source(ms) => SourceTime(Time::from_millis(ms)),
    })
}

/// The keyframe moment of `p` (of any param when `p` is None) nearest to the
/// given time and strictly within half a frame of it, if any. A client
/// echoing millisecond-rounded times (29.97 fps frames are not whole
/// milliseconds) or addressing an off-frame key still hits it. With `at_ms`
/// only keys inside the clip count — the inspector's "keyframe here" rule
/// (`anim_ui::visible_key_near`); hidden ones are `source_ms` business.
fn find(seq: &Sequence, c: &Clip, p: Option<&TrackKey>, op: &Value, at: &str, src: &str) -> Result<Option<SourceTime>, BridgeError> {
    let frame = seq.frame_rate.frame_duration();
    let h = anim_ui::half_frame(frame);
    Ok(match when(op, at, src)? {
        When::Timeline(ms) => {
            // The raw time, not the snapped one: a visible key in the clip's last half frame
            // is still reachable by its echoed ms.
            let t = Time::from_millis(ms);
            if t < c.timeline_in - h || t >= c.timeline_out + h {
                return Err(outside(c, ms));
            }
            anim_ui::visible_key_near(c, p.cloned(), t, frame)
        }
        When::Source(ms) => {
            let s = Time::from_millis(ms);
            c.animation.times().into_iter().filter(|a| p.is_none_or(|q| c.animation.keys_at(*a).contains(q))).filter(|a| (a.0 - s).abs() < h).min_by_key(|a| (a.0 - s).abs())
        }
    })
}

/// Like `find`, but a missing keyframe is an error.
fn existing(seq: &Sequence, c: &Clip, p: Option<&TrackKey>, op: &Value, at: &str, src: &str) -> Result<SourceTime, BridgeError> {
    find(seq, c, p, op, at, src)?.ok_or_else(|| {
        bad("no keyframe there (looked within half a frame of the given time; keyframes made by an earlier op of the same call are not visible yet)")
    })
}

/// An effect number's `(min, max, default)` from the registry; `None` for a
/// clip property (and for a track keyframe ops may not edit, which `param`
/// has already refused).
fn effect_number(c: &Clip, k: &TrackKey) -> Option<(f64, f64, f64)> {
    match kadr_timeline::track_def(c, k).ok()??.kind {
        ParamKind::Number { min, max, default } => Some((min, max, default)),
        _ => None,
    }
}

/// The accepted `value` range (same limits as `set_transform` for the
/// transform params; fractions of the frame for crops; the registry's for
/// effect numbers).
fn value_range(c: &Clip, k: &TrackKey) -> (f64, f64) {
    match k {
        TrackKey::Clip(p) => match p {
            AnimParam::X | AnimParam::Y => (-20_000.0, 20_000.0),
            AnimParam::Scale => (0.01, 16.0),
            AnimParam::Rotation => (-3600.0, 3600.0),
            AnimParam::Opacity | AnimParam::CropLeft | AnimParam::CropRight | AnimParam::CropTop | AnimParam::CropBottom => (0.0, 1.0),
            AnimParam::Exposure => (-2.0, 2.0),
            AnimParam::Contrast | AnimParam::Saturation => (0.0, 2.0),
            AnimParam::Temperature => (-1.0, 1.0),
        },
        TrackKey::Fx(_) => effect_number(c, k).map_or((0.0, 0.0), |(lo, hi, _)| (lo, hi)),
    }
}

/// A track's static value after `clear_animation` without `keep_ms`.
fn default_of(c: &Clip, k: &TrackKey) -> f64 {
    match k {
        TrackKey::Clip(p) => p.default_value(),
        TrackKey::Fx(_) => effect_number(c, k).map_or(0.0, |(_, _, d)| d),
    }
}

/// A static value derived from keyframes (`clear_animation` `keep_ms`): an
/// effect number is kept in its range — what the picture showed, as the
/// engine writes it (ruling R12; a file from elsewhere may hold keyframes
/// outside the range). Clip properties are written as they are.
fn static_of(c: &Clip, k: &TrackKey, v: f64) -> f64 {
    effect_number(c, k).map_or(v, |(lo, hi, _)| v.clamp(lo, hi))
}

fn set_keyframe(seq: &Sequence, id: ClipId, c: &Clip, op: &Value) -> Result<EditCommand, BridgeError> {
    let p = param(c, op)?;
    let value = op.get("value").and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad("value missing or not a number"))?;
    let (lo, hi) = value_range(c, &p);
    if !(lo..=hi).contains(&value) {
        return Err(bad(format!("{} out of range {lo}..{hi} (got {value})", p.key_string())));
    }
    let ease = ease(op)?;
    // A key within half a frame is updated, not shadowed by a near-duplicate.
    let at = match find(seq, c, Some(&p), op, "at_ms", "source_ms")? {
        Some(at) => {
            if c.animation.track(&p).iter().any(|k| k.at == at && k.value == value && ease.as_ref().is_none_or(|e| *e == k.ease)) {
                return Err(bad("no change: that keyframe already has this value and ease"));
            }
            at
        }
        None => new_time(seq, c, op, "at_ms", "source_ms")?,
    };
    Ok(EditCommand::SetKeyframe { clip: id, param: p, at, value, ease })
}

fn remove_keyframe(seq: &Sequence, id: ClipId, c: &Clip, op: &Value) -> Result<EditCommand, BridgeError> {
    let p = param(c, op)?;
    let at = existing(seq, c, Some(&p), op, "at_ms", "source_ms")?;
    // The last keyframe's value stays as the static value; the engine keeps
    // an effect number's in its range (ruling R12).
    Ok(EditCommand::RemoveKeyframe { clip: id, param: p, at, keep: None })
}

fn clear_animation(seq: &Sequence, id: ClipId, c: &Clip, op: &Value) -> Result<EditCommand, BridgeError> {
    let listed = match op.get("params").filter(|v| !v.is_null()) {
        None => None,
        Some(Value::Array(a)) => Some(a.iter().map(|v| param_named(c, v.as_str().unwrap_or_default())).collect::<Result<Vec<_>, _>>()?),
        Some(_) => return Err(bad("`params` must be an array of param names")),
    };
    // Only animated tracks are reset (a static value is not the animation's
    // business), and only those keyframe commands may edit: a newer Kadr's
    // effect keeps its keyframes (ruling R8).
    let animated: Vec<TrackKey> = c
        .animation
        .keys()
        .into_iter()
        .filter(|k| kadr_timeline::track_def(c, k).is_ok())
        .filter(|k| listed.as_ref().is_none_or(|l| l.contains(k)))
        .collect();
    if animated.is_empty() {
        return Err(bad(match listed {
            Some(_) => "clip has no animation on the listed params",
            None if !c.animation.is_empty() => "the clip's keyframes are read-only: they belong to an effect this version cannot edit (from a newer Kadr) or to no effect on the clip",
            None => "clip has no animation",
        }));
    }
    let keep = ms_arg(op, "keep_ms")?.map(|ms| seq.frame_rate.snap(Time::from_millis(ms)));
    let statics = animated
        .into_iter()
        .map(|k| {
            let v = match keep {
                Some(t) => static_of(c, &k, c.track_value(&k, t).unwrap_or_else(|| default_of(c, &k))),
                None => default_of(c, &k),
            };
            (k, v)
        })
        .collect();
    Ok(EditCommand::ClearAnimation { clip: id, statics })
}

fn set_video_fade(seq: &Sequence, id: ClipId, c: &Clip, op: &Value) -> Result<EditCommand, BridgeError> {
    if seq.locate_clip(id).is_some_and(|(ti, _)| seq.tracks[ti].kind == TrackKind::Audio) {
        return Err(bad("set_video_fade is for picture fades on video clips; audio fades are not set by this op"));
    }
    let fade = |key: &str, cur: Time| -> Result<Time, BridgeError> {
        match ms_arg(op, key)? {
            None => Ok(cur),
            Some(ms) if ms < 0 => Err(bad(format!("`{key}` must not be negative"))),
            Some(ms) => Ok(seq.frame_rate.snap(Time::from_millis(ms))),
        }
    };
    if op.get("fade_in_ms").is_none() && op.get("fade_out_ms").is_none() {
        return Err(bad("give fade_in_ms and/or fade_out_ms"));
    }
    let f = EdgeFades { fade_in: fade("fade_in_ms", c.video_fade.fade_in)?, fade_out: fade("fade_out_ms", c.video_fade.fade_out)? };
    if f.fade_in + f.fade_out > c.duration() {
        return Err(bad(format!("fade_in_ms + fade_out_ms must not exceed the clip length ({} ms)", c.duration().as_millis())));
    }
    if f == c.video_fade {
        return Err(bad("no change: the clip already has these fades"));
    }
    Ok(EditCommand::SetClipProperty { clip: id, prop: ClipProperty::VideoFade(f) })
}

/// Keyframe and fade ops; `None` for every other `type`.
pub fn anim_op(_project: &Project, seq: &Sequence, op: &Value) -> Option<Result<Vec<EditCommand>, BridgeError>> {
    let ty = op.get("type").and_then(Value::as_str)?;
    let allowed = fields(ty)?;
    Some((|| {
        if let Some(extra) = op.as_object().into_iter().flat_map(|o| o.keys()).find(|k| !matches!(k.as_str(), "type" | "reason") && !allowed.contains(&k.as_str())) {
            return Err(bad(format!("unknown field `{extra}` for {ty}")));
        }
        let (id, c) = clip(seq, op)?;
        if let Some((ti, _)) = seq.locate_clip(id) {
            let t = &seq.tracks[ti];
            if t.locked {
                return Err(bad(format!("track {} is locked", t.name)));
            }
            if ty != "set_video_fade" && t.kind != TrackKind::Video {
                return Err(bad("keyframes can only be set on clips of video tracks"));
            }
        }
        Ok(vec![match ty {
            "set_keyframe" => set_keyframe(seq, id, c, op)?,
            "remove_keyframe" => remove_keyframe(seq, id, c, op)?,
            "set_keyframe_ease" => {
                let p = param(c, op)?;
                let ease = ease(op)?.ok_or_else(|| bad("ease missing"))?;
                let at = existing(seq, c, Some(&p), op, "at_ms", "source_ms")?;
                if c.animation.track(&p).iter().any(|k| k.at == at && k.ease == ease) {
                    return Err(bad("no change: that keyframe already has this ease"));
                }
                EditCommand::SetKeyframeEase { clip: id, param: p, at, ease }
            }
            "move_keyframes" => {
                let p = op.get("param").filter(|v| !v.is_null()).map(|_| param(c, op)).transpose()?;
                let from = existing(seq, c, p.as_ref(), op, "from_ms", "from_source_ms")?;
                let to_ms = ms_arg(op, "to_ms")?.ok_or_else(|| bad("to_ms missing"))?;
                let to = SourceTime(c.source_time_at(at_timeline(seq, c, to_ms)?));
                if to == from {
                    return Err(bad("no change: the keyframes are already at that moment"));
                }
                // Every track of the moment: the editable ones; a read-only
                // track (a newer Kadr's effect) keeps its keyframe (ruling R8).
                let params: Vec<TrackKey> = p.map_or_else(|| kadr_timeline::editable_keys_at(c, from), |p| vec![p]);
                if params.is_empty() {
                    return Err(bad("the keyframes at this moment are read-only: they belong to an effect this version cannot edit (from a newer Kadr) or to no effect on the clip"));
                }
                if let Some(q) = params.iter().find(|q| c.animation.track(*q).iter().any(|k| k.at == to)) {
                    return Err(bad(format!("{} already has a keyframe at {to_ms} ms; move or remove it first", q.key_string())));
                }
                EditCommand::MoveKeyframes { clip: id, moves: params.into_iter().map(|q| (q, from, to)).collect() }
            }
            "clear_animation" => clear_animation(seq, id, c, op)?,
            "set_video_fade" => set_video_fade(seq, id, c, op)?,
            _ => unreachable!("`fields` lists exactly the owned types"),
        }])
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, FrameRate, TimeRange};
    use kadr_project::{AnimParam, Clip, Project};
    use kadr_project::SourceTime;
    use kadr_timeline::EditEngine;
    use serde_json::json;

    fn project() -> (Project, String) {
        let mut p = Project::new("t");
        p.sequence_mut().frame_rate = FrameRate::FPS_29_97;
        let c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(10), Time::from_secs(20)), Time::from_secs(5));
        let id = c.id.to_string();
        p.sequence_mut().tracks[0].clips.push(c);
        (p, id)
    }

    fn run(p: &mut Project, op: Value) -> Result<(), BridgeError> {
        let cmds = anim_op(p, p.sequence(), &op).expect("owned op")?;
        let mut e = EditEngine::new();
        for c in cmds {
            e.execute(p, c).map_err(|e| BridgeError::new("edit_rejected", e.to_string()))?;
        }
        Ok(())
    }

    fn project_at(fps: FrameRate) -> (Project, String) {
        let (mut p, id) = project();
        p.sequence_mut().frame_rate = fps;
        (p, id)
    }

    #[test]
    fn at_ms_is_converted_to_exact_source_time_and_found_again_after_ms_rounding() {
        // Frames at these rates are not whole milliseconds; at_ms 6040 lands on a frame
        // whose time is NOT a whole number of ms, so the echo from get_state is inexact.
        for fps in [FrameRate::FPS_29_97, FrameRate::FPS_23_976, FrameRate::FPS_59_94] {
            let (mut p, id) = project_at(fps);
            run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "scale", "at_ms": 6_040, "value": 2.0})).unwrap();
            let c = &p.sequence().tracks[0].clips[0];
            let k_at = c.animation.track(AnimParam::Scale)[0].at;
            assert_eq!(k_at.0, c.source_time_at(fps.snap(Time::from_millis(6_040))), "frame-snapped, exact");
            let tl = c.timeline_time_of(k_at.0);
            let echoed = tl.as_millis();
            assert_ne!(Time::from_millis(echoed), tl, "{fps}: the echo must be a rounded value");
            run(&mut p, json!({"type": "set_keyframe_ease", "clip_id": id, "param": "scale", "at_ms": echoed, "ease": "hold"})).unwrap();
            assert_eq!(p.sequence().tracks[0].clips[0].animation.track(AnimParam::Scale)[0].ease, kadr_project::Ease::Hold, "{fps}");
            // set_keyframe with the echoed time updates the key instead of adding a neighbour.
            run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "scale", "at_ms": echoed, "value": 3.0})).unwrap();
            let t = p.sequence().tracks[0].clips[0].animation.track(AnimParam::Scale).to_vec();
            assert_eq!((t.len(), t[0].value, t[0].at), (1, 3.0, k_at), "{fps}");
            run(&mut p, json!({"type": "remove_keyframe", "clip_id": id, "param": "scale", "at_ms": echoed})).unwrap();
            assert!(p.sequence().tracks[0].clips[0].animation.is_empty(), "{fps}");
        }
    }

    #[test]
    fn set_keyframe_with_the_echoed_source_ms_updates_the_key() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_040, "value": 1.0})).unwrap();
        let at = p.sequence().tracks[0].clips[0].animation.track(AnimParam::X)[0].at;
        let echoed = at.0.as_millis();
        assert_ne!(Time::from_millis(echoed), at.0, "the source echo is rounded too");
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "source_ms": echoed, "value": 5.0})).unwrap();
        let t = p.sequence().tracks[0].clips[0].animation.track(AnimParam::X).to_vec();
        assert_eq!((t.len(), t[0].value, t[0].at), (1, 5.0, at));
        // Far from any key it adds one.
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "source_ms": echoed + 500, "value": 6.0})).unwrap();
        assert_eq!(p.sequence().tracks[0].clips[0].animation.track(AnimParam::X).len(), 2);
    }

    #[test]
    fn off_frame_keys_of_a_speed_changed_clip_are_updated_by_their_at_ms_echo() {
        let (mut p, id) = project();
        {
            // Source 10..20 s shown over timeline 5..10 s: 2x speed.
            let c = &mut p.sequence_mut().tracks[0].clips[0];
            c.timeline_out = Time::from_secs(10);
        }
        // source 12 000 ms is timeline 6 000 ms exactly, which is not a frame boundary at 29.97.
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "y", "source_ms": 12_000, "value": 1.0})).unwrap();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "y", "at_ms": 6_000, "value": 2.0})).unwrap();
        let t = p.sequence().tracks[0].clips[0].animation.track(AnimParam::Y).to_vec();
        assert_eq!((t.len(), t[0].value, t[0].at.0), (1, 2.0, Time::from_secs(12)), "updated, not a near-duplicate");
        run(&mut p, json!({"type": "remove_keyframe", "clip_id": id, "param": "y", "at_ms": 6_000})).unwrap();
        assert!(p.sequence().tracks[0].clips[0].animation.is_empty());
    }

    #[test]
    fn a_visible_key_in_the_clips_last_half_frame_is_reachable_by_its_echo() {
        let (mut p, id) = project();
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        let tl = c.timeline_out - Time::from_flicks(100_000);
        let s = SourceTime(c.source_time_at(tl));
        c.animation.set_key(AnimParam::X, s, 1.0, None);
        let echoed = tl.as_millis();
        assert!(Time::from_millis(echoed) < c.timeline_out);
        run(&mut p, json!({"type": "set_keyframe_ease", "clip_id": id, "param": "x", "at_ms": echoed, "ease": "hold"})).unwrap();
        assert_eq!(p.sequence().tracks[0].clips[0].animation.track(AnimParam::X)[0].ease, kadr_project::Ease::Hold);
    }

    #[test]
    fn set_keyframe_value_must_be_in_the_params_range() {
        let (mut p, id) = project();
        for (param, ok, bad_v) in [
            ("x", 20_000.0, 20_000.5), ("y", -20_000.0, -20_001.0), ("scale", 16.0, 0.0), ("rotation", -3600.0, 3600.5), ("opacity", 1.0, 1.5),
            ("crop_left", 1.0, -0.1), ("crop_right", 0.0, 2.0), ("crop_top", 0.5, 1.1), ("crop_bottom", 0.5, -1.0), ("exposure", -2.0, 2.5),
            ("contrast", 2.0, 3.0), ("saturation", 0.0, -0.5), ("temperature", -1.0, 1.5),
        ] {
            run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": param, "at_ms": 6_000, "value": ok})).unwrap();
            let e = run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": param, "at_ms": 7_000, "value": bad_v})).unwrap_err();
            assert!(e.message.contains(&format!("{param} out of range")), "{}", e.message);
        }
        assert_eq!(p.sequence().tracks[0].clips[0].animation.times().len(), 1, "rejected values created nothing");
    }

    #[test]
    fn keyframe_ops_say_why_a_track_does_not_take_them() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0})).unwrap();
        let a = Clip::new(AssetId::new(), "a", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
        let aid = a.id.to_string();
        p.sequence_mut().tracks.iter_mut().find(|t| t.kind == kadr_project::TrackKind::Audio).unwrap().clips.push(a);
        let e = run(&mut p, json!({"type": "set_keyframe", "clip_id": aid, "param": "x", "at_ms": 1_000, "value": 1.0})).unwrap_err();
        assert!(e.message.contains("video track"), "{}", e.message);
        p.sequence_mut().tracks[0].locked = true;
        for op in [
            json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 7_000, "value": 1.0}),
            json!({"type": "remove_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000}),
            json!({"type": "set_keyframe_ease", "clip_id": id, "param": "x", "at_ms": 6_000, "ease": "hold"}),
            json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 7_000}),
            json!({"type": "clear_animation", "clip_id": id}),
            json!({"type": "set_video_fade", "clip_id": id, "fade_in_ms": 500}),
        ] {
            let e = run(&mut p, op.clone()).unwrap_err();
            assert!(e.message.contains("locked"), "{op}: {}", e.message);
        }
    }

    #[test]
    fn move_keyframes_onto_an_existing_key_is_explained() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0})).unwrap();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 8_000, "value": 2.0})).unwrap();
        let e = run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 8_000, "param": null})).unwrap_err();
        assert!(e.message.contains("already") && e.message.contains("x"), "{}", e.message);
        // "param": null means all params at that moment, like an absent param.
        run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 9_000, "param": null})).unwrap();
    }

    #[test]
    fn repeating_a_keyframe_or_an_ease_is_no_change() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_040, "value": 1.0})).unwrap();
        let echoed = {
            let c = &p.sequence().tracks[0].clips[0];
            c.timeline_time_of(c.animation.track(AnimParam::X)[0].at.0).as_millis()
        };
        for op in [
            json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": echoed, "value": 1.0}),
            json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": echoed, "value": 1.0, "ease": "ease_in_out"}),
            json!({"type": "set_keyframe_ease", "clip_id": id, "param": "x", "at_ms": echoed, "ease": "ease_in_out"}),
        ] {
            let e = run(&mut p, op.clone()).unwrap_err();
            assert!(e.message.contains("no change"), "{op}: {}", e.message);
        }
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": echoed, "value": 1.0, "ease": "linear"})).unwrap();
    }

    #[test]
    fn a_missing_keyframe_hints_at_same_call_ops() {
        let (mut p, id) = project();
        let e = run(&mut p, json!({"type": "remove_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000})).unwrap_err();
        assert!(e.message.contains("same call"), "{}", e.message);
    }

    #[test]
    fn hidden_keyframes_use_source_ms_and_at_ms_must_be_inside_the_clip() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "source_ms": 2_000, "value": 1.0})).unwrap();
        assert_eq!(p.sequence().tracks[0].clips[0].animation.times()[0].0, Time::from_secs(2));
        let e = run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 1_000, "value": 1.0})).unwrap_err();
        assert!(e.message.contains("outside the clip"), "{}", e.message);
        let e = run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "glow", "at_ms": 6_000, "value": 1.0})).unwrap_err();
        assert!(e.message.contains("param"), "{}", e.message);
    }

    #[test]
    fn move_and_fade_and_clear() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0})).unwrap();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "y", "at_ms": 6_000, "value": 1.0})).unwrap();
        run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 8_000})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert_eq!(c.animation.times().len(), 1);
        assert_eq!(FrameRate::FPS_29_97.snap(c.timeline_time_of(c.animation.times()[0].0)), FrameRate::FPS_29_97.snap(Time::from_secs(8)));
        run(&mut p, json!({"type": "set_video_fade", "clip_id": id, "fade_in_ms": 500})).unwrap();
        assert_eq!(p.sequence().tracks[0].clips[0].video_fade.fade_in, FrameRate::FPS_29_97.snap(Time::from_millis(500)));
        run(&mut p, json!({"type": "clear_animation", "clip_id": id})).unwrap();
        assert!(p.sequence().tracks[0].clips[0].animation.is_empty());
    }

    #[test]
    fn other_types_are_not_owned() {
        let (p, _) = project();
        assert!(anim_op(&p, p.sequence(), &json!({"type": "split_clip", "at_ms": 1})).is_none());
    }

    // R24: set_transform on an animated property would change nothing; it is
    // rejected with the way out instead of a silent success.
    #[test]
    fn set_transform_on_an_animated_property_is_rejected() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "rotation", "at_ms": 6_000, "value": 30.0})).unwrap();
        let edit = |op: Value| crate::mcp_api::edit_commands(&p, &[], &json!([op]));
        let e = edit(json!({"type": "set_transform", "clip_id": id, "rotation_deg": 45.0})).unwrap_err();
        assert_eq!(e.code, "edit_rejected");
        assert_eq!(e.message, "op #0: rotation is animated (keyframes); use set_keyframe or clear_animation");
        assert!(edit(json!({"type": "set_transform", "clip_id": id, "scale": 1.5})).is_ok(), "a static property still takes it");
    }

    // R20 (a): clear_animation resets only what is animated.
    #[test]
    fn clear_animation_without_params_leaves_static_properties_alone() {
        let (mut p, id) = project();
        p.sequence_mut().tracks[0].clips[0].transform.scale = 1.5;
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 40.0})).unwrap();
        run(&mut p, json!({"type": "clear_animation", "clip_id": id})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.transform.scale, 1.5, "a static 150 % scale survives");
        assert_eq!(c.transform.x, AnimParam::X.default_value(), "the animated x goes to its default");
    }

    #[test]
    fn clear_animation_keep_ms_freezes_the_picture_at_that_time() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_040, "value": 0.0})).unwrap();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 8_040, "value": 100.0, "ease": "linear"})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        let echoed = c.timeline_time_of(c.animation.track(AnimParam::X)[1].at.0).as_millis();
        assert_ne!(Time::from_millis(echoed), c.timeline_time_of(c.animation.track(AnimParam::X)[1].at.0), "an inexact echo");
        run(&mut p, json!({"type": "clear_animation", "clip_id": id, "keep_ms": echoed})).unwrap();
        assert!((p.sequence().tracks[0].clips[0].transform.x - 100.0).abs() < 1e-9);
    }

    #[test]
    fn clear_animation_rejects_a_clip_or_param_list_without_animation() {
        let (mut p, id) = project();
        let e = run(&mut p, json!({"type": "clear_animation", "clip_id": id})).unwrap_err();
        assert!(e.message.contains("no animation"), "{}", e.message);
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 40.0})).unwrap();
        p.sequence_mut().tracks[0].clips[0].transform.scale = 1.5;
        let e = run(&mut p, json!({"type": "clear_animation", "clip_id": id, "params": ["scale"]})).unwrap_err();
        assert!(e.message.contains("no animation"), "{}", e.message);
        assert_eq!(p.sequence().tracks[0].clips[0].transform.scale, 1.5);
        // Listed params that are not animated are skipped; the animated one is cleared.
        run(&mut p, json!({"type": "clear_animation", "clip_id": id, "params": ["scale", "x"]})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.transform.scale, 1.5);
        let e = run(&mut p, json!({"type": "clear_animation", "clip_id": id, "params": ["glow"]})).unwrap_err();
        assert!(e.message.contains("param"), "{}", e.message);
    }

    // R20 (b): absurd milliseconds are rejected, never an overflow panic.
    #[test]
    fn huge_milliseconds_are_rejected_instead_of_overflowing() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0})).unwrap();
        let huge = i64::MAX / 2;
        for op in [
            json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": huge, "value": 1.0}),
            json!({"type": "set_keyframe", "clip_id": id, "param": "x", "source_ms": huge, "value": 1.0}),
            json!({"type": "set_keyframe", "clip_id": id, "param": "x", "source_ms": -huge, "value": 1.0}),
            json!({"type": "remove_keyframe", "clip_id": id, "param": "x", "at_ms": huge}),
            json!({"type": "remove_keyframe", "clip_id": id, "param": "x", "source_ms": huge}),
            json!({"type": "set_keyframe_ease", "clip_id": id, "param": "x", "at_ms": huge, "ease": "linear"}),
            json!({"type": "move_keyframes", "clip_id": id, "from_ms": huge, "to_ms": 7_000}),
            json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": huge}),
            json!({"type": "move_keyframes", "clip_id": id, "from_source_ms": huge, "to_ms": 7_000}),
            json!({"type": "clear_animation", "clip_id": id, "keep_ms": huge}),
            json!({"type": "set_video_fade", "clip_id": id, "fade_in_ms": huge}),
            json!({"type": "set_video_fade", "clip_id": id, "fade_out_ms": huge}),
        ] {
            let e = run(&mut p, op.clone()).unwrap_err();
            assert_eq!(e.code, "edit_rejected", "{op}");
            assert!(e.message.contains("out of range"), "{op}: {}", e.message);
        }
    }

    // R20 (c): picture fades only.
    #[test]
    fn set_video_fade_rejects_audio_tracks() {
        let (mut p, _) = project();
        let a = Clip::new(AssetId::new(), "a", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
        let aid = a.id.to_string();
        p.sequence_mut().tracks.iter_mut().find(|t| t.kind == kadr_project::TrackKind::Audio).unwrap().clips.push(a);
        let e = run(&mut p, json!({"type": "set_video_fade", "clip_id": aid, "fade_in_ms": 500})).unwrap_err();
        assert_eq!(e.code, "edit_rejected");
        assert!(e.message.contains("audio"), "{}", e.message);
    }

    // R20 (d): a move onto the same moment would record an empty undo entry.
    #[test]
    fn move_keyframes_to_the_same_moment_is_rejected_as_no_change() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0})).unwrap();
        let e = run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 6_000})).unwrap_err();
        assert!(e.message.contains("no change"), "{}", e.message);
        // An ms-rounded echo of the same moment snaps to the same frame: still no change.
        let e = run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_006, "to_ms": 6_005})).unwrap_err();
        assert!(e.message.contains("no change"), "{}", e.message);
    }

    #[test]
    fn malformed_input_is_a_clear_rejection() {
        let (mut p, id) = project();
        let msg = |p: &mut Project, op: Value| run(p, op).unwrap_err().message;
        assert!(msg(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "value": 1.0})).contains("exactly one of"));
        assert!(msg(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "source_ms": 1, "value": 1.0})).contains("exactly one of"));
        assert!(msg(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000.5, "value": 1.0})).contains("integer"));
        assert!(msg(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0, "ease": "bouncy"})).contains("ease"));
        assert!(msg(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0, "colour": 1})).contains("colour"));
        assert!(msg(&mut p, json!({"type": "set_keyframe", "clip_id": "nope", "param": "x", "at_ms": 6_000, "value": 1.0})).contains("clip_id"));
        assert!(msg(&mut p, json!({"type": "remove_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000})).contains("no keyframe"));
        assert!(msg(&mut p, json!({"type": "set_video_fade", "clip_id": id})).contains("fade_in_ms"));
        assert!(msg(&mut p, json!({"type": "set_video_fade", "clip_id": id, "fade_in_ms": -5})).contains("negative"));
    }

    #[test]
    fn effect_numbers_animate_through_the_keyframe_ops() {
        let (mut p, id) = project();
        let cid = ClipId::parse(&id).unwrap();
        let e = kadr_effects::fresh(kadr_effects::recipe("smooth_zoom").unwrap(), p.sequence().clip(cid).unwrap(), Time::from_secs(6));
        let fid = e.id;
        p.sequence_mut().tracks[0].clips[0].effects.push(e);
        let strength = format!("fx:{fid}:strength");
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": strength, "value": 0.9, "at_ms": 6_000})).unwrap();
        assert!(p.sequence().tracks[0].clips[0].animation.is_animated(TrackKey::fx(fid, "strength")));
        let err = |p: &Project, op: Value| run(&mut p.clone(), op).unwrap_err().message;
        assert!(err(&p, json!({"type": "set_keyframe", "clip_id": id, "param": strength, "value": 1.5, "at_ms": 7_000})).contains("out of range 0..1"));
        assert!(err(&p, json!({"type": "set_keyframe", "clip_id": id, "param": format!("fx:{fid}:direction"), "value": 1.0, "at_ms": 7_000})).contains("only numbers"));
        assert!(err(&p, json!({"type": "set_keyframe", "clip_id": id, "param": format!("fx:{}:strength", kadr_core::EffectId::new()), "value": 0.5, "at_ms": 7_000})).contains("no effect"));
        assert!(err(&p, json!({"type": "set_keyframe", "clip_id": id, "param": "fx:junk", "value": 0.5, "at_ms": 7_000})).contains("malformed"));
        run(&mut p, json!({"type": "clear_animation", "clip_id": id, "params": [strength]})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.effect(fid).unwrap().number("strength"), Some(0.6), "the effect's default");
    }

    /// A clip with a smooth zoom whose strength is keyframed at 6 s and 8 s (and nothing else).
    fn fx_only() -> (Project, String, kadr_core::EffectId) {
        let (mut p, id) = project();
        let cid = ClipId::parse(&id).unwrap();
        let e = kadr_effects::fresh(kadr_effects::recipe("smooth_zoom").unwrap(), p.sequence().clip(cid).unwrap(), Time::from_secs(6));
        let fid = e.id;
        p.sequence_mut().tracks[0].clips[0].effects.push(e);
        let strength = format!("fx:{fid}:strength");
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": strength, "value": 0.2, "at_ms": 6_000})).unwrap();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": strength, "value": 1.0, "at_ms": 8_000, "ease": "linear"})).unwrap();
        (p, id, fid)
    }

    /// Task 2 review: `clear_animation` without `params` resets effect tracks too.
    #[test]
    fn clear_animation_resets_a_clip_animated_only_through_an_effect() {
        let (mut p, id, fid) = fx_only();
        p.sequence_mut().tracks[0].clips[0].effects[0].params.insert("strength".into(), kadr_project::ParamValue::Number(0.3));
        run(&mut p, json!({"type": "clear_animation", "clip_id": id})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.effect(fid).unwrap().number("strength"), Some(0.6), "the registry's default, not the old static");
        // keep_ms freezes the effect number at that time.
        let (mut p, id, fid) = fx_only();
        let c = &p.sequence().tracks[0].clips[0];
        let at = c.timeline_time_of(c.animation.track(TrackKey::fx(fid, "strength"))[1].at.0).as_millis();
        run(&mut p, json!({"type": "clear_animation", "clip_id": id, "keep_ms": at})).unwrap();
        assert_eq!(p.sequence().tracks[0].clips[0].effect(fid).unwrap().number("strength"), Some(1.0));
    }

    /// A newer Kadr's effect is read-only (spec §2): every keyframe op that
    /// names one of its numbers says so instead of the engine's bare error.
    #[test]
    fn keyframe_ops_on_a_newer_kadrs_effect_say_why_not() {
        let (mut p, id) = project();
        let future = kadr_project::AppliedEffect::new("smooth_zoom", 9, [("strength".to_string(), kadr_project::ParamValue::Number(0.5))].into());
        let k = TrackKey::fx(future.id, "strength");
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        let at = SourceTime(c.source_time_at(Time::from_secs(6)));
        c.effects.push(future);
        c.animation.set_key(&k, at, 0.5, None);
        let key = k.key_string();
        for op in [
            json!({"type": "set_keyframe", "clip_id": id, "param": key, "value": 0.4, "at_ms": 7_000}),
            json!({"type": "remove_keyframe", "clip_id": id, "param": key, "at_ms": 6_000}),
            json!({"type": "set_keyframe_ease", "clip_id": id, "param": key, "at_ms": 6_000, "ease": "hold"}),
            json!({"type": "move_keyframes", "clip_id": id, "param": key, "from_ms": 6_000, "to_ms": 7_000}),
            json!({"type": "clear_animation", "clip_id": id, "params": [key]}),
        ] {
            let e = run(&mut p.clone(), op.clone()).unwrap_err().message;
            assert!(e.contains("newer Kadr") && e.contains("smooth_zoom"), "{op}: {e}");
        }
        // Without params: its track alone is no animation this version can clear…
        let e = run(&mut p.clone(), json!({"type": "clear_animation", "clip_id": id})).unwrap_err().message;
        assert!(e.contains("cannot edit"), "{e}");
        // …and next to a clip property it stays as it is.
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 4.0})).unwrap();
        run(&mut p, json!({"type": "clear_animation", "clip_id": id})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(!c.animation.is_animated(AnimParam::X));
        assert_eq!(c.animation.track(&k).len(), 1, "the newer effect's keyframe stayed");
    }

    /// Ruling R12: a static derived from keyframes is kept in the effect
    /// number's range like the engine keeps it — what the picture showed —
    /// so an out-of-range key from elsewhere can be cleared or removed in
    /// one op. Only a value the client gives is refused when out of range.
    #[test]
    fn an_out_of_range_static_for_an_effect_number_is_kept_in_its_range() {
        let (mut p, id, fid) = fx_only();
        let k = TrackKey::fx(fid, "strength");
        // A file from elsewhere: a keyframe beyond the parameter's range.
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        let late = c.animation.track(&k)[1].at;
        c.animation.set_key(&k, late, 1.5, None);
        let at = c.timeline_time_of(late.0).as_millis();
        let cmds = anim_op(&p, p.sequence(), &json!({"type": "clear_animation", "clip_id": id, "keep_ms": at})).unwrap().unwrap();
        assert!(matches!(&cmds[..], [EditCommand::ClearAnimation { statics, .. }] if statics[..] == [(k.clone(), 1.0)]), "the command carries the clamped value: {cmds:?}");
        let mut cleared = p.clone();
        run(&mut cleared, json!({"type": "clear_animation", "clip_id": id, "keep_ms": at})).unwrap();
        let c = &cleared.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.effect(fid).unwrap().number("strength"), Some(1.0), "the picture at keep_ms: clamped to 1");
        // Removing the last keyframe keeps its value, clamped, as the static one.
        let early = p.sequence().tracks[0].clips[0].animation.track(&k)[0].at;
        let mut last = p.clone();
        last.sequence_mut().tracks[0].clips[0].animation.remove_key(&k, early);
        run(&mut last, json!({"type": "remove_keyframe", "clip_id": id, "param": k.key_string(), "at_ms": at})).unwrap();
        let c = &last.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.effect(fid).unwrap().number("strength"), Some(1.0));
        // A value the client gives out of range is still refused.
        let e = run(&mut p.clone(), json!({"type": "set_keyframe", "clip_id": id, "param": k.key_string(), "value": 1.5, "at_ms": at})).unwrap_err().message;
        assert!(e.contains("out of range 0..1"), "{e}");
        // An in-range last keyframe goes, its value staying.
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        c.animation.remove_key(&k, early);
        c.animation.set_key(&k, late, 0.8, None);
        run(&mut p, json!({"type": "remove_keyframe", "clip_id": id, "param": k.key_string(), "at_ms": at})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(c.animation.is_empty());
        assert_eq!(c.effect(fid).unwrap().number("strength"), Some(0.8));
    }

    /// Ruling R8: moving every keyframe of a moment moves the editable ones
    /// and leaves a newer Kadr's effect's keyframes where they are; a moment
    /// of those alone is refused with the reason.
    #[test]
    fn move_keyframes_leaves_read_only_tracks_where_they_are() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "x", "at_ms": 6_000, "value": 1.0})).unwrap();
        let future = kadr_project::AppliedEffect::new("glitch_v2", 3, [("amount".to_string(), kadr_project::ParamValue::Number(0.5))].into());
        let newer = TrackKey::fx(future.id, "amount");
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        let at = c.animation.track(AnimParam::X)[0].at;
        c.effects.push(future);
        c.animation.set_key(&newer, at, 0.5, None);
        run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 8_000})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert_ne!(c.animation.track(AnimParam::X)[0].at, at, "x moved");
        assert_eq!(c.animation.track(&newer).iter().map(|k| k.at).collect::<Vec<_>>(), [at], "the newer effect's key stayed");
        let e = run(&mut p, json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 9_000})).unwrap_err().message;
        assert!(e.contains("belong to an effect this version cannot edit"), "{e}");
    }

    /// An orphan `fx:` track (its effect is not on the clip; only a file
    /// from elsewhere has one) is read-only too, and the refusal does not
    /// claim an effect exists.
    #[test]
    fn an_orphan_effect_track_is_read_only_and_named_so() {
        let (mut p, id) = project();
        let orphan = TrackKey::fx(kadr_core::EffectId::new(), "strength");
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        let at = SourceTime(c.source_time_at(Time::from_secs(6)));
        c.animation.set_key(&orphan, at, 0.5, None);
        for op in [json!({"type": "clear_animation", "clip_id": id}), json!({"type": "move_keyframes", "clip_id": id, "from_ms": 6_000, "to_ms": 8_000})] {
            let e = run(&mut p.clone(), op.clone()).unwrap_err().message;
            assert!(e.contains("read-only") && e.contains("no effect on the clip"), "{op}: {e}");
        }
        let e = run(&mut p, json!({"type": "remove_keyframe", "clip_id": id, "param": orphan.key_string(), "at_ms": 6_000})).unwrap_err().message;
        assert!(e.contains("no effect"), "{e}");
    }
}
