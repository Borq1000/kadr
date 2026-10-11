//! Effects applied to a clip (effects spec §2). A recipe is named by id;
//! what it does lives in `kadr-effects`. Everything here is data: a recipe,
//! a parameter variant or a field this build does not know is kept
//! verbatim and never evaluated (spec §2 «Прямая совместимость»).

use crate::{Clip, SourceTime, MAX_KEY_FLICKS};
use kadr_core::{EffectId, Time};
use serde::de::Deserializer;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// One parameter value of an applied effect. The recipe's `ParamDef`
/// (`kadr-effects`) says which variant a parameter takes.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    /// Strength 0..1, a duration in seconds…
    Number(f64),
    /// One of the recipe's options: a direction, a preset, a loudness target.
    Choice(String),
    /// A point of the clip's picture as fractions 0..1 (0, 0 = top left).
    Point { x: f64, y: f64 },
    /// Straight RGBA, components 0..1.
    Color([f64; 4]),
    /// A moment of the source media («Момент» of Punch In): source-bound.
    Moment(SourceTime),
    /// A variant from a newer Kadr (or a shape this build cannot read): kept, never used.
    Unknown(Value),
}

impl ParamValue {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            ParamValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A known variant, or `None` (the caller keeps the JSON as `Unknown`).
    fn from_json(v: &Value) -> Option<ParamValue> {
        let o = v.as_object().filter(|o| o.len() == 1)?;
        let (k, p) = o.iter().next()?;
        Some(match k.as_str() {
            "number" => ParamValue::Number(p.as_f64().filter(|n| n.is_finite())?),
            "choice" => ParamValue::Choice(p.as_str()?.to_string()),
            "point" => {
                let o = p.as_object().filter(|o| o.len() == 2)?;
                ParamValue::Point { x: o.get("x")?.as_f64()?, y: o.get("y")?.as_f64()? }
            }
            "color" => {
                let a = p.as_array().filter(|a| a.len() == 4)?;
                let c: Vec<f64> = a.iter().map(Value::as_f64).collect::<Option<_>>()?;
                ParamValue::Color([c[0], c[1], c[2], c[3]])
            }
            // A moment beyond ±MAX_KEY_FLICKS would overflow the time conversions: kept, not used.
            "moment" => ParamValue::Moment(SourceTime(Time(p.as_i64().filter(|f| f.unsigned_abs() <= MAX_KEY_FLICKS)?))),
            _ => return None,
        })
    }
}

impl Serialize for ParamValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (k, v) = match self {
            ParamValue::Unknown(v) => return v.serialize(s),
            ParamValue::Number(n) => ("number", json!(n)),
            ParamValue::Choice(c) => ("choice", json!(c)),
            ParamValue::Point { x, y } => ("point", json!({"x": x, "y": y})),
            ParamValue::Color(c) => ("color", json!(c)),
            ParamValue::Moment(m) => ("moment", json!(m.0.flicks())),
        };
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry(k, &v)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for ParamValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(ParamValue::from_json(&v).unwrap_or(ParamValue::Unknown(v)))
    }
}

fn yes() -> bool {
    true
}
fn one() -> u16 {
    1
}
fn full() -> (f64, f64) {
    (0.0, 1.0)
}
fn is_full(p: &(f64, f64)) -> bool {
    *p == full()
}

/// An effect on a clip (spec §2). The clip's `effects` list is the order
/// of application.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppliedEffect {
    pub id: EffectId,
    /// Registry id, e.g. "smooth_zoom" (`kadr-effects`).
    pub recipe: String,
    /// The recipe's `compile` version this effect was made with.
    #[serde(default = "one")]
    pub version: u16,
    #[serde(default)]
    pub params: BTreeMap<String, ParamValue>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Spec §1: the part of the effect's progress this clip shows.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub progress: (f64, f64),
    /// Fields from a newer Kadr, kept verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl AppliedEffect {
    pub fn new(recipe: impl Into<String>, version: u16, params: BTreeMap<String, ParamValue>) -> Self {
        AppliedEffect { id: EffectId::new(), recipe: recipe.into(), version, params, enabled: true, progress: full(), extra: BTreeMap::new() }
    }

    /// Spec §1: the progress at `frac` (0..1) of the clip's visible length.
    pub fn progress_at(&self, frac: f64) -> f64 {
        let (from, to) = self.progress;
        from + (to - from) * frac
    }

    /// The halves' ranges for a split at `frac` of the visible length:
    /// `(from, c)` and `(c, to)`, `c = progress_at(frac)` — the picture at
    /// the joint stays as it was (spec §1). A cut lies inside the clip, so
    /// `frac` is clamped into 0..1 (a non-finite one — a zero-length clip's
    /// 0 / 0 — counts as 0), and a cut that still is not finite (a range
    /// too wide for f64 arithmetic) falls back to `from`: a non-finite
    /// progress would not survive a save (written `null`, the effect
    /// dropped on reload).
    pub fn split_progress(&self, frac: f64) -> ((f64, f64), (f64, f64)) {
        let frac = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
        let c = Some(self.progress_at(frac)).filter(|c| c.is_finite()).unwrap_or(self.progress.0);
        ((self.progress.0, c), (c, self.progress.1))
    }

    /// A `Number` parameter's stored value.
    pub fn number(&self, key: &str) -> Option<f64> {
        self.params.get(key).and_then(ParamValue::as_number)
    }
}

/// `Clip::effects` from a file; never fails the load. An entry that is not
/// an effect at all (not an object, no `id` or `recipe`, a field of the
/// wrong type) is dropped with a warning, and so is a later entry with an
/// id already seen (ids are unique per clip). Unknown recipes and fields
/// survive (spec §2).
pub fn deserialize_effects<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<AppliedEffect>, D::Error> {
    let Value::Array(items) = Value::deserialize(d)? else {
        tracing::warn!("ignoring clip effects that are not a list");
        return Ok(vec![]);
    };
    let mut out: Vec<AppliedEffect> = Vec::with_capacity(items.len());
    for item in items {
        match serde_json::from_value::<AppliedEffect>(item) {
            Ok(e) if out.iter().any(|x| x.id == e.id) => tracing::warn!(id = %e.id, "dropping a duplicate clip effect"),
            Ok(e) => out.push(e),
            Err(err) => tracing::warn!(error = %err, "dropping an unreadable clip effect"),
        }
    }
    Ok(out)
}

impl Clip {
    pub fn effect(&self, id: EffectId) -> Option<&AppliedEffect> {
        self.effects.iter().find(|e| e.id == id)
    }
    pub fn effect_mut(&mut self, id: EffectId) -> Option<&mut AppliedEffect> {
        self.effects.iter_mut().find(|e| e.id == id)
    }
    /// The clip's effect of `recipe` (one instance per recipe, spec §2).
    pub fn effect_of(&self, recipe: &str) -> Option<&AppliedEffect> {
        self.effects.iter().find(|e| e.recipe == recipe)
    }

    /// Applies `e`, one instance per recipe (spec §2): a new recipe is
    /// appended; one the clip has is replaced in place — its id and position
    /// stay, its `fx:` tracks go. When a lenient load left the clip two of
    /// the recipe, the first is replaced that way and the others go with
    /// their tracks: the clip ends with exactly the one applied. `false`
    /// when nothing changed (then nothing was touched).
    pub fn put_effect(&mut self, mut e: AppliedEffect) -> bool {
        let mut same = self.effects.iter().enumerate().filter(|(_, x)| x.recipe == e.recipe).map(|(i, x)| (i, x.id));
        let Some((first, id)) = same.next() else {
            if self.effect(e.id).is_some() {
                e.id = EffectId::new(); // never two effects with one id on a clip
            }
            self.effects.push(e);
            return true;
        };
        let others: Vec<EffectId> = same.map(|(_, id)| id).collect();
        e.id = id;
        let mut had_tracks = false;
        for id in std::iter::once(id).chain(others.iter().copied()) {
            had_tracks |= self.animation.remove_effect(id);
        }
        if !had_tracks && others.is_empty() && self.effects[first] == e {
            return false;
        }
        // The others all come after the first: removing them keeps its index.
        self.effects.retain(|x| !others.contains(&x.id));
        self.effects[first] = e;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;
    use serde_json::json;

    #[test]
    fn param_values_are_saved_in_their_tagged_form_and_read_back() {
        let cases = [
            (ParamValue::Number(0.6), json!({"number": 0.6})),
            (ParamValue::Choice("in".into()), json!({"choice": "in"})),
            (ParamValue::Point { x: 0.25, y: 0.75 }, json!({"point": {"x": 0.25, "y": 0.75}})),
            (ParamValue::Color([1.0, 0.5, 0.0, 1.0]), json!({"color": [1.0, 0.5, 0.0, 1.0]})),
            (ParamValue::Moment(SourceTime(Time::from_secs(2))), json!({"moment": 1_411_200_000})),
        ];
        for (v, j) in cases {
            assert_eq!(serde_json::to_value(&v).unwrap(), j, "{v:?}");
            assert_eq!(serde_json::from_value::<ParamValue>(j).unwrap(), v);
        }
    }

    /// Spec §2: an unknown variant (or a known one in a shape this build
    /// cannot read) is kept as it was and never used.
    #[test]
    fn an_unknown_or_malformed_param_variant_is_kept_verbatim() {
        for j in [
            json!({"curve": {"bezier": [0.1, 0.2]}}),
            json!({"number": "x"}),
            json!({"point": {"x": 0.5}}),
            json!({"point": {"x": 0.5, "y": 0.5, "z": 1.0}}),
            json!({"moment": i64::MAX}),
            json!({"number": 1.5, "unit": "s"}),
            json!(5),
        ] {
            let v: ParamValue = serde_json::from_value(j.clone()).unwrap();
            assert_eq!(v, ParamValue::Unknown(j.clone()));
            assert_eq!(serde_json::to_value(&v).unwrap(), j);
        }
    }

    /// Review focus 5: a recipe, a parameter variant and a field from a
    /// newer Kadr survive load → save unchanged.
    #[test]
    fn an_effect_from_a_newer_kadr_survives_load_and_save() {
        let future = json!({
            "id": EffectId::new(), "recipe": "glitch_v2", "version": 4, "enabled": true,
            "params": {"amount": {"number": 0.3}, "curve": {"bezier": [0.1, 0.2]}},
            "mask": {"shape": "oval", "feather": 0.2}
        });
        let e: AppliedEffect = serde_json::from_value(future.clone()).unwrap();
        assert_eq!((e.recipe.as_str(), e.version, e.progress), ("glitch_v2", 4, (0.0, 1.0)));
        assert!(matches!(e.params["curve"], ParamValue::Unknown(_)));
        assert_eq!(e.extra["mask"], future["mask"]);
        assert_eq!(serde_json::to_value(&e).unwrap(), future);
    }

    /// Spec §2: unknown parameters of a known recipe are kept too.
    #[test]
    fn an_unknown_param_of_a_known_recipe_is_kept() {
        let stored = json!({"id": EffectId::new(), "recipe": "smooth_zoom", "version": 1, "enabled": true, "params": {"strength": {"number": 0.5}, "future_amount": {"number": 2.5}}});
        let e: AppliedEffect = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(e.number("future_amount"), Some(2.5));
        assert_eq!(serde_json::to_value(&e).unwrap(), stored);
    }

    #[test]
    fn a_partial_range_is_written_and_a_full_one_is_not() {
        let mut e = AppliedEffect::new("smooth_zoom", 1, BTreeMap::new());
        assert!(serde_json::to_value(&e).unwrap().get("progress").is_none());
        e.progress = (0.25, 1.0);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["progress"], json!([0.25, 1.0]));
        assert_eq!(serde_json::from_value::<AppliedEffect>(v).unwrap(), e);
    }

    #[derive(serde::Deserialize)]
    struct Holder {
        #[serde(deserialize_with = "deserialize_effects")]
        effects: Vec<AppliedEffect>,
    }

    /// Bad effect data never fails the project: entries that are not effects
    /// at all are dropped, ids stay unique per clip.
    #[test]
    fn effects_load_leniently_and_keep_ids_unique() {
        let id = EffectId::new();
        let list = json!([7, {"recipe": "x"}, {"id": id, "recipe": "smooth_zoom"}, {"id": id, "recipe": "handheld"},
                          {"id": EffectId::new(), "recipe": "a", "progress": "bad"}]);
        let h: Holder = serde_json::from_value(json!({ "effects": list })).unwrap();
        assert_eq!(h.effects.len(), 1);
        assert_eq!(h.effects[0].recipe, "smooth_zoom", "the first of an id wins");
        assert_eq!((h.effects[0].version, h.effects[0].enabled), (1, true), "defaults");
        for bad in [json!(null), json!("x"), json!({"a": 1})] {
            let h: Holder = serde_json::from_value(json!({ "effects": bad })).unwrap();
            assert!(h.effects.is_empty());
        }
    }

    #[test]
    fn a_split_cuts_the_progress_range_where_the_cut_is() {
        let mut e = AppliedEffect::new("smooth_zoom", 1, BTreeMap::new());
        assert_eq!(e.split_progress(0.25), ((0.0, 0.25), (0.25, 1.0)));
        e.progress = (0.5, 1.0);
        assert_eq!(e.progress_at(0.5), 0.75);
        assert_eq!(e.split_progress(0.5), ((0.5, 0.75), (0.75, 1.0)));
    }

    /// A non-finite progress is written `[null, x]` and drops the whole
    /// effect on reload: a degenerate split point (a zero-length clip's
    /// 0 / 0, an infinite fraction) or an absurd range from a file never
    /// produces one.
    #[test]
    fn a_split_never_produces_a_non_finite_progress() {
        let mut e = AppliedEffect::new("smooth_zoom", 1, BTreeMap::new());
        e.progress = (0.2, 0.6);
        for frac in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -3.0, 7.0] {
            let ((a, c), (c2, b)) = e.split_progress(frac);
            assert!([a, c, c2, b].iter().all(|v| v.is_finite()), "{frac}");
            assert_eq!((a, b, c), (0.2, 0.6, c2), "{frac}: the outer ends stay, the halves meet");
            assert!((0.2..=0.6).contains(&c), "{frac}: the cut lies within the range, {c}");
        }
        e.progress = (-1e308, 1e308);
        let ((_, c), _) = e.split_progress(0.5);
        assert!(c.is_finite(), "{c}");
    }

    #[test]
    fn a_clip_finds_its_effects_by_id_and_recipe() {
        let mut c = crate::Clip::new(kadr_core::AssetId::new(), "c", kadr_core::TimeRange::new(Time::ZERO, Time::from_secs(1)), Time::ZERO);
        let e = AppliedEffect::new("handheld", 1, BTreeMap::new());
        let id = e.id;
        c.effects.push(e);
        assert_eq!(c.effect(id).map(|e| e.recipe.as_str()), Some("handheld"));
        assert_eq!(c.effect_of("handheld").map(|e| e.id), Some(id));
        c.effect_mut(id).unwrap().enabled = false;
        assert!(!c.effects[0].enabled);
        assert!(c.effect(EffectId::new()).is_none() && c.effect_of("smooth_zoom").is_none());
    }

    #[test]
    fn put_effect_keeps_one_instance_per_recipe_in_place() {
        let mut c = crate::Clip::new(kadr_core::AssetId::new(), "c", kadr_core::TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
        let zoom = AppliedEffect::new("smooth_zoom", 1, BTreeMap::from([("strength".to_string(), ParamValue::Number(0.6))]));
        let zid = zoom.id;
        assert!(c.put_effect(zoom.clone()));
        assert!(c.put_effect(AppliedEffect::new("handheld", 1, BTreeMap::new())));
        let k = crate::TrackKey::fx(zid, "strength");
        c.animation.set_key(k.clone(), SourceTime(Time::ZERO), 0.2, None);
        let mut again = zoom.clone();
        again.id = EffectId::new();
        assert!(c.put_effect(again.clone()), "the same parameters, but its keyframes go");
        assert_eq!((c.effects[0].id, c.effects[0].recipe.as_str()), (zid, "smooth_zoom"), "same id, same place");
        assert!(!c.animation.is_animated(&k));
        assert!(!c.put_effect(again), "nothing left to change");
        let mut odd = AppliedEffect::new("cinematic_push", 1, BTreeMap::new());
        odd.id = zid;
        assert!(c.put_effect(odd));
        assert_ne!(c.effects[2].id, zid, "a new effect never takes an id the clip uses");
    }

    /// A lenient load keeps ids unique but not recipes: a clip may arrive
    /// with two of one recipe. Applying that recipe leaves exactly one —
    /// the first, in its place with its id — and the others go with their
    /// tracks, so the clip shows what was applied and nothing more.
    #[test]
    fn put_effect_collapses_a_recipe_the_clip_has_twice_into_the_first() {
        let mut c = crate::Clip::new(kadr_core::AssetId::new(), "c", kadr_core::TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
        let zoom = |s: f64| AppliedEffect::new("smooth_zoom", 1, BTreeMap::from([("strength".to_string(), ParamValue::Number(s))]));
        let (first, hand, second) = (zoom(0.3), AppliedEffect::new("handheld", 1, BTreeMap::new()), zoom(0.9));
        let ids = [first.id, hand.id, second.id];
        c.effects = vec![first.clone(), hand, second];
        let (k1, k2) = (crate::TrackKey::fx(ids[0], "strength"), crate::TrackKey::fx(ids[2], "strength"));
        c.animation.set_key(k2.clone(), SourceTime(Time::ZERO), 0.1, None);
        assert!(c.put_effect(first.clone()), "the first is as given and has no keyframes, but the second goes");
        assert_eq!(c.effects.iter().map(|e| e.id).collect::<Vec<_>>(), [ids[0], ids[1]]);
        assert!(!c.animation.is_animated(&k2), "the second's tracks went with it");
        assert!(!c.put_effect(first), "one instance, unchanged: nothing to do");
        c.effects.push(zoom(0.9));
        c.animation.set_key(k1.clone(), SourceTime(Time::ZERO), 0.2, None);
        assert!(c.put_effect(zoom(0.5)));
        assert_eq!(c.effects.iter().map(|e| (e.id, e.number("strength"))).collect::<Vec<_>>(), [(ids[0], Some(0.5)), (ids[1], None)]);
        assert!(c.animation.is_empty());
    }
}
