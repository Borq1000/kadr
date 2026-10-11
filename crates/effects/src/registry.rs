//! The recipe registry (effects spec §3) and what a new effect starts with.

use crate::motion;
use crate::params::{default_value, ParamDef, ParamKind, Params, NORMAL};
use crate::PlacementDelta;
use kadr_core::Time;
use kadr_project::{AppliedEffect, Clip, ParamValue, SourceTime, TrackKind};
use kadr_scene::SizeU;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Motion,
    Look,
    Light,
    Transition,
    Audio,
}

impl Category {
    /// Tab order (spec §6.1).
    pub const ALL: [Category; 5] = [Category::Motion, Category::Look, Category::Light, Category::Transition, Category::Audio];

    pub fn key(self) -> &'static str {
        match self {
            Category::Motion => "motion",
            Category::Look => "look",
            Category::Light => "light",
            Category::Transition => "transition",
            Category::Audio => "audio",
        }
    }

    /// i18n key of the filter chip: `fx.cat.<key>`.
    pub fn label_key(self) -> String {
        format!("fx.cat.{}", self.key())
    }
}

/// What an effect goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A clip with a picture (video track).
    Clip,
    /// A cut between two clips (transitions, E3).
    Cut,
    /// A clip on an audio track (E4).
    AudioClip,
}

impl Target {
    pub fn key(self) -> &'static str {
        match self {
            Target::Clip => "clip",
            Target::Cut => "cut",
            Target::AudioClip => "audio_clip",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostClass {
    Cheap,
    Medium,
}

/// What a `compile` knows about the frame (spec §3, refinement 2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileCtx {
    /// Spec §1 at this frame (may lie outside 0..1 past the clip's edges).
    pub progress: f64,
    /// Seconds of the whole progress range at the clip's current length:
    /// `progress × clip_secs` is the effect's own clock, continuous across a split.
    pub clip_secs: f64,
    /// The source frame shown (source-bound parameters, «Момент»).
    pub source_time: SourceTime,
    /// Source seconds per timeline second.
    pub speed: f64,
    pub canvas: SizeU,
    /// The clip picture's width / height (the whole picture, before crop).
    pub aspect: f64,
    /// The visible crop window: its edges `(x0, y0, x1, y1)` as fractions of
    /// the picture (left, top, right, bottom) = `(crop_left, crop_top,
    /// 1 − crop_right, 1 − crop_bottom)` at this frame; `UNCROPPED` when the
    /// clip has no crop. Motion recipes keep this window covering its own
    /// on-screen footprint (ruling R6).
    pub window: (f64, f64, f64, f64),
    /// Timeline time of the frame.
    pub frame: Time,
    /// From the effect's id (noise).
    pub seed: u64,
}

impl CompileCtx {
    /// The window of a clip without crop: the whole picture.
    pub const UNCROPPED: (f64, f64, f64, f64) = (0.0, 0.0, 1.0, 1.0);
}

/// What one effect adds to a frame. E1 recipes only move the picture;
/// colour primitives (E2), overlays and transition builds (E3) and audio
/// operations (E4) join here.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub placement: PlacementDelta,
    pub effects: Vec<kadr_scene::Effect>,
}

impl Compiled {
    pub fn motion(placement: PlacementDelta) -> Compiled {
        Compiled { placement, effects: vec![] }
    }
}

pub struct RecipeDef {
    pub id: &'static str,
    pub category: Category,
    pub target: Target,
    /// The current `compile`'s version (spec §2 «Версии рецептов»).
    pub version: u16,
    /// 1–5 parameters, in inspector order.
    pub params: &'static [ParamDef],
    pub cost: CostClass,
    pub compile: fn(&Params, &CompileCtx) -> Compiled,
}

impl RecipeDef {
    /// i18n key of the effect's name: `fx.<id>`.
    pub fn name_key(&self) -> String {
        format!("fx.{}", self.id)
    }

    pub fn param(&self, key: &str) -> Option<&'static ParamDef> {
        self.params.iter().find(|p| p.key == key)
    }
}

const STRENGTH: ParamDef = ParamDef { key: "strength", kind: ParamKind::Number { min: 0.0, max: 1.0, default: NORMAL } };
const POINT: ParamDef = ParamDef { key: "point", kind: ParamKind::Point };
const MOMENT: ParamDef = ParamDef { key: "moment", kind: ParamKind::Moment };
const ZOOM_DIRECTION: ParamDef = ParamDef { key: "direction", kind: ParamKind::Choice { options: &["in", "out"], default: "in" } };
const PAN_DIRECTION: ParamDef = ParamDef { key: "direction", kind: ParamKind::Choice { options: &["left", "right", "up", "down"], default: "right" } };

static REGISTRY: [RecipeDef; 5] = [
    RecipeDef { id: "smooth_zoom", category: Category::Motion, target: Target::Clip, version: 1, params: &[ZOOM_DIRECTION, STRENGTH, POINT], cost: CostClass::Cheap, compile: motion::smooth_zoom },
    RecipeDef { id: "cinematic_push", category: Category::Motion, target: Target::Clip, version: 1, params: &[STRENGTH, POINT], cost: CostClass::Cheap, compile: motion::cinematic_push },
    RecipeDef { id: "punch_in", category: Category::Motion, target: Target::Clip, version: 1, params: &[STRENGTH, MOMENT, POINT], cost: CostClass::Cheap, compile: motion::punch_in },
    RecipeDef { id: "smooth_pan", category: Category::Motion, target: Target::Clip, version: 1, params: &[PAN_DIRECTION, STRENGTH], cost: CostClass::Cheap, compile: motion::smooth_pan },
    RecipeDef { id: "handheld", category: Category::Motion, target: Target::Clip, version: 1, params: &[STRENGTH], cost: CostClass::Cheap, compile: motion::handheld },
];

/// Every recipe this build runs, in card order.
pub fn registry() -> &'static [RecipeDef] {
    &REGISTRY
}

pub fn recipe(id: &str) -> Option<&'static RecipeDef> {
    REGISTRY.iter().find(|d| d.id == id)
}

/// The definition that evaluates `e`: its recipe at its version. `None`
/// for a recipe or a version from a newer Kadr (kept, not run — spec §2).
pub fn definition(e: &AppliedEffect) -> Option<&'static RecipeDef> {
    recipe(&e.recipe).filter(|d| d.version == e.version)
}

/// Whether an effect for `target` goes on a clip of a `kind` track.
pub fn fits(target: Target, kind: TrackKind) -> bool {
    matches!((target, kind), (Target::Clip, TrackKind::Video) | (Target::AudioClip, TrackKind::Audio))
}

/// Where a new effect's «Момент» goes (spec §5): the source frame at `at`
/// when `at` is on the clip (the playhead, a drop point), else the clip's
/// visible middle (refinement 4).
pub fn default_moment(clip: &Clip, at: Time) -> SourceTime {
    let t = if clip.timeline_in <= at && at < clip.timeline_out { at } else { clip.timeline_in + Time(clip.duration().flicks() / 2) };
    SourceTime(clip.source_time_at(t))
}

/// Gives every declared parameter missing from `params` its default.
pub fn complete(def: &RecipeDef, params: &mut BTreeMap<String, ParamValue>, clip: &Clip, at: Time) {
    for d in def.params {
        if !params.contains_key(d.key) {
            params.insert(d.key.to_string(), default_value(d, default_moment(clip, at)));
        }
    }
}

/// A new effect of `def` on `clip` at «Обычно», applied at timeline time `at`.
pub fn fresh(def: &RecipeDef, clip: &Clip, at: Time) -> AppliedEffect {
    let mut params = BTreeMap::new();
    complete(def, &mut params, clip, at);
    AppliedEffect::new(def.id, def.version, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, TimeRange};

    #[test]
    fn the_registry_holds_the_five_motion_effects_in_card_order() {
        let ids: Vec<&str> = registry().iter().map(|d| d.id).collect();
        assert_eq!(ids, ["smooth_zoom", "cinematic_push", "punch_in", "smooth_pan", "handheld"]);
        for d in registry() {
            assert_eq!((d.category, d.target, d.version, d.cost), (Category::Motion, Target::Clip, 1, CostClass::Cheap), "{}", d.id);
            assert!(!d.params.is_empty() && d.params.len() <= 5, "{}: 1–5 parameters", d.id);
            assert!(d.param("strength").is_some(), "{} has a strength", d.id);
            assert_eq!(d.name_key(), format!("fx.{}", d.id));
        }
        let keys = |id: &str| recipe(id).unwrap().params.iter().map(|p| p.key).collect::<Vec<_>>();
        assert_eq!(keys("smooth_zoom"), ["direction", "strength", "point"], "spec §5 order");
        assert_eq!(keys("punch_in"), ["strength", "moment", "point"]);
        assert_eq!(keys("smooth_pan"), ["direction", "strength"]);
        assert_eq!(keys("handheld"), ["strength"]);
    }

    /// Spec §2: a recipe or a version from a newer Kadr is kept, not run.
    #[test]
    fn unknown_recipes_and_newer_versions_have_no_definition() {
        let mut e = AppliedEffect::new("smooth_zoom", 1, BTreeMap::new());
        assert_eq!(definition(&e).map(|d| d.id), Some("smooth_zoom"));
        e.version = 2;
        assert!(definition(&e).is_none());
        assert!(definition(&AppliedEffect::new("glitch_v2", 1, BTreeMap::new())).is_none());
        assert!(recipe("glitch_v2").is_none());
    }

    #[test]
    fn fresh_effects_take_normal_strength_the_centre_and_the_frame_where_applied() {
        // Timeline 10..20 s showing source 30..40 s.
        let c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10));
        let e = fresh(recipe("punch_in").unwrap(), &c, Time::from_secs(12));
        assert_eq!(e.params["strength"], ParamValue::Number(0.6));
        assert_eq!(e.params["point"], ParamValue::Point { x: 0.5, y: 0.5 });
        assert_eq!(e.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(32))));
        assert_eq!((e.enabled, e.progress, e.version, e.recipe.as_str()), (true, (0.0, 1.0), 1, "punch_in"));
        let off = fresh(recipe("punch_in").unwrap(), &c, Time::from_secs(25));
        assert_eq!(off.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(35))), "applied off the clip: its middle");
        assert_eq!(fresh(recipe("smooth_pan").unwrap(), &c, Time::ZERO).params["direction"], ParamValue::Choice("right".into()));
        let mut given = BTreeMap::from([("strength".to_string(), ParamValue::Number(0.9))]);
        complete(recipe("handheld").unwrap(), &mut given, &c, Time::ZERO);
        assert_eq!(given.len(), 1, "a given value stays, nothing else is declared");
        assert_eq!(given["strength"], ParamValue::Number(0.9));
    }

    #[test]
    fn effects_fit_their_tracks() {
        assert!(fits(Target::Clip, TrackKind::Video));
        assert!(!fits(Target::Clip, TrackKind::Audio));
        assert!(fits(Target::AudioClip, TrackKind::Audio));
        assert!(!fits(Target::Cut, TrackKind::Video), "a transition is not a clip effect");
        assert_eq!(Category::Motion.label_key(), "fx.cat.motion");
    }
}
