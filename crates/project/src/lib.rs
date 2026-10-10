//! Serializable project model (`.kadr`, JSON). The project references source
//! media by path and never contains or modifies it.

pub mod ai_log;
pub mod analysis;
pub mod animation;
pub mod decisions;
pub mod fades;
pub mod io;
pub mod model;
pub mod sequence;

pub use ai_log::*;
pub use analysis::*;
pub use animation::*;
pub use decisions::*;
pub use fades::*;
pub use model::*;
pub use sequence::*;
