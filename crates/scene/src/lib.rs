//! Kadr's evaluated scene (render spec §4): what one output frame shows, as
//! plain data. Produced by the timeline's scene evaluator, consumed by
//! renderers through kadr-render. No pixels, no I/O, no timeline, no FFmpeg.

pub mod geom;
pub mod scene;

pub use geom::*;
pub use scene::*;
