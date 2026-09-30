//! The evaluated scene of one output frame (render spec §4.1). References
//! media by id and source time — never pixels: a resolver turns those into
//! CPU frames today and GPU textures later without the scene changing.

use crate::geom::{Placement, RectF, SizeU};
use kadr_core::{AssetId, ClipId, ColorInfo, Time, TransitionId};

/// Stable identity of a layer (from a clip or transition id): the key for
/// caches and, later, GPU resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayerId(pub u128);

impl From<ClipId> for LayerId {
    fn from(c: ClipId) -> Self {
        LayerId(c.0.as_u128())
    }
}

impl From<TransitionId> for LayerId {
    fn from(t: TransitionId) -> Self {
        LayerId(t.0.as_u128())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Video,
    Image,
}

/// Which media a layer shows. What is actually decoded (original or proxy,
/// at which size, into CPU memory or a GPU texture) is the resolver's call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaRef {
    pub media: AssetId,
    pub stream: u32,
    pub kind: SourceKind,
    /// Size as displayed (sample aspect ratio and rotation applied).
    pub display_size: SizeU,
    pub color: ColorInfo,
}

/// A colour in the working space (render spec §6): non-linear Rec.709,
/// straight alpha, components in [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const TRANSPARENT: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    /// Drawn in place of a media frame that cannot be had (offline, decode
    /// error): an opaque dark red, like «Media Offline» in other editors.
    pub const MISSING: Rgba = Rgba { r: 0.45, g: 0.05, b: 0.08, a: 1.0 };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Add,
    Multiply,
    Screen,
}

/// Formulas: effects spec «Примитивы цвета».
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorAdjust {
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
}

impl ColorAdjust {
    pub const NEUTRAL: ColorAdjust = ColorAdjust { exposure: 0.0, contrast: 1.0, saturation: 1.0, temperature: 0.0, tint: 0.0 };
    pub fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }
}

/// Render primitives only — never a recipe or a CPU filter object.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Effect {
    ColorAdjust(ColorAdjust),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransitionOp {
    Dissolve,
    DipToColor(Rgba),
    /// A straight edge sweeping across the canvas. The edge moves along the
    /// unit vector `(cos angle, sin angle)` in canvas pixels (y points down);
    /// the region the edge has already passed shows `to`, the rest `from`.
    /// `angle` 0 = the edge moves left → right (the incoming picture appears
    /// on the left); π = right → left (incoming from the right, like
    /// FFmpeg's `wipeleft`); π/2 = top → bottom. `softness` is the width of
    /// the blended band, in canvas pixels. Exact mask: crate docs,
    /// "Rendering contract".
    Wipe { angle: f32, softness: f32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitionLayer {
    pub op: TransitionOp,
    /// 0 at the start of the window, 1 at its end.
    pub progress: f32,
    pub from: Vec<Layer>,
    pub to: Vec<Layer>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent {
    Media { media: MediaRef, source_time: Time },
    Solid(Rgba),
    Transition(Box<TransitionLayer>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: LayerId,
    pub content: LayerContent,
    pub placement: Placement,
    /// Visible part of the content, in local pixels (render spec §5).
    pub crop: RectF,
    pub opacity: f32,
    pub blend: BlendMode,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderQuality {
    PreviewFast,
    PreviewHigh,
    Export,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpec {
    pub size: SizeU,
    pub quality: RenderQuality,
    pub color: ColorInfo,
}

impl OutputSpec {
    /// SDR working-space output of `size` pixels.
    pub fn new(size: SizeU, quality: RenderQuality) -> Self {
        OutputSpec { size, quality, color: ColorInfo::WORKING_SDR }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameScene {
    /// Timeline time.
    pub time: Time,
    /// Logical canvas = the sequence size; layer geometry is in its pixels.
    pub canvas: SizeU,
    pub output: OutputSpec,
    pub background: Rgba,
    /// Bottom to top, invisible layers already removed.
    pub layers: Vec<Layer>,
}

impl FrameScene {
    pub fn empty(time: Time, canvas: SizeU, output: OutputSpec) -> Self {
        FrameScene { time, canvas, output, background: Rgba::BLACK, layers: vec![] }
    }
}
