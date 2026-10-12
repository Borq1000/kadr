//! The evaluated scene of one output frame (render spec §4.1). References
//! media by id and source time — never pixels: a resolver turns those into
//! CPU frames today and GPU textures later without the scene changing.

use crate::geom::{Placement, RectF, SizeU};
use crate::primitives::{Gradient, Grain, Noise, SplitTone, ToneCurve, Vignette};
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

impl LayerId {
    /// The `n`-th layer made for what `base` names — a clip effect's
    /// overlays, a transition's band: `base` with its top bit flipped, xor
    /// `n` — never `base` itself, one id per `n`.
    pub fn derived(base: u128, n: u32) -> LayerId {
        LayerId(base ^ (1 << 127) ^ n as u128)
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

/// Render primitives only — never a recipe or a CPU filter object. Exact
/// formulas: crate docs, "Point operations".
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Effect {
    ColorAdjust(ColorAdjust),
    ToneCurve(ToneCurve),
    SplitTone(SplitTone),
    Vignette(Vignette),
    Grain(Grain),
}

impl Effect {
    /// Whether the primitive is neutral: an adjust equal to
    /// [`ColorAdjust::NEUTRAL`], an identity curve ([`ToneCurve::is_neutral`]),
    /// a split tone, vignette or grain whose amounts are 0. A neutral
    /// primitive is skipped wherever it stands in a chain (an identity tone
    /// curve does not clamp its input); a chain of only neutral primitives is
    /// skipped entirely, its final clamp included (crate docs, "Point
    /// operations").
    pub fn is_neutral(&self) -> bool {
        match self {
            Effect::ColorAdjust(a) => a.is_neutral(),
            Effect::ToneCurve(c) => c.is_neutral(),
            Effect::SplitTone(s) => s.is_neutral(),
            Effect::Vignette(v) => v.is_neutral(),
            Effect::Grain(g) => g.is_neutral(),
        }
    }

    /// Whether the primitive reads the pixel's canvas position.
    pub fn is_spatial(&self) -> bool {
        matches!(self, Effect::Vignette(_) | Effect::Grain(_))
    }
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
    /// The incoming side over the outgoing one, `out = to + from·(1 − to.a)`:
    /// both pictures whole (Сдвиг moves them edge to edge). `progress` is
    /// not used.
    Over,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitionLayer {
    pub op: TransitionOp,
    /// 0 at the start of the window, 1 at its end.
    pub progress: f32,
    pub from: Vec<Layer>,
    pub to: Vec<Layer>,
}

/// A layer that makes its own picture (effects spec §4 «генераторы»):
/// plain data; exact formulas in the crate docs, "Generators".
#[derive(Clone, Debug, PartialEq)]
pub enum Generator {
    Gradient(Gradient),
    Noise(Noise),
}

impl Generator {
    /// The premultiplied colour at local point `(x, y)` (the normative helper).
    pub fn at(&self, x: f64, y: f64) -> [f64; 4] {
        match self {
            Generator::Gradient(g) => g.at(x, y),
            Generator::Noise(n) => n.at(x, y),
        }
    }
}

/// The text engine's picture of a text layer at one instant (titles spec
/// §5.2): opaque to the scene and to renderers. The resolver asks it for
/// texels at the scale the output needs (`kadr-playback`, `text`).
pub trait TextPicture: Send + Sync + std::fmt::Debug {
    /// Equal keys ⇒ pixel-identical rasters at every scale: the cache key.
    fn key(&self) -> u64;
    /// The picture's box — the layer's `placement.size`, canvas px — at
    /// `scale` raster px per canvas px: premultiplied RGBA8 in the working
    /// space, `max(1, ⌈size · scale⌉)` px each way.
    fn render(&self, scale: f32, pool: &kadr_core::FramePool) -> kadr_core::CpuFrame;
}

/// A text layer (titles spec §5.2). Layers compare by their picture's key.
#[derive(Clone, Debug)]
pub struct TextLayer {
    pub picture: std::sync::Arc<dyn TextPicture>,
}

impl PartialEq for TextLayer {
    fn eq(&self, other: &Self) -> bool {
        self.picture.key() == other.picture.key()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent {
    Media { media: MediaRef, source_time: Time },
    Solid(Rgba),
    Transition(Box<TransitionLayer>),
    Generator(Generator),
    Text(TextLayer),
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
