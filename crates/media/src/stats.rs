//! Counts buffers that hold a whole frame (render spec §10, §11), per
//! thread, so a pipeline stage can report allocations per frame measured,
//! not assumed.

use std::cell::Cell;

thread_local! {
    static FRAME_ALLOCS: Cell<u64> = const { Cell::new(0) };
}

/// Frame buffers the media layer has allocated on the calling thread.
pub fn frame_allocs_on_this_thread() -> u64 {
    FRAME_ALLOCS.with(|c| c.get())
}

pub(crate) fn note_frame_alloc() {
    FRAME_ALLOCS.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_buffers_are_counted_per_thread() {
        let before = frame_allocs_on_this_thread();
        let _black = crate::RgbaFrame::black(16, 16);
        assert_eq!(frame_allocs_on_this_thread(), before + 1);
        let other = std::thread::spawn(|| {
            let _b = crate::RgbaFrame::black(16, 16);
            frame_allocs_on_this_thread()
        })
        .join()
        .unwrap();
        assert_eq!(other, 1, "another thread's count starts at zero");
        assert_eq!(frame_allocs_on_this_thread(), before + 1, "and does not leak into ours");
    }
}
