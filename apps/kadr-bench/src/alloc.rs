//! Counts frame-sized (≥ 1 MiB) heap allocations, so benchmarks report
//! allocations per frame as a measured number.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub const LARGE: usize = 1 << 20;
static LARGE_ALLOCS: AtomicU64 = AtomicU64::new(0);

pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.size() >= LARGE {
            LARGE_ALLOCS.fetch_add(1, Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if l.size() >= LARGE {
            LARGE_ALLOCS.fetch_add(1, Relaxed);
        }
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        if new_size >= LARGE {
            LARGE_ALLOCS.fetch_add(1, Relaxed);
        }
        unsafe { System.realloc(p, l, new_size) }
    }
}

/// Frame-sized allocations made so far by the whole process.
pub fn large_allocs() -> u64 {
    LARGE_ALLOCS.load(Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_frame_sized_allocations_are_counted() {
        let before = large_allocs();
        let small = std::hint::black_box(vec![1u8; 1000]);
        let big = std::hint::black_box(vec![1u8; 2 << 20]);
        assert_eq!(large_allocs() - before, 1);
        drop((small, big));
    }
}
