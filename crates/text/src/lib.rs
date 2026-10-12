//! Kadr's text engine (titles spec §5.1): the fonts, layout, pixels,
//! animation and built-in styles of text clips. Pure Rust: parley (with
//! fontique and harfrust) lays text out, swash (with zeno) rasterises it.
//! The evaluator builds a [`TextFrame`] per text clip and instant; the
//! resolver asks it for a raster through `kadr_scene::TextPicture`.

pub mod anim;
pub mod fonts;
pub mod frame;
pub mod layout;
pub(crate) mod raster;
pub mod shape;

pub use anim::{Clock, FrameAnim, GlyphAnim};
pub use fonts::{fonts, Family, FontError, FontSource, BUNDLED_FAMILIES, DEFAULT_FAMILY};
pub use frame::{frame_size, Bounds, TextFrame};
pub use layout::{layout, Laid};

/// Tests that compare layouts or keys across calls hold this while the
/// font test adds a font (which bumps the font generation in between).
#[cfg(test)]
pub(crate) static TEST_FONTS: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// A 64-bit hash of a value's JSON (in-process cache keys only), streamed
/// into the hasher without building the JSON.
pub(crate) fn hash_of(v: &impl serde::Serialize) -> u64 {
    use std::hash::Hasher;
    struct Sink(std::collections::hash_map::DefaultHasher);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(std::collections::hash_map::DefaultHasher::new());
    let _ = serde_json::to_writer(&mut sink, v);
    sink.0.finish()
}
