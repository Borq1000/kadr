//! MCP `edit` ops for keyframes and clip fades (keyframes spec §7). Times
//! arrive as milliseconds and are turned into exact `Time`/`SourceTime`
//! at once; nothing below this module sees milliseconds.
//!
//! Like every `edit` op these are resolved against the project as it was
//! before the call, so a later op in the same batch does not see keyframes
//! an earlier one created.

use kadr_core::{ClipId, Time};
use kadr_mcp_bridge::BridgeError;
use kadr_project::{AnimParam, Clip, EdgeFades, Ease, Project, Sequence, SourceTime, TrackKind};
use kadr_timeline::{ClipProperty, EditCommand};
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

fn param_named(k: &str) -> Result<AnimParam, BridgeError> {
    AnimParam::from_key(k).ok_or_else(|| bad(format!("unknown param {k:?} (one of {})", AnimParam::ALL.map(|p| p.key()).join(", "))))
}

fn param(op: &Value) -> Result<AnimParam, BridgeError> {
    param_named(op.get("param").and_then(Value::as_str).unwrap_or_default())
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

fn half(seq: &Sequence) -> Time {
    Time(seq.frame_rate.frame_duration().flicks() / 2)
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
/// only keys inside the clip count (hidden ones are `source_ms` business).
fn find(seq: &Sequence, c: &Clip, p: Option<AnimParam>, op: &Value, at: &str, src: &str) -> Result<Option<SourceTime>, BridgeError> {
    let h = half(seq);
    Ok(match when(op, at, src)? {
        When::Timeline(ms) => {
            // The raw time, not the snapped one: a visible key in the clip's last half frame
            // is still reachable by its echoed ms.
            let t = Time::from_millis(ms);
            if t < c.timeline_in - h || t >= c.timeline_out + h {
                return Err(outside(c, ms));
            }
            let params = p.map_or_else(|| c.animation.params(), |p| vec![p]);
            params
                .into_iter()
                .flat_map(|q| c.animation.track(q).iter().map(|k| k.at))
                .map(|a| (a, c.timeline_time_of(a.0)))
                .filter(|(_, tl)| *tl >= c.timeline_in && *tl < c.timeline_out && (*tl - t).abs() < h)
                .min_by_key(|(_, tl)| (*tl - t).abs())
                .map(|(a, _)| a)
        }
        When::Source(ms) => {
            let s = Time::from_millis(ms);
            c.animation.times().into_iter().filter(|a| p.is_none_or(|q| c.animation.params_at(*a).contains(&q))).filter(|a| (a.0 - s).abs() < h).min_by_key(|a| (a.0 - s).abs())
        }
    })
}

/// Like `find`, but a missing keyframe is an error.
fn existing(seq: &Sequence, c: &Clip, p: Option<AnimParam>, op: &Value, at: &str, src: &str) -> Result<SourceTime, BridgeError> {
    find(seq, c, p, op, at, src)?.ok_or_else(|| {
        bad("no keyframe there (looked within half a frame of the given time; keyframes made by an earlier op of the same call are not visible yet)")
    })
}

/// The accepted `value` range of a property (same limits as `set_transform`
/// for the transform params; fractions of the frame for crops).
fn value_range(p: AnimParam) -> (f64, f64) {
    match p {
        AnimParam::X | AnimParam::Y => (-20_000.0, 20_000.0),
        AnimParam::Scale => (0.01, 16.0),
        AnimParam::Rotation => (-3600.0, 3600.0),
        AnimParam::Opacity | AnimParam::CropLeft | AnimParam::CropRight | AnimParam::CropTop | AnimParam::CropBottom => (0.0, 1.0),
        AnimParam::Exposure => (-2.0, 2.0),
        AnimParam::Contrast | AnimParam::Saturation => (0.0, 2.0),
        AnimParam::Temperature => (-1.0, 1.0),
    }
}

fn set_keyframe(seq: &Sequence, id: ClipId, c: &Clip, op: &Value) -> Result<EditCommand, BridgeError> {
    let p = param(op)?;
    let value = op.get("value").and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad("value missing or not a number"))?;
    let (lo, hi) = value_range(p);
    if !(lo..=hi).contains(&value) {
        return Err(bad(format!("{} out of range {lo}..{hi} (got {value})", p.key())));
    }
    let ease = ease(op)?;
    // A key within half a frame is updated, not shadowed by a near-duplicate.
    let at = match find(seq, c, Some(p), op, "at_ms", "source_ms")? {
        Some(at) => {
            if c.animation.track(p).iter().any(|k| k.at == at && k.value == value && ease.as_ref().is_none_or(|e| *e == k.ease)) {
                return Err(bad("no change: that keyframe already has this value and ease"));
            }
            at
        }
        None => new_time(seq, c, op, "at_ms", "source_ms")?,
    };
    Ok(EditCommand::SetKeyframe { clip: id, param: p, at, value, ease })
}

fn clear_animation(seq: &Sequence, id: ClipId, c: &Clip, op: &Value) -> Result<EditCommand, BridgeError> {
    let listed = match op.get("params").filter(|v| !v.is_null()) {
        None => None,
        Some(Value::Array(a)) => Some(a.iter().map(|v| param_named(v.as_str().unwrap_or_default())).collect::<Result<Vec<_>, _>>()?),
        Some(_) => return Err(bad("`params` must be an array of param names")),
    };
    // Only animated properties are reset; a static value is not the animation's business.
    let animated: Vec<AnimParam> = match &listed {
        None => c.animation.params(),
        Some(l) => c.animation.params().into_iter().filter(|p| l.contains(p)).collect(),
    };
    if animated.is_empty() {
        return Err(bad(if listed.is_some() { "clip has no animation on the listed params" } else { "clip has no animation" }));
    }
    let keep = ms_arg(op, "keep_ms")?.map(|ms| seq.frame_rate.snap(Time::from_millis(ms)));
    let statics = animated.into_iter().map(|p| (p, keep.map_or(p.default_value(), |t| c.anim_value(p, t)))).collect();
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
            "remove_keyframe" => {
                let p = param(op)?;
                let at = existing(seq, c, Some(p), op, "at_ms", "source_ms")?;
                EditCommand::RemoveKeyframe { clip: id, param: p, at, keep: None }
            }
            "set_keyframe_ease" => {
                let p = param(op)?;
                let ease = ease(op)?.ok_or_else(|| bad("ease missing"))?;
                let at = existing(seq, c, Some(p), op, "at_ms", "source_ms")?;
                if c.animation.track(p).iter().any(|k| k.at == at && k.ease == ease) {
                    return Err(bad("no change: that keyframe already has this ease"));
                }
                EditCommand::SetKeyframeEase { clip: id, param: p, at, ease }
            }
            "move_keyframes" => {
                let p = op.get("param").filter(|v| !v.is_null()).map(|_| param(op)).transpose()?;
                let from = existing(seq, c, p, op, "from_ms", "from_source_ms")?;
                let to_ms = ms_arg(op, "to_ms")?.ok_or_else(|| bad("to_ms missing"))?;
                let to = SourceTime(c.source_time_at(at_timeline(seq, c, to_ms)?));
                if to == from {
                    return Err(bad("no change: the keyframes are already at that moment"));
                }
                let params = p.map_or_else(|| c.animation.params_at(from), |p| vec![p]);
                if let Some(q) = params.iter().find(|q| c.animation.track(**q).iter().any(|k| k.at == to)) {
                    return Err(bad(format!("{} already has a keyframe at {to_ms} ms; move or remove it first", q.key())));
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
}
