//! Deterministic edit engine: commands, undo/redo, snapping, and what
//! preview and export share: the scene evaluator (`scene`, what is seen)
//! and the audio segments (`composition`, what is heard). Knows nothing
//! about GUI, AI or renderers.

pub mod commands;
pub mod composition;
pub mod engine;
pub mod fx;
pub mod multicam;
pub mod ops;
pub mod scene;
pub mod snap;
pub mod text;

pub use commands::{editable_keys_at, track_def, ClipProperty, EditCommand, EditError, InsertMode, TrackFlag, TrimEdge};
pub use engine::{EditEngine, HistoryEntry};
