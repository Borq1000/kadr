//! Effect recipes (effects spec §3): what an effect on a clip does at a
//! frame. A recipe is data plus a `compile` function from its parameter
//! values and the frame's context to scene primitives — in E1 a placement
//! delta (motion). Depends on core, scene and project; the evaluator in
//! `kadr-timeline` calls it, renderers never see a recipe.

pub mod delta;
pub mod noise;
pub mod params;

pub use delta::PlacementDelta;
pub use noise::Noise1D;
pub use params::*;
