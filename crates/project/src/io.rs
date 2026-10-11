//! `.kadr` persistence with crash safety.
//!
//! Save = serialize → `<file>.tmp` → fsync → keep previous as `<file>.bak` →
//! rename tmp over target. A rename within one NTFS volume is atomic, so a
//! crash at any point leaves either the old or the new project intact.

use crate::model::{now_ms, Project, FORMAT_VERSION};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const EXTENSION: &str = "kadr";

#[derive(Debug, Error)]
pub enum ProjectIoError {
    #[error("i/o error on {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("project file {path} is corrupt: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("project was saved by a newer Kadr (format {found}, supported {supported})")]
    TooNew { found: u32, supported: u32 },
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ProjectIoError + '_ {
    move |source| ProjectIoError::Io { path: path.to_path_buf(), source }
}

pub fn to_json(project: &Project) -> Vec<u8> {
    serde_json::to_vec_pretty(project).expect("project serialization is infallible")
}

/// Writes bytes atomically to `path` (tmp + fsync + rename).
pub fn write_atomic(path: &Path, bytes: &[u8], keep_backup: bool) -> Result<(), ProjectIoError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let tmp = sidecar(path, "tmp");
    {
        let mut f = File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(bytes).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
    }
    if keep_backup && path.exists() {
        let bak = sidecar(path, "bak");
        // Best effort: a failed backup must not block saving.
        if let Err(e) = fs::copy(path, &bak) {
            tracing::warn!(error = %e, "could not write project backup");
        }
    }
    fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(())
}

/// Saves the project, updating relative media paths and the modified stamp.
pub fn save(project: &mut Project, path: &Path) -> Result<(), ProjectIoError> {
    project.modified_ms = now_ms();
    project.format_version = FORMAT_VERSION;
    let base = path.parent().unwrap_or(Path::new("."));
    for a in &mut project.assets {
        a.relative_path = relative_to(&a.path, base);
    }
    write_atomic(path, &to_json(project), true)?;
    // A successful explicit save supersedes any autosave.
    let _ = fs::remove_file(autosave_path(path));
    tracing::info!(path = %path.display(), "project saved");
    Ok(())
}

pub fn load(path: &Path) -> Result<Project, ProjectIoError> {
    let bytes = fs::read(path).map_err(io_err(path))?;
    let mut project = parse(&bytes, path)?;
    relink(&mut project, path.parent().unwrap_or(Path::new(".")));
    Ok(project)
}

pub fn parse(bytes: &[u8], path: &Path) -> Result<Project, ProjectIoError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|source| ProjectIoError::Parse { path: path.to_path_buf(), source })?;
    let found = value.get("format_version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if found > FORMAT_VERSION {
        return Err(ProjectIoError::TooNew { found, supported: FORMAT_VERSION });
    }
    let value = migrate(value, found);
    let mut project: Project = serde_json::from_value(value).map_err(|source| ProjectIoError::Parse { path: path.to_path_buf(), source })?;
    // D2: a transition whose clips no longer meet (a hand-edited file) goes, as an edit would remove it.
    for seq in &mut project.sequences {
        for t in seq.reconcile_transitions() {
            tracing::warn!(id = %t.id, recipe = %t.recipe, "dropping a transition whose clips do not meet at a cut");
        }
    }
    Ok(project)
}

/// Upgrades older JSON layouts in place. Format 1 is the first release;
/// format 2 replaced the never-evaluated string-keyed `keyframes` with
/// source-bound `animation` (keyframes spec §2.2); format 3 replaced the
/// never-evaluated `effects` with `AppliedEffect`s (effects spec §2).
/// Transitions kept their `kind` in format 3: they are converted by content
/// on every load (`migrate_transitions`, effects E3), without another
/// format bump.
fn migrate(mut value: serde_json::Value, from: u32) -> serde_json::Value {
    if from < 2 {
        for_each_clip(&mut value, migrate_clip_keyframes);
    }
    if from < 3 {
        for_each_clip(&mut value, migrate_clip_effects);
        // The upgraded document is a format-3 document in memory too.
        if let Some(obj) = value.as_object_mut() {
            obj.insert("format_version".into(), 3.into());
        }
    }
    // E3, on every load: transitions named by an old kind become recipes on their cut.
    for_each_sequence(&mut value, migrate_transitions);
    value
}

/// Every clip object of every sequence; malformed levels are skipped
/// (the parse after the migration reports them).
fn for_each_clip(value: &mut serde_json::Value, mut f: impl FnMut(&mut serde_json::Value)) {
    use serde_json::Value;
    let Some(seqs) = value.get_mut("sequences").and_then(Value::as_array_mut) else { return };
    for clip in seqs
        .iter_mut()
        .flat_map(|s| s.get_mut("tracks").and_then(Value::as_array_mut).into_iter().flatten())
        .flat_map(|t| t.get_mut("clips").and_then(Value::as_array_mut).into_iter().flatten())
    {
        f(clip);
    }
}

/// Every sequence object; malformed levels are skipped.
fn for_each_sequence(value: &mut serde_json::Value, mut f: impl FnMut(&mut serde_json::Value)) {
    let Some(seqs) = value.get_mut("sequences").and_then(serde_json::Value::as_array_mut) else { return };
    for s in seqs {
        f(s);
    }
}

/// A transition with `kind` and no `recipe` — as Kadr wrote them before E3,
/// its cut named by `track` and `at` — becomes the kind's recipe
/// (`TransitionKind::recipe`, D1) on the clip pair there: the clip of that
/// track ending at `at` (`from`) and the one starting there (`to`). One on
/// no cut (invisible before) is dropped with a log line; anything else
/// malformed is left to the lenient loader. Never panics.
fn migrate_transitions(seq: &mut serde_json::Value) {
    use serde_json::{json, Value};
    // (track id, clip id, timeline_in, timeline_out) of every readable clip.
    let clips: Vec<(Value, Value, i64, i64)> = seq
        .get("tracks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|t| {
            let track = t.get("id").cloned().unwrap_or(Value::Null);
            t.get("clips").and_then(Value::as_array).into_iter().flatten().filter_map(move |c| Some((track.clone(), c.get("id")?.clone(), c.get("timeline_in")?.as_i64()?, c.get("timeline_out")?.as_i64()?)))
        })
        .collect();
    let Some(list) = seq.get_mut("transitions").and_then(Value::as_array_mut) else { return };
    list.retain_mut(|tr| {
        let Some(o) = tr.as_object_mut() else { return true };
        if o.contains_key("recipe") {
            // A legacy entry — it still has the old `kind` — given a recipe
            // (a hand-edited file): the recipe names it, and its old fields
            // would land in `extra` and be written back (E3-R4), so they go.
            // Without `kind`, `track` and `at` are a newer Kadr's: kept.
            if o.remove("kind").is_some() {
                o.remove("track");
                o.remove("at");
            }
            return true;
        }
        let Some(kind) = o.get("kind").and_then(|k| serde_json::from_value::<crate::TransitionKind>(k.clone()).ok()) else { return true };
        let pair = o.get("track").cloned().zip(o.get("at").and_then(Value::as_i64)).and_then(|(track, at)| {
            let from = clips.iter().find(|c| c.0 == track && c.3 == at)?.1.clone();
            let to = clips.iter().find(|c| c.0 == track && c.2 == at)?.1.clone();
            Some((from, to))
        });
        let Some((from, to)) = pair else {
            tracing::warn!(transition = ?o.get("id"), "dropping an old transition that sits on no cut");
            return false;
        };
        let (recipe, params) = kind.recipe();
        for k in ["kind", "track", "at"] {
            o.remove(k);
        }
        o.insert("recipe".into(), json!(recipe));
        o.insert("version".into(), json!(1));
        o.insert("params".into(), serde_json::to_value(&params).unwrap_or_else(|_| json!({})));
        o.insert("from".into(), from);
        o.insert("to".into(), to);
        true
    });
}

/// Format 2 → 3: `effects: [{id, kind, enabled, params: {name: number}}]`
/// becomes `AppliedEffect`s — `recipe = kind`, numbers become
/// `{"number": v}`, version 1, full progress. An unknown `kind` is an
/// unknown recipe (kept, not run); an entry that is not an object stays as
/// it is and the lenient loader drops it.
fn migrate_clip_effects(clip: &mut serde_json::Value) {
    use serde_json::{json, Value};
    let Some(list) = clip.get_mut("effects").and_then(Value::as_array_mut) else { return };
    for e in list.iter_mut() {
        let Some(o) = e.as_object_mut() else { continue };
        if let Some(kind) = o.remove("kind") {
            o.entry("recipe").or_insert(kind);
        }
        if let Some(params) = o.get_mut("params").and_then(Value::as_object_mut) {
            for v in params.values_mut() {
                if v.is_number() {
                    *v = json!({ "number": v.clone() });
                }
            }
        }
        o.entry("version").or_insert(json!(1));
    }
}

fn migrate_clip_keyframes(clip: &mut serde_json::Value) {
    use serde_json::{json, Map, Value};
    let Some(Value::Array(old)) = clip.as_object_mut().and_then(|o| o.remove("keyframes")) else { return };
    let num = |k: &str| clip.get(k).and_then(Value::as_i64).unwrap_or(0);
    let (sin, sout, tin, tout) = (num("source_in"), num("source_out"), num("timeline_in"), num("timeline_out"));
    let mut anim: Map<String, Value> = Map::new();
    for k in old {
        let prop = k.get("property").and_then(Value::as_str).unwrap_or_default().to_string();
        let param = match prop.as_str() {
            "transform.x" => Some("x"),
            "transform.y" => Some("y"),
            "transform.scale" => Some("scale"),
            "transform.rotation_deg" => Some("rotation"),
            "transform.opacity" => Some("opacity"),
            "transform.crop_left" => Some("crop_left"),
            "transform.crop_right" => Some("crop_right"),
            "transform.crop_top" => Some("crop_top"),
            "transform.crop_bottom" => Some("crop_bottom"),
            "color.exposure" => Some("exposure"),
            "color.contrast" => Some("contrast"),
            "color.saturation" => Some("saturation"),
            "color.temperature" => Some("temperature"),
            _ => None,
        };
        let entry = match param {
            Some(p) => {
                let rel = k.get("time").and_then(Value::as_i64).unwrap_or(0);
                // A garbage time must not overflow the load: it stays beyond
                // MAX_KEY_FLICKS, so the track loads as unknown (kept, not run).
                let at = if rel.unsigned_abs() > crate::MAX_KEY_FLICKS {
                    kadr_core::Time(rel)
                } else {
                    let rel = kadr_core::Time(rel).mul_ratio(sout.saturating_sub(sin), tout.saturating_sub(tin).max(1));
                    kadr_core::Time(sin.saturating_add(rel.flicks()))
                };
                let ease = k.get("interpolation").cloned().unwrap_or(json!("ease_in_out"));
                (p.to_string(), json!({"at": at.flicks(), "value": k.get("value").cloned().unwrap_or(Value::Null), "ease": ease}))
            }
            None => (prop, k),
        };
        anim.entry(entry.0).or_insert_with(|| json!([])).as_array_mut().unwrap().push(entry.1);
    }
    if !anim.is_empty() {
        clip["animation"] = Value::Object(anim);
    }
}

/// If an absolute media path no longer exists but the path relative to the
/// project does, switch to it (project folder was moved/copied).
fn relink(project: &mut Project, base: &Path) {
    for a in &mut project.assets {
        if a.path.exists() {
            continue;
        }
        if let Some(rel) = &a.relative_path {
            let candidate = base.join(rel);
            if candidate.exists() {
                tracing::info!(from = %a.path.display(), to = %candidate.display(), "relinked media");
                a.path = candidate;
            }
        }
    }
}

fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    // Only handles media inside the project folder tree; anything else keeps
    // its absolute path.
    path.strip_prefix(base).ok().map(Path::to_path_buf)
}

fn sidecar(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

pub fn autosave_path(project_path: &Path) -> PathBuf {
    sidecar(project_path, "autosave")
}

pub fn write_autosave(project: &Project, autosave: &Path) -> Result<(), ProjectIoError> {
    write_atomic(autosave, &to_json(project), false)
}

/// Returns the autosave file if it is newer than the project file — i.e. the
/// previous session ended without saving (crash, power loss).
pub fn recovery_candidate(project_path: &Path) -> Option<PathBuf> {
    let auto = autosave_path(project_path);
    let auto_m = fs::metadata(&auto).and_then(|m| m.modified()).ok()?;
    match fs::metadata(project_path).and_then(|m| m.modified()) {
        Ok(proj_m) if proj_m >= auto_m => None,
        _ => Some(auto),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.kadr");
        write_atomic(&p, b"one", true).unwrap();
        write_atomic(&p, b"two", true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read(sidecar(&p, "bak")).unwrap(), b"one");
        assert!(!sidecar(&p, "tmp").exists());
    }

    #[test]
    fn rejects_newer_format() {
        let json = format!(r#"{{"format_version": {}}}"#, FORMAT_VERSION + 1);
        let err = parse(json.as_bytes(), Path::new("x.kadr")).unwrap_err();
        assert!(matches!(err, ProjectIoError::TooNew { .. }));
    }

    /// Format 1 clip JSON with the old string-keyed keyframes, 2x speed:
    /// source 10..30 s over timeline 0..10 s.
    fn v1_project() -> serde_json::Value {
        let p = crate::Project::new("old");
        let mut v = serde_json::to_value(&p).unwrap();
        v["format_version"] = serde_json::json!(1);
        let f = |s: f64| (s * kadr_core::FLICKS_PER_SECOND as f64) as i64;
        let clip = serde_json::json!({
            "id": kadr_core::ClipId::new(), "asset": kadr_core::AssetId::new(), "name": "c",
            "source_in": f(10.0), "source_out": f(30.0), "timeline_in": 0, "timeline_out": f(10.0),
            "keyframes": [
                {"property": "transform.scale", "time": f(1.0), "value": 1.2, "interpolation": "linear"},
                {"property": "color.exposure", "time": 0, "value": 0.5, "interpolation": "hold"},
                {"property": "audio.gain_db", "time": 0, "value": -6.0, "interpolation": "ease_in_out"}
            ]
        });
        v["sequences"][0]["tracks"][0]["clips"] = serde_json::json!([clip]);
        v
    }

    #[test]
    fn format_1_keyframes_migrate_to_source_bound_animation() {
        let bytes = serde_json::to_vec(&v1_project()).unwrap();
        let p = parse(&bytes, Path::new("old.kadr")).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        let scale = c.animation.track(crate::AnimParam::Scale);
        assert_eq!(scale.len(), 1);
        assert_eq!(scale[0].at, crate::SourceTime(kadr_core::Time::from_secs(12)), "timeline 1 s at 2x = source 12 s");
        assert_eq!((scale[0].value, &scale[0].ease), (1.2, &crate::Ease::Linear));
        assert_eq!(c.animation.track(crate::AnimParam::Exposure)[0].ease, crate::Ease::Hold);
        assert_eq!(c.animation.unknown_len(), 1, "audio.gain_db is not a V1 animation param: kept, not run");
        let saved = serde_json::to_value(&p).unwrap();
        assert!(saved["sequences"][0]["tracks"][0]["clips"][0].get("keyframes").is_none(), "old field never written");
        assert_eq!(saved["format_version"], serde_json::json!(FORMAT_VERSION));
    }

    /// R25: a garbage old keyframe time neither panics the migration nor runs.
    #[test]
    fn a_format_1_keyframe_with_a_garbage_time_loads_as_unknown() {
        let mut v = v1_project();
        v["sequences"][0]["tracks"][0]["clips"][0]["keyframes"][0]["time"] = serde_json::json!(i64::MAX);
        let p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("old.kadr")).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        assert!(!c.animation.is_animated(crate::AnimParam::Scale), "never run");
        assert_eq!(c.animation.unknown_len(), 2, "kept with audio.gain_db");
        assert!(c.animation.is_animated(crate::AnimParam::Exposure), "the rest migrates");
    }

    #[test]
    fn a_format_3_file_is_too_new_for_a_format_2_reader() {
        assert_eq!(FORMAT_VERSION, 3);
        let json = format!(r#"{{"format_version": {}}}"#, FORMAT_VERSION + 1);
        assert!(matches!(parse(json.as_bytes(), Path::new("x.kadr")).unwrap_err(), ProjectIoError::TooNew { .. }));
    }

    /// Format 2 clip JSON with the old, never-evaluated effects list.
    fn v2_project() -> serde_json::Value {
        let p = crate::Project::new("old");
        let mut v = serde_json::to_value(&p).unwrap();
        v["format_version"] = serde_json::json!(2);
        let f = |s: i64| s * kadr_core::FLICKS_PER_SECOND;
        let clip = serde_json::json!({
            "id": kadr_core::ClipId::new(), "asset": kadr_core::AssetId::new(), "name": "c",
            "source_in": 0, "source_out": f(10), "timeline_in": 0, "timeline_out": f(10),
            "effects": [
                {"id": kadr_core::EffectId::new(), "kind": "blur", "enabled": false, "params": {"radius": 4.5}},
                {"id": kadr_core::EffectId::new(), "kind": "smooth_zoom", "enabled": true, "params": {"strength": 1}}
            ]
        });
        v["sequences"][0]["tracks"][0]["clips"] = serde_json::json!([clip]);
        v
    }

    #[test]
    fn format_2_effects_migrate_to_applied_effects() {
        let p = parse(&serde_json::to_vec(&v2_project()).unwrap(), Path::new("old.kadr")).unwrap();
        let fx = &p.sequence().tracks[0].clips[0].effects;
        assert_eq!(fx.len(), 2);
        assert_eq!((fx[0].recipe.as_str(), fx[0].enabled, fx[0].version, fx[0].progress), ("blur", false, 1, (0.0, 1.0)), "an unknown kind is an unknown recipe: kept");
        assert_eq!(fx[0].params["radius"], crate::ParamValue::Number(4.5));
        assert_eq!(fx[1].params["strength"], crate::ParamValue::Number(1.0));
        let saved = serde_json::to_value(&p).unwrap();
        assert!(saved["sequences"][0]["tracks"][0]["clips"][0]["effects"][0].get("kind").is_none(), "the old field is never written");
        assert_eq!(saved["format_version"], serde_json::json!(3));
    }

    /// Two clips on V1 of `v2_project` meeting at 10 s; returns their ids.
    fn with_pair(v: &mut serde_json::Value) -> (kadr_core::ClipId, kadr_core::ClipId) {
        let f = |s: i64| s * kadr_core::FLICKS_PER_SECOND;
        let (a, b) = (kadr_core::ClipId::new(), kadr_core::ClipId::new());
        let clip = |id: kadr_core::ClipId, tin: i64| serde_json::json!({"id": id, "asset": kadr_core::AssetId::new(), "name": "c", "source_in": 0, "source_out": f(10), "timeline_in": f(tin), "timeline_out": f(tin + 10)});
        v["sequences"][0]["tracks"][0]["clips"] = serde_json::json!([clip(a, 0), clip(b, 10)]);
        (a, b)
    }

    /// D1, on every load (format 2 and 3 alike): the old kinds become their
    /// recipes on the clip pair at their cut; one on no cut is dropped; the
    /// old fields are never written again.
    #[test]
    fn old_transition_kinds_become_recipes_on_their_cut() {
        let f = |s: i64| s * kadr_core::FLICKS_PER_SECOND;
        for format in [2, 3] {
            let mut v = v2_project();
            v["format_version"] = serde_json::json!(format);
            let (a, b) = with_pair(&mut v);
            let track = v["sequences"][0]["tracks"][0]["id"].clone();
            let tr = |kind: &str, at: i64| serde_json::json!({"id": kadr_core::TransitionId::new(), "kind": kind, "track": track, "at": at, "duration": f(1)});
            for (kind, recipe, params) in [
                ("cross_dissolve", "clean_fade", serde_json::json!({"style": {"choice": "clean"}})),
                ("dip_to_black", "fade_through_color", serde_json::json!({"color": {"choice": "black"}})),
                ("wipe", "wipe", serde_json::json!({"direction": {"choice": "left"}, "softness": {"number": 0.0}})),
            ] {
                v["sequences"][0]["transitions"] = serde_json::json!([tr(kind, f(10)), tr(kind, f(5))]);
                let p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("old.kadr")).unwrap();
                let t = &p.sequence().transitions;
                assert_eq!(t.len(), 1, "{kind}: the one at 5 s sat on no cut (invisible before): dropped");
                assert_eq!((t[0].recipe.as_str(), t[0].version, t[0].from, t[0].to, t[0].duration), (recipe, 1, a, b, kadr_core::Time::from_secs(1)), "{kind}");
                let saved = serde_json::to_value(&p).unwrap();
                let s = &saved["sequences"][0]["transitions"][0];
                assert_eq!(s["params"], params, "{kind}");
                assert!(s.get("kind").is_none() && s.get("track").is_none() && s.get("at").is_none(), "{kind}: the old fields are never written");
                assert_eq!(saved["format_version"], serde_json::json!(3), "no new format (R2)");
            }
        }
    }

    /// E3-R4: a transition with both a `recipe` and the old `kind` (a
    /// hand-edited file) is read by its recipe; `kind`, `track` and `at`
    /// are dropped, never kept in `extra` and written back. Only a legacy
    /// entry loses them: beside a recipe alone they are a newer Kadr's
    /// fields, kept (E3-R6).
    #[test]
    fn old_fields_beside_a_recipe_are_dropped() {
        let f = |s: i64| s * kadr_core::FLICKS_PER_SECOND;
        let mut v = v2_project();
        let (a, b) = with_pair(&mut v);
        let track = v["sequences"][0]["tracks"][0]["id"].clone();
        v["sequences"][0]["transitions"] = serde_json::json!([{"id": kadr_core::TransitionId::new(), "recipe": "wipe", "kind": "cross_dissolve", "track": track, "at": f(10), "from": a, "to": b, "duration": f(1)}]);
        let p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("both.kadr")).unwrap();
        let t = &p.sequence().transitions;
        assert_eq!((t.len(), t[0].recipe.as_str(), t[0].from, t[0].to, t[0].extra.is_empty()), (1, "wipe", a, b, true));
        let saved = serde_json::to_value(&p).unwrap();
        let s = &saved["sequences"][0]["transitions"][0];
        assert!(s.get("kind").is_none() && s.get("track").is_none() && s.get("at").is_none(), "the old fields are never written: {s}");

        v["sequences"][0]["transitions"] = serde_json::json!([{"id": kadr_core::TransitionId::new(), "recipe": "wipe", "track": "V1", "at": 5, "from": a, "to": b, "duration": f(1)}]);
        let p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("newer.kadr")).unwrap();
        let s = &serde_json::to_value(&p).unwrap()["sequences"][0]["transitions"][0];
        assert_eq!((s["track"].clone(), s["at"].clone()), (serde_json::json!("V1"), serde_json::json!(5)), "no `kind`: a newer Kadr's fields, kept");
    }

    /// Bad transition data never fails the project and never panics.
    #[test]
    fn malformed_transitions_are_dropped_not_fatal() {
        for transitions in [
            serde_json::json!("x"),
            serde_json::json!(null),
            serde_json::json!([7, "s", null, [1]]),
            serde_json::json!([{"kind": "cross_dissolve"}]),
            serde_json::json!([{"kind": "iris", "track": "t", "at": 0, "duration": 5}]),
            serde_json::json!([{"kind": "wipe", "track": 5, "at": "x"}]),
            serde_json::json!([{"recipe": "clean_fade"}]),
        ] {
            let mut v = v2_project();
            with_pair(&mut v);
            v["sequences"][0]["transitions"] = transitions.clone();
            let p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("bad.kadr")).unwrap_or_else(|e| panic!("{transitions}: {e}"));
            assert!(p.sequence().transitions.is_empty(), "{transitions}");
        }
    }

    /// Spec §2: a transition from a newer Kadr — recipe, version, an unknown
    /// parameter variant, a field — survives parse → save byte for byte.
    #[test]
    fn a_transition_from_a_newer_kadr_survives_parse_and_save() {
        let mut p = crate::Project::new("future");
        let f = kadr_core::Time::from_secs;
        let a = crate::Clip::new(kadr_core::AssetId::new(), "a", kadr_core::TimeRange::new(f(0), f(4)), f(0));
        let b = crate::Clip::new(kadr_core::AssetId::new(), "b", kadr_core::TimeRange::new(f(0), f(4)), f(4));
        let params = [("amount".to_string(), crate::ParamValue::Number(0.4)), ("shape".to_string(), crate::ParamValue::Unknown(serde_json::json!({"star": 5})))].into();
        let mut t = crate::Transition::new("iris_v2", 3, params, a.id, b.id, f(1));
        t.extra.insert("mask".into(), serde_json::json!({"feather": 0.2}));
        p.sequence_mut().tracks[0].clips.extend([a, b]);
        p.sequence_mut().transitions.push(t);
        let saved = to_json(&p);
        let loaded = parse(&saved, Path::new("future.kadr")).unwrap();
        assert_eq!(to_json(&loaded), saved, "parse → save gives the same bytes");
        assert_eq!(loaded, p);
    }

    /// D2 at load: a saved transition whose clips no longer meet (a
    /// hand-edited file) is dropped, as an edit would have removed it.
    #[test]
    fn a_saved_transition_whose_clips_do_not_meet_is_dropped_on_load() {
        let mut p = crate::Project::new("gap");
        let f = kadr_core::Time::from_secs;
        let a = crate::Clip::new(kadr_core::AssetId::new(), "a", kadr_core::TimeRange::new(f(0), f(4)), f(0));
        let b = crate::Clip::new(kadr_core::AssetId::new(), "b", kadr_core::TimeRange::new(f(0), f(4)), f(5));
        p.sequence_mut().transitions.push(crate::Transition::legacy(crate::TransitionKind::Wipe, a.id, b.id, f(1)));
        p.sequence_mut().tracks[0].clips.extend([a, b]);
        assert!(parse(&to_json(&p), Path::new("gap.kadr")).unwrap().sequence().transitions.is_empty());
    }

    /// R25: a garbage old effects list never panics the migration.
    #[test]
    fn malformed_format_2_effects_never_panic() {
        for effects in [r#""x""#, "[7]", r#"[{"kind": 5}]"#, r#"[{"id": "nope", "kind": "a"}]"#, r#"[{"kind": "a", "params": [1]}]"#, "null"] {
            let json = format!(r#"{{"format_version":2,"sequences":[{{"tracks":[{{"clips":[{{"effects":{effects}}}]}}]}}]}}"#);
            let r = parse(json.as_bytes(), Path::new("bad.kadr"));
            // The rest of this skeleton is not a project: a parse error, never a panic.
            assert!(matches!(r, Err(ProjectIoError::Parse { .. })), "{effects}: {r:?}");
        }
    }

    /// Migration leniency (plan, Global Constraints): a valid format-2
    /// project whose old effects list holds garbage loads, the garbage
    /// dropped and a readable entry kept — bad effect data never costs the
    /// project.
    #[test]
    fn garbage_in_a_format_2_effects_list_is_dropped_not_fatal() {
        let ok = serde_json::json!({"id": kadr_core::EffectId::new(), "kind": "smooth_zoom", "params": {"strength": 0.5}});
        for (effects, kept) in [
            (serde_json::json!("x"), 0),
            (serde_json::json!(null), 0),
            (serde_json::json!({"kind": "blur"}), 0),
            (serde_json::json!([7, "s", null, [1]]), 0),
            (serde_json::json!([{"kind": 5}, {"id": "nope", "kind": "a"}, {"kind": "a", "params": [1]}]), 0),
            (serde_json::json!([{"id": kadr_core::EffectId::new(), "kind": "a", "enabled": "yes"}, ok.clone(), 7]), 1),
        ] {
            let mut v = v2_project();
            v["sequences"][0]["tracks"][0]["clips"][0]["effects"] = effects.clone();
            let p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("old.kadr")).unwrap_or_else(|e| panic!("{effects}: {e}"));
            let fx = &p.sequence().tracks[0].clips[0].effects;
            assert_eq!(fx.len(), kept, "{effects}: {fx:?}");
            if kept == 1 {
                assert_eq!((fx[0].recipe.as_str(), &fx[0].params["strength"]), ("smooth_zoom", &crate::ParamValue::Number(0.5)));
            }
            assert_eq!(serde_json::to_value(&p).unwrap()["format_version"], serde_json::json!(3));
        }
    }

    #[test]
    fn malformed_pre_v2_structure_is_a_parse_error_not_a_panic() {
        for json in [
            r#"{"format_version":1,"sequences":[1]}"#,
            r#"{"format_version":1,"sequences":[{"tracks":["x"]}]}"#,
            r#"{"format_version":1,"sequences":[{"tracks":[{"clips":[7]}]}]}"#,
        ] {
            let r = parse(json.as_bytes(), Path::new("bad.kadr"));
            assert!(matches!(r, Err(ProjectIoError::Parse { .. })), "{json}: {r:?}");
        }
    }
}
