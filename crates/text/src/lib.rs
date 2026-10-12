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
pub use fonts::{fonts, Family, FontSource, BUNDLED_FAMILIES, DEFAULT_FAMILY};
pub use frame::{frame_size, Bounds, TextFrame};
pub use layout::{layout, Laid};

/// Tests that compare layouts or keys across calls hold this while the
/// font test adds a font (which bumps the font generation in between).
#[cfg(test)]
pub(crate) static TEST_FONTS: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// A 64-bit hash of a value's JSON (in-process cache keys only).
pub(crate) fn hash_of(v: &impl serde::Serialize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(v).unwrap_or_default().hash(&mut h);
    h.finish()
}
