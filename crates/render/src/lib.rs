//! Renderers (render spec §4.3): turn a [`FrameScene`] plus the real inputs
//! of its layers into pixels, by the rendering contract in `kadr-scene`'s
//! crate docs. No timeline, no project, no FFmpeg: a resolver prepares the
//! inputs, a renderer only draws.
//!
//! Today: [`CpuRenderer`]. Later a `WgpuRenderer` implements the same
//! [`Renderer`] trait; a GPU input becomes one more [`LayerInput`] variant.

mod cost;
mod cpu;

pub use cost::{layer_cost, scene_cost, Cost, CostClass};
pub use cpu::CpuRenderer;

use kadr_core::CpuFrame;
use kadr_scene::FrameScene;
use std::sync::Arc;
use std::time::Duration;

/// Why a media layer has no frame. The renderer draws it as
/// `Rgba::MISSING` either way; the reason is for telemetry and UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingReason {
    Offline,
    DecodeFailed,
    /// Not decoded in time (a preview may show the frame later).
    NotReady,
}

/// The real input of one scene layer, parallel to `FrameScene::layers`.
#[derive(Clone, Debug)]
pub enum LayerInput {
    /// A decoded frame: RGBA8, colour already converted to the working
    /// space (non-linear Rec.709 R'G'B', full range); alpha as its
    /// `color.alpha` says (`Opaque`, `Straight` or `Premultiplied`).
    Cpu(Arc<CpuFrame>),
    Missing(MissingReason),
    /// A layer without an external input (`Solid`).
    None,
    /// The inputs of a transition layer's `from` and `to` layers.
    Transition { from: Vec<LayerInput>, to: Vec<LayerInput> },
}

#[derive(Clone, Debug, Default)]
pub struct RenderInputs {
    pub layers: Vec<LayerInput>,
}

pub struct PreparedFrame<'a> {
    pub scene: &'a FrameScene,
    pub inputs: &'a RenderInputs,
}

/// A CPU destination: premultiplied RGBA8, `width × height` =
/// `scene.output.size`, rows `stride` bytes apart. It may be any memory —
/// a pooled frame or a display buffer — so presenting needs no copy.
pub struct CpuTarget<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: &'a mut [u8],
}

impl<'a> CpuTarget<'a> {
    pub fn from_frame(f: &'a mut CpuFrame) -> Self {
        CpuTarget { width: f.width, height: f.height, stride: f.stride, data: &mut f.data }
    }

    /// Tightly packed `width × height` RGBA8 bytes.
    pub fn packed(width: u32, height: u32, data: &'a mut [u8]) -> Self {
        CpuTarget { width, height, stride: width as usize * 4, data }
    }
}

pub enum RenderTarget<'a> {
    Cpu(CpuTarget<'a>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderStats {
    /// Whole render call.
    pub composite: Duration,
    /// Part of `composite` spent in effect chains (0 when fused and not separable).
    pub effects: Duration,
    /// Top-level layers drawn (culled or empty ones not counted).
    pub layers_drawn: u32,
    /// Layers drawn by a fast path (row copy) instead of per-pixel sampling.
    pub fast_paths: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The target is not `scene.output.size`, or its buffer is too small.
    TargetMismatch { expected: (u32, u32), got: (u32, u32) },
    /// `inputs` does not match the scene's layers (count or kind).
    InputMismatch(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::TargetMismatch { expected, got } => write!(f, "render target is {}x{}, the scene needs {}x{}", got.0, got.1, expected.0, expected.1),
            RenderError::InputMismatch(m) => write!(f, "render inputs do not match the scene: {m}"),
        }
    }
}

impl std::error::Error for RenderError {}

pub trait Renderer {
    fn name(&self) -> &str;
    fn render(&mut self, frame: &PreparedFrame, target: &mut RenderTarget) -> Result<RenderStats, RenderError>;
}
