# Keyframes and Clip Fades Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clips animate position, scale, rotation, opacity, crop and colour over time through source-bound keyframes, and fade in/out from their edges with handles on the timeline and fields in the inspector.

**Architecture:** A typed `Animation` (per-parameter keyframe tracks in *source media time*) and `EdgeFades` (durations from the clip's visible edges) live on `Clip` in `kadr-project`. The scene evaluator (`kadr-timeline::scene`) resolves animated values and the fade factor per frame into the existing `Layer` fields, so renderer and playback do not change. Edits go through new `EditEngine` commands (one undo step per gesture); the editor adds `◀ ◆ ▶` to inspector rows, fade handles and keyframe diamonds to timeline clips, and MCP gets matching `edit` ops.

**Tech Stack:** Rust 2021 workspace, `serde`/`serde_json`, Slint 1.18 UI (`apps/editor/ui/*.slint`), `kadr-i18n` JSON catalogs, `cargo test` / `cargo clippy` on Windows (Git Bash).

**Spec:** `docs/superpowers/specs/2026-10-01-keyframes-design.md` (read it before any task; section numbers below refer to it).

## Global Constraints

- All time is integer flicks (`kadr_core::Time`, 705 600 000 per second). Timeline ↔ source only through `Clip::source_time_at(t)` / `Clip::timeline_time_of(s)`. f64 appears once, in a segment's progress `u`.
- Keyframes are **source-bound** (`SourceTime`); fades are **clip-bound** durations; nothing timeline-bound is added. Trim, move, speed, slip and split never rewrite keyframe times; only `SwitchAngle` shifts them (by the sync-offset difference).
- Easing formulas are saved project semantics (spec §3.1): `ease_in_out` = `u < ½ ? 4u³ : 1 − (2 − 2u)³ / 2`; `linear` = `u`; `ease_in` = `u³`; `ease_out` = `1 − (1 − u)³`; `hold` = `0` for `u < 1`. Default for a new keyframe = `EaseInOut`; a keyframe inserted inside a segment inherits that segment's `ease`.
- `final_opacity(t) = resolved_opacity(t) × fade(t)`; resolved = static **or** animated, never both.
- Fade contract: first frame of a fade-in and last frame of a fade-out fully transparent; `F = 0` → factor 1 with no division. Invariant `fade_in + fade_out ≤ duration`.
- Unknown `AnimParam` keys and unknown `Ease` variants survive load → save unchanged (as JSON values) and are never executed.
- `FORMAT_VERSION` goes 1 → 2; `io::migrate` converts the old `keyframes` field; it is never written again.
- `kadr-scene`, `kadr-render`, `kadr-playback` are not modified.
- Every user-facing string exists in both `crates/i18n/locales/en.json` and `ru.json` (the `crates/i18n/tests/sources.rs` test enforces it); Slint uses `Tr.t("key")`, Rust uses `kadr_i18n::{t, tf}`.
- One gesture (slider drag, fade-handle drag, diamond drag, click) = at most one undo entry; intermediate states never enter history.
- Commits end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`. Work on branch `feature/keyframes`.

## Review Focus

1. **Playhead outside the inspected clip** while it has animated properties: values show the state at the nearest clip edge, `◆` and animated sliders are disabled with a hint — no keyframe is silently created at a hidden time (test in Task 8, `anim_ui::edit_time`).
2. **Garbage in the project file** (a known track that is not an array, `NaN`/`null` values): load never fails because of animation data; an unparsable known track is kept as unknown; a non-finite value evaluates to the property's default (tests in Task 2 and Task 5).
3. **Fade edge cases**: a one-frame clip, fades that no longer fit after a speed-up, and transition overhang (the evaluator asks for a clip outside its own range): no panic, no NaN, factor in `[0, 1]` (tests in Task 5 and Task 6).
4. **Keyframe drag onto another keyframe of the same property** is rejected with the collision message, and a drag cannot leave the visible clip range; MCP `at_ms` rounded to milliseconds still finds the existing keyframe via the half-frame rule (tests in Task 7, Task 10 and Task 11).
5. **Split at speed ≠ 1**: integer flooring in `trim_start`/`trim_end` can move a half's source mapping by one flick; the split property test pins "≤ 1 flick of source time, exact at speed 1" so a real regression (a jump) still fails (test in Task 5). This is a documented refinement of the spec's "exactly equal", reported to the owner.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/core/src/ease.rs` (new) | `Curve` enum and `Curve::apply(u)` — the five formulas |
| `crates/project/src/animation.rs` (new) | `SourceTime`, `AnimParam`, `Ease`, `Keyframe`, `Animation` (+ custom serde with `unknown`), editing ops, `AnimError` |
| `crates/project/src/fades.rs` (new) | `EdgeFades`, `FadeEdge`, `clamp_fades` |
| `crates/project/src/sequence.rs` | `Clip` gains `animation`, `video_fade`; loses `keyframes`; old `Keyframe`/`Interpolation` removed; `Clip` helpers `anim_value`, `key_near`, `clamp_fades` |
| `crates/project/src/io.rs`, `model.rs` | format 2 + migration of the old field |
| `crates/timeline/src/scene.rs` | `ClipState`, `clip_state_at`, `fade_at`, normalisation; `clip_layer` uses them |
| `crates/timeline/src/ops.rs`, `commands.rs` | fade clamping on trim/split/speed, `SwitchAngle` shift, keyframe commands, `ClipProperty::VideoFade`, `EditError::KeyframeCollision` |
| `apps/editor/src/anim_ui.rs` (new) | pure, unit-tested UI logic: param ↔ inspector prop names, key states, edit time, mark clustering, nearest key, fade drag value |
| `apps/editor/src/inspector.rs`, `ui/inspector.slint`, `ui/widgets.slint` | `KeyNav`, animated sliders, key actions, fade section |
| `apps/editor/src/timeline_ui.rs`, `ui/timeline.slint`, `ui/app.slint`, `src/app.rs`, `src/keys.rs` | fade handles, diamonds, keyframe menu, Alt+←/→ |
| `apps/editor/src/mcp_anim.rs` (new), `mcp_api.rs`, `mcp_state.rs`, `apps/kadr-mcp/src/tools.rs`, `docs/mcp.md` | MCP ops and state |
| `crates/project-scenes/tests/animation_pixels.rs` (new), `apps/kadr-bench/src/scene_bench.rs` | end-to-end pixel check, evaluator bench row |
| `crates/i18n/locales/en.json`, `ru.json` | all new strings |

---

### Task 1: Easing curves (`kadr-core::ease`)

**Files:**
- Create: `crates/core/src/ease.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod ease;`)

**Interfaces:**
- Produces: `kadr_core::ease::Curve { EaseInOut, Linear, EaseIn, EaseOut, Hold }`, `Curve::apply(self, u: f64) -> f64` (input clamped to `[0,1]`, `NaN` → 0).

- [ ] **Step 1: Write the failing test** — create `crates/core/src/ease.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const PTS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    fn table(c: Curve) -> Vec<f64> {
        PTS.iter().map(|u| c.apply(*u)).collect()
    }

    #[test]
    fn control_points_are_fixed_semantics() {
        assert_eq!(table(Curve::Linear), vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(table(Curve::EaseInOut), vec![0.0, 0.0625, 0.5, 0.9375, 1.0]);
        assert_eq!(table(Curve::EaseIn), vec![0.0, 0.015625, 0.125, 0.421875, 1.0]);
        assert_eq!(table(Curve::EaseOut), vec![0.0, 0.578125, 0.875, 0.984375, 1.0]);
        assert_eq!(table(Curve::Hold), vec![0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn every_curve_is_monotonic_and_clamped() {
        for c in [Curve::EaseInOut, Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::Hold] {
            let mut prev = c.apply(-1.0);
            assert_eq!(prev, 0.0, "{c:?} below 0");
            for i in 0..=1000 {
                let v = c.apply(i as f64 / 1000.0);
                assert!(v >= prev, "{c:?} not monotonic at {i}");
                prev = v;
            }
            assert_eq!(c.apply(2.0), 1.0, "{c:?} above 1");
            assert_eq!(c.apply(f64::NAN), 0.0, "{c:?} NaN");
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-core ease`
Expected: compile error `cannot find type Curve` (after adding `pub mod ease;` to `lib.rs`).

- [ ] **Step 3: Implement** — put this above the test module and add `pub mod ease;` to `crates/core/src/lib.rs` (after `pub mod color;`):

```rust
//! Easing curves shared by keyframe animation and, later, effect recipes.
//! The formulas are part of saved project semantics (keyframes spec §3.1):
//! never change one — add a new variant instead.

/// A curve `[0, 1] → [0, 1]`, `apply(0) = 0`, `apply(1) = 1`, monotonic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Cubic in-out ("Smooth"): `u < ½ ? 4u³ : 1 − (2 − 2u)³ / 2`.
    EaseInOut,
    Linear,
    /// Cubic in: `u³`.
    EaseIn,
    /// Cubic out: `1 − (1 − u)³`.
    EaseOut,
    /// Holds the start value until the next keyframe.
    Hold,
}

impl Curve {
    pub fn apply(self, u: f64) -> f64 {
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        match self {
            Curve::Linear => u,
            Curve::EaseInOut => {
                if u < 0.5 {
                    4.0 * u * u * u
                } else {
                    1.0 - (2.0 - 2.0 * u).powi(3) / 2.0
                }
            }
            Curve::EaseIn => u * u * u,
            Curve::EaseOut => 1.0 - (1.0 - u).powi(3),
            Curve::Hold => {
                if u < 1.0 {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p kadr-core ease`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/ease.rs crates/core/src/lib.rs
git commit -m "core: easing curves with fixed control points (keyframes spec §3.1)"
```

---

### Task 2: Animation model and forward-compatible serde (`kadr-project::animation`)

**Files:**
- Create: `crates/project/src/animation.rs`, `crates/project/src/fades.rs`
- Modify: `crates/project/src/lib.rs` (`pub mod animation; pub mod fades; pub use animation::*; pub use fades::*;`)
- Modify: `crates/project/src/sequence.rs` — `Clip` fields; remove old `Keyframe` and `Interpolation`
- Modify: `crates/project/tests/serialization.rs` (it used the old `Keyframe`)

**Interfaces:**
- Consumes: `kadr_core::ease::Curve` (Task 1).
- Produces:
  - `pub struct SourceTime(pub Time)` — `Copy, Ord, Hash, Default, Serialize(transparent)`.
  - `pub enum AnimParam { X, Y, Scale, Rotation, Opacity, CropLeft, CropRight, CropTop, CropBottom, Exposure, Contrast, Saturation, Temperature }` with `ALL: [AnimParam; 13]`, `key() -> &'static str`, `from_key(&str) -> Option<Self>`, `read(&Transform, &ColorAdjust) -> f64`, `write(&mut Transform, &mut ColorAdjust, f64)`, `default_value() -> f64`, `index() -> usize` (position in `ALL`). Serialises as its key string.
  - `pub enum Ease { EaseInOut, Linear, EaseIn, EaseOut, Hold, Unknown(serde_json::Value) }` (`Default = EaseInOut`), `curve() -> Curve` (`Unknown` → `Linear`), `key() -> Option<&'static str>`, `from_key(&str) -> Option<Ease>`.
  - `pub struct Keyframe { pub at: SourceTime, pub value: f64, pub ease: Ease }`.
  - `pub struct Animation` (private fields `tracks: BTreeMap<AnimParam, Vec<Keyframe>>`, `unknown: BTreeMap<String, serde_json::Value>`), `Default`, `PartialEq`, custom `Serialize`/`Deserialize`; accessors `is_empty()`, `params() -> Vec<AnimParam>`, `track(p) -> &[Keyframe]`, `is_animated(p) -> bool`, `unknown_len() -> usize`.
  - `pub struct EdgeFades { pub fade_in: Time, pub fade_out: Time }` (`Default`, serde).
  - `Clip { …, pub animation: Animation, pub video_fade: EdgeFades, … }` (both `#[serde(default)]`); field `keyframes` removed.

- [ ] **Step 1: Write the failing tests** — `crates/project/src/animation.rs` test module (types come in Step 3):

```rust
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
```

And in `crates/project/src/fades.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_fades_default_to_zero_and_serialise_as_flicks() {
        let f = EdgeFades { fade_in: Time::from_secs(1), fade_out: Time::ZERO };
        assert_eq!(serde_json::to_value(f).unwrap(), serde_json::json!({"fade_in": 705_600_000, "fade_out": 0}));
        assert_eq!(EdgeFades::default(), EdgeFades { fade_in: Time::ZERO, fade_out: Time::ZERO });
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-project animation fades`
Expected: compile errors (types missing).

- [ ] **Step 3: Implement the types.** `crates/project/src/fades.rs` (above its tests):

```rust
//! Clip-bound fades (keyframes spec §1, §3.3): durations measured from the
//! clip's visible edges, not keyframes.

use kadr_core::Time;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeFades {
    pub fade_in: Time,
    pub fade_out: Time,
}
```

`crates/project/src/animation.rs` (above its tests). `set_key` is used by the tests here and fully specified in Task 3; implement it now as given in Task 3 Step 3 (the whole `impl Animation` editing block) — Task 3 adds its tests and the remaining ops.

```rust
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

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap()
    }

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
    pub const KNOWN: [Ease; 5] = [Ease::EaseInOut, Ease::Linear, Ease::EaseIn, Ease::EaseOut, Ease::Hold];

    pub fn curve(&self) -> Curve {
        match self {
            Ease::EaseInOut => Curve::EaseInOut,
            Ease::Linear | Ease::Unknown(_) => Curve::Linear,
            Ease::EaseIn => Curve::EaseIn,
            Ease::EaseOut => Curve::EaseOut,
            Ease::Hold => Curve::Hold,
        }
    }

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
    pub fn params(&self) -> Vec<AnimParam> {
        self.tracks.keys().copied().collect()
    }
    pub fn track(&self, p: AnimParam) -> &[Keyframe] {
        self.tracks.get(&p).map_or(&[], Vec::as_slice)
    }
    pub fn is_animated(&self, p: AnimParam) -> bool {
        self.tracks.contains_key(&p)
    }
    pub fn unknown_len(&self) -> usize {
        self.unknown.len()
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
```

Note: `serde_json` cannot hold `NaN`, so a `null` value fails `f64` parsing and the track goes to `unknown`; the `is_finite` check guards values built in code.

In `crates/project/src/sequence.rs`: delete `pub struct Keyframe` and `pub enum Interpolation`; in `Clip` replace

```rust
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
```

with

```rust
    /// Source-bound keyframe tracks (keyframes spec §2).
    #[serde(default, skip_serializing_if = "Animation::is_stored_empty")]
    pub animation: Animation,
    /// Clip-bound fades of the picture (audio uses `audio.fade_*`).
    #[serde(default)]
    pub video_fade: EdgeFades,
```

add `use crate::{Animation, EdgeFades};` at the top, and in `Clip::new` replace `keyframes: vec![],` with `animation: Animation::default(), video_fade: EdgeFades::default(),`. Add to `impl Animation` in `animation.rs`:

```rust
    /// Nothing to write (no known and no unknown tracks).
    pub fn is_stored_empty(&self) -> bool {
        self.tracks.is_empty() && self.unknown.is_empty()
    }
```

In `crates/project/tests/serialization.rs` replace the `clip.keyframes.push(Keyframe { … });` block with:

```rust
    clip.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_millis(5_500)), 0.5, Some(Ease::EaseInOut));
    clip.video_fade = EdgeFades { fade_in: Time::from_millis(500), fade_out: Time::ZERO };
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-project`
Expected: all pass (including `serialization.rs` round trip).

- [ ] **Step 5: Commit**

```bash
git add crates/project
git commit -m "project: typed source-bound Animation and EdgeFades on Clip; unknown params and curves kept verbatim"
```

---

### Task 3: Animation evaluation and editing ops; fade clamping

**Files:**
- Modify: `crates/project/src/animation.rs`, `crates/project/src/fades.rs`, `crates/project/src/sequence.rs` (`impl Clip`)

**Interfaces:**
- Consumes: Task 2 types.
- Produces (all on `Animation` unless noted):
  - `value_at(&self, p, s: SourceTime) -> Option<f64>` — `None` when not animated.
  - `set_key(&mut self, p, at, value, ease: Option<Ease>)` — insert or update; update keeps the old `ease` when `None`; insert with `None` uses the inheritance rule.
  - `remove_key(&mut self, p, at) -> Option<Keyframe>` — drops the track when it empties.
  - `move_key(&mut self, p, from, to) -> Result<(), AnimError>`; `AnimError::{NoKeyframe, Collision}`.
  - `set_ease(&mut self, p, at, ease) -> Result<(), AnimError>`.
  - `clear(&mut self, p) -> Vec<Keyframe>`.
  - `shift_all(&mut self, delta: Time)`.
  - `times(&self) -> Vec<SourceTime>` — distinct, sorted, across known params.
  - `params_at(&self, at) -> Vec<AnimParam>` — params with a keyframe exactly at `at`.
  - `fades::FadeEdge { In, Out }`, `fades::clamp_fades(fade_in: &mut Time, fade_out: &mut Time, dur: Time, first: FadeEdge)`.
  - `Clip::anim_value(&self, p, t: Time) -> f64` (static or animated at timeline `t`, source time **not** clamped), `Clip::key_near(&self, p, t: Time, tol: Time) -> Option<SourceTime>` (nearest keyframe whose `timeline_time_of` is within `tol` of `t`, strict `<`), `Clip::clamp_fades(&mut self, first: FadeEdge)` (video and audio fades).

- [ ] **Step 1: Write the failing tests** (append to the `animation.rs` test module):

```rust
    fn two(p: AnimParam, e: Ease) -> Animation {
        let mut a = Animation::default();
        a.set_key(p, st(10_000), 0.0, Some(e));
        a.set_key(p, st(12_000), 1.0, None);
        a
    }

    #[test]
    fn value_before_between_and_after_keyframes() {
        let a = two(AnimParam::Opacity, Ease::Linear);
        assert_eq!(a.value_at(AnimParam::Opacity, st(0)), Some(0.0), "before the first: its value");
        assert_eq!(a.value_at(AnimParam::Opacity, st(11_000)), Some(0.5));
        assert_eq!(a.value_at(AnimParam::Opacity, st(99_000)), Some(1.0), "after the last: its value");
        assert_eq!(a.value_at(AnimParam::X, st(11_000)), None, "not animated");
    }

    #[test]
    fn the_segment_uses_the_first_keyframes_ease() {
        let a = two(AnimParam::Opacity, Ease::EaseInOut);
        assert_eq!(a.value_at(AnimParam::Opacity, st(10_500)), Some(0.0625));
        let h = two(AnimParam::Opacity, Ease::Hold);
        assert_eq!(h.value_at(AnimParam::Opacity, st(11_999)), Some(0.0));
        assert_eq!(h.value_at(AnimParam::Opacity, st(12_000)), Some(1.0));
    }

    #[test]
    fn a_new_keyframe_inside_a_segment_inherits_its_ease_otherwise_smooth() {
        let mut a = two(AnimParam::X, Ease::Linear);
        a.set_key(AnimParam::X, st(11_000), 0.3, None);
        assert_eq!(a.track(AnimParam::X)[1].ease, Ease::Linear, "inherited from 10 s");
        a.set_key(AnimParam::X, st(20_000), 2.0, None);
        assert_eq!(a.track(AnimParam::X)[3].ease, Ease::EaseInOut, "after the last");
        a.set_key(AnimParam::X, st(1_000), 2.0, None);
        assert_eq!(a.track(AnimParam::X)[0].ease, Ease::EaseInOut, "before the first");
        let mut b = Animation::default();
        b.set_key(AnimParam::Y, st(0), 1.0, None);
        assert_eq!(b.track(AnimParam::Y)[0].ease, Ease::EaseInOut, "first of a property");
    }

    #[test]
    fn updating_a_keyframe_keeps_its_ease_unless_given() {
        let mut a = two(AnimParam::X, Ease::Hold);
        a.set_key(AnimParam::X, st(10_000), 5.0, None);
        assert_eq!((a.track(AnimParam::X)[0].value, &a.track(AnimParam::X)[0].ease), (5.0, &Ease::Hold));
    }

    #[test]
    fn removing_the_last_keyframe_drops_the_track() {
        let mut a = two(AnimParam::X, Ease::Linear);
        assert!(a.remove_key(AnimParam::X, st(10_000)).is_some());
        assert!(a.is_animated(AnimParam::X));
        assert!(a.remove_key(AnimParam::X, st(12_000)).is_some());
        assert!(!a.is_animated(AnimParam::X));
        assert!(a.remove_key(AnimParam::X, st(12_000)).is_none());
    }

    #[test]
    fn moving_onto_another_keyframe_is_a_collision() {
        let mut a = two(AnimParam::X, Ease::Linear);
        assert_eq!(a.move_key(AnimParam::X, st(10_000), st(12_000)), Err(AnimError::Collision));
        assert_eq!(a.move_key(AnimParam::X, st(1), st(2)), Err(AnimError::NoKeyframe));
        a.move_key(AnimParam::X, st(10_000), st(13_000)).unwrap();
        let t: Vec<i64> = a.track(AnimParam::X).iter().map(|k| k.at.0.as_millis()).collect();
        assert_eq!(t, vec![12_000, 13_000], "resorted");
    }

    #[test]
    fn times_are_distinct_across_params_and_shift_all_moves_everything() {
        let mut a = two(AnimParam::X, Ease::Linear);
        a.set_key(AnimParam::Scale, st(12_000), 2.0, None);
        a.set_key(AnimParam::Scale, st(15_000), 2.0, None);
        assert_eq!(a.times(), vec![st(10_000), st(12_000), st(15_000)]);
        assert_eq!(a.params_at(st(12_000)), vec![AnimParam::X, AnimParam::Scale]);
        a.shift_all(Time::from_millis(-2_000));
        assert_eq!(a.times(), vec![st(8_000), st(10_000), st(13_000)]);
    }
```

Append to `fades.rs` tests:

```rust
    fn s(x: i64) -> Time {
        Time::from_secs(x)
    }

    fn clamped(fi: i64, fo: i64, dur: i64, first: FadeEdge) -> (i64, i64) {
        let (mut a, mut b) = (s(fi), s(fo));
        clamp_fades(&mut a, &mut b, s(dur), first);
        (a.as_millis() / 1000, b.as_millis() / 1000)
    }

    #[test]
    fn fades_that_fit_are_untouched() {
        assert_eq!(clamped(3, 4, 10, FadeEdge::In), (3, 4));
    }

    #[test]
    fn the_edited_edge_shrinks_first() {
        assert_eq!(clamped(3, 4, 5, FadeEdge::In), (1, 4));
        assert_eq!(clamped(3, 4, 5, FadeEdge::Out), (3, 2));
        assert_eq!(clamped(3, 4, 2, FadeEdge::In), (0, 2), "each ≤ duration, then the edited one gives way");
        assert_eq!(clamped(3, 4, 0, FadeEdge::Out), (0, 0));
    }
```

Append to `crates/project/src/sequence.rs` a test module (or the existing one):

```rust
#[cfg(test)]
mod animation_clip_tests {
    use super::*;
    use crate::{AnimParam, FadeEdge, SourceTime};

    fn clip_2x() -> Clip {
        // Source 10..30 s shown over timeline 0..10 s: 2× speed.
        let mut c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(10), Time::from_secs(30)), Time::ZERO);
        c.timeline_out = Time::from_secs(10);
        c
    }

    #[test]
    fn anim_value_reads_static_or_animated_in_source_time() {
        let mut c = clip_2x();
        c.transform.scale = 1.5;
        assert_eq!(c.anim_value(AnimParam::Scale, Time::from_secs(1)), 1.5);
        c.animation.set_key(AnimParam::Scale, SourceTime(Time::from_secs(10)), 1.0, Some(crate::Ease::Linear));
        c.animation.set_key(AnimParam::Scale, SourceTime(Time::from_secs(14)), 2.0, None);
        assert_eq!(c.anim_value(AnimParam::Scale, Time::from_secs(1)), 1.5, "timeline 1 s = source 12 s");
    }

    #[test]
    fn key_near_uses_timeline_distance_strictly_inside_the_tolerance() {
        let mut c = clip_2x();
        c.animation.set_key(AnimParam::X, SourceTime(Time::from_secs(14)), 1.0, None); // timeline 2 s
        let half = Time::from_millis(16);
        assert_eq!(c.key_near(AnimParam::X, Time::from_millis(2_010), half), Some(SourceTime(Time::from_secs(14))));
        assert_eq!(c.key_near(AnimParam::X, Time::from_millis(2_016), half), None);
        assert_eq!(c.key_near(AnimParam::Y, Time::from_secs(2), half), None);
    }

    #[test]
    fn clamp_fades_covers_video_and_audio() {
        let mut c = clip_2x();
        c.video_fade = crate::EdgeFades { fade_in: Time::from_secs(8), fade_out: Time::from_secs(8) };
        c.audio.fade_in = Time::from_secs(8);
        c.audio.fade_out = Time::from_secs(8);
        c.clamp_fades(FadeEdge::Out);
        assert_eq!((c.video_fade.fade_in, c.video_fade.fade_out), (Time::from_secs(8), Time::from_secs(2)));
        assert_eq!((c.audio.fade_in, c.audio.fade_out), (Time::from_secs(8), Time::from_secs(2)));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-project`
Expected: compile errors for the missing methods.

- [ ] **Step 3: Implement.** In `animation.rs` add:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimError {
    NoKeyframe,
    Collision,
}

impl Animation {
    pub fn value_at(&self, p: AnimParam, s: SourceTime) -> Option<f64> {
        let keys = self.tracks.get(&p)?;
        let i = keys.partition_point(|k| k.at <= s);
        Some(match (i.checked_sub(1).map(|j| &keys[j]), keys.get(i)) {
            (None, Some(first)) => first.value,
            (Some(last), None) => last.value,
            (Some(k0), Some(k1)) => {
                let u = (s.0 - k0.at.0).flicks() as f64 / (k1.at.0 - k0.at.0).flicks() as f64;
                k0.value + (k1.value - k0.value) * k0.ease.curve().apply(u)
            }
            (None, None) => return None,
        })
    }

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

    pub fn remove_key(&mut self, p: AnimParam, at: SourceTime) -> Option<Keyframe> {
        let keys = self.tracks.get_mut(&p)?;
        let i = keys.binary_search_by_key(&at, |k| k.at).ok()?;
        let k = keys.remove(i);
        if keys.is_empty() {
            self.tracks.remove(&p);
        }
        Some(k)
    }

    pub fn move_key(&mut self, p: AnimParam, from: SourceTime, to: SourceTime) -> Result<(), AnimError> {
        let keys = self.tracks.get_mut(&p).ok_or(AnimError::NoKeyframe)?;
        let i = keys.binary_search_by_key(&from, |k| k.at).map_err(|_| AnimError::NoKeyframe)?;
        if from == to {
            return Ok(());
        }
        if keys.iter().any(|k| k.at == to) {
            return Err(AnimError::Collision);
        }
        keys[i].at = to;
        keys.sort_by_key(|k| k.at);
        Ok(())
    }

    pub fn set_ease(&mut self, p: AnimParam, at: SourceTime, ease: Ease) -> Result<(), AnimError> {
        let keys = self.tracks.get_mut(&p).ok_or(AnimError::NoKeyframe)?;
        let i = keys.binary_search_by_key(&at, |k| k.at).map_err(|_| AnimError::NoKeyframe)?;
        keys[i].ease = ease;
        Ok(())
    }

    pub fn clear(&mut self, p: AnimParam) -> Vec<Keyframe> {
        self.tracks.remove(&p).unwrap_or_default()
    }

    pub fn shift_all(&mut self, delta: Time) {
        for keys in self.tracks.values_mut() {
            for k in keys {
                k.at = SourceTime(k.at.0 + delta);
            }
        }
    }

    pub fn times(&self) -> Vec<SourceTime> {
        let mut t: Vec<SourceTime> = self.tracks.values().flatten().map(|k| k.at).collect();
        t.sort();
        t.dedup();
        t
    }

    pub fn params_at(&self, at: SourceTime) -> Vec<AnimParam> {
        self.tracks.iter().filter(|(_, keys)| keys.binary_search_by_key(&at, |k| k.at).is_ok()).map(|(p, _)| *p).collect()
    }
}
```

In `fades.rs` add:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeEdge {
    In,
    Out,
}

/// Keeps both fades in `[0, dur]` and `fade_in + fade_out ≤ dur`; when they
/// no longer fit, the fade on `first` (the edge that moved) gives way
/// first, the other only once the first reached zero (keyframes spec §4).
pub fn clamp_fades(fade_in: &mut Time, fade_out: &mut Time, dur: Time, first: FadeEdge) {
    let dur = dur.max(Time::ZERO);
    *fade_in = (*fade_in).max(Time::ZERO).min(dur);
    *fade_out = (*fade_out).max(Time::ZERO).min(dur);
    let over = *fade_in + *fade_out - dur;
    if over > Time::ZERO {
        let (a, b) = match first {
            FadeEdge::In => (fade_in, fade_out),
            FadeEdge::Out => (fade_out, fade_in),
        };
        let cut = over.min(*a);
        *a -= cut;
        *b -= over - cut;
    }
}
```

In `sequence.rs` `impl Clip` add:

```rust
    /// The value property `p` has at timeline time `t`: the keyframe track
    /// when it exists (source time extrapolated past the clip's edges),
    /// otherwise the static field.
    pub fn anim_value(&self, p: crate::AnimParam, t: Time) -> f64 {
        self.animation
            .value_at(p, crate::SourceTime(self.source_time_at(t)))
            .unwrap_or_else(|| p.read(&self.transform, &self.color))
    }

    /// The keyframe of `p` shown within `tol` (exclusive) of timeline `t`, nearest first.
    pub fn key_near(&self, p: crate::AnimParam, t: Time, tol: Time) -> Option<crate::SourceTime> {
        self.animation
            .track(p)
            .iter()
            .map(|k| (k.at, (self.timeline_time_of(k.at.0) - t).abs()))
            .filter(|(_, d)| *d < tol)
            .min_by_key(|(_, d)| *d)
            .map(|(at, _)| at)
    }

    /// Re-establishes `fade_in + fade_out ≤ duration` for picture and sound.
    pub fn clamp_fades(&mut self, first: crate::FadeEdge) {
        let d = self.duration();
        crate::clamp_fades(&mut self.video_fade.fade_in, &mut self.video_fade.fade_out, d, first);
        crate::clamp_fades(&mut self.audio.fade_in, &mut self.audio.fade_out, d, first);
    }
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-project`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/project
git commit -m "project: keyframe evaluation, insertion rule, edits; clip fade clamping"
```

---

### Task 4: Project format 2 and migration of the old `keyframes` field

**Files:**
- Modify: `crates/project/src/model.rs` (`FORMAT_VERSION` → 2), `crates/project/src/io.rs` (`migrate`)

**Interfaces:**
- Consumes: Task 2 serde shape (`animation` map, `Keyframe { at, value, ease }`).
- Produces: `io::migrate(value, from)` converting format 1 files; `parse` of a format-2 file in a format-1 build already fails with `TooNew` (existing behaviour, now tested for 2).

- [ ] **Step 1: Write the failing tests** (in `io.rs` tests):

```rust
    /// Format 1 clip JSON with the old string-keyed keyframes, 2× speed:
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
        assert_eq!(scale[0].at, crate::SourceTime(kadr_core::Time::from_secs(12)), "timeline 1 s at 2× = source 12 s");
        assert_eq!((scale[0].value, &scale[0].ease), (1.2, &crate::Ease::Linear));
        assert_eq!(c.animation.track(crate::AnimParam::Exposure)[0].ease, crate::Ease::Hold);
        assert_eq!(c.animation.unknown_len(), 1, "audio.gain_db is not a V1 animation param: kept, not run");
        let saved = serde_json::to_value(&p).unwrap();
        assert!(saved["sequences"][0]["tracks"][0]["clips"][0].get("keyframes").is_none(), "old field never written");
        assert_eq!(saved["format_version"], serde_json::json!(FORMAT_VERSION));
    }

    #[test]
    fn a_format_2_file_is_too_new_for_a_format_1_reader() {
        assert_eq!(FORMAT_VERSION, 2);
        let json = format!(r#"{{"format_version": {}}}"#, FORMAT_VERSION + 1);
        assert!(matches!(parse(json.as_bytes(), Path::new("x.kadr")).unwrap_err(), ProjectIoError::TooNew { .. }));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-project io`
Expected: FAIL (`keyframes` is an unknown field ignored → animation empty; `FORMAT_VERSION == 1`).

- [ ] **Step 3: Implement.** `model.rs`: `pub const FORMAT_VERSION: u32 = 2;`. `io.rs`:

```rust
/// Upgrades older JSON layouts in place. Format 1 is the first release;
/// format 2 replaced the never-evaluated string-keyed `keyframes` with
/// source-bound `animation` (keyframes spec §2.2).
fn migrate(mut value: serde_json::Value, from: u32) -> serde_json::Value {
    if from < 2 {
        if let Some(seqs) = value.get_mut("sequences").and_then(|s| s.as_array_mut()) {
            for clip in seqs.iter_mut().flat_map(|s| s["tracks"].as_array_mut().into_iter().flatten()).flat_map(|t| t["clips"].as_array_mut().into_iter().flatten()) {
                migrate_clip_keyframes(clip);
            }
        }
    }
    value
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
                let rel = kadr_core::Time(k.get("time").and_then(Value::as_i64).unwrap_or(0));
                let at = kadr_core::Time(sin) + rel.mul_ratio(sout - sin, (tout - tin).max(1));
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
```

Replace the old `migrate(value, found)` call's signature usage accordingly (it already passes `found`). Update the existing `rejects_newer_format` test if it duplicates — keep both.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-project`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/project
git commit -m "project: format 2; old keyframes migrate to source-bound animation"
```

---

### Task 5: Evaluator — animated values, fade, invariants

**Files:**
- Modify: `crates/timeline/src/scene.rs`

**Interfaces:**
- Consumes: `Clip::anim_value`, `Animation`, `AnimParam::{read, write}` (Tasks 2–3).
- Produces:
  - `pub struct ClipState { pub transform: Transform, pub color: kadr_project::ColorAdjust, pub fade: f64 }`
  - `pub fn clip_state_at(clip: &Clip, t: Time, frame: Time) -> ClipState` — animated values in source time, normalised (§3.4), fade (§3.3).
  - `pub fn fade_at(clip: &Clip, t: Time, frame: Time) -> f64`.
  - `clip_layer(project, clip, t, canvas, frame: Time)` — **new `frame` parameter** (sequence frame duration); every caller passes `seq.frame_rate.frame_duration()`.

- [ ] **Step 1: Write the failing tests** (in `scene.rs` tests):

```rust
    use kadr_project::{AnimParam, Ease, EdgeFades, SourceTime};

    fn st(ms: i64) -> SourceTime {
        SourceTime(Time::from_millis(ms))
    }
    const F30: Time = Time(705_600_000 / 30);

    fn animated_clip() -> Clip {
        // Source 10..20 s at timeline 0..10 s; opacity 0 → 1 over source 10..12 s (linear).
        let mut c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(Time::from_secs(10), Time::from_secs(20)), Time::ZERO);
        c.animation.set_key(AnimParam::Opacity, st(10_000), 0.0, Some(Ease::Linear));
        c.animation.set_key(AnimParam::Opacity, st(12_000), 1.0, None);
        c
    }

    #[test]
    fn animated_opacity_is_evaluated_in_source_time() {
        let c = animated_clip();
        assert_eq!(clip_state_at(&c, Time::from_secs(1), F30).transform.opacity, 0.5);
        let mut trimmed = c.clone();
        crate::ops::trim_start(&mut trimmed, Time::from_secs(1));
        assert_eq!(clip_state_at(&trimmed, Time::from_secs(1), F30).transform.opacity, 0.5, "same content frame, same value");
        let mut fast = c.clone();
        fast.timeline_out = Time::from_secs(5); // 2×
        assert_eq!(clip_state_at(&fast, Time::from_millis(500), F30).transform.opacity, 0.5, "timeline 0.5 s = source 11 s");
    }

    #[test]
    fn hidden_keyframes_still_shape_the_visible_part() {
        let mut c = animated_clip();
        crate::ops::trim_start(&mut c, Time::from_secs(1)); // source 11 s now at the clip start
        assert_eq!(clip_state_at(&c, c.timeline_in, F30).transform.opacity, 0.5);
    }

    #[test]
    fn fade_contract_first_and_last_frames_transparent_zero_is_one() {
        let mut c = animated_clip();
        c.animation = Default::default();
        c.video_fade = EdgeFades { fade_in: Time::from_secs(1), fade_out: Time::from_secs(1) };
        assert_eq!(fade_at(&c, Time::ZERO, F30), 0.0, "first frame of the fade-in");
        assert_eq!(fade_at(&c, Time::from_secs(1), F30), 1.0, "frame F_in / d");
        assert_eq!(fade_at(&c, c.timeline_out - F30, F30), 0.0, "last frame of the fade-out");
        c.video_fade = EdgeFades::default();
        assert_eq!(fade_at(&c, Time::ZERO, F30), 1.0);
    }

    /// Review focus 3: transition overhang, one-frame clips.
    #[test]
    fn fade_outside_the_clip_or_on_a_one_frame_clip_is_finite() {
        let mut c = animated_clip();
        c.video_fade = EdgeFades { fade_in: Time::from_secs(1), fade_out: Time::from_secs(1) };
        for t in [Time::from_secs(-3), Time::from_secs(30)] {
            let f = fade_at(&c, t, F30);
            assert!((0.0..=1.0).contains(&f), "{f} at {t:?}");
        }
        c.timeline_out = F30;
        assert!(fade_at(&c, Time::ZERO, F30).is_finite());
    }

    #[test]
    fn final_opacity_is_resolved_times_fade_and_zero_is_culled() {
        let mut p = Project::new("t");
        let id = place(&mut p, 0, &hd(), 0, 0, 10_000);
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        c.transform.opacity = 0.8;
        c.video_fade = EdgeFades { fade_in: Time::from_secs(2), fade_out: Time::ZERO };
        let l = &scene_at(&p, 1_000).layers[0];
        assert_eq!(l.id, LayerId::from(id));
        assert!((l.opacity - 0.4).abs() < 1e-6, "{}", l.opacity);
        assert!(scene_at(&p, 0).layers.is_empty(), "fade 0 → the layer is not decoded");
    }

    #[test]
    fn invariants_empty_crop_negative_scale_and_two_turns() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 10_000);
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        c.transform.crop_left = 0.6;
        c.transform.crop_right = 0.5;
        assert!(scene_at(&p, 1_000).layers.is_empty(), "no visible area");
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        c.transform = Transform::default();
        c.transform.scale = -1.0;
        assert!(scene_at(&p, 1_000).layers.is_empty(), "scale ≤ 0");
        let c = &mut p.sequence_mut().tracks[0].clips[0];
        c.transform.scale = 0.5; // not full-frame, so it is not an occluder question
        c.animation.set_key(AnimParam::Rotation, st(0), 0.0, Some(Ease::Linear));
        c.animation.set_key(AnimParam::Rotation, st(10_000), 720.0, None);
        let rot = scene_at(&p, 5_000).layers[0].placement.rotation;
        assert!((rot - 360f32.to_radians()).abs() < 1e-4, "{rot}: no modulo 360");
    }

    /// Review focus 2: a non-finite value evaluates to the default.
    #[test]
    fn non_finite_values_become_defaults() {
        let mut c = animated_clip();
        c.animation = Default::default();
        c.transform.scale = f64::NAN;
        c.color.saturation = f64::INFINITY;
        let s = clip_state_at(&c, Time::ZERO, F30);
        assert_eq!((s.transform.scale, s.color.saturation), (1.0, 1.0));
    }

    /// Spec §4 / review focus 5: split never changes the picture. Exact at
    /// speed 1; at other speeds integer flooring may move a half's source
    /// mapping by one flick, which is far below one 8-bit step.
    #[test]
    fn split_halves_match_the_original_on_every_frame() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        let eases = [Ease::EaseInOut, Ease::Linear, Ease::EaseIn, Ease::EaseOut, Ease::Hold];
        for case in 0..200 {
            let dur_f = 30 + rnd(300) as i64; // frames
            let speed_num = if case % 4 == 0 { 1 } else { 1 + rnd(7) as i64 };
            let speed_den = if case % 4 == 0 { 1 } else { 1 + rnd(5) as i64 };
            let sin = Time::from_millis(rnd(20_000) as i64);
            let tl_dur = F30.mul_ratio(dur_f, 1);
            let mut c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(sin, sin + tl_dur.mul_ratio(speed_num, speed_den)), Time::from_secs(5));
            c.timeline_out = c.timeline_in + tl_dur;
            for _ in 0..(1 + rnd(5)) {
                let p = AnimParam::ALL[rnd(13) as usize];
                let at = SourceTime(sin - Time::from_secs(2) + Time::from_millis(rnd((tl_dur.as_millis() * speed_num / speed_den + 4_000) as u64) as i64));
                c.animation.set_key(p, at, rnd(1000) as f64 / 10.0, Some(eases[rnd(5) as usize].clone()));
            }
            let cut = c.timeline_in + F30.mul_ratio(1 + rnd(dur_f as u64 - 2) as i64, 1);
            let (l, r) = crate::ops::split_clip(&c, cut, &mut crate::ops::LinkRemap::default());
            for f in 0..dur_f {
                let t = c.timeline_in + F30.mul_ratio(f, 1);
                let half = if t < cut { &l } else { &r };
                let ds = (half.source_time_at(t) - c.source_time_at(t)).abs();
                assert!(ds <= Time(1), "case {case} frame {f}: source drift {ds:?}");
                let (a, b) = (clip_state_at(&c, t, F30), clip_state_at(half, t, F30));
                if speed_num == speed_den {
                    assert_eq!((&a.transform, &a.color), (&b.transform, &b.color), "case {case} frame {f}");
                } else {
                    for p in AnimParam::ALL {
                        let (x, y) = (p.read(&a.transform, &a.color), p.read(&b.transform, &b.color));
                        assert!((x - y).abs() <= 1e-6 * x.abs().max(1.0), "case {case} frame {f} {p:?}: {x} vs {y}");
                    }
                }
            }
        }
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-timeline scene`
Expected: compile errors (`clip_state_at`, `fade_at` missing).

- [ ] **Step 3: Implement.** Above `clip_layer` in `scene.rs`:

```rust
/// A clip's visual state at timeline time `t` (keyframes spec §3):
/// animated values resolved in source time, normalised at this boundary
/// (§3.4), and the clip-bound fade factor (§3.3).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipState {
    pub transform: Transform,
    pub color: ClipColor,
    pub fade: f64,
}

pub fn clip_state_at(clip: &Clip, t: Time, frame: Time) -> ClipState {
    let mut transform = clip.transform.clone();
    let mut color = clip.color.clone();
    if !clip.animation.is_empty() {
        let s = kadr_project::SourceTime(clip.source_time_at(t));
        for p in clip.animation.params() {
            if let Some(v) = clip.animation.value_at(p, s) {
                p.write(&mut transform, &mut color, v);
            }
        }
    }
    normalise(&mut transform, &mut color);
    ClipState { transform, color, fade: fade_at(clip, t, frame) }
}

/// Non-finite → default; opacity and crop in `[0, 1]`; contrast and
/// saturation ≥ 0; exposure and temperature in the inspector's ranges.
/// Rotation is left as is (0 → 720° is two turns). Scale ≤ 0 and empty
/// crops are dropped by `clip_layer`, not here.
fn normalise(t: &mut Transform, c: &mut ClipColor) {
    for p in kadr_project::AnimParam::ALL {
        let v = p.read(t, c);
        let v = if v.is_finite() { v } else { p.default_value() };
        use kadr_project::AnimParam as P;
        let v = match p {
            P::Opacity | P::CropLeft | P::CropRight | P::CropTop | P::CropBottom => v.clamp(0.0, 1.0),
            P::Contrast | P::Saturation => v.max(0.0),
            P::Exposure => v.clamp(-2.0, 2.0),
            P::Temperature => v.clamp(-1.0, 1.0),
            _ => v,
        };
        p.write(t, c, v);
    }
}

/// Clip-bound fade factor (spec §3.3). `t` is clamped into the clip's own
/// frames, so a transition asking for the clip beyond its edge gets the
/// edge's value; zero-length fades are 1 without dividing.
pub fn fade_at(clip: &Clip, t: Time, frame: Time) -> f64 {
    let last = (clip.timeline_out - frame).max(clip.timeline_in);
    let tt = t.max(clip.timeline_in).min(last);
    let part = |num: Time, len: Time| if len <= Time::ZERO { 1.0 } else { (num.flicks() as f64 / len.flicks() as f64).clamp(0.0, 1.0) };
    part(tt - clip.timeline_in, clip.video_fade.fade_in) * part(last - tt, clip.video_fade.fade_out)
}
```

Change `clip_layer` to take `frame: Time` and use the state:

```rust
pub fn clip_layer(project: &Project, clip: &Clip, t: Time, canvas: SizeU, frame: Time) -> Option<Layer> {
    // … asset, kind, display_size, source_time unchanged …
    let st = clip_state_at(clip, t, frame);
    if st.transform.scale <= 0.0 {
        return None;
    }
    let placement = placement_of(&st.transform, fit_contain(display_size, canvas), canvas);
    let crop = crop_of(&st.transform, placement.size)?;
    let mut effects = vec![];
    let color = color_of(&st.color);
    if !color.is_neutral() {
        effects.push(Effect::ColorAdjust(color));
    }
    Some(Layer {
        // … id, content unchanged …
        placement,
        crop,
        opacity: (st.transform.opacity * st.fade) as f32,
        blend: BlendMode::Normal,
        effects,
    })
}
```

Update callers: in `track_layer` → `clip_layer(project, clip, t, canvas, seq.frame_rate.frame_duration())`; in `transition_layer` both calls likewise. Run `cargo build --workspace` and fix any other caller the compiler lists the same way (pass the sequence's `frame_rate.frame_duration()`).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-timeline && cargo test --workspace`
Expected: all pass (golden and playback tests unchanged: static clips evaluate exactly as before).

- [ ] **Step 5: Commit**

```bash
git add crates/timeline
git commit -m "timeline/scene: animated values in source time, clip fades, normalisation at the evaluation boundary"
```

---

### Task 6: Edit rules — fade clamping on trim/split/speed, `SwitchAngle` shifts keyframes

**Files:**
- Modify: `crates/timeline/src/ops.rs` (`trim_start`, `trim_end`, `split_clip`), `crates/timeline/src/commands.rs` (`ClipProperty::Speed`, `SwitchAngle`)
- Test: `crates/timeline/tests/animation_edits.rs` (new)

**Interfaces:**
- Consumes: `Clip::clamp_fades`, `FadeEdge`, `Animation::shift_all`.
- Produces: all duration-changing edits keep the fade invariant; `SwitchAngle` shifts every keyframe by `new source_in − old source_in`.

- [ ] **Step 1: Write the failing tests** — `crates/timeline/tests/animation_edits.rs`:

```rust
use kadr_core::{FrameRate, Time, TimeRange};
use kadr_project::*;
use kadr_timeline::*;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

/// One 10 s clip (source 100..110 s) on V1 with fades 3 s / 4 s and a keyframe.
fn setup() -> (Project, EditEngine, kadr_core::ClipId) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let mut c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(s(100), s(110)), s(0));
    c.video_fade = EdgeFades { fade_in: s(3), fade_out: s(4) };
    c.audio.fade_in = s(3);
    c.audio.fade_out = s(4);
    c.animation.set_key(AnimParam::Scale, SourceTime(s(105)), 2.0, None);
    let id = c.id;
    p.sequence_mut().tracks[0].clips.push(c);
    (p, EditEngine::new(), id)
}

fn fades(p: &Project, id: kadr_core::ClipId) -> (i64, i64) {
    let c = p.sequence().clip(id).unwrap();
    (c.video_fade.fade_in.as_millis(), c.video_fade.fade_out.as_millis())
}

#[test]
fn trimming_the_start_shrinks_fade_in_first_and_keeps_keyframes() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::TrimClip { clip: id, edge: TrimEdge::Start, to: s(5), ripple: false }).unwrap();
    assert_eq!(fades(&p, id), (1_000, 4_000));
    let c = p.sequence().clip(id).unwrap();
    assert_eq!(c.animation.times(), vec![SourceTime(s(105))], "source-bound: untouched");
    assert_eq!((c.audio.fade_in, c.audio.fade_out), (s(1), s(4)), "audio fades follow the same rule");
    e.undo(&mut p);
    assert_eq!(fades(&p, id), (3_000, 4_000), "undo restores the stored values");
}

#[test]
fn trimming_the_end_or_speeding_up_shrinks_fade_out_first() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::TrimClip { clip: id, edge: TrimEdge::End, to: s(5), ripple: false }).unwrap();
    assert_eq!(fades(&p, id), (3_000, 2_000));
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Speed(2.0) }).unwrap();
    assert_eq!(fades(&p, id), (3_000, 2_000), "5 s long now; fade durations are not scaled");
    assert_eq!(p.sequence().clip(id).unwrap().animation.times(), vec![SourceTime(s(105))]);
}

#[test]
fn split_zeroes_the_inner_fades_and_copies_the_animation() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::Split { at: s(6), clips: Some(vec![id]) }).unwrap();
    let t = &p.sequence().tracks[0].clips;
    assert_eq!((t[0].video_fade.fade_in, t[0].video_fade.fade_out), (s(3), Time::ZERO));
    assert_eq!((t[1].video_fade.fade_in, t[1].video_fade.fade_out), (Time::ZERO, s(4)));
    assert_eq!(t[0].animation, t[1].animation, "each half has a full copy");
}

#[test]
fn moving_a_clip_touches_neither_fades_nor_keyframes() {
    let (mut p, mut e, id) = setup();
    let before = p.sequence().clip(id).unwrap().clone();
    e.execute(&mut p, EditCommand::MoveClips { clips: vec![id], delta: s(20), track_delta: 0 }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    assert_eq!((&c.video_fade, &c.animation), (&before.video_fade, &before.animation));
}
```

Append to `crates/timeline/tests/engine.rs` (uses its `multicam_project`):

```rust
#[test]
fn switch_angle_shifts_keyframes_by_the_sync_difference() {
    let (mut p, mut e, clip, _) = multicam_project(10, 20);
    let c = p.sequence_mut().tracks[0].clips.iter_mut().find(|c| c.id == clip).unwrap();
    c.animation.set_key(AnimParam::Scale, SourceTime(s(12)), 2.0, None); // CAM1 source 12 s = group 12 s
    e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1, at: None }).unwrap();
    let c = p.sequence().clip(clip).unwrap();
    assert_eq!(c.animation.times(), vec![SourceTime(s(10))], "group 12 s is CAM2 source 10 s");
    assert_eq!(c.timeline_time_of(s(10)), s(2), "same place on the timeline");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-timeline --test animation_edits --test engine`
Expected: FAIL on the fade values and the SwitchAngle keyframe.

- [ ] **Step 3: Implement.** `ops.rs`:

```rust
/// Moves the start edge to `t`, keeping the source frame under `timeline_out`.
pub fn trim_start(clip: &mut Clip, t: Time) {
    clip.source_in = clip.source_time_at(t);
    clip.timeline_in = t;
    clip.clamp_fades(kadr_project::FadeEdge::In);
}

pub fn trim_end(clip: &mut Clip, t: Time) {
    clip.source_out = clip.source_time_at(t);
    clip.timeline_out = t;
    clip.clamp_fades(kadr_project::FadeEdge::Out);
}
```

In `split_clip`, zero the inner fades **before** trimming (so the clamp sees the final values):

```rust
    let mut left = clip.clone();
    let mut right = clip.clone();
    // Fades belong to the outer edges.
    left.audio.fade_out = Time::ZERO;
    left.video_fade.fade_out = Time::ZERO;
    right.audio.fade_in = Time::ZERO;
    right.video_fade.fade_in = Time::ZERO;
    trim_end(&mut left, t);
    trim_start(&mut right, t);
    right.id = ClipId::new();
    right.link = links.right_link(clip.link);
    (left, right)
```

`commands.rs`, `ClipProperty::Speed` branch — after `c.timeline_out = out;` add `c.clamp_fades(kadr_project::FadeEdge::Out);`. `SwitchAngle` — before `c.source_in = src.start;` capture and shift:

```rust
                let c = &mut seq.tracks[ti].clips[ci];
                let shift = src.start - c.source_in;
                c.animation.shift_all(shift);
                c.asset = asset;
                c.source_in = src.start;
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-timeline`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/timeline
git commit -m "timeline: fades clamp on trim/split/speed (edited edge first); SwitchAngle shifts keyframes"
```

---

### Task 7: Keyframe and fade commands in `EditEngine`

**Files:**
- Modify: `crates/timeline/src/commands.rs`, `crates/timeline/src/lib.rs` (re-exports unchanged), `crates/i18n/locales/en.json`, `ru.json`
- Test: `crates/timeline/tests/animation_edits.rs`

**Interfaces:**
- Consumes: Task 3 ops.
- Produces (new `EditCommand` variants, all `Serialize/Deserialize`):
  - `SetKeyframe { clip: ClipId, param: AnimParam, at: SourceTime, value: f64, ease: Option<Ease> }` — video clips only (`KindMismatch` on audio tracks), `value` finite (`InvalidRange`).
  - `RemoveKeyframe { clip, param, at, keep: Option<f64> }` — when the track empties, the static field becomes `keep` (the caller's value at the playhead) or else the removed keyframe's value.
  - `MoveKeyframes { clip, moves: Vec<(AnimParam, SourceTime, SourceTime)> }` — all or nothing; collision → `EditError::KeyframeCollision`.
  - `SetKeyframeEase { clip, param, at, ease: Ease }`.
  - `ClearAnimation { clip, statics: Vec<(AnimParam, f64)> }` — removes those tracks and sets each static field (reset-to-default and "clear, keep the picture" are the same command with different values).
  - `ClipProperty::VideoFade(EdgeFades)` — each ≥ 0 and sum ≤ duration, else `InvalidRange`.
  - `EditError::KeyframeCollision` → `"err.edit.keyframe_collision"`; labels `cmd.keyframe`, `cmd.remove_keyframe`, `cmd.move_keyframes`, `cmd.keyframe_ease`, `cmd.clear_animation`.

- [ ] **Step 1: Write the failing tests** (append to `animation_edits.rs`):

```rust
fn scale_track(p: &Project, id: kadr_core::ClipId) -> Vec<(i64, f64)> {
    p.sequence().clip(id).unwrap().animation.track(AnimParam::Scale).iter().map(|k| (k.at.0.as_millis(), k.value)).collect()
}

#[test]
fn set_and_remove_keyframe_are_one_undo_step_each() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(108)), value: 3.0, ease: None }).unwrap();
    assert_eq!(scale_track(&p, id), vec![(105_000, 2.0), (108_000, 3.0)]);
    e.undo(&mut p);
    assert_eq!(scale_track(&p, id), vec![(105_000, 2.0)]);
}

#[test]
fn removing_the_last_keyframe_keeps_the_picture_at_the_playhead() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::RemoveKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(105)), keep: Some(2.0) }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    assert!(!c.animation.is_animated(AnimParam::Scale));
    assert_eq!(c.transform.scale, 2.0);
}

#[test]
fn moving_keyframes_onto_another_is_rejected_whole() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframe { clip: id, param: AnimParam::Scale, at: SourceTime(s(108)), value: 3.0, ease: None }).unwrap();
    let before = p.sequence().clone();
    let r = e.execute(&mut p, EditCommand::MoveKeyframes { clip: id, moves: vec![(AnimParam::Scale, SourceTime(s(105)), SourceTime(s(108)))] });
    assert_eq!(r, Err(EditError::KeyframeCollision));
    assert_eq!(p.sequence(), &before);
    assert_eq!(EditCommand::error_key(&EditError::KeyframeCollision), "err.edit.keyframe_collision");
}

#[test]
fn ease_and_clear_animation() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframeEase { clip: id, param: AnimParam::Scale, at: SourceTime(s(105)), ease: Ease::Hold }).unwrap();
    assert_eq!(p.sequence().clip(id).unwrap().animation.track(AnimParam::Scale)[0].ease, Ease::Hold);
    e.execute(&mut p, EditCommand::ClearAnimation { clip: id, statics: vec![(AnimParam::Scale, 1.0), (AnimParam::X, 0.0)] }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    assert!(c.animation.is_empty());
    assert_eq!((c.transform.scale, c.transform.x), (1.0, 0.0));
}

#[test]
fn keyframes_are_video_only_and_values_finite() {
    let (mut p, mut e, id) = setup();
    let bad = EditCommand::SetKeyframe { clip: id, param: AnimParam::X, at: SourceTime(s(101)), value: f64::NAN, ease: None };
    assert_eq!(e.execute(&mut p, bad), Err(EditError::InvalidRange));
    let a = Clip::new(kadr_core::AssetId::new(), "a", TimeRange::new(s(0), s(5)), s(0));
    let aid = a.id;
    p.sequence_mut().tracks[1].clips.push(a); // A1
    let on_audio = EditCommand::SetKeyframe { clip: aid, param: AnimParam::X, at: SourceTime(s(1)), value: 1.0, ease: None };
    assert_eq!(e.execute(&mut p, on_audio), Err(EditError::KindMismatch));
}

#[test]
fn video_fade_property_is_validated() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetClipProperty { clip: id, prop: ClipProperty::VideoFade(EdgeFades { fade_in: s(1), fade_out: s(2) }) }).unwrap();
    assert_eq!(fades(&p, id), (1_000, 2_000));
    let too_long = EditCommand::SetClipProperty { clip: id, prop: ClipProperty::VideoFade(EdgeFades { fade_in: s(6), fade_out: s(5) }) };
    assert_eq!(e.execute(&mut p, too_long), Err(EditError::InvalidRange));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-timeline --test animation_edits`
Expected: compile errors (variants missing).

- [ ] **Step 3: Implement.** In `commands.rs`: import `kadr_project::{AnimParam, AnimError, Ease, EdgeFades, SourceTime}`. Add `KeyframeCollision` to `EditError` (`#[error("a keyframe already exists at that time")]`) and its `error_key`. Add `VideoFade(EdgeFades)` to `ClipProperty` and its arm in `SetClipProperty`:

```rust
                    ClipProperty::VideoFade(f) => {
                        if f.fade_in < Time::ZERO || f.fade_out < Time::ZERO || f.fade_in + f.fade_out > c.duration() {
                            return Err(EditError::InvalidRange);
                        }
                        c.video_fade = *f;
                    }
```

Add the variants to `EditCommand` (before `Batch`):

```rust
    /// Inserts or updates a keyframe (`ease: None` → keep / inherit, spec §3.1).
    SetKeyframe { clip: ClipId, param: AnimParam, at: SourceTime, value: f64, ease: Option<Ease> },
    /// Removes a keyframe; if it was the property's last, the static value
    /// becomes `keep` (the value at the playhead) or the removed value.
    RemoveKeyframe { clip: ClipId, param: AnimParam, at: SourceTime, keep: Option<f64> },
    /// Moves keyframes (all or nothing); a move onto another keyframe of the
    /// same property is rejected.
    MoveKeyframes { clip: ClipId, moves: Vec<(AnimParam, SourceTime, SourceTime)> },
    SetKeyframeEase { clip: ClipId, param: AnimParam, at: SourceTime, ease: Ease },
    /// Drops the tracks of the listed properties and sets their static values.
    ClearAnimation { clip: ClipId, statics: Vec<(AnimParam, f64)> },
```

Labels (in `label()`): `SetKeyframe` → `"cmd.keyframe"`, `RemoveKeyframe` → `"cmd.remove_keyframe"`, `MoveKeyframes` → `"cmd.move_keyframes"`, `SetKeyframeEase` → `"cmd.keyframe_ease"`, `ClearAnimation` → `"cmd.clear_animation"`.

Apply arms plus a helper:

```rust
/// The unlocked video clip `id` for an animation edit.
fn anim_clip<'a>(seq: &'a mut Sequence, id: ClipId) -> Result<&'a mut Clip, EditError> {
    let (ti, ci) = seq.locate_clip(id).ok_or(EditError::ClipNotFound)?;
    let t = &mut seq.tracks[ti];
    if t.locked {
        return Err(EditError::TrackLocked(t.name.clone()));
    }
    if t.kind != TrackKind::Video {
        return Err(EditError::KindMismatch);
    }
    Ok(&mut t.clips[ci])
}

fn anim_err(e: AnimError) -> EditError {
    match e {
        AnimError::NoKeyframe => EditError::NoOp,
        AnimError::Collision => EditError::KeyframeCollision,
    }
}
```

```rust
            EditCommand::SetKeyframe { clip, param, at, value, ease } => {
                if !value.is_finite() {
                    return Err(EditError::InvalidRange);
                }
                anim_clip(seq, *clip)?.animation.set_key(*param, *at, *value, ease.clone());
                Ok(())
            }
            EditCommand::RemoveKeyframe { clip, param, at, keep } => {
                let c = anim_clip(seq, *clip)?;
                let k = c.animation.remove_key(*param, *at).ok_or(EditError::NoOp)?;
                if !c.animation.is_animated(*param) {
                    param.write(&mut c.transform, &mut c.color, keep.filter(|v| v.is_finite()).unwrap_or(k.value));
                }
                Ok(())
            }
            EditCommand::MoveKeyframes { clip, moves } => {
                let c = anim_clip(seq, *clip)?;
                if moves.is_empty() {
                    return Err(EditError::NoOp);
                }
                // Remove every moving keyframe first, so keyframes can swap past each other.
                let mut taken = vec![];
                for (p, from, to) in moves {
                    let k = c.animation.remove_key(*p, *from).ok_or(EditError::NoOp)?;
                    taken.push((*p, *to, k));
                }
                for (p, to, k) in taken {
                    if c.animation.track(p).iter().any(|x| x.at == to) {
                        return Err(EditError::KeyframeCollision); // the engine restores the snapshot
                    }
                    c.animation.set_key(p, to, k.value, Some(k.ease));
                }
                Ok(())
            }
            EditCommand::SetKeyframeEase { clip, param, at, ease } => {
                anim_clip(seq, *clip)?.animation.set_ease(*param, *at, ease.clone()).map_err(anim_err)
            }
            EditCommand::ClearAnimation { clip, statics } => {
                let c = anim_clip(seq, *clip)?;
                if statics.is_empty() {
                    return Err(EditError::NoOp);
                }
                for (p, v) in statics {
                    c.animation.clear(*p);
                    p.write(&mut c.transform, &mut c.color, *v);
                }
                Ok(())
            }
```

Note: `set_key` on a moved keyframe re-inserts it with its own `ease` (`Some(k.ease)`), so moving never changes a curve.

i18n (both files; RU in `ru.json`):

| key | en | ru |
|---|---|---|
| `cmd.keyframe` | Keyframe | Ключевой кадр |
| `cmd.keyframe_named` | Keyframe: {name} | Ключевой кадр: {name} |
| `cmd.remove_keyframe` | Remove keyframe | Удалить ключевой кадр |
| `cmd.move_keyframes` | Move keyframes | Перемещение ключевых кадров |
| `cmd.keyframe_ease` | Keyframe interpolation | Интерполяция кадра |
| `cmd.clear_animation` | Clear animation | Сброс анимации |
| `cmd.reset_section` | Reset: {name} | Сброс: {name} |
| `cmd.fade_named` | {name}: {from} → {to} | {name}: {from} → {to} |
| `err.edit.keyframe_collision` | A keyframe already exists at that time | В этой точке уже есть ключевой кадр |

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-timeline && cargo test -p kadr-i18n`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/timeline crates/i18n
git commit -m "timeline: keyframe commands, ClearAnimation, VideoFade property, collision error"
```

---

### Task 8: Inspector — `◀ ◆ ▶`, animated sliders, fade section

**Files:**
- Create: `apps/editor/src/anim_ui.rs` (+ `mod anim_ui;` in `main.rs`)
- Modify: `apps/editor/src/inspector.rs`, `apps/editor/ui/inspector.slint`, `apps/editor/ui/widgets.slint`, `apps/editor/ui/app.slint`, `apps/editor/src/app.rs` (callback wiring, inspector refresh throttle field), `apps/editor/src/timeline_ui.rs` (`refresh_playhead`), locales

**Interfaces:**
- Consumes: Task 7 commands; `Clip::{anim_value, key_near}`.
- Produces (in `anim_ui.rs`, all pure):
  - `pub fn param_of(prop: &str) -> Option<AnimParam>` — inspector prop names `x y scale rotation opacity crop-l crop-r crop-t crop-b exposure contrast saturation`.
  - `pub fn label_key(p: AnimParam) -> &'static str` — i18n key of the property's label.
  - `pub fn half_frame(seq: &Sequence) -> Time`.
  - `pub fn edit_time(clip: &Clip, playhead: Time, frame: Time) -> Option<Time>` — `Some(playhead)` when `timeline_in ≤ playhead < timeline_out`, else `None` (review focus 1).
  - `pub fn shown_time(clip: &Clip, playhead: Time, frame: Time) -> Time` — playhead clamped into `[timeline_in, timeline_out − frame]`.
  - `pub struct KeyInfo { pub state: i32, pub prev: Option<Time>, pub next: Option<Time> }` (`state`: 0 not animated, 1 animated, no key here, 2 key here) and `pub fn key_info(clip, p, playhead, frame) -> KeyInfo` — prev/next among **visible** keyframes (timeline time in `[in, out)`), excluding the one "here".
- Slint: `export struct KeyState { state: int, prev: bool, next: bool, enabled: bool }`, `export component KeyNav`; `InspectorData` gains `keys: [KeyState]` (indexed by `AnimParam::index()`), `outside: bool`, `vfade-in`, `vfade-out`, `fade-max: float`; `Inspector` gains `callback key-action(string /*prop*/, string /*action*/)`.

- [ ] **Step 1: Write the failing tests** — `apps/editor/src/anim_ui.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, TimeRange};
    use kadr_project::{Ease, SourceTime};

    const F: Time = Time(705_600_000 / 30);

    fn clip() -> Clip {
        // timeline 10..20 s, source 0..10 s
        Clip::new(AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::from_secs(10))
    }

    #[test]
    fn inspector_props_map_to_params() {
        assert_eq!(param_of("crop-l"), Some(AnimParam::CropLeft));
        assert_eq!(param_of("speed"), None);
        for p in AnimParam::ALL.into_iter().filter(|p| *p != AnimParam::Temperature) {
            assert!(PROPS.iter().any(|(_, q)| *q == p), "{p:?} has an inspector row");
        }
    }

    /// Review focus 1.
    #[test]
    fn outside_the_clip_there_is_no_edit_time_and_values_show_the_edge() {
        let c = clip();
        assert_eq!(edit_time(&c, Time::from_secs(5), F), None);
        assert_eq!(edit_time(&c, Time::from_secs(20), F), None, "timeline_out is outside");
        assert_eq!(edit_time(&c, Time::from_secs(12), F), Some(Time::from_secs(12)));
        assert_eq!(shown_time(&c, Time::from_secs(5), F), Time::from_secs(10));
        assert_eq!(shown_time(&c, Time::from_secs(25), F), Time::from_secs(20) - F);
    }

    #[test]
    fn key_info_states_and_visible_neighbours() {
        let mut c = clip();
        assert_eq!(key_info(&c, AnimParam::X, Time::from_secs(12), F).state, 0);
        for (src, v) in [(-5, 0.0), (2, 1.0), (5, 2.0), (15, 3.0)] {
            c.animation.set_key(AnimParam::X, SourceTime(Time::from_secs(src)), v, Some(Ease::Linear));
        }
        let at = key_info(&c, AnimParam::X, Time::from_secs(15), F);
        assert_eq!((at.state, at.prev, at.next), (2, Some(Time::from_secs(12)), None), "hidden −5 s and 25 s are not neighbours");
        let between = key_info(&c, AnimParam::X, Time::from_secs(13), F);
        assert_eq!((between.state, between.prev, between.next), (1, Some(Time::from_secs(12)), Some(Time::from_secs(15))));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-editor anim_ui`
Expected: compile errors.

- [ ] **Step 3: Implement `anim_ui.rs`:**

```rust
//! Pure keyframe/fade UI logic shared by the inspector and the timeline.

use kadr_core::Time;
use kadr_project::{AnimParam, Clip, Sequence};

/// Inspector property names of the animatable rows (Temperature has no row).
pub const PROPS: [(&str, AnimParam); 12] = [
    ("x", AnimParam::X),
    ("y", AnimParam::Y),
    ("scale", AnimParam::Scale),
    ("rotation", AnimParam::Rotation),
    ("opacity", AnimParam::Opacity),
    ("crop-l", AnimParam::CropLeft),
    ("crop-r", AnimParam::CropRight),
    ("crop-t", AnimParam::CropTop),
    ("crop-b", AnimParam::CropBottom),
    ("exposure", AnimParam::Exposure),
    ("contrast", AnimParam::Contrast),
    ("saturation", AnimParam::Saturation),
];

pub fn param_of(prop: &str) -> Option<AnimParam> {
    PROPS.iter().find(|(n, _)| *n == prop).map(|(_, p)| *p)
}

pub fn label_key(p: AnimParam) -> &'static str {
    match p {
        AnimParam::X => "inspector.pos_x",
        AnimParam::Y => "inspector.pos_y",
        AnimParam::Scale => "inspector.scale",
        AnimParam::Rotation => "inspector.rotation",
        AnimParam::Opacity => "inspector.opacity",
        AnimParam::CropLeft => "inspector.crop_l",
        AnimParam::CropRight => "inspector.crop_r",
        AnimParam::CropTop => "inspector.crop_t",
        AnimParam::CropBottom => "inspector.crop_b",
        AnimParam::Exposure => "inspector.exposure",
        AnimParam::Contrast => "inspector.contrast",
        AnimParam::Saturation => "inspector.saturation",
        AnimParam::Temperature => "inspector.temperature",
    }
}

pub fn half_frame(seq: &Sequence) -> Time {
    Time(seq.frame_rate.frame_duration().flicks() / 2)
}

pub fn edit_time(clip: &Clip, playhead: Time, _frame: Time) -> Option<Time> {
    (clip.timeline_in <= playhead && playhead < clip.timeline_out).then_some(playhead)
}

pub fn shown_time(clip: &Clip, playhead: Time, frame: Time) -> Time {
    playhead.max(clip.timeline_in).min((clip.timeline_out - frame).max(clip.timeline_in))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyInfo {
    pub state: i32,
    pub prev: Option<Time>,
    pub next: Option<Time>,
}

pub fn key_info(clip: &Clip, p: AnimParam, playhead: Time, frame: Time) -> KeyInfo {
    if !clip.animation.is_animated(p) {
        return KeyInfo { state: 0, prev: None, next: None };
    }
    let half = Time(frame.flicks() / 2);
    let visible: Vec<Time> = clip
        .animation
        .track(p)
        .iter()
        .map(|k| clip.timeline_time_of(k.at.0))
        .filter(|t| *t >= clip.timeline_in && *t < clip.timeline_out)
        .collect();
    let here = edit_time(clip, playhead, frame).is_some() && clip.key_near(p, playhead, half).is_some();
    KeyInfo {
        state: if here { 2 } else { 1 },
        prev: visible.iter().rev().copied().find(|t| *t <= playhead - half),
        next: visible.iter().copied().find(|t| *t >= playhead + half),
    }
}
```

`widgets.slint` — add after `PropSlider`:

```slint
export struct KeyState { state: int, prev: bool, next: bool, enabled: bool }

// ◀ ◆ ▶ next to an animatable property (keyframes spec §6.1).
export component KeyNav inherits HorizontalLayout {
    in property <KeyState> k;
    callback nav(string); // "prev" | "toggle" | "next"
    spacing: 0px;
    width: 54px;
    for b[i] in [{ g: "◀", a: "prev", tip: Tr.t("inspector.key.prev") }, { g: root.k.state == 2 ? "◆" : "◇", a: "toggle", tip: Tr.t("inspector.key.toggle") }, { g: "▶", a: "next", tip: Tr.t("inspector.key.next") }]: Rectangle {
        width: 18px;
        property <bool> on: i == 0 ? root.k.prev : i == 2 ? root.k.next : root.k.enabled;
        Text {
            text: b.g;
            font-size: i == 1 ? 13px : 9px;
            horizontal-alignment: center;
            vertical-alignment: center;
            color: !parent.on ? Theme.text-faint : (i == 1 && root.k.state > 0) ? Theme.accent : (bta.has-hover ? Theme.text : Theme.text-dim);
        }
        bta := TouchArea {
            enabled: parent.on;
            mouse-cursor: parent.on ? pointer : default;
            clicked => { root.nav(b.a); }
        }
    }
}
```

`PropSlider`: add `in property <bool> animated;` and, in the label `Text`, prefix the text: `text: (root.animated ? "● " : "") + root.label;` with the dot coloured by making the whole label `Theme.accent` when animated: `color: root.animated ? Theme.accent : (existing expression)`. (Import `Tr` in `widgets.slint` if not imported.)

`inspector.slint`: add `KeyNav, KeyState` to the import; extend `InspectorData` with `keys: [KeyState], outside: bool, vfade-in: float, vfade-out: float, fade-max: float`; add `callback key-action(string, string);`. Wrap each of the 12 animatable rows like this (x shown; index = `AnimParam::index()`: x 0, y 1, scale 2, rotation 3, opacity 4, crop-l 5, crop-r 6, crop-t 7, crop-b 8, exposure 9, contrast 10, saturation 11):

```slint
                if root.d.has-video-props: kx := ContextMenuArea {
                    Menu {
                        Menu {
                            title: Tr.t("anim.ease");
                            MenuItem { title: Tr.t("anim.ease.ease_in_out"); enabled: root.d.keys[0].state == 2; activated => { root.key-action("x", "ease:ease_in_out"); } }
                            MenuItem { title: Tr.t("anim.ease.linear"); enabled: root.d.keys[0].state == 2; activated => { root.key-action("x", "ease:linear"); } }
                            MenuItem { title: Tr.t("anim.ease.ease_in"); enabled: root.d.keys[0].state == 2; activated => { root.key-action("x", "ease:ease_in"); } }
                            MenuItem { title: Tr.t("anim.ease.ease_out"); enabled: root.d.keys[0].state == 2; activated => { root.key-action("x", "ease:ease_out"); } }
                            MenuItem { title: Tr.t("anim.ease.hold"); enabled: root.d.keys[0].state == 2; activated => { root.key-action("x", "ease:hold"); } }
                        }
                        MenuItem { title: Tr.t("anim.clear"); enabled: root.d.keys[0].state > 0; activated => { root.key-action("x", "clear"); } }
                    }
                    HorizontalLayout {
                        spacing: 4px;
                        PropSlider { label: Tr.t("inspector.pos_x"); animated: root.d.keys[0].state > 0; enabled: !(root.d.outside && root.d.keys[0].state > 0); value: root.d.x; minimum: -1920; maximum: 1920; default-value: 0; decimals: 0; unit: " px"; changed(v) => { root.live("x", v); } committed(v) => { root.commit("x", v); } }
                        KeyNav { k: root.d.keys[0]; nav(a) => { root.key-action("x", a); } }
                    }
                }
```

Repeat for the other 11 rows with their existing slider properties, prop names and indices. After the crop rows add the hint (shown only when needed):

```slint
                if root.d.has-video-props && root.d.outside: Text { text: Tr.t("inspector.key.outside"); font-size: Theme.font-xs; color: Theme.text-faint; wrap: word-wrap; }
```

New fade section for video clips (after the colour rows, before the stabilisation row):

```slint
                if root.d.has-video-props: SectionTitle { text: Tr.t("inspector.fades"); }
                if root.d.has-video-props: PropSlider { label: Tr.t("inspector.fade_in"); value: root.d.vfade-in; minimum: 0; maximum: max(root.d.fade-max, 0.01); default-value: 0; unit: " s"; changed(v) => { root.live("vfade-in", v); } committed(v) => { root.commit("vfade-in", v); } }
                if root.d.has-video-props: PropSlider { label: Tr.t("inspector.fade_out"); value: root.d.vfade-out; minimum: 0; maximum: max(root.d.fade-max, 0.01); default-value: 0; unit: " s"; changed(v) => { root.live("vfade-out", v); } committed(v) => { root.commit("vfade-out", v); } }
```

Change the two audio fade sliders' `maximum: 5` to `maximum: max(root.d.fade-max, 0.01)`.

`app.slint`: in the `Inspector { … }` block add `key-action(p, a) => { root.inspector-key(p, a); root.focus-editor(); }` and declare `callback inspector-key(string, string);` next to the other inspector callbacks. `app.rs` `wire_callbacks`: `cb!(ui.on_inspector_key, |p, a| |app: &mut App| app.inspector_key(&p, &a));`.

`inspector.rs` changes:

1. `apply()` gains the video fades and snaps durations to frames (the caller passes the frame):

```rust
fn apply(c: &mut Clip, prop: &str, v: f64, frame: Time) {
    // Fade durations from a slider: seconds → whole frames (rounded).
    let frames = |secs: f64| {
        let f = frame.flicks().max(1);
        Time((Time::from_secs_f64(secs.max(0.0)).flicks() + f / 2) / f * f)
    };
    // … existing transform/colour/audio arms, with fades using `frames(v)`:
    // "fade-in"  => c.audio.fade_in  = frames(v).min(c.duration() - c.audio.fade_out),
    // "fade-out" => c.audio.fade_out = frames(v).min(c.duration() - c.audio.fade_in),
    // "vfade-in" => c.video_fade.fade_in  = frames(v).min(c.duration() - c.video_fade.fade_out),
    // "vfade-out"=> c.video_fade.fade_out = frames(v).min(c.duration() - c.video_fade.fade_in),
}
```

   and `property_for` maps `"vfade-in" | "vfade-out"` → `ClipProperty::VideoFade(c.video_fade)`.

2. Animated properties write keyframes instead of statics. In `inspector_live`, after the snapshot is taken and before `apply`:

```rust
        let fr = self.project.sequence().frame_rate;
        let half = crate::anim_ui::half_frame(self.project.sequence());
        if let Some(p) = crate::anim_ui::param_of(prop) {
            let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
            if c.animation.is_animated(p) {
                let Some(t) = crate::anim_ui::edit_time(c, self.playhead, fr.frame_duration()) else { return };
                let at = c.key_near(p, t, half).unwrap_or(kadr_project::SourceTime(c.source_time_at(t)));
                c.animation.set_key(p, at, v as f64, None);
                self.preview.invalidate();
                self.refresh_inspector();
                self.request_frame();
                return;
            }
        }
```

   In `inspector_commit`, after restoring the snapshot and before building `cmd`:

```rust
        if let Some(p) = crate::anim_ui::param_of(prop) {
            let seq = self.project.sequence();
            let c = &seq.tracks[ti].clips[ci];
            if c.animation.is_animated(p) {
                let Some(t) = crate::anim_ui::edit_time(c, self.playhead, seq.frame_rate.frame_duration()) else { return };
                let at = c.key_near(p, t, crate::anim_ui::half_frame(seq)).unwrap_or(kadr_project::SourceTime(c.source_time_at(t)));
                let label = tf("cmd.keyframe_named", &[("name", &t(crate::anim_ui::label_key(p)))]);
                let cmd = EditCommand::SetKeyframe { clip: id, param: p, at, value: v as f64, ease: None };
                self.execute(EditCommand::Batch { label, commands: vec![cmd] });
                return;
            }
        }
```

   (rename the local `t` of the playhead time to `tt` where it would shadow `kadr_i18n::t`.) Pass `seq.frame_rate.frame_duration()` to every `apply` call.

3. `refresh_inspector` for a video clip: values are shown at `shown_time` and key states are filled:

```rust
            let fr_d = fr.frame_duration();
            let at = crate::anim_ui::shown_time(c, self.playhead, fr_d);
            let v = |p| c.anim_value(p, at) as f32;
            use kadr_project::AnimParam as P;
            d.x = v(P::X); d.y = v(P::Y); d.scale = v(P::Scale); d.rotation = v(P::Rotation); d.opacity = v(P::Opacity);
            d.crop_l = v(P::CropLeft); d.crop_r = v(P::CropRight); d.crop_t = v(P::CropTop); d.crop_b = v(P::CropBottom);
            d.exposure = v(P::Exposure); d.contrast = v(P::Contrast); d.saturation = v(P::Saturation);
            d.outside = crate::anim_ui::edit_time(c, self.playhead, fr_d).is_none();
            let keys: Vec<crate::KeyState> = P::ALL
                .iter()
                .map(|p| {
                    let k = crate::anim_ui::key_info(c, *p, self.playhead, fr_d);
                    crate::KeyState { state: k.state, prev: k.prev.is_some(), next: k.next.is_some(), enabled: !d.outside }
                })
                .collect();
            d.keys = ModelRc::new(VecModel::from(keys));
            d.vfade_in = c.video_fade.fade_in.as_secs_f64() as f32;
            d.vfade_out = c.video_fade.fade_out.as_secs_f64() as f32;
            d.fade_max = c.duration().as_secs_f64() as f32;
```

   (these replace the existing `d.x = t.x …` assignments; audio clips keep `fade_max` too.)

4. New `inspector_key`:

```rust
    pub fn inspector_key(&mut self, prop: &str, action: &str) {
        let (Some(id), Some(p)) = (self.inspected(), crate::anim_ui::param_of(prop)) else { return };
        let seq = self.project.sequence();
        let Some(c) = seq.clip(id) else { return };
        let frame = seq.frame_rate.frame_duration();
        let half = crate::anim_ui::half_frame(seq);
        let info = crate::anim_ui::key_info(c, p, self.playhead, frame);
        let edit = crate::anim_ui::edit_time(c, self.playhead, frame);
        let here = edit.and_then(|tt| c.key_near(p, tt, half));
        let value_here = c.anim_value(p, crate::anim_ui::shown_time(c, self.playhead, frame));
        let name = t(crate::anim_ui::label_key(p));
        let cmd = match action {
            "prev" => return info.prev.into_iter().for_each(|tt| self.set_playhead(tt)),
            "next" => return info.next.into_iter().for_each(|tt| self.set_playhead(tt)),
            "toggle" => match (edit, here) {
                (Some(_), Some(at)) => EditCommand::RemoveKeyframe { clip: id, param: p, at, keep: Some(value_here) },
                (Some(tt), None) => EditCommand::SetKeyframe { clip: id, param: p, at: kadr_project::SourceTime(c.source_time_at(tt)), value: value_here, ease: None },
                (None, _) => return,
            },
            "clear" => EditCommand::ClearAnimation { clip: id, statics: vec![(p, value_here)] },
            a if a.starts_with("ease:") => {
                let (Some(at), Some(e)) = (here, kadr_project::Ease::from_key(&a[5..])) else { return };
                EditCommand::SetKeyframeEase { clip: id, param: p, at, ease: e }
            }
            _ => return,
        };
        let label = tf("cmd.keyframe_named", &[("name", &name)]);
        self.execute(EditCommand::Batch { label, commands: vec![cmd] });
    }
```

5. `inspector_reset` for `"transform"` and `"color"` clears animation of that section and sets defaults (spec §5, one undo step):

```rust
        let params: Vec<kadr_project::AnimParam> = match section {
            "transform" => kadr_project::AnimParam::ALL[..9].to_vec(),
            "color" => kadr_project::AnimParam::ALL[9..].to_vec(),
            _ => vec![],
        };
        let prop = match section { /* existing match */ };
        let mut commands = vec![EditCommand::SetClipProperty { clip: id, prop }];
        if !params.is_empty() {
            commands.push(EditCommand::ClearAnimation { clip: id, statics: params.iter().map(|p| (*p, p.default_value())).collect() });
        }
        let label = tf("cmd.reset_section", &[("name", &t(if section == "color" { "inspector.video" } else { "inspector.transform" }))]);
        if self.execute(EditCommand::Batch { label, commands }) {
            self.toast(t("toast.reset_done"));
        }
```

   (`ClearAnimation` with a clip that has no animation still succeeds: `clear` of an absent track is a no-op and statics are written.)

6. Inspector refresh during playback at ~10 Hz (spec §6.1): add `pub inspector_refreshed: Option<std::time::Instant>` to `App` (init `None`), and at the end of `refresh_playhead` in `timeline_ui.rs`:

```rust
        let animated = self.inspected().and_then(|id| self.project.sequence().clip(id)).is_some_and(|c| !c.animation.is_empty());
        if animated {
            let due = !self.playing || self.inspector_refreshed.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(100));
            if due {
                self.inspector_refreshed = Some(std::time::Instant::now());
                self.refresh_inspector();
            }
        }
```

i18n (both catalogs):

| key | en | ru |
|---|---|---|
| `inspector.key.prev` | Previous keyframe | Предыдущий ключевой кадр |
| `inspector.key.toggle` | Add or remove a keyframe at the playhead | Поставить или убрать ключевой кадр в плейхеде |
| `inspector.key.next` | Next keyframe | Следующий ключевой кадр |
| `inspector.key.outside` | Move the playhead onto the clip to edit its animation | Поставьте плейхед на клип, чтобы менять его анимацию |
| `inspector.fades` | Fade in and out | Появление и исчезновение |
| `inspector.temperature` | Temperature | Температура |
| `anim.ease` | Interpolation | Интерполяция |
| `anim.ease.ease_in_out` | Smooth | Плавно |
| `anim.ease.linear` | Linear | Линейно |
| `anim.ease.ease_in` | Ease in | Разгон |
| `anim.ease.ease_out` | Ease out | Торможение |
| `anim.ease.hold` | Hold | Удержание |
| `anim.clear` | Reset property animation | Сбросить анимацию свойства |

(`inspector.fade_in` / `inspector.fade_out` already exist; check their RU text reads «Появление» / «Исчезновение» and fix it if not.)

- [ ] **Step 4: Run to verify**

Run: `cargo test -p kadr-editor && cargo test -p kadr-i18n && cargo build -p kadr-editor`
Expected: all pass, the editor builds.

- [ ] **Step 5: Commit**

```bash
git add apps/editor crates/i18n
git commit -m "editor/inspector: keyframe navigation and toggles, animated sliders, interpolation menu, video fades"
```

---

### Task 9: Timeline fade handles (video and audio)

**Files:**
- Modify: `apps/editor/src/anim_ui.rs`, `apps/editor/src/timeline_ui.rs`, `apps/editor/ui/timeline.slint`, `apps/editor/ui/app.slint`, locales

**Interfaces:**
- Consumes: `ClipProperty::VideoFade`, `ClipProperty::Audio`, `Clip::clamp_fades`.
- Produces:
  - `anim_ui::FADE_R: f64 = 6.0`, `anim_ui::HANDLE_INSET: f64 = 9.0`, `anim_ui::HEADER_H: f64 = 18.0` (the clip's top strip incl. its 2 px margin).
  - `anim_ui::handle_x(x0, x1, fade_px, edge: FadeEdge) -> f64` — centre of a handle, kept `HANDLE_INSET` from the clip edge.
  - `anim_ui::fade_drag_value(edge, x, x0, x1, pps, frame, other: Time, dur: Time) -> Time` — frame-snapped, `[0, dur − other]`.
  - `Hit::Fade { id, edge: FadeEdge }`, `Drag::Fade { clip, edge, is_audio, orig: (Time, Time), value: Time }`.
  - `ClipView.show-fade-handles: bool`; `Timeline.tip-text: string`, `tip-x: length`, `tip-y: length`.

- [ ] **Step 1: Write the failing tests** (in `anim_ui.rs` tests):

```rust
    #[test]
    fn handles_stay_inside_the_clip_and_track_the_fade() {
        use kadr_project::FadeEdge;
        assert_eq!(handle_x(100.0, 300.0, 0.0, FadeEdge::In), 109.0, "zero fade: inset from the edge");
        assert_eq!(handle_x(100.0, 300.0, 50.0, FadeEdge::In), 150.0);
        assert_eq!(handle_x(100.0, 300.0, 0.0, FadeEdge::Out), 291.0);
        assert_eq!(handle_x(100.0, 300.0, 80.0, FadeEdge::Out), 220.0);
    }

    #[test]
    fn fade_drag_snaps_to_frames_and_respects_the_other_fade() {
        use kadr_project::FadeEdge;
        let pps = 100.0; // px per second
        let dur = Time::from_secs(4);
        let v = fade_drag_value(FadeEdge::In, 100.0 + 51.0, 100.0, 500.0, pps, F, Time::ZERO, dur);
        assert_eq!(v, F.mul_ratio(15, 1), "0.51 s → 15 frames at 30 fps");
        let v = fade_drag_value(FadeEdge::Out, 500.0 - 390.0, 100.0, 500.0, pps, F, Time::from_secs(1), dur);
        assert_eq!(v, Time::from_secs(3), "capped at duration − fade_in");
        assert_eq!(fade_drag_value(FadeEdge::In, 50.0, 100.0, 500.0, pps, F, Time::ZERO, dur), Time::ZERO);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-editor anim_ui`
Expected: compile errors.

- [ ] **Step 3: Implement.** `anim_ui.rs`:

```rust
use kadr_project::FadeEdge;

pub const FADE_R: f64 = 6.0;
pub const HANDLE_INSET: f64 = 9.0;
pub const HEADER_H: f64 = 18.0;

pub fn handle_x(x0: f64, x1: f64, fade_px: f64, edge: FadeEdge) -> f64 {
    match edge {
        FadeEdge::In => (x0 + fade_px).max(x0 + HANDLE_INSET).min(x1 - HANDLE_INSET),
        FadeEdge::Out => (x1 - fade_px).min(x1 - HANDLE_INSET).max(x0 + HANDLE_INSET),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn fade_drag_value(edge: FadeEdge, x: f64, x0: f64, x1: f64, pps: f64, frame: Time, other: Time, dur: Time) -> Time {
    let px = match edge {
        FadeEdge::In => x - x0,
        FadeEdge::Out => x1 - x,
    };
    let f = frame.flicks().max(1);
    let raw = Time::from_secs_f64((px / pps).max(0.0));
    let snapped = Time((raw.flicks() + f / 2) / f * f);
    snapped.min((dur - other).max(Time::ZERO))
}
```

`timeline_ui.rs`:

1. `Hit` gains `Fade { id: ClipId, edge: FadeEdge }`; `Drag` gains `Fade { clip: ClipId, edge: FadeEdge, is_audio: bool, orig: (Time, Time), value: Time }`; `TimelineUi` gains `pub hover_clip: Option<ClipId>` (default `None`). Add a helper:

```rust
    /// (fade_in, fade_out) the clip shows on the timeline: picture fades on video tracks, sound fades on audio tracks.
    fn clip_fades(seq: &Sequence, c: &Clip) -> (Time, Time, bool) {
        let audio = seq.locate_clip(c.id).is_some_and(|(ti, _)| seq.tracks[ti].kind == TrackKind::Audio);
        if audio { (c.audio.fade_in, c.audio.fade_out, true) } else { (c.video_fade.fade_in, c.video_fade.fade_out, false) }
    }
```

2. In `hit()`, before the trim-edge loop, test handles of selected or hovered clips (and clips with a fade), only inside the header strip and only for clips at least 40 px wide:

```rust
        let row = rs[ri];
        if y < row.y + crate::anim_ui::HEADER_H {
            for c in &track.clips {
                let (x0, x1) = (self.time_to_x(c.timeline_in), self.time_to_x(c.timeline_out));
                if x1 - x0 < 40.0 {
                    continue;
                }
                let (fi, fo, _) = Self::clip_fades(seq, c);
                let shown = self.tl.selection.contains(&c.id) || self.tl.hover_clip == Some(c.id) || fi > Time::ZERO || fo > Time::ZERO;
                if !shown {
                    continue;
                }
                for (edge, f) in [(FadeEdge::In, fi), (FadeEdge::Out, fo)] {
                    let hx = crate::anim_ui::handle_x(x0, x1, f.as_secs_f64() * self.tl.pps, edge);
                    if (x - hx).abs() <= crate::anim_ui::FADE_R {
                        return Hit::Fade { id: c.id, edge };
                    }
                }
            }
        }
```

3. `pointer_down`: `Hit::Fade { id, edge }` → select the clip if not selected, then

```rust
                let seq = self.project.sequence();
                let c = seq.clip(id).unwrap();
                let (fi, fo, is_audio) = Self::clip_fades(seq, c);
                let value = if edge == FadeEdge::In { fi } else { fo };
                self.tl.drag = Some(Drag::Fade { clip: id, edge, is_audio, orig: (fi, fo), value });
```

4. `pointer_move`: in the `None` branch update `hover_clip` (the clip under the pointer, `None` when empty; `refresh_timeline()` when it changed) and use cursor kind 1 (`ew-resize`) for `Hit::Fade` as for trim edges. New branch:

```rust
            Some(Drag::Fade { clip, edge, is_audio, orig, .. }) => {
                let seq = self.project.sequence();
                let Some(c) = seq.clip(clip) else { return };
                let (x0, x1) = (self.time_to_x(c.timeline_in), self.time_to_x(c.timeline_out));
                let other = if edge == FadeEdge::In { orig.1 } else { orig.0 };
                let value = crate::anim_ui::fade_drag_value(edge, x, x0, x1, self.tl.pps, seq.frame_rate.frame_duration(), other, c.duration());
                let (ti, ci) = seq.locate_clip(clip).unwrap();
                let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
                let slot = match (is_audio, edge) {
                    (false, FadeEdge::In) => &mut c.video_fade.fade_in,
                    (false, FadeEdge::Out) => &mut c.video_fade.fade_out,
                    (true, FadeEdge::In) => &mut c.audio.fade_in,
                    (true, FadeEdge::Out) => &mut c.audio.fade_out,
                };
                *slot = value;
                let key = if edge == FadeEdge::In { "tl.fade.in_tip" } else { "tl.fade.out_tip" };
                let ui = self.ui();
                ui.set_tl_tip_text(tf(key, &[("t", &format!("{:.2} s", value.as_secs_f64()))]).into());
                ui.set_tl_tip_x(x as f32);
                ui.set_tl_tip_y(y as f32);
                self.tl.drag = Some(Drag::Fade { clip, edge, is_audio, orig, value });
                if !is_audio {
                    self.preview.invalidate();
                    self.request_frame();
                }
                self.refresh_inspector();
                self.refresh_timeline();
            }
```

5. `pointer_up`: restore the original fades and execute one command:

```rust
            Some(Drag::Fade { clip, edge, is_audio, orig, value }) => {
                self.ui().set_tl_tip_text("".into());
                if let Some((ti, ci)) = self.project.sequence().locate_clip(clip) {
                    let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
                    if is_audio { (c.audio.fade_in, c.audio.fade_out) = orig } else { (c.video_fade.fade_in, c.video_fade.fade_out) = orig }
                    let new = if edge == FadeEdge::In { (value, orig.1) } else { (orig.0, value) };
                    if new != orig {
                        let prop = if is_audio {
                            let mut a = c.audio.clone();
                            (a.fade_in, a.fade_out) = new;
                            ClipProperty::Audio(a)
                        } else {
                            ClipProperty::VideoFade(kadr_project::EdgeFades { fade_in: new.0, fade_out: new.1 })
                        };
                        let name = t(if edge == FadeEdge::In { "inspector.fade_in" } else { "inspector.fade_out" });
                        let old = if edge == FadeEdge::In { orig.0 } else { orig.1 };
                        let label = tf("cmd.fade_named", &[("name", &name), ("from", &format!("{:.2} s", old.as_secs_f64())), ("to", &format!("{:.2} s", value.as_secs_f64()))]);
                        self.execute(EditCommand::Batch { label, commands: vec![EditCommand::SetClipProperty { clip, prop }] });
                    }
                }
                self.preview.invalidate();
                self.request_frame();
            }
```

6. Clip views: `fade_in`/`fade_out` come from `Self::clip_fades(&seq, c)`; add `show_fade_handles: sel.contains(&c.id) || self.tl.hover_clip == Some(c.id) || fi > ZERO || fo > ZERO` (only when `width >= 40`).

`timeline.slint`: `ClipView` gains `show-fade-handles: bool`; in `ClipRect` after the two fade `Path`s:

```slint
    if root.cv.show-fade-handles: Rectangle {
        x: clamp(root.cv.fade-in, 9px, parent.width - 9px) - 5px;
        y: 3px;
        width: 10px; height: 10px; border-radius: 5px;
        background: #ffffff; border-width: 1px; border-color: #00000080;
    }
    if root.cv.show-fade-handles: Rectangle {
        x: clamp(parent.width - root.cv.fade-out, 9px, parent.width - 9px) - 5px;
        y: 3px;
        width: 10px; height: 10px; border-radius: 5px;
        background: #ffffff; border-width: 1px; border-color: #00000080;
    }
```

(Handle centre `y` = 2 px clip margin + 8 px = inside the 16 px header strip, matching `HEADER_H`.) `Timeline` gains `in property <string> tip-text; in property <length> tip-x; in property <length> tip-y;` and inside the lanes area, above the playhead:

```slint
                    if root.tip-text != "": Rectangle {
                        x: min(root.tip-x + 12px, parent.width - tip.preferred-width - 16px);
                        y: max(root.tip-y - 26px, 0px);
                        width: tip.preferred-width + 12px; height: 20px; border-radius: 4px;
                        background: Theme.bg3;
                        tip := Text { text: root.tip-text; font-size: Theme.font-xs; color: Theme.text; horizontal-alignment: center; vertical-alignment: center; }
                    }
```

`app.slint`: declare `in property <string> tl-tip-text; in property <length> tl-tip-x; in property <length> tl-tip-y;` on the window and pass them into `tl := Timeline { … tip-text: root.tl-tip-text; tip-x: root.tl-tip-x; tip-y: root.tl-tip-y; }`.

i18n: `tl.fade.in_tip` — "Fade in {t}" / «Появление {t}»; `tl.fade.out_tip` — "Fade out {t}" / «Исчезновение {t}».

- [ ] **Step 4: Run to verify**

Run: `cargo test -p kadr-editor && cargo test -p kadr-i18n && cargo build -p kadr-editor`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add apps/editor crates/i18n
git commit -m "editor/timeline: fade handles for video and audio clips with live preview and one undo step"
```

---

### Task 10: Timeline keyframe diamonds, keyframe menu, Alt+←/→

**Files:**
- Modify: `apps/editor/src/anim_ui.rs`, `apps/editor/src/timeline_ui.rs`, `apps/editor/src/keys.rs`, `apps/editor/ui/timeline.slint`, `apps/editor/ui/app.slint`, locales

**Interfaces:**
- Consumes: `Animation::{times, params_at}`, `MoveKeyframes`, `SetKeyframeEase`, `RemoveKeyframe`.
- Produces:
  - `anim_ui::CLUSTER_PX: f64 = 6.0`, `anim_ui::KEY_STRIP_H: f64 = 14.0`.
  - `anim_ui::key_marks(xs: &[f64]) -> Vec<(f64, bool)>` — render-only clusters (x of the first member, `true` when > 1 member).
  - `anim_ui::nearest_key(xs: &[(SourceTime, f64)], x: f64, r: f64) -> Option<SourceTime>` — the nearest **real** time within `r` px.
  - `anim_ui::visible_neighbour(times: &[Time], playhead: Time, half: Time, forward: bool) -> Option<Time>`.
  - `Hit::Key { id, at: SourceTime }`; `Drag::Keys { clip, at: SourceTime, orig: Animation, to: Time, started: bool, start_x: f64 }`.
  - `TimelineUi.menu_key: Option<(ClipId, SourceTime)>`; `ClipView.keys: [KeyMark]` (`KeyMark { x: length, cluster: bool }`), `hidden-left: bool`, `hidden-right: bool`; `Timeline.menu-kind: int` (0 clip menu, 1 keyframe menu).

- [ ] **Step 1: Write the failing tests** (in `anim_ui.rs` tests):

```rust
    #[test]
    fn marks_cluster_only_for_drawing() {
        assert_eq!(key_marks(&[10.0, 13.0, 30.0, 35.9, 50.0]), vec![(10.0, true), (30.0, true), (50.0, false)]);
        assert_eq!(key_marks(&[10.0, 17.0]), vec![(10.0, false), (17.0, false)], "6 px apart or more: separate");
    }

    /// A click on a cluster picks the nearest *real* moment, never the group.
    #[test]
    fn nearest_key_returns_a_real_time() {
        let s = |ms| SourceTime(Time::from_millis(ms));
        let xs = [(s(1_000), 10.0), (s(1_040), 13.0), (s(9_000), 90.0)];
        assert_eq!(nearest_key(&xs, 12.0, 6.0), Some(s(1_040)));
        assert_eq!(nearest_key(&xs, 10.4, 6.0), Some(s(1_000)));
        assert_eq!(nearest_key(&xs, 50.0, 6.0), None);
    }

    #[test]
    fn alt_arrows_step_between_visible_keyframes() {
        let ts = [Time::from_secs(1), Time::from_secs(3), Time::from_secs(5)];
        let half = Time::from_millis(16);
        assert_eq!(visible_neighbour(&ts, Time::from_secs(3), half, true), Some(Time::from_secs(5)));
        assert_eq!(visible_neighbour(&ts, Time::from_secs(3), half, false), Some(Time::from_secs(1)));
        assert_eq!(visible_neighbour(&ts, Time::from_secs(5), half, true), None);
    }
```

Add to `crates/timeline/tests/animation_edits.rs` (review focus 4, the drag's command):

```rust
#[test]
fn a_diamond_drag_moves_every_property_of_that_moment() {
    let (mut p, mut e, id) = setup();
    e.execute(&mut p, EditCommand::SetKeyframe { clip: id, param: AnimParam::X, at: SourceTime(s(105)), value: 9.0, ease: None }).unwrap();
    let c = p.sequence().clip(id).unwrap();
    let at = SourceTime(s(105));
    let moves = c.animation.params_at(at).into_iter().map(|q| (q, at, SourceTime(s(107)))).collect();
    e.execute(&mut p, EditCommand::MoveKeyframes { clip: id, moves }).unwrap();
    assert_eq!(p.sequence().clip(id).unwrap().animation.times(), vec![SourceTime(s(107))]);
    e.undo(&mut p);
    assert_eq!(p.sequence().clip(id).unwrap().animation.times(), vec![SourceTime(s(105))], "one undo step");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-editor anim_ui && cargo test -p kadr-timeline --test animation_edits`
Expected: editor tests fail to compile; the timeline test passes already (it pins Task 7 behaviour the drag relies on).

- [ ] **Step 3: Implement.** `anim_ui.rs`:

```rust
use kadr_project::SourceTime;

pub const CLUSTER_PX: f64 = 6.0;
pub const KEY_STRIP_H: f64 = 14.0;

pub fn key_marks(xs: &[f64]) -> Vec<(f64, bool)> {
    let mut out: Vec<(f64, bool)> = vec![];
    let mut last = f64::NEG_INFINITY;
    for &x in xs {
        match out.last_mut() {
            Some(m) if x - last < CLUSTER_PX => m.1 = true,
            _ => out.push((x, false)),
        }
        last = x;
    }
    out
}

pub fn nearest_key(xs: &[(SourceTime, f64)], x: f64, r: f64) -> Option<SourceTime> {
    xs.iter().map(|(at, kx)| (*at, (kx - x).abs())).filter(|(_, d)| *d <= r).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(at, _)| at)
}

pub fn visible_neighbour(times: &[Time], playhead: Time, half: Time, forward: bool) -> Option<Time> {
    if forward {
        times.iter().copied().find(|t| *t >= playhead + half)
    } else {
        times.iter().rev().copied().find(|t| *t <= playhead - half)
    }
}
```

Note `key_marks` chains: each mark's distance is measured from the previous *key*, so a dense run of keys collapses into one drawn diamond; the data is never merged.

`timeline_ui.rs`:

1. A helper returning a clip's visible keyframe positions:

```rust
    /// (source time, x in lane pixels) of the clip's keyframe moments that are on screen inside the clip.
    fn key_positions(&self, c: &Clip) -> Vec<(kadr_project::SourceTime, f64)> {
        c.animation
            .times()
            .into_iter()
            .map(|at| (at, c.timeline_time_of(at.0)))
            .filter(|(_, t)| *t >= c.timeline_in && *t < c.timeline_out)
            .map(|(at, t)| (at, self.time_to_x(t)))
            .collect()
    }
```

2. `hit()`: after the fade-handle check and before the trim loop, for video rows only:

```rust
        if row.kind == TrackKind::Video && y >= row.y + row.h - 2.0 - crate::anim_ui::KEY_STRIP_H {
            for c in track.clips.iter().filter(|c| !c.animation.is_empty()) {
                if let Some(at) = crate::anim_ui::nearest_key(&self.key_positions(c), x, crate::anim_ui::CLUSTER_PX) {
                    return Hit::Key { id: c.id, at };
                }
            }
        }
```

3. `pointer_down`: `Hit::Key { id, at }` → `self.tl.selection = vec![id]`; `self.tl.drag = Some(Drag::Keys { clip: id, at, orig: c.animation.clone(), to: c.timeline_time_of(at.0), started: false, start_x: x })`. Right press (`kind 0, button 1`): set `self.tl.menu_key` and the menu kind **before** Slint shows a menu:

```rust
            0 if button == 1 => {
                let hit = self.hit(x, y);
                let (kind, key) = match hit {
                    Hit::Key { id, at } => (1, Some((id, at))),
                    _ => (0, None),
                };
                self.tl.menu_key = key;
                self.ui().set_tl_menu_kind(kind);
                if let Hit::Clip { id, .. } | Hit::Key { id, .. } | Hit::Fade { id, .. } = hit {
                    if !self.tl.selection.contains(&id) {
                        self.tl.selection = vec![id];
                        self.refresh_timeline();
                        self.refresh_inspector();
                    }
                }
            }
```

4. `pointer_move` branch for `Drag::Keys`: once `|x − start_x| > DRAG_THRESHOLD`, compute `to = frame_rate.snap(x_to_time(x))` clamped into `[timeline_in, timeline_out − frame]`; apply live on a copy of `orig` — for each `p in orig.params_at(at)`: `anim.move_key(p, at, SourceTime(c.source_time_at(to)))`; when any move fails (collision) keep the previous preview and set the tip to `t("err.edit.keyframe_collision")`; otherwise write the moved animation into the clip, set the tip to the timecode of `to`, `preview.invalidate()`, `request_frame()`, `refresh_timeline()`, `refresh_inspector()`.

5. `pointer_up` for `Drag::Keys`: clear the tip; restore `orig` into the clip; if not `started` → `self.set_playhead(to)` (the click case); else if `to` differs from the original position → execute

```rust
                        let new_at = kadr_project::SourceTime(c.source_time_at(to));
                        let moves = orig.params_at(at).into_iter().map(|p| (p, at, new_at)).collect();
                        self.execute(EditCommand::MoveKeyframes { clip, moves });
```

   (a collision surfaces through `execute`'s existing error toast with `err.edit.keyframe_collision`).

6. `tl_context` gains `"key-delete"` and `"key-ease:<key>"` using `self.tl.menu_key`:

```rust
            a if a == "key-delete" || a.starts_with("key-ease:") => {
                let Some((clip, at)) = self.tl.menu_key else { return };
                let Some(c) = self.project.sequence().clip(clip) else { return };
                let params = c.animation.params_at(at);
                let commands: Vec<EditCommand> = match a.strip_prefix("key-ease:").map(kadr_project::Ease::from_key) {
                    Some(Some(ease)) => params.into_iter().map(|param| EditCommand::SetKeyframeEase { clip, param, at, ease: ease.clone() }).collect(),
                    Some(None) => return,
                    None => params
                        .into_iter()
                        .map(|param| EditCommand::RemoveKeyframe { clip, param, at, keep: Some(c.anim_value(param, self.playhead.max(c.timeline_in).min(c.timeline_out))) })
                        .collect(),
                };
                if !commands.is_empty() {
                    let label = t(if a == "key-delete" { "cmd.remove_keyframe" } else { "cmd.keyframe_ease" });
                    self.execute(EditCommand::Batch { label, commands });
                }
            }
```

7. Clip views (video clips with animation): `keys = key_marks(&xs_relative_to_clip)` → `KeyMark { x, cluster }`; `hidden_left = c.animation.times().iter().any(|a| c.timeline_time_of(a.0) < c.timeline_in)`; `hidden_right` likewise with `>= timeline_out`.

8. `keys.rs`: replace `if alt { return false; }` with

```rust
        if alt {
            if is(text, Key::LeftArrow) || is(text, Key::RightArrow) {
                self.jump_keyframe(is(text, Key::RightArrow));
                return true;
            }
            return false;
        }
```

   and add `r("Alt + ← / →", "keys.keyframe"),` to the shortcut list after `"keys.edit_points"`. In `timeline_ui.rs`:

```rust
    /// Alt+←/→: previous / next visible keyframe of the inspected clip.
    pub fn jump_keyframe(&mut self, forward: bool) {
        let seq = self.project.sequence();
        let Some(c) = self.inspected().and_then(|id| seq.clip(id)) else { return };
        let times: Vec<Time> = c.animation.times().into_iter().map(|a| c.timeline_time_of(a.0)).filter(|t| *t >= c.timeline_in && *t < c.timeline_out).collect();
        if let Some(t) = crate::anim_ui::visible_neighbour(&times, self.playhead, crate::anim_ui::half_frame(seq), forward) {
            self.stop_playback();
            self.set_playhead(t);
        }
    }
```

`timeline.slint`: `export struct KeyMark { x: length, cluster: bool }`; `ClipView` gains `keys: [KeyMark], hidden-left: bool, hidden-right: bool`. In `ClipRect` (video only):

```slint
    for k in root.cv.keys: Rectangle {
        x: k.x - 5px;
        y: parent.height - 12px;
        width: 10px; height: 10px;
        rotation-angle: 45deg;
        background: k.cluster ? Theme.accent : #ffe08a;
        border-width: 1px; border-color: #00000099;
    }
    if root.cv.hidden-left: Text { x: 2px; y: parent.height - 14px; text: "‹"; font-size: 12px; color: #ffe08a; }
    if root.cv.hidden-right: Text { x: parent.width - 8px; y: parent.height - 14px; text: "›"; font-size: 12px; color: #ffe08a; }
```

`Timeline`: `in property <int> menu-kind;`, a second `ContextMenuArea` for keyframes next to `lane-menu`:

```slint
                    key-menu := ContextMenuArea {
                        Menu {
                            Menu {
                                title: Tr.t("anim.ease");
                                MenuItem { title: Tr.t("anim.ease.ease_in_out"); activated => { root.context-action("key-ease:ease_in_out"); } }
                                MenuItem { title: Tr.t("anim.ease.linear"); activated => { root.context-action("key-ease:linear"); } }
                                MenuItem { title: Tr.t("anim.ease.ease_in"); activated => { root.context-action("key-ease:ease_in"); } }
                                MenuItem { title: Tr.t("anim.ease.ease_out"); activated => { root.context-action("key-ease:ease_out"); } }
                                MenuItem { title: Tr.t("anim.ease.hold"); activated => { root.context-action("key-ease:hold"); } }
                            }
                            MenuItem { title: Tr.t("tl.key.delete"); activated => { root.context-action("key-delete"); } }
                        }
                    }
```

and in the lane `TouchArea`'s right-press line show `key-menu` when `root.menu-kind == 1`, else `lane-menu` (the `lane-pointer` call above it runs synchronously and sets `menu-kind` first):

```slint
                                if (e.kind == PointerEventKind.down && e.button == PointerEventButton.right) {
                                    if (root.menu-kind == 1) { key-menu.show({ x: self.absolute-position.x - key-menu.absolute-position.x + self.mouse-x, y: self.absolute-position.y - key-menu.absolute-position.y + self.mouse-y }); }
                                    else { lane-menu.show({ x: self.absolute-position.x - lane-menu.absolute-position.x + self.mouse-x, y: self.absolute-position.y - lane-menu.absolute-position.y + self.mouse-y }); }
                                }
```

`app.slint`: `in property <int> tl-menu-kind;` passed as `menu-kind: root.tl-menu-kind;`.

i18n: `tl.key.delete` — "Delete keyframes" / «Удалить кадры»; `keys.keyframe` — "Previous / next keyframe" / «Предыдущий / следующий ключевой кадр».

- [ ] **Step 4: Run to verify**

Run: `cargo test -p kadr-editor && cargo test -p kadr-timeline && cargo test -p kadr-i18n && cargo build -p kadr-editor`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add apps/editor crates/i18n crates/timeline/tests
git commit -m "editor/timeline: keyframe diamonds with render-only clusters, drag, keyframe menu, Alt+arrows"
```

---

### Task 11: MCP ops and state

**Files:**
- Create: `apps/editor/src/mcp_anim.rs` (+ `mod mcp_anim;` in `main.rs`)
- Modify: `apps/editor/src/mcp_api.rs` (`edit_op`), `apps/editor/src/mcp_state.rs` (clip JSON), `apps/kadr-mcp/src/tools.rs` (`EDIT_DESCRIPTION`), `apps/kadr-mcp/tests/tools.rs` (op list), `docs/mcp.md`

**Interfaces:**
- Consumes: Task 7 commands, `Clip::{key_near, anim_value, source_time_at, timeline_time_of}`.
- Produces: `pub fn anim_op(project: &Project, seq: &Sequence, op: &Value) -> Option<Result<Vec<EditCommand>, BridgeError>>` — `None` for types it does not own. Ops:
  - `set_keyframe`: `clip_id`, `param`, `value`, `at_ms` **or** `source_ms`, `ease?`
  - `remove_keyframe`: `clip_id`, `param`, `at_ms` or `source_ms`
  - `move_keyframes`: `clip_id`, `from_ms` or `from_source_ms`, `to_ms`, `param?` (all params of that moment when absent)
  - `set_keyframe_ease`: `clip_id`, `param`, `at_ms` or `source_ms`, `ease`
  - `clear_animation`: `clip_id`, `params?` (default all known), `keep_ms?` (statics = value at that time; default = defaults)
  - `set_video_fade`: `clip_id`, `fade_in_ms?`, `fade_out_ms?`
- Time rules: `at_ms` → `frame_rate.snap(Time::from_millis(at_ms))`, must be inside `[timeline_in, timeline_out)`, then `source_time_at`; existing keyframes are found with `key_near(p, t, half_frame)`; `source_ms` → exact `SourceTime(Time::from_millis(source_ms))`, existing keyframes within half a frame of source time.
- `get_state` clip JSON gains `"video_fade": {"fade_in_ms", "fade_out_ms"}` and `"animation": {param: [{"at_ms", "source_ms", "value", "ease", "visible"}]}` (`ease` = key or `"unknown"`).

- [ ] **Step 1: Write the failing tests** (in `mcp_anim.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, FrameRate, TimeRange};
    use kadr_project::{AnimParam, Clip, Project};
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

    #[test]
    fn at_ms_is_converted_to_exact_source_time_and_found_again_after_ms_rounding() {
        let (mut p, id) = project();
        run(&mut p, json!({"type": "set_keyframe", "clip_id": id, "param": "scale", "at_ms": 6_000, "value": 2.0})).unwrap();
        let c = &p.sequence().tracks[0].clips[0];
        let k = &c.animation.track(AnimParam::Scale)[0];
        assert_eq!(k.at.0, c.source_time_at(FrameRate::FPS_29_97.snap(Time::from_millis(6_000))), "frame-snapped, exact");
        // 29.97 fps frames are not whole milliseconds; a client echoing at_ms still hits the keyframe.
        let echoed = c.timeline_time_of(k.at.0).as_millis();
        run(&mut p, json!({"type": "set_keyframe_ease", "clip_id": id, "param": "scale", "at_ms": echoed, "ease": "hold"})).unwrap();
        assert_eq!(p.sequence().tracks[0].clips[0].animation.track(AnimParam::Scale)[0].ease, kadr_project::Ease::Hold);
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
}
```

Add to `mcp_state.rs` tests:

```rust
    #[test]
    fn clips_report_fades_and_animation_in_both_time_domains() {
        let mut p = Project::new("t");
        let mut c = Clip::new(kadr_core::AssetId::new(), "c", TimeRange::new(Time::from_secs(10), Time::from_secs(20)), Time::ZERO);
        c.video_fade.fade_in = Time::from_millis(500);
        c.animation.set_key(kadr_project::AnimParam::Opacity, kadr_project::SourceTime(Time::from_secs(12)), 0.5, None);
        c.animation.set_key(kadr_project::AnimParam::Opacity, kadr_project::SourceTime(Time::from_secs(25)), 1.0, None);
        p.sequence_mut().tracks[0].clips.push(c);
        let v = timeline_json(p.sequence(), &[], Time::ZERO);
        let cj = &v["tracks"][0]["clips"][0];
        assert_eq!(cj["video_fade"], serde_json::json!({"fade_in_ms": 500, "fade_out_ms": 0}));
        let k = &cj["animation"]["opacity"];
        assert_eq!((k[0]["at_ms"].as_i64(), k[0]["source_ms"].as_i64(), k[0]["visible"].as_bool()), (Some(2_000), Some(12_000), Some(true)));
        assert_eq!(k[0]["ease"], "ease_in_out");
        assert_eq!(k[1]["visible"], false, "source 25 s is past the clip's out point");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-editor mcp_anim mcp_state`
Expected: compile errors.

- [ ] **Step 3: Implement** `mcp_anim.rs`:

```rust
//! MCP `edit` ops for keyframes and clip fades (keyframes spec §7). Times
//! arrive as milliseconds and are turned into exact `Time`/`SourceTime`
//! at once; nothing below this module sees milliseconds.

use kadr_core::{ClipId, Time};
use kadr_mcp_bridge::BridgeError;
use kadr_project::{AnimParam, Clip, EdgeFades, Ease, Project, Sequence, SourceTime};
use kadr_timeline::{ClipProperty, EditCommand};
use serde_json::Value;

fn bad(msg: impl Into<String>) -> BridgeError {
    BridgeError::new("edit_rejected", msg.into())
}

fn clip<'a>(seq: &'a Sequence, op: &Value) -> Result<(ClipId, &'a Clip), BridgeError> {
    let id = op.get("clip_id").and_then(Value::as_str).and_then(ClipId::parse).ok_or_else(|| bad("clip_id missing or malformed"))?;
    Ok((id, seq.clip(id).ok_or_else(|| bad(format!("unknown clip {id}")))?))
}

fn param(op: &Value) -> Result<AnimParam, BridgeError> {
    let k = op.get("param").and_then(Value::as_str).unwrap_or_default();
    AnimParam::from_key(k).ok_or_else(|| bad(format!("unknown param {k:?} (one of {})", AnimParam::ALL.map(|p| p.key()).join(", "))))
}

fn half(seq: &Sequence) -> Time {
    Time(seq.frame_rate.frame_duration().flicks() / 2)
}

/// A timeline position inside the clip, frame-snapped.
fn at_timeline(seq: &Sequence, c: &Clip, ms: i64) -> Result<Time, BridgeError> {
    let t = seq.frame_rate.snap(Time::from_millis(ms));
    if t < c.timeline_in || t >= c.timeline_out {
        return Err(bad(format!("{ms} ms is outside the clip ({}..{} ms); use source_ms for hidden keyframes", c.timeline_in.as_millis(), c.timeline_out.as_millis())));
    }
    Ok(t)
}

/// Where a new keyframe goes: `at_ms` (timeline) or `source_ms` (source).
fn new_time(seq: &Sequence, c: &Clip, op: &Value, at: &str, src: &str) -> Result<SourceTime, BridgeError> {
    match (op.get(at).and_then(Value::as_i64), op.get(src).and_then(Value::as_i64)) {
        (Some(ms), None) => Ok(SourceTime(c.source_time_at(at_timeline(seq, c, ms)?))),
        (None, Some(ms)) => Ok(SourceTime(Time::from_millis(ms))),
        _ => Err(bad(format!("give exactly one of {at} or {src}"))),
    }
}

/// An existing keyframe moment of `p` (or of any param when `p` is None).
fn existing(seq: &Sequence, c: &Clip, p: Option<AnimParam>, op: &Value, at: &str, src: &str) -> Result<SourceTime, BridgeError> {
    let h = half(seq);
    let found = match (op.get(at).and_then(Value::as_i64), op.get(src).and_then(Value::as_i64)) {
        (Some(ms), None) => {
            let t = at_timeline(seq, c, ms)?;
            let params = p.map_or(c.animation.params(), |p| vec![p]);
            params.into_iter().filter_map(|q| c.key_near(q, t, h)).min_by_key(|a| (c.timeline_time_of(a.0) - t).abs())
        }
        (None, Some(ms)) => {
            let s = Time::from_millis(ms);
            c.animation.times().into_iter().filter(|a| p.is_none_or(|q| c.animation.params_at(*a).contains(&q))).filter(|a| (a.0 - s).abs() < h).min_by_key(|a| (a.0 - s).abs())
        }
        _ => return Err(bad(format!("give exactly one of {at} or {src}"))),
    };
    found.ok_or_else(|| bad("no keyframe there"))
}

pub fn anim_op(project: &Project, seq: &Sequence, op: &Value) -> Option<Result<Vec<EditCommand>, BridgeError>> {
    let _ = project;
    let ty = op.get("type").and_then(Value::as_str)?;
    let r = (|| -> Result<Vec<EditCommand>, BridgeError> {
        let (id, c) = clip(seq, op)?;
        Ok(vec![match ty {
            "set_keyframe" => {
                let p = param(op)?;
                let value = op.get("value").and_then(Value::as_f64).ok_or_else(|| bad("value missing"))?;
                let ease = match op.get("ease").and_then(Value::as_str) {
                    Some(k) => Some(Ease::from_key(k).ok_or_else(|| bad(format!("unknown ease {k:?}")))?),
                    None => None,
                };
                EditCommand::SetKeyframe { clip: id, param: p, at: new_time(seq, c, op, "at_ms", "source_ms")?, value, ease }
            }
            "remove_keyframe" => {
                let p = param(op)?;
                let at = existing(seq, c, Some(p), op, "at_ms", "source_ms")?;
                EditCommand::RemoveKeyframe { clip: id, param: p, at, keep: None }
            }
            "set_keyframe_ease" => {
                let p = param(op)?;
                let k = op.get("ease").and_then(Value::as_str).unwrap_or_default();
                let ease = Ease::from_key(k).ok_or_else(|| bad(format!("unknown ease {k:?}")))?;
                EditCommand::SetKeyframeEase { clip: id, param: p, at: existing(seq, c, Some(p), op, "at_ms", "source_ms")?, ease }
            }
            "move_keyframes" => {
                let p = op.get("param").map(|_| param(op)).transpose()?;
                let from = existing(seq, c, p, op, "from_ms", "from_source_ms")?;
                let to_ms = op.get("to_ms").and_then(Value::as_i64).ok_or_else(|| bad("to_ms missing"))?;
                let to = SourceTime(c.source_time_at(at_timeline(seq, c, to_ms)?));
                let params = p.map_or_else(|| c.animation.params_at(from), |p| vec![p]);
                EditCommand::MoveKeyframes { clip: id, moves: params.into_iter().map(|q| (q, from, to)).collect() }
            }
            "clear_animation" => {
                let params: Vec<AnimParam> = match op.get("params").and_then(Value::as_array) {
                    Some(a) => a.iter().map(|v| v.as_str().and_then(AnimParam::from_key).ok_or_else(|| bad(format!("unknown param {v}")))).collect::<Result<_, _>>()?,
                    None => AnimParam::ALL.to_vec(),
                };
                let keep = op.get("keep_ms").and_then(Value::as_i64).map(|ms| seq.frame_rate.snap(Time::from_millis(ms)));
                let statics = params.into_iter().map(|p| (p, keep.map_or(p.default_value(), |t| c.anim_value(p, t)))).collect();
                EditCommand::ClearAnimation { clip: id, statics }
            }
            "set_video_fade" => {
                let ms = |k: &str, cur: Time| op.get(k).and_then(Value::as_i64).map_or(cur, |v| seq.frame_rate.snap(Time::from_millis(v.max(0))));
                let f = EdgeFades { fade_in: ms("fade_in_ms", c.video_fade.fade_in), fade_out: ms("fade_out_ms", c.video_fade.fade_out) };
                EditCommand::SetClipProperty { clip: id, prop: ClipProperty::VideoFade(f) }
            }
            _ => return Err(bad("not an animation op")),
        }])
    })();
    match ty {
        "set_keyframe" | "remove_keyframe" | "set_keyframe_ease" | "move_keyframes" | "clear_animation" | "set_video_fade" => Some(r),
        _ => None,
    }
}
```

(Use the crate's actual `BridgeError` import path from `mcp_api.rs`; if `BridgeError::new` takes `&str` codes, keep `"edit_rejected"`. `bool::is_none_or` needs Rust ≥ 1.82 — the workspace already uses it in `inspector.rs`.)

`mcp_api.rs` `edit_op`: first line of the function:

```rust
    if let Some(r) = crate::mcp_anim::anim_op(project, seq, op) {
        return r;
    }
```

`mcp_state.rs` clip JSON (next to `"source_in_ms"`):

```rust
                "video_fade": {"fade_in_ms": ms(c.video_fade.fade_in), "fade_out_ms": ms(c.video_fade.fade_out)},
                "animation": c.animation.params().into_iter().map(|p| (p.key().to_string(), json!(c.animation.track(p).iter().map(|k| {
                    let tl = c.timeline_time_of(k.at.0);
                    json!({"at_ms": ms(tl), "source_ms": ms(k.at.0), "value": k.value, "ease": k.ease.key().unwrap_or("unknown"), "visible": tl >= c.timeline_in && tl < c.timeline_out})
                }).collect::<Vec<_>>()))).collect::<serde_json::Map<_, _>>(),
```

`apps/kadr-mcp/src/tools.rs` — append to `EDIT_DESCRIPTION` before "Unknown fields are rejected.":

```
- \"set_keyframe\": clip_id, param, value, at_ms | source_ms, ease? (params: x y scale rotation opacity crop_left crop_right crop_top crop_bottom exposure contrast saturation temperature; ease: ease_in_out (default for new animation) linear ease_in ease_out hold; at_ms is timeline time inside the clip, snapped to a frame; source_ms addresses source time, e.g. hidden keyframes)
- \"remove_keyframe\": clip_id, param, at_ms | source_ms (finds the keyframe within half a frame)
- \"set_keyframe_ease\": clip_id, param, at_ms | source_ms, ease
- \"move_keyframes\": clip_id, from_ms | from_source_ms, to_ms, param? (all properties at that moment when omitted)
- \"clear_animation\": clip_id, params? (default all), keep_ms? (static values = the picture at that time; default: property defaults)
- \"set_video_fade\": clip_id, fade_in_ms?, fade_out_ms? (picture fades from the clip's edges; fade_in + fade_out ≤ clip length)
```

Add the six names to the op list in `apps/kadr-mcp/tests/tools.rs`. In `docs/mcp.md`, under «Семантические команды», add a paragraph (RU) describing the six ops, the two time forms and the `get_state` fields `video_fade` and `animation`.

- [ ] **Step 4: Run to verify**

Run: `cargo test -p kadr-editor && cargo test -p kadr-mcp`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add apps/editor apps/kadr-mcp docs/mcp.md
git commit -m "mcp: keyframe and fade edit ops (timeline or source ms → exact time); animation and fades in get_state"
```

---

### Task 12: End-to-end pixels, evaluator bench, acceptance, review

**Files:**
- Create: `crates/project-scenes/tests/animation_pixels.rs`
- Modify: `crates/project-scenes/Cargo.toml` (dev-dependency `kadr-render = { path = "../render" }`), `apps/kadr-bench/src/scene_bench.rs`, `ARCHITECTURE.md` (one paragraph on keyframes/fades in the evaluator)

**Interfaces:**
- Consumes: everything above.
- Produces: a pixel-level guarantee (evaluate → `CpuRenderer`), a bench row `scene / 10 clips × 13 animated params`, the live acceptance record.

- [ ] **Step 1: Write the pixel test** — `crates/project-scenes/tests/animation_pixels.rs`:

```rust
//! Evaluator → renderer: animated opacity and a clip fade produce the
//! expected pixels (keyframes spec §8 "пиксели"). No decoder: the media
//! layer gets a solid white frame.

use kadr_core::color::AlphaMode;
use kadr_core::{ColorInfo, CpuFrame, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::{AnimParam, Clip, EdgeFades, Ease, MediaAsset, Project, SourceTime};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{LayerContent, OutputSpec, RenderQuality, SizeU};
use kadr_timeline::scene::evaluate;
use std::sync::Arc;

fn project() -> Project {
    let mut p = Project::new("px");
    p.sequence_mut().frame_rate = FrameRate::FPS_30;
    p.sequence_mut().width = 64;
    p.sequence_mut().height = 36;
    let video = VideoInfo { width: 64, height: 36, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("w.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None });
    let mut c = Clip::new(a.id, "w", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
    // Opacity 0 → 1 over source 1..2 s; the fade-out spans the last second
    // (2.967 .. 3.967 s, the last frame starting at 4 s − 1/30 s).
    c.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_secs(1)), 0.0, Some(Ease::Linear));
    c.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_secs(2)), 1.0, None);
    c.video_fade = EdgeFades { fade_in: Time::ZERO, fade_out: Time::from_secs(1) };
    p.assets.push(a);
    p.sequence_mut().tracks[0].clips.push(c);
    p
}

/// Centre pixel's red channel at timeline time `t`.
fn red_at(p: &Project, t: Time) -> u8 {
    let out = OutputSpec::new(SizeU::new(64, 36), RenderQuality::Export);
    let scene = evaluate(p, p.sequence(), t, &out);
    let white = Arc::new(CpuFrame::from_rgba8(64, 36, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, vec![255; 64 * 36 * 4]));
    let inputs = RenderInputs { layers: scene.layers.iter().map(|l| match l.content { LayerContent::Media { .. } => LayerInput::Cpu(white.clone()), _ => LayerInput::None }).collect() };
    let mut buf = vec![0u8; 64 * 36 * 4];
    CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(64, 36, &mut buf))).unwrap();
    buf[(18 * 64 + 32) * 4]
}

#[test]
fn animated_opacity_and_fade_reach_the_pixels() {
    let p = project();
    let ms = Time::from_millis;
    assert_eq!(red_at(&p, ms(500)), 0, "before the first keyframe: opacity 0 (culled, black)");
    let half = red_at(&p, ms(1_500));
    assert!((half as i32 - 128).abs() <= 1, "halfway: {half}");
    assert_eq!(red_at(&p, ms(2_500)), 255, "opaque, before the fade-out");
    let mid_fade = red_at(&p, ms(3_500));
    assert!((110..=130).contains(&mid_fade), "opacity 1 × fade (3.967 − 3.5) / 1 ≈ 0.47: {mid_fade}");
    let last = Time::from_secs(4) - FrameRate::FPS_30.frame_duration();
    assert_eq!(red_at(&p, last), 0, "last frame of the fade-out is transparent");
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p kadr-project-scenes --test animation_pixels`
Expected: PASS (if the halfway value is off by more than 1 LSB, check `clip_layer`'s opacity path before touching the test).

- [ ] **Step 3: Bench row** — in `scene_bench.rs` add a second project and row (not part of `cargo test`; numbers go to `docs/perf/`):

```rust
/// 10 full-length clips on 10 video tracks, each with all 13 parameters animated (3 keyframes each).
fn animated_project() -> Project {
    let mut p = Project::new("bench-anim");
    let a = asset(MediaKind::Video, 1920, 1080);
    p.assets.push(a.clone());
    for i in 1..10 {
        p.sequence_mut().tracks.insert(i, Track::new(TrackKind::Video, format!("V{}", i + 1)));
    }
    for i in 0..10 {
        let mut c = Clip::new(a.id, "c", TimeRange::new(Time::ZERO, Time::from_secs(60)), Time::ZERO);
        c.transform.scale = 0.3;
        for p_ in kadr_project::AnimParam::ALL {
            for (k, s) in [0i64, 30, 60].iter().enumerate() {
                c.animation.set_key(p_, kadr_project::SourceTime(Time::from_secs(*s)), p_.default_value() + k as f64 * 0.1, None);
            }
        }
        p.sequence_mut().tracks[i].clips.push(c);
    }
    p
}
```

and in `run()` after the existing loop, the same batching over `animated_project()` pushing `r.push("scene", "10 clips × 13 animated params", metric, us(v), "µs")`. Run `cargo run --release -p kadr-bench -- scene` and record p50/p90 against the 100 µs target in a short section of `docs/perf/2026-10-01-keyframes.md` (new file: machine, command, numbers, verdict).

- [ ] **Step 4: Full suite and clippy**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: exit 0, no new warnings in touched files.

- [ ] **Step 5: Live acceptance through `kadr-mcp`** (release build; real clips from the owner's test folder, never modified). Check and record in `docs/perf/2026-10-01-keyframes.md`, with screenshots or frames where visual:
  - first keyframe (`◆`) causes no visual jump;
  - a two-keyframe scale animation with default «Плавно» plays smoothly (frames at ¼, ½, ¾ match the ease-in-out curve);
  - dragging a diamond immediately changes the preview;
  - trim from the start keeps the animation on the same content frame;
  - split changes nothing visually (frames before/after at the cut);
  - changing speed stretches/compresses the animation with the media;
  - fade handles are easy to grab in a small window and do not fight with trim;
  - the inspector does not jitter during playback;
  - RU and EN layouts intact (inspector rows with `◀ ◆ ▶`, fade section, keyframe menu);
  - Alt+←/→ jump between keyframes and do not open the window menu.

- [ ] **Step 6: Commit and review**

```bash
git add crates/project-scenes apps/kadr-bench docs/perf ARCHITECTURE.md
git commit -m "keyframes: end-to-end pixel test, evaluator bench row, acceptance record"
```

Then run a whole-branch review with an Opus reviewer (spec compliance, the five Review Focus items, i18n completeness) and fix what it confirms before opening the PR.
