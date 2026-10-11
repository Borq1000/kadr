//! Effect recipes (effects spec §3): what an effect on a clip, or a
//! transition on a cut, does at a frame. A recipe is data plus a `compile`
//! function from its parameter values and the frame's context to scene
//! primitives — for a clip a placement delta (motion), colour primitives
//! (Looks, Vignette) or generator overlays (Light Leak); for a cut a
//! [`TransitionBuild`]. Depends on core, scene and project; the evaluator in
//! `kadr-timeline` calls it, renderers never see a recipe.

pub mod delta;
pub mod json;
pub mod leak;
pub mod look;
pub mod motion;
pub mod noise;
pub mod params;
pub mod registry;
pub mod transition;

pub use delta::PlacementDelta;
pub use noise::Noise1D;
pub use params::*;
pub use registry::*;
pub use transition::{default_duration, transition_params, transition_seed, TransitionBuild, TransitionCtx};
