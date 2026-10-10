//! Keyframe animation (keyframes spec §2). Keyframes are bound to the
//! clip's *source* time, so trim, move, speed, slip and split never rewrite
//! them. Unknown parameters and curves from newer versions are kept
//! verbatim (spec §2.1) and never executed.

use crate::{ColorAdjust, Transform};
use kadr_core::ease::Curve;
use kadr_core::Time;
use serde::de::Deserializer;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A moment in the clip's source media (the source-bound time domain).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceTime(pub Time);

/// An animatable clip property. Future effect parameters will be added as
/// `Effect { effect, key }` with typed values (`AnimValue`); today every
/// parameter is an `f64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnimParam {
    X,
    Y,
    Scale,
    Rotation,
    Opacity,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    Exposure,
    Contrast,
    Saturation,
    Temperature,
}

impl AnimParam {
    /// Every parameter, in the order the UI lists them.
    pub const ALL: [AnimParam; 13] = [
        AnimParam::X,
        AnimParam::Y,
        AnimParam::Scale,
        AnimParam::Rotation,
        AnimParam::Opacity,
        AnimParam::CropLeft,
        AnimParam::CropRight,
        AnimParam::CropTop,
        AnimParam::CropBottom,
        AnimParam::Exposure,
        AnimParam::Contrast,
        AnimParam::Saturation,
        AnimParam::Temperature,
    ];

    /// The snake_case key this parameter is saved under.
    pub fn key(self) -> &'static str {
        match self {
            AnimParam::X => "x",
            AnimParam::Y => "y",
            AnimParam::Scale => "scale",
            AnimParam::Rotation => "rotation",
            AnimParam::Opacity => "opacity",
            AnimParam::CropLeft => "crop_left",
            AnimParam::CropRight => "crop_right",
            AnimParam::CropTop => "crop_top",
            AnimParam::CropBottom => "crop_bottom",
            AnimParam::Exposure => "exposure",
            AnimParam::Contrast => "contrast",
            AnimParam::Saturation => "saturation",
            AnimParam::Temperature => "temperature",
        }
    }

    /// Inverse of [`AnimParam::key`]; `None` for keys this build does not know.
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    /// Position in [`AnimParam::ALL`].
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap()
    }

    /// The static (un-animated) value of this parameter.
    pub fn read(self, t: &Transform, c: &ColorAdjust) -> f64 {
        match self {
            AnimParam::X => t.x,
            AnimParam::Y => t.y,
            AnimParam::Scale => t.scale,
            AnimParam::Rotation => t.rotation_deg,
            AnimParam::Opacity => t.opacity,
            AnimParam::CropLeft => t.crop_left,
            AnimParam::CropRight => t.crop_right,
            AnimParam::CropTop => t.crop_top,
            AnimParam::CropBottom => t.crop_bottom,
            AnimParam::Exposure => c.exposure,
            AnimParam::Contrast => c.contrast,
            AnimParam::Saturation => c.saturation,
            AnimParam::Temperature => c.temperature,
        }
    }

    /// Stores `v` into the field [`AnimParam::read`] reads.
    pub fn write(self, t: &mut Transform, c: &mut ColorAdjust, v: f64) {
        let slot = match self {
            AnimParam::X => &mut t.x,
            AnimParam::Y => &mut t.y,
            AnimParam::Scale => &mut t.scale,
            AnimParam::Rotation => &mut t.rotation_deg,
            AnimParam::Opacity => &mut t.opacity,
            AnimParam::CropLeft => &mut t.crop_left,
            AnimParam::CropRight => &mut t.crop_right,
            AnimParam::CropTop => &mut t.crop_top,
            AnimParam::CropBottom => &mut t.crop_bottom,
            AnimParam::Exposure => &mut c.exposure,
            AnimParam::Contrast => &mut c.contrast,
            AnimParam::Saturation => &mut c.saturation,
            AnimParam::Temperature => &mut c.temperature,
        };
        *slot = v;
    }

    /// The value of this parameter on a fresh clip.
    pub fn default_value(self) -> f64 {
        self.read(&Transform::default(), &ColorAdjust::default())
    }
}

impl Serialize for AnimParam {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for AnimParam {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        AnimParam::from_key(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown animation parameter {s}")))
    }
}

/// Curve of the segment *after* a keyframe (the last keyframe's is unused).
/// Reserved for later, not implemented: `Auto` (monotone cubic spline with
/// continuous tangents across a chain) and `Bezier { out, in }` — new
/// variants, never a change of an existing one.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Ease {
    #[default]
    EaseInOut,
    Linear,
    EaseIn,
    EaseOut,
    Hold,
    /// A curve from a newer Kadr, kept verbatim; evaluated as `Linear`.
    Unknown(Value),
}

impl Ease {
    /// Every curve this build can evaluate and edit.
    pub const KNOWN: [Ease; 5] = [Ease::EaseInOut, Ease::Linear, Ease::EaseIn, Ease::EaseOut, Ease::Hold];

    /// The easing function evaluated for this segment.
    pub fn curve(&self) -> Curve {
        match self {
            Ease::EaseInOut => Curve::EaseInOut,
            Ease::Linear | Ease::Unknown(_) => Curve::Linear,
            Ease::EaseIn => Curve::EaseIn,
            Ease::EaseOut => Curve::EaseOut,
            Ease::Hold => Curve::Hold,
        }
    }

    /// The snake_case key a known curve is saved as; `None` for `Unknown`.
    pub fn key(&self) -> Option<&'static str> {
        Some(match self {
            Ease::EaseInOut => "ease_in_out",
            Ease::Linear => "linear",
            Ease::EaseIn => "ease_in",
            Ease::EaseOut => "ease_out",
            Ease::Hold => "hold",
            Ease::Unknown(_) => return None,
        })
    }

    /// Inverse of [`Ease::key`] over the known curves.
    pub fn from_key(s: &str) -> Option<Ease> {
        Self::KNOWN.into_iter().find(|e| e.key() == Some(s))
    }
}

impl Serialize for Ease {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Ease::Unknown(v) => v.serialize(s),
            known => s.serialize_str(known.key().unwrap()),
        }
    }
}

impl<'de> Deserialize<'de> for Ease {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(v.as_str().and_then(Ease::from_key).unwrap_or(Ease::Unknown(v)))
    }
}

/// One key of a parameter's track: `value` at source moment `at`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub at: SourceTime,
    pub value: f64,
    #[serde(default)]
    pub ease: Ease,
}

/// Per-parameter keyframe tracks. Invariant: every stored track is
/// non-empty and sorted by strictly increasing `at`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animation {
    tracks: BTreeMap<AnimParam, Vec<Keyframe>>,
    unknown: BTreeMap<String, Value>,
}

impl Animation {
    /// No executable track (unknown ones may still be stored).
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
    /// Nothing to write (no known and no unknown tracks).
    pub fn is_stored_empty(&self) -> bool {
        self.tracks.is_empty() && self.unknown.is_empty()
    }
    /// The animated parameters, in [`AnimParam::ALL`] order.
    pub fn params(&self) -> Vec<AnimParam> {
        self.tracks.keys().copied().collect()
    }
    /// The keyframes of `p` sorted by time; empty when `p` is not animated.
    pub fn track(&self, p: AnimParam) -> &[Keyframe] {
        self.tracks.get(&p).map_or(&[], Vec::as_slice)
    }
    /// `p` has at least one keyframe.
    pub fn is_animated(&self, p: AnimParam) -> bool {
        self.tracks.contains_key(&p)
    }
    /// Number of tracks kept verbatim because this build cannot run them.
    pub fn unknown_len(&self) -> usize {
        self.unknown.len()
    }

    /// Adds a keyframe or, at an existing time, replaces its value (and its
    /// ease when one is given). A new keyframe inside a segment inherits
    /// that segment's ease; elsewhere it gets `ease` or the default.
    pub fn set_key(&mut self, p: AnimParam, at: SourceTime, value: f64, ease: Option<Ease>) {
        let keys = self.tracks.entry(p).or_default();
        match keys.binary_search_by_key(&at, |k| k.at) {
            Ok(i) => {
                keys[i].value = value;
                if let Some(e) = ease {
                    keys[i].ease = e;
                }
            }
            Err(i) => {
                let inherited = if i > 0 && i < keys.len() { keys[i - 1].ease.clone() } else { Ease::default() };
                keys.insert(i, Keyframe { at, value, ease: ease.unwrap_or(inherited) });
            }
        }
    }
}

impl Serialize for Animation {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.tracks.len() + self.unknown.len()))?;
        for (p, keys) in &self.tracks {
            m.serialize_entry(p.key(), keys)?;
        }
        for (k, v) in &self.unknown {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Animation {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, Value>::deserialize(d)?;
        let mut a = Animation::default();
        for (k, v) in raw {
            let parsed = AnimParam::from_key(&k).and_then(|p| serde_json::from_value::<Vec<Keyframe>>(v.clone()).ok().map(|keys| (p, keys)));
            match parsed {
                Some((p, mut keys)) if keys.iter().all(|k| k.value.is_finite()) && !keys.is_empty() => {
                    // Stable sort keeps file order among equal times; keep the last of each.
                    keys.sort_by_key(|k| k.at);
                    let mut dedup: Vec<Keyframe> = Vec::with_capacity(keys.len());
                    for k in keys {
                        match dedup.last_mut() {
                            Some(last) if last.at == k.at => *last = k,
                            _ => dedup.push(k),
                        }
                    }
                    a.tracks.insert(p, dedup);
                }
                _ => {
                    // Unknown key, unparsable or non-finite track: keep it, never run it.
                    a.unknown.insert(k, v);
                }
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn st(ms: i64) -> SourceTime {
        SourceTime(Time::from_millis(ms))
    }

    #[test]
    fn params_round_trip_through_their_keys() {
        for (i, p) in AnimParam::ALL.iter().enumerate() {
            assert_eq!(AnimParam::from_key(p.key()), Some(*p));
            assert_eq!(p.index(), i);
        }
        assert_eq!(AnimParam::from_key("future_glow"), None);
    }

    #[test]
    fn read_write_cover_every_transform_and_colour_field() {
        let (mut tr, mut co) = (Transform::default(), ColorAdjust::default());
        for (i, p) in AnimParam::ALL.iter().enumerate() {
            assert_eq!(p.read(&tr, &co), p.default_value(), "{p:?} default");
            p.write(&mut tr, &mut co, 10.0 + i as f64);
        }
        for (i, p) in AnimParam::ALL.iter().enumerate() {
            assert_eq!(p.read(&tr, &co), 10.0 + i as f64, "{p:?}");
        }
    }

    #[test]
    fn known_animation_serialises_as_one_map() {
        let mut a = Animation::default();
        a.set_key(AnimParam::Scale, st(1000), 1.0, None);
        a.set_key(AnimParam::Scale, st(2000), 1.5, Some(Ease::Linear));
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v, json!({"scale": [
            {"at": 705_600_000, "value": 1.0, "ease": "ease_in_out"},
            {"at": 1_411_200_000, "value": 1.5, "ease": "linear"}
        ]}));
        assert_eq!(serde_json::from_value::<Animation>(v).unwrap(), a);
    }

    /// Spec §2.1: a key and a curve from a newer Kadr survive load → save → load.
    #[test]
    fn unknown_param_and_unknown_ease_are_preserved_and_not_executed() {
        let future = json!({
            "future_glow": [{"at": 0, "value": {"r": 1, "g": 0}}],
            "opacity": [{"at": 0, "value": 0.5, "ease": {"bezier": [0.1, 0.2, 0.3, 0.4]}},
                        {"at": 705_600_000, "value": 1.0, "ease": "linear"}]
        });
        let a: Animation = serde_json::from_value(future.clone()).unwrap();
        assert_eq!(a.params(), vec![AnimParam::Opacity], "future_glow is not executed");
        assert_eq!(a.unknown_len(), 1);
        assert!(matches!(a.track(AnimParam::Opacity)[0].ease, Ease::Unknown(_)));
        assert_eq!(a.track(AnimParam::Opacity)[0].ease.curve(), Curve::Linear);
        let saved = serde_json::to_value(&a).unwrap();
        assert_eq!(saved["future_glow"], future["future_glow"]);
        assert_eq!(saved["opacity"][0]["ease"], future["opacity"][0]["ease"]);
        let again: Animation = serde_json::from_value(saved).unwrap();
        assert_eq!(again, a);
    }

    /// Review focus 2: bad animation data never fails the project.
    #[test]
    fn an_unparsable_known_track_is_kept_as_unknown() {
        let bad = json!({"scale": "not an array", "x": [{"at": 0, "value": null}]});
        let a: Animation = serde_json::from_value(bad.clone()).unwrap();
        assert!(a.is_empty(), "nothing executable");
        assert_eq!(serde_json::to_value(&a).unwrap(), bad, "and nothing lost");
    }

    #[test]
    fn keyframes_load_sorted_and_deduplicated() {
        let v = json!({"x": [{"at": 20, "value": 2.0}, {"at": 10, "value": 1.0}, {"at": 20, "value": 3.0}]});
        let a: Animation = serde_json::from_value(v).unwrap();
        let t: Vec<(i64, f64)> = a.track(AnimParam::X).iter().map(|k| (k.at.0.flicks(), k.value)).collect();
        assert_eq!(t, vec![(10, 1.0), (20, 3.0)], "the last of equal times wins");
        assert_eq!(a.track(AnimParam::X)[0].ease, Ease::EaseInOut, "missing ease = default");
    }
}
