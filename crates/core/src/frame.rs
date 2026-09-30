//! CPU frames and the pool their buffers come from (render spec §4.4, §11).
//! A frame-sized buffer is allocated once and then reused: decoders,
//! renderers and the display hand `Arc<CpuFrame>` around without copies, and
//! a dropped frame gives its buffer back to the pool it came from.

use crate::color::ColorInfo;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 4 bytes per pixel, R G B A. Whether alpha is straight or
    /// premultiplied is `CpuFrame::color.alpha`.
    Rgba8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgba8 => 4,
        }
    }
}

struct PoolInner {
    /// Free buffers by exact byte length.
    free: Mutex<HashMap<usize, Vec<Vec<u8>>>>,
    /// Free bytes kept at most; beyond that a returned buffer is released.
    max_free_bytes: usize,
    free_bytes: AtomicU64,
    allocations: AtomicU64,
}

/// Reusable frame buffers. Cheap to clone (shared).
#[derive(Clone)]
pub struct FramePool {
    inner: Arc<PoolInner>,
}

impl std::fmt::Debug for FramePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FramePool(allocations: {}, free bytes: {})", self.allocations(), self.free_bytes())
    }
}

impl FramePool {
    /// A pool keeping at most `max_free_bytes` of unused buffers.
    pub fn new(max_free_bytes: usize) -> Self {
        FramePool {
            inner: Arc::new(PoolInner { free: Mutex::new(HashMap::new()), max_free_bytes, free_bytes: AtomicU64::new(0), allocations: AtomicU64::new(0) }),
        }
    }

    /// A buffer of exactly `len` bytes. A reused buffer keeps its old
    /// contents (callers overwrite it); only a fresh one is zeroed.
    pub fn take(&self, len: usize) -> PooledBuf {
        let reused = {
            let mut free = self.inner.free.lock().unwrap_or_else(|e| e.into_inner());
            free.get_mut(&len).and_then(Vec::pop)
        };
        let data = match reused {
            Some(v) => {
                self.inner.free_bytes.fetch_sub(len as u64, Ordering::Relaxed);
                v
            }
            None => {
                self.inner.allocations.fetch_add(1, Ordering::Relaxed);
                vec![0u8; len]
            }
        };
        PooledBuf { data, pool: Arc::downgrade(&self.inner) }
    }

    /// Buffers this pool has allocated (not reused) so far.
    pub fn allocations(&self) -> u64 {
        self.inner.allocations.load(Ordering::Relaxed)
    }

    /// Bytes currently waiting for reuse.
    pub fn free_bytes(&self) -> u64 {
        self.inner.free_bytes.load(Ordering::Relaxed)
    }
}

/// A byte buffer that returns to its pool when dropped.
pub struct PooledBuf {
    data: Vec<u8>,
    pool: Weak<PoolInner>,
}

impl PooledBuf {
    /// A buffer that belongs to no pool (tests, one-off frames).
    pub fn detached(data: Vec<u8>) -> Self {
        PooledBuf { data, pool: Weak::new() }
    }
}

impl Deref for PooledBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl DerefMut for PooledBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Drop for PooledBuf {
    fn drop(&mut self) {
        let Some(pool) = self.pool.upgrade() else { return };
        let len = self.data.len();
        if pool.free_bytes.load(Ordering::Relaxed) + len as u64 > pool.max_free_bytes as u64 {
            return;
        }
        pool.free_bytes.fetch_add(len as u64, Ordering::Relaxed);
        let mut free = pool.free.lock().unwrap_or_else(|e| e.into_inner());
        free.entry(len).or_default().push(std::mem::take(&mut self.data));
    }
}

/// An image in CPU memory. Rows are `stride` bytes apart (≥ width × bytes
/// per pixel); `color` says what the values mean (spec §6).
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    pub color: ColorInfo,
    pub data: PooledBuf,
}

impl std::fmt::Debug for CpuFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CpuFrame({}x{} {:?} {:?})", self.width, self.height, self.format, self.color.alpha)
    }
}

impl CpuFrame {
    /// A tightly packed RGBA8 frame from `pool` (contents unspecified).
    pub fn rgba8(pool: &FramePool, width: u32, height: u32, color: ColorInfo) -> Self {
        let stride = width as usize * 4;
        CpuFrame { width, height, stride, format: PixelFormat::Rgba8, color, data: pool.take(stride * height as usize) }
    }

    /// A tightly packed RGBA8 frame owning `data` (no pool).
    pub fn from_rgba8(width: u32, height: u32, color: ColorInfo, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), width as usize * height as usize * 4, "RGBA8 data size");
        CpuFrame { width, height, stride: width as usize * 4, format: PixelFormat::Rgba8, color, data: PooledBuf::detached(data) }
    }

    pub fn row(&self, y: u32) -> &[u8] {
        let start = y as usize * self.stride;
        &self.data[start..start + self.width as usize * self.format.bytes_per_pixel()]
    }

    pub fn row_mut(&mut self, y: u32) -> &mut [u8] {
        let start = y as usize * self.stride;
        let len = self.width as usize * self.format.bytes_per_pixel();
        &mut self.data[start..start + len]
    }

    pub fn byte_len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_buffer_is_reused_without_a_new_allocation() {
        let pool = FramePool::new(64 << 20);
        let a = pool.take(1000);
        let ptr = a.as_ptr();
        drop(a);
        assert_eq!(pool.free_bytes(), 1000);
        let b = pool.take(1000);
        assert_eq!(b.as_ptr(), ptr, "the same buffer comes back");
        assert_eq!(pool.allocations(), 1);
        assert_eq!(pool.free_bytes(), 0);
        let c = pool.take(2000);
        assert_eq!(pool.allocations(), 2, "another size is another buffer");
        drop((b, c));
    }

    #[test]
    fn the_pool_keeps_at_most_its_byte_budget() {
        let pool = FramePool::new(1500);
        let (a, b) = (pool.take(1000), pool.take(1000));
        drop(a);
        drop(b);
        assert_eq!(pool.free_bytes(), 1000, "the second buffer would exceed the budget and is released");
    }

    #[test]
    fn buffers_outliving_their_pool_are_simply_freed() {
        let pool = FramePool::new(1 << 20);
        let buf = pool.take(10);
        drop(pool);
        drop(buf);
        let detached = PooledBuf::detached(vec![1, 2, 3]);
        assert_eq!(&detached[..], &[1, 2, 3]);
    }

    #[test]
    fn frames_expose_rows_by_stride() {
        let pool = FramePool::new(1 << 20);
        let mut f = CpuFrame::rgba8(&pool, 3, 2, ColorInfo::WORKING_SDR);
        assert_eq!((f.stride, f.byte_len()), (12, 24));
        f.row_mut(1).copy_from_slice(&[9; 12]);
        assert_eq!(f.row(1), &[9; 12]);
        let g = CpuFrame::from_rgba8(1, 1, ColorInfo::IMAGE_SRGB, vec![1, 2, 3, 4]);
        assert_eq!(g.row(0), &[1, 2, 3, 4]);
    }
}
