//! CPU renderer: the rendering contract (kadr-scene crate docs) on
//! premultiplied RGBA8, rows in parallel with rayon.

use crate::{PreparedFrame, RenderError, RenderStats, RenderTarget, Renderer};
use kadr_core::FramePool;

pub struct CpuRenderer {
    /// Transition buffers.
    pool: FramePool,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuRenderer {
    pub fn new() -> Self {
        CpuRenderer { pool: FramePool::new(256 << 20) }
    }
}

impl Renderer for CpuRenderer {
    fn name(&self) -> &str {
        "cpu"
    }

    fn render(&mut self, _frame: &PreparedFrame, _target: &mut RenderTarget) -> Result<RenderStats, RenderError> {
        let _ = &self.pool;
        todo!("M2 task: CpuRenderer")
    }
}
