//! The recipe registry (effects spec §3) and what a new effect starts with.

use crate::leak;
use crate::look;
use crate::motion;
use crate::transition::{self, TransitionBuild, TransitionCtx};
use crate::params::{default_value, ParamDef, ParamKind, Params, NORMAL};
use crate::PlacementDelta;
use kadr_core::Time;
use kadr_project::{AppliedEffect, Clip, ParamValue, SourceTime, TrackKind, Transition};
use kadr_scene::SizeU;
use std::collections::BTreeMap;
use std::sync::LazyLock;

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

    /// A context for a still outside any clip's timing — golden images,
    /// benches: progress 0, a 1 s clip at speed 1, source time 0, uncropped,
    /// the canvas's aspect.
    pub fn still(canvas: SizeU, frame: Time, seed: u64) -> CompileCtx {
        CompileCtx { progress: 0.0, clip_secs: 1.0, source_time: SourceTime(Time::ZERO), speed: 1.0, canvas, aspect: canvas.w as f64 / canvas.h.max(1) as f64, window: Self::UNCROPPED, frame, seed }
    }

    /// The clip's picture in canvas pixels at its scale 1 — its placement's
    /// size: the picture of aspect `aspect` fitted into the canvas (a
    /// non-finite or non-positive aspect counts as the canvas's). An
    /// overlay's geometry is in these local pixels.
    pub fn picture(&self) -> (f64, f64) {
        let (cw, ch) = (self.canvas.w as f64, self.canvas.h.max(1) as f64);
        let a = if self.aspect.is_finite() && self.aspect > 0.0 { self.aspect } else { cw / ch };
        if cw / ch > a { (ch * a, ch) } else { (cw, cw / a) }
    }
}

/// What one effect adds to a frame of its clip: motion recipes move the
/// picture, colour recipes (Look, Vignette) add primitives after the clip's
/// correction, light recipes (Light Leak) add generator layers over the
/// picture; audio operations (E4) join here. A transition's frame is a
/// [`TransitionBuild`].
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub placement: PlacementDelta,
    pub effects: Vec<kadr_scene::Effect>,
    /// Generator layers over the clip's picture, in order (D7): the
    /// evaluator puts each on the clip layer's placement and crop, directly
    /// above it.
    pub overlays: Vec<Overlay>,
}

impl Default for Compiled {
    /// Nothing: the picture stays where it is, unchanged.
    fn default() -> Self {
        Compiled { placement: PlacementDelta::IDENTITY, effects: vec![], overlays: vec![] }
    }
}

impl Compiled {
    pub fn motion(placement: PlacementDelta) -> Compiled {
        Compiled { placement, ..Compiled::default() }
    }

    /// A colour effect's frame: primitives only, the picture stays where it is.
    pub fn colour(effects: Vec<kadr_scene::Effect>) -> Compiled {
        Compiled { effects, ..Compiled::default() }
    }

    /// A light effect's frame: generator layers over the picture.
    pub fn overlays(overlays: Vec<Overlay>) -> Compiled {
        Compiled { overlays, ..Compiled::default() }
    }
}

/// A generator layer an effect draws over a picture (D7, refinement 6):
/// its geometry in the picture's local pixels (`CompileCtx::picture`), its
/// blend, and its opacity before the picture layer's own.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub generator: kadr_scene::Generator,
    pub blend: kadr_scene::BlendMode,
    pub opacity: f32,
}

/// How a recipe compiles (refinement 6): a clip effect's frame, or a
/// transition's on a cut.
#[derive(Clone, Copy)]
pub enum Compile {
    Clip(fn(&Params, &CompileCtx) -> Compiled),
    Cut(fn(&Params, &TransitionCtx) -> TransitionBuild),
}

pub struct RecipeDef {
    pub id: &'static str,
    pub category: Category,
    pub target: Target,
    /// The current `compile`'s version (spec §2 «Версии рецептов»).
    pub version: u16,
    /// 1–5 parameters, in inspector order.
    pub params: &'static [ParamDef],
    /// The `Choice` parameter whose options are the recipe's variants —
    /// one card each, named `fx.<id>.<option>`, picked from a list in the
    /// inspector (the Look's `preset`, refinement 1); `None`: one card.
    pub variants: Option<&'static str>,
    pub cost: CostClass,
    pub compile: Compile,
}

impl RecipeDef {
    /// i18n key of the effect's name: `fx.<id>`.
    pub fn name_key(&self) -> String {
        format!("fx.{}", self.id)
    }

    /// A clip effect's frame; nothing for a transition recipe.
    pub fn compile_clip(&self, p: &Params, ctx: &CompileCtx) -> Compiled {
        match self.compile {
            Compile::Clip(f) => f(p, ctx),
            Compile::Cut(_) => Compiled::default(),
        }
    }

    /// A transition's frame; `None` for a clip recipe.
    pub fn compile_cut(&self, p: &Params, ctx: &TransitionCtx) -> Option<TransitionBuild> {
        match self.compile {
            Compile::Cut(f) => Some(f(p, ctx)),
            Compile::Clip(_) => None,
        }
    }

    pub fn param(&self, key: &str) -> Option<&'static ParamDef> {
        self.params.iter().find(|p| p.key == key)
    }

    /// The variants (refinement 1): the options of the `variants` parameter, or none.
    pub fn variant_options(&self) -> &'static [&'static str] {
        match self.variants.and_then(|k| self.param(k)).map(|p| p.kind) {
            Some(ParamKind::Choice { options, .. }) => options,
            _ => &[],
        }
    }

    /// i18n key of a choice option's name: `fx.<id>.<option>` for a variant
    /// (one source for the card, the list and the block's title, D7),
    /// `fx.choice.<option>` for any other choice.
    pub fn option_name_key(&self, param: &str, option: &str) -> String {
        if self.variants == Some(param) { format!("fx.{}.{option}", self.id) } else { format!("fx.choice.{option}") }
    }

    /// The variant stored `params` hold: their choice when this build has
    /// it, the default when they hold none; `None` for a recipe without
    /// variants or a variant this build lacks (a newer Kadr's preset).
    pub fn variant_of(&self, params: &BTreeMap<String, ParamValue>) -> Option<&'static str> {
        let key = self.variants?;
        let ParamKind::Choice { options, default } = self.param(key)?.kind else { return None };
        match params.get(key) {
            None => Some(default),
            Some(ParamValue::Choice(c)) => options.iter().copied().find(|o| *o == c.as_str()),
            Some(_) => None,
        }
    }
}

const STRENGTH: ParamDef = ParamDef { key: "strength", kind: ParamKind::Number { min: 0.0, max: 1.0, default: NORMAL } };
const POINT: ParamDef = ParamDef { key: "point", kind: ParamKind::Point };
const MOMENT: ParamDef = ParamDef { key: "moment", kind: ParamKind::Moment };
const ZOOM_DIRECTION: ParamDef = ParamDef { key: "direction", kind: ParamKind::Choice { options: &["in", "out"], default: "in" } };
const PAN_DIRECTION: ParamDef = ParamDef { key: "direction", kind: ParamKind::Choice { options: &["left", "right", "up", "down"], default: "right" } };

const SOFTNESS: ParamDef = ParamDef { key: "softness", kind: ParamKind::Number { min: 0.0, max: 1.0, default: 0.5 } };
const LEAK_COLOR: ParamDef = ParamDef { key: "color", kind: ParamKind::Choice { options: &["warm", "pink", "cold"], default: "warm" } };
const LEAK_VARIANT: ParamDef = ParamDef { key: "variant", kind: ParamKind::Choice { options: &["1", "2", "3"], default: "1" } };
const FADE_STYLE: ParamDef = ParamDef { key: "style", kind: ParamKind::Choice { options: &["clean", "film"], default: "clean" } };
const DIP_COLOR: ParamDef = ParamDef { key: "color", kind: ParamKind::Choice { options: &["black", "white"], default: "black" } };
const CUT_DIRECTION: ParamDef = ParamDef { key: "direction", kind: ParamKind::Choice { options: &["left", "right", "up", "down"], default: "left" } };
const SWEEP_COLOR: ParamDef = ParamDef { key: "color", kind: ParamKind::Choice { options: &["warm", "white", "cold"], default: "warm" } };
const WIPE_SOFTNESS: ParamDef = ParamDef { key: "softness", kind: ParamKind::Number { min: 0.0, max: 1.0, default: 0.25 } };

/// The Look's parameters: its `preset` options are `looks.json`'s keys
/// (refinement 2), known only at run time.
static LOOK_PARAMS: LazyLock<[ParamDef; 2]> =
    LazyLock::new(|| [ParamDef { key: "preset", kind: ParamKind::Choice { options: look::preset_keys(), default: look::DEFAULT_PRESET } }, STRENGTH]);

static REGISTRY: LazyLock<[RecipeDef; 13]> = LazyLock::new(|| {
    use Compile::{Clip, Cut};
    [
        RecipeDef { id: "smooth_zoom", category: Category::Motion, target: Target::Clip, version: 1, params: &[ZOOM_DIRECTION, STRENGTH, POINT], variants: None, cost: CostClass::Cheap, compile: Clip(motion::smooth_zoom) },
        RecipeDef { id: "cinematic_push", category: Category::Motion, target: Target::Clip, version: 1, params: &[STRENGTH, POINT], variants: None, cost: CostClass::Cheap, compile: Clip(motion::cinematic_push) },
        RecipeDef { id: "punch_in", category: Category::Motion, target: Target::Clip, version: 1, params: &[STRENGTH, MOMENT, POINT], variants: None, cost: CostClass::Cheap, compile: Clip(motion::punch_in) },
        RecipeDef { id: "smooth_pan", category: Category::Motion, target: Target::Clip, version: 1, params: &[PAN_DIRECTION, STRENGTH], variants: None, cost: CostClass::Cheap, compile: Clip(motion::smooth_pan) },
        RecipeDef { id: "handheld", category: Category::Motion, target: Target::Clip, version: 1, params: &[STRENGTH], variants: None, cost: CostClass::Cheap, compile: Clip(motion::handheld) },
        RecipeDef { id: "look", category: Category::Look, target: Target::Clip, version: 1, params: &*LOOK_PARAMS, variants: Some("preset"), cost: CostClass::Medium, compile: Clip(look::look) },
        RecipeDef { id: "vignette", category: Category::Light, target: Target::Clip, version: 1, params: &[STRENGTH, SOFTNESS], variants: None, cost: CostClass::Cheap, compile: Clip(look::vignette) },
        RecipeDef { id: "light_leak", category: Category::Light, target: Target::Clip, version: 1, params: &[STRENGTH, LEAK_COLOR, LEAK_VARIANT], variants: None, cost: CostClass::Medium, compile: Clip(leak::light_leak) },
        RecipeDef { id: "clean_fade", category: Category::Transition, target: Target::Cut, version: 1, params: &[FADE_STYLE], variants: None, cost: CostClass::Cheap, compile: Cut(transition::clean_fade) },
        RecipeDef { id: "fade_through_color", category: Category::Transition, target: Target::Cut, version: 1, params: &[DIP_COLOR], variants: None, cost: CostClass::Cheap, compile: Cut(transition::fade_through_color) },
        RecipeDef { id: "smooth_push", category: Category::Transition, target: Target::Cut, version: 1, params: &[CUT_DIRECTION], variants: None, cost: CostClass::Cheap, compile: Cut(transition::smooth_push) },
        RecipeDef { id: "light_sweep", category: Category::Transition, target: Target::Cut, version: 1, params: &[CUT_DIRECTION, SWEEP_COLOR], variants: None, cost: CostClass::Medium, compile: Cut(transition::light_sweep) },
        RecipeDef { id: "wipe", category: Category::Transition, target: Target::Cut, version: 1, params: &[CUT_DIRECTION, WIPE_SOFTNESS], variants: None, cost: CostClass::Cheap, compile: Cut(transition::wipe) },
    ]
});

/// Every recipe this build runs, in card order.
pub fn registry() -> &'static [RecipeDef] {
    &REGISTRY[..]
}

pub fn recipe(id: &str) -> Option<&'static RecipeDef> {
    registry().iter().find(|d| d.id == id)
}

/// The definition that evaluates `e`: its recipe at its version, holding a
/// variant this build has. `None` for a recipe, a version or a variant (a
/// Look preset) from a newer Kadr — kept, not run (spec §2, refinement 3).
pub fn definition(e: &AppliedEffect) -> Option<&'static RecipeDef> {
    recipe(&e.recipe).filter(|d| d.version == e.version && (d.variants.is_none() || d.variant_of(&e.params).is_some()))
}

/// The definition that evaluates transition `tr`: a cut recipe at its
/// version. `None` for a recipe or version from a newer Kadr (kept, never
/// run: a hard cut, shown «Недоступен») and for a clip recipe's id.
pub fn transition_definition(tr: &Transition) -> Option<&'static RecipeDef> {
    recipe(&tr.recipe).filter(|d| d.target == Target::Cut && d.version == tr.version)
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
    fn a_transition_runs_only_with_a_cut_recipe_at_its_version() {
        let mk = |recipe: &str, version: u16| Transition::new(recipe, version, BTreeMap::new(), kadr_core::ClipId::new(), kadr_core::ClipId::new(), Time::from_secs(1));
        assert_eq!(transition_definition(&mk("wipe", 1)).map(|d| d.id), Some("wipe"));
        assert!(transition_definition(&mk("wipe", 2)).is_none(), "a newer version");
        assert!(transition_definition(&mk("iris", 1)).is_none(), "a newer recipe");
        assert!(transition_definition(&mk("look", 1)).is_none(), "a clip recipe is no transition");
    }

    #[test]
    fn the_registry_holds_motion_looks_light_then_transitions_in_card_order() {
        let ids: Vec<&str> = registry().iter().map(|d| d.id).collect();
        assert_eq!(ids, ["smooth_zoom", "cinematic_push", "punch_in", "smooth_pan", "handheld", "look", "vignette", "light_leak", "clean_fade", "fade_through_color", "smooth_push", "light_sweep", "wipe"]);
        for d in registry() {
            assert_eq!(d.version, 1, "{}", d.id);
            assert!(!d.params.is_empty() && d.params.len() <= 5, "{}: 1–5 parameters", d.id);
            assert_eq!(d.name_key(), format!("fx.{}", d.id));
            if d.category == Category::Transition {
                assert_eq!((d.target, d.variants), (Target::Cut, None), "{}", d.id);
                assert!(matches!(d.compile, Compile::Cut(_)) && d.param("strength").is_none(), "{}: a transition has a length, not a strength", d.id);
                assert!(d.params.iter().all(|p| matches!(p.kind, ParamKind::Number { .. } | ParamKind::Choice { .. })), "{}: no point or moment on a cut", d.id);
            } else {
                assert_eq!(d.target, Target::Clip, "{}", d.id);
                assert!(matches!(d.compile, Compile::Clip(_)) && d.param("strength").is_some(), "{} has a strength", d.id);
            }
        }
        for d in &registry()[..5] {
            assert_eq!((d.category, d.cost, d.variants), (Category::Motion, CostClass::Cheap, None), "{}", d.id);
        }
        let (look, vignette, leak) = (recipe("look").unwrap(), recipe("vignette").unwrap(), recipe("light_leak").unwrap());
        assert_eq!((look.category, look.cost, look.variants), (Category::Look, CostClass::Medium, Some("preset")));
        assert_eq!((vignette.category, vignette.cost, vignette.variants), (Category::Light, CostClass::Cheap, None));
        assert_eq!((leak.category, leak.cost, leak.variants), (Category::Light, CostClass::Medium, None), "one card: its variant is a plain choice");
        let keys = |id: &str| recipe(id).unwrap().params.iter().map(|p| p.key).collect::<Vec<_>>();
        assert_eq!(keys("smooth_zoom"), ["direction", "strength", "point"], "spec §5 order");
        assert_eq!(keys("punch_in"), ["strength", "moment", "point"]);
        assert_eq!(keys("smooth_pan"), ["direction", "strength"]);
        assert_eq!(keys("handheld"), ["strength"]);
        assert_eq!(keys("look"), ["preset", "strength"]);
        assert_eq!(keys("vignette"), ["strength", "softness"]);
        assert_eq!(keys("light_leak"), ["strength", "color", "variant"]);
        assert_eq!(keys("clean_fade"), ["style"]);
        assert_eq!(keys("fade_through_color"), ["color"]);
        assert_eq!(keys("smooth_push"), ["direction"]);
        assert_eq!(keys("light_sweep"), ["direction", "color"]);
        assert_eq!(keys("wipe"), ["direction", "softness"]);
        let c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
        let e = fresh(look, &c, Time::ZERO);
        assert_eq!((e.params["preset"].clone(), e.params["strength"].clone()), (ParamValue::Choice("cinematic".into()), ParamValue::Number(0.6)));
        assert_eq!(fresh(vignette, &c, Time::ZERO).params["softness"], ParamValue::Number(0.5));
        assert_eq!(fresh(leak, &c, Time::ZERO).params["variant"], ParamValue::Choice("1".into()));
        assert!(recipe("wipe").unwrap().compile_cut(&Params::resolve(&[], &BTreeMap::new(), |_| None), &TransitionCtx { progress: 0.0, canvas: SizeU::new(2, 2), output: SizeU::new(2, 2), seed: 0 }).is_some());
        assert!(look.compile_cut(&Params::resolve(&[], &BTreeMap::new(), |_| None), &TransitionCtx { progress: 0.0, canvas: SizeU::new(2, 2), output: SizeU::new(2, 2), seed: 0 }).is_none(), "a clip recipe has no cut frame");
    }

    /// Refinements 1–3: the Look's presets are its variants, named from one
    /// source; a preset this build lacks is a newer Kadr's effect.
    #[test]
    fn the_look_has_one_variant_per_preset_named_in_its_own_keys() {
        let look = recipe("look").unwrap();
        assert_eq!(look.variant_options(), crate::look::preset_keys(), "the options are looks.json's keys");
        assert!(recipe("smooth_zoom").unwrap().variant_options().is_empty());
        assert_eq!(look.option_name_key("preset", "warm"), "fx.look.warm");
        assert_eq!(recipe("smooth_zoom").unwrap().option_name_key("direction", "in"), "fx.choice.in");
        let with = |v: Option<ParamValue>| {
            let mut e = AppliedEffect::new("look", 1, BTreeMap::new());
            if let Some(v) = v {
                e.params.insert("preset".into(), v);
            }
            e
        };
        assert_eq!(look.variant_of(&with(Some(ParamValue::Choice("vintage".into()))).params), Some("vintage"));
        assert_eq!(look.variant_of(&with(None).params), Some("cinematic"), "missing: the default");
        assert!(definition(&with(Some(ParamValue::Choice("bleach_bypass".into())))).is_none(), "a newer Kadr's preset is not run");
        assert!(definition(&with(Some(ParamValue::Number(1.0)))).is_none());
        assert!(definition(&with(None)).is_some() && definition(&with(Some(ParamValue::Choice("warm".into())))).is_some());
    }

    /// Refinement 3 at the project level: a Look of a preset from a newer
    /// Kadr — with its strength keyframed — comes through `io::parse` and a
    /// save as the same bytes, and is still not run after the round trip.
    #[test]
    fn a_look_of_a_preset_this_build_lacks_survives_parse_and_save() {
        use kadr_project::{io, Ease, Project, SourceTime, TrackKey};
        let mut p = Project::new("future look");
        let mut c = Clip::new(AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(4)), Time::ZERO);
        let params = [("preset".to_string(), ParamValue::Choice("bleach_bypass".into())), ("strength".to_string(), ParamValue::Number(0.8))].into_iter().collect();
        let look = AppliedEffect::new("look", 1, params);
        c.animation.set_key(TrackKey::fx(look.id, "strength"), SourceTime(Time::from_secs(1)), 0.3, Some(Ease::Linear));
        c.effects.push(look);
        p.sequence_mut().tracks[0].clips.push(c);
        let saved = io::to_json(&p);
        let loaded = io::parse(&saved, std::path::Path::new("future.kadr")).unwrap();
        assert_eq!(io::to_json(&loaded), saved, "parse → save gives the same bytes");
        assert_eq!(loaded, p);
        let e = &loaded.sequence().tracks[0].clips[0].effects[0];
        assert_eq!(e.params["preset"], ParamValue::Choice("bleach_bypass".into()), "the preset is kept as written");
        assert!(definition(e).is_none(), "a newer Kadr's preset is not run");
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
