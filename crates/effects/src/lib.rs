//! Effect recipes (effects spec §3): what an effect on a clip does at a
//! frame. A recipe is data plus a `compile` function from its parameter
//! values and the frame's context to scene primitives — a placement delta
//! (motion) or colour primitives (Looks, light). Depends on core, scene and
//! project; the evaluator in `kadr-timeline` calls it, renderers never see a
//! recipe.

pub mod delta;
pub mod json;
pub mod look;
pub mod motion;
pub mod noise;
pub mod params;
pub mod registry;

pub use delta::PlacementDelta;
pub use noise::Noise1D;
pub use params::*;
pub use registry::*;
