//! Kadr playback (render spec §4.2, §8–10): real frames for scenes.
//!
//! ```text
//!  SceneSource ──scene_at──► Resolver ──► RenderInputs ──► Renderer ──► FrameSink
//!  (app, M4)                   │  ▲                                      (display)
//!                    requests  ▼  │ frames (Arc<CpuFrame>, pooled)
//!                 decoder sessions ──► FrameCache (LRU by bytes)
//!                 (thread + stream each)
//! ```
//!
//! - [`Resolver`]: per media layer, decode size (footprint quantised to a
//!   fraction of the display size) and source frame (clamped to the last
//!   frame); cache or decoder session; waits per [`Mode`].
//! - Decoder sessions: one thread and one stream per (media, decode size,
//!   position); read forward within a window, reopen otherwise; always
//!   towards the newest target; close when idle.
//! - Generations: every show/play/stop is a new generation; waiters and
//!   session work of older ones are abandoned.
//! - [`PreviewPlayer`]: paces rendered frames against a [`Clock`] with the
//!   pure [`pace()`] / [`PlaySchedule`], drops late frames, prefetches the
//!   scene ahead, records [`kadr_core::perf::FramePerf`] per frame.
//!
//! Depends on core, scene, render and media only — never on the timeline or
//! the project (spec §3).
//!
//! # Threads
//!
//! - `PreviewPlayer::{show, play, stop, set_source, set_output, generation}`:
//!   any thread (the UI thread), never block on rendering or decoding —
//!   except that `show`/`play`/`stop` wait for a `FrameSink::present` in
//!   progress (so none of an older generation follows them).
//! - `FrameSink`, `Clock::now`, `SceneSource`: the player thread
//!   (`kadr-player`). Decoding: one `kadr-decode` thread per session.
//! - Audio sync: implement [`Clock`] over the audio engine's clock (timeline
//!   time from samples played, `None` when stopped); start the audio at
//!   `from`, then call `play(from)`; `stop()` both.

pub mod cache;
pub mod decoders;
pub mod pace;
pub mod player;
pub mod resolver;
mod session;
pub mod source;
pub mod testing;

pub use cache::{FrameCache, FrameKey};
pub use decoders::{Decoders, FfmpegDecoders};
pub use pace::{pace, Clock, Pace, PlaySchedule, Step, WallClock};
pub use player::{FrameInfo, FrameSink, PlayerConfig, PreviewPlayer};
pub use resolver::{decode_size, Mode, Resolver, ResolverConfig, ResolverStats};
pub use source::{MediaSource, SceneSource};
