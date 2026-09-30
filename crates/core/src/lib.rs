//! Core domain primitives shared by every Kadr crate: integer time, frame
//! rates, timecode and strongly-typed ids. No GUI, no I/O.

pub mod cancel;
pub mod color;
pub mod frame;
pub mod id;
pub mod media_info;
pub mod perf;
pub mod time;
pub mod timecode;

pub use cancel::CancelToken;
pub use color::ColorInfo;
pub use frame::{CpuFrame, FramePool, PixelFormat, PooledBuf};
pub use id::*;
pub use media_info::{AudioInfo, MediaInfo, MediaKind, VideoInfo};
pub use time::{FrameRate, Time, TimeRange, FLICKS_PER_SECOND};
pub use timecode::{format_duration, Timecode};
