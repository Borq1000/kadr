//! Deterministic edit engine: commands, undo/redo, snapping and the
//! composition plan shared by preview and export. Knows nothing about GUI or AI.

pub mod commands;
pub mod composition;
pub mod engine;
pub mod multicam;
pub mod ops;
pub mod scene;
pub mod snap;

pub use commands::{ClipProperty, EditCommand, EditError, InsertMode, TrackFlag, TrimEdge};
pub use engine::{EditEngine, HistoryEntry};
