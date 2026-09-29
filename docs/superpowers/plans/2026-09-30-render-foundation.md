# Render Foundation (M0–M6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Kadr's two FFmpeg-based composition paths (preview: one FFmpeg process per segment; export: one big `filter_complex`) with one pipeline — Timeline → Scene Evaluator → `FrameScene` → Resolver → CPU Renderer → Preview/Export — that is multi-layer, measured and ready for a later wgpu renderer.

**Architecture:** Pure data scene (`kadr-scene`) produced by a pure evaluator in `kadr-timeline`; renderers in `kadr-render` (CPU first); decoding, caching, cancellation and pacing in `kadr-playback`; FFmpeg only decodes and encodes. Every stage ends with numbers recorded under `docs/perf/`.

**Tech Stack:** Rust 2024, Slint 1.18, FFmpeg CLI (8.x), std threads; `rayon` enters in M2 (its own plan).

**Spec:** `docs/superpowers/specs/2026-09-30-render-architecture-design.md` (approved). Related, not implemented here: `docs/superpowers/specs/2026-09-30-effects-system-design.md`.

## Stage map and exit criteria

Detailed tasks below cover **M0 and M1**. The plans for M2–M6 are written when the previous stage's numbers are recorded (M2's renderer choices depend on M0/M1 measurements). No GPU or effects work before M6 is complete.

| Stage | Delivers | Exit criterion (measurable) |
|---|---|---|
| **M0** Baseline | `kadr_core::perf`, legacy preview instrumentation, MCP `get_perf`, `kadr-bench` (`baseline`, `live`) | `docs/perf/2026-09-30-m0-baseline.md` committed with: seek latency p50/p90 (bench + live), decode fps 1080p/4K H.264/HEVC full & ½, frame-sized allocations and copies per frame, UI-thread copy ms, playback dropped frames, legacy export fps |
| **M1** Scene + evaluator | `kadr_core::color`, probe with color tags + SAR, `kadr-scene`, `kadr_timeline::scene::{evaluate, cull}` | All evaluator tests green incl. equivalence with `video_at` on single-track projects; `evaluate` cost p99 recorded (target < 50 µs for a 3-layer scene) in `docs/perf/…-m1-scene.md` |
| M2 CpuRenderer | `kadr-render` (`Renderer` trait, `PreparedFrame`, `CpuRenderer`), `kadr_core::frame` (pool) | Golden PNG tests: alpha over, multi-layer, transforms/rotation, crop-in-place, opacity, blend modes, dissolve/dip/wipe, color reference (spec §6 list); bench: 1080p and 4K render ms for 1/2/3 layers; **0 frame-sized allocations per frame after warm-up** |
| M3 Playback | `kadr-playback`: decoder sessions (source frames, explicit YUV matrix/range), frame cache, generations/cancellation, prefetch, `PreviewPlayer`, telemetry | Seek latency p50/p90 vs M0; scrub test proves stale requests are dropped; 0 frame-sized allocations per decoded frame in steady state; simulated-clock A/V test (≤ 1 frame drift over 10 min) |
| M4 Preview switch | Preview on the new pipeline behind `KADR_RENDERER=legacy\|cpu` (default `cpu` after acceptance), DEV overlay | MCP acceptance: PiP, transitions and clip look visible in preview; dropped frames ≤ 1 % over 60 s for 1080p 3 layers at ½ and 1080p 1 layer at Full; UI-thread frame copies 0; numbers vs M0 |
| M5 Export switch | `ExportRunner` → rawvideo stdin encoder; audio still via the FFmpeg audio graph | PSNR ≥ 40 dB vs legacy export on a single-track reference; A/V sync test (20 min, ≥ 50 cuts, ≥ 10 transitions) within ±1 frame; export fps 1080p/4K vs M0 |
| M6 Remove legacy | Delete `look_filter`, video part of `build_graph`, `StreamRequest.look`, `video_at`-driven preview | Full test suite green; no perf regression > 10 % vs M4/M5 numbers |

## Global Constraints

- Rust edition 2024, `rust-version = 1.88`; workspace dependencies only; **no new external crates in M0–M1**.
- `kadr-core` stays std + `serde` + `uuid`; `kadr-scene` depends only on `kadr-core` (data + pure geometry, no pixels, no I/O).
- Internal time is `kadr_core::Time` (i64 flicks) and integer frame numbers via `FrameRate`; no accumulation of f32/f64 seconds (spec §8). A single f64 ratio for a transition's progress is allowed.
- Coordinates are canvas pixels (sequence pixels, square); normalized UV only for sampling; crop is a rectangle in local layer space and never re-centres the remaining image (spec §5).
- Colour metadata is explicit everywhere a source or output is described (spec §6); untagged video: width ≥ 1280 or height > 576 → BT.709; height 576 → BT.601/625; otherwise BT.601/525 (Ruling R7); limited range; video alpha from the pixel format and the `alpha_mode` tag (Ruling R10).
- Tests never touch the user's running `kadr.exe` or `%LOCALAPPDATA%\Kadr`: headless runs use `KADR_DATA_DIR` in a temp dir.
- Before rebuilding the editor in `target/debug`: `mv target/debug/kadr.exe target/debug/kadr-old-$RANDOM.exe 2>/dev/null` (the user may be running it); delete `kadr-old-*` at the end of the task.
- Every user-visible string goes through `kadr_i18n` with RU and EN (none are expected in M0–M1; MCP output is English JSON).
- Commits end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`; branch `feature/render-foundation`.

## Review Focus

1. A transition on a clip whose media has nothing before its in-point (source starts at 0) → the incoming layer's source time is clamped to 0, never negative. → Task 11 test `transition_source_times_extend_past_the_clip_and_clamp_at_zero`.
2. Phone video with rotation metadata and anamorphic (SAR ≠ 1) video → placed by *display* size, not by coded size. → Task 8 test `display_size_applies_sar_then_rotation`, Task 10 test `rotated_and_anamorphic_sources_fit_by_display_size`.
3. A clip shorter than its transition / two transition windows overlapping on one track → deterministic choice (earlier cut), progress in [0, 1]. → Task 11 test `overlapping_windows_pick_the_earlier_cut`.
4. Playhead outside the sequence, in a gap, or an empty project → a valid empty (black) scene, no panic. → Task 10 test `outside_the_sequence_or_in_a_gap_the_scene_is_empty_black`.
5. A PNG logo with transparency covering the whole frame must not hide the video underneath (culling must not treat images as opaque). → Task 12 test `png_logo_with_alpha_never_occludes`.

---

## M0 — Baseline measurements

### Task 1: `kadr_core::perf` — frame telemetry types

**Files:**
- Create: `crates/core/src/perf.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Produces: `kadr_core::perf::{FramePerf, LayerTiming, Stats, PerfSummary, PerfRing}` with `PerfRing::{new(usize), push(FramePerf), snapshot() -> Vec<FramePerf>, clear(), summary() -> PerfSummary}`, `Stats::of(Vec<Duration>) -> Stats`, `FramePerf::decode_total() -> Duration`.

- [ ] **Step 1: Write the failing tests** — create `crates/core/src/perf.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn stats_use_nearest_rank_percentiles() {
        let s = Stats::of((1..=100).map(ms).collect());
        assert_eq!((s.count, s.p50, s.p90, s.p99, s.max), (100, ms(50), ms(90), ms(99), ms(100)));
        assert_eq!(Stats::of(vec![]), Stats::default());
        let one = Stats::of(vec![ms(7)]);
        assert_eq!((one.p50, one.p99, one.max), (ms(7), ms(7), ms(7)));
    }

    #[test]
    fn ring_keeps_the_newest_frames() {
        let ring = PerfRing::new(3);
        for i in 1..=5 {
            ring.push(FramePerf { total: ms(i), ..Default::default() });
        }
        let totals: Vec<Duration> = ring.snapshot().iter().map(|f| f.total).collect();
        assert_eq!(totals, vec![ms(3), ms(4), ms(5)]);
        ring.clear();
        assert!(ring.snapshot().is_empty());
    }

    #[test]
    fn summary_separates_dropped_frames_and_averages_counters() {
        let ring = PerfRing::new(10);
        ring.push(FramePerf {
            total: ms(10),
            decode: vec![LayerTiming { layer: 1, time: ms(3) }, LayerTiming { layer: 2, time: ms(1) }],
            present: ms(2),
            frame_allocs: 1,
            frame_copies: 2,
            bytes_copied: 100,
            ..Default::default()
        });
        ring.push(FramePerf { decode: vec![LayerTiming { layer: 1, time: ms(6) }], dropped: true, frame_allocs: 1, ..Default::default() });
        ring.push(FramePerf { total: ms(30), seek_latency: Some(ms(30)), ..Default::default() });
        let s = ring.summary();
        assert_eq!((s.frames, s.dropped), (3, 1));
        assert_eq!(s.total.count, 2, "dropped frames are not in frame-time stats");
        assert_eq!(s.decode.count, 3, "decode work counts even when the frame is dropped");
        assert_eq!(s.decode.max, ms(6));
        assert_eq!((s.seek.count, s.seek.p50), (1, ms(30)));
        assert!((s.frame_allocs_per_frame - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.frame_copies_per_frame - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.bytes_copied_per_frame - 100.0 / 3.0).abs() < 1e-9);
    }
}
```

Add `pub mod perf;` to `crates/core/src/lib.rs` (after `pub mod media_info;`).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-core perf`
Expected: compile errors — `Stats`, `PerfRing`, `FramePerf`, `LayerTiming` not found.

- [ ] **Step 3: Implement** — put this above the test module in `crates/core/src/perf.rs`:

```rust
//! Frame-level performance telemetry (render spec §10): what producing one
//! displayed or encoded frame cost, stage by stage. Collected by the
//! playback layer; read by the DEV overlay, MCP `get_perf` and benchmarks.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// Decode wait attributed to one layer. `layer` is an opaque key (e.g. the
/// low 64 bits of a clip id); 0 when there is a single source.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayerTiming {
    pub layer: u64,
    pub time: Duration,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FramePerf {
    pub total: Duration,
    pub decode: Vec<LayerTiming>,
    pub evaluate: Duration,
    pub resolve: Duration,
    pub upload: Duration,
    pub composite: Duration,
    pub effects: Duration,
    pub present: Duration,
    pub cache_hits: u32,
    pub cache_misses: u32,
    /// Produced but never shown (late, or for a request that was superseded).
    pub dropped: bool,
    /// Request → first frame shown, for a seek or scrub.
    pub seek_latency: Option<Duration>,
    /// Buffers holding a whole frame allocated for this frame.
    pub frame_allocs: u32,
    /// Whole-frame memory copies made for this frame, and their bytes.
    pub frame_copies: u32,
    pub bytes_copied: u64,
}

impl FramePerf {
    pub fn decode_total(&self) -> Duration {
        self.decode.iter().map(|d| d.time).sum()
    }
}

/// Nearest-rank percentiles of a set of durations.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub count: usize,
    pub p50: Duration,
    pub p90: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Stats {
    pub fn of(mut v: Vec<Duration>) -> Stats {
        if v.is_empty() {
            return Stats::default();
        }
        v.sort_unstable();
        let n = v.len();
        let rank = |p: f64| v[((p * n as f64).ceil() as usize).clamp(1, n) - 1];
        Stats { count: n, p50: rank(0.50), p90: rank(0.90), p99: rank(0.99), max: v[n - 1] }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerfSummary {
    pub frames: usize,
    pub dropped: usize,
    /// Shown frames only.
    pub total: Stats,
    /// All frames, dropped included: decoding them was paid for.
    pub decode: Stats,
    pub composite: Stats,
    pub present: Stats,
    pub seek: Stats,
    pub frame_allocs_per_frame: f64,
    pub frame_copies_per_frame: f64,
    pub bytes_copied_per_frame: f64,
}

/// The last `capacity` frames; safe to push from any thread.
pub struct PerfRing {
    capacity: usize,
    frames: Mutex<VecDeque<FramePerf>>,
}

impl PerfRing {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        PerfRing { capacity, frames: Mutex::new(VecDeque::with_capacity(capacity)) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<FramePerf>> {
        self.frames.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, f: FramePerf) {
        let mut q = self.lock();
        if q.len() == self.capacity {
            q.pop_front();
        }
        q.push_back(f);
    }

    pub fn snapshot(&self) -> Vec<FramePerf> {
        self.lock().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    pub fn summary(&self) -> PerfSummary {
        let frames = self.snapshot();
        let n = frames.len();
        let per = |sum: f64| if n == 0 { 0.0 } else { sum / n as f64 };
        let shown = || frames.iter().filter(|f| !f.dropped);
        PerfSummary {
            frames: n,
            dropped: frames.iter().filter(|f| f.dropped).count(),
            total: Stats::of(shown().map(|f| f.total).collect()),
            decode: Stats::of(frames.iter().map(FramePerf::decode_total).collect()),
            composite: Stats::of(shown().map(|f| f.composite).collect()),
            present: Stats::of(shown().map(|f| f.present).collect()),
            seek: Stats::of(frames.iter().filter_map(|f| f.seek_latency).collect()),
            frame_allocs_per_frame: per(frames.iter().map(|f| f.frame_allocs as f64).sum()),
            frame_copies_per_frame: per(frames.iter().map(|f| f.frame_copies as f64).sum()),
            bytes_copied_per_frame: per(frames.iter().map(|f| f.bytes_copied as f64).sum()),
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p kadr-core perf`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/perf.rs crates/core/src/lib.rs
git commit -m "core: frame telemetry types (FramePerf, PerfRing, Stats) for the render foundation

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 2: Count frame buffers in `kadr-media`; instrument the legacy preview

**Files:**
- Create: `crates/media/src/stats.rs`
- Modify: `crates/media/src/lib.rs` (module + `RgbaFrame::black`), `crates/media/src/ffmpeg/frames.rs` (`FfmpegStream::next_frame`, `parse_pam`)
- Modify: `apps/editor/src/preview.rs`

**Interfaces:**
- Consumes: `kadr_core::perf::{FramePerf, LayerTiming, PerfRing}` (Task 1).
- Produces: `kadr_media::stats::frame_allocs_on_this_thread() -> u64`; `PreviewController.perf: Arc<PerfRing>` (capacity 600) and `PreviewController.seek_started: Option<(u64, Instant)>`; `App::on_preview_frame(generation, frame, show_loading_done, perf: FramePerf)`.

- [ ] **Step 1: Write the failing test** — create `crates/media/src/stats.rs` with only its test, and add `pub mod stats;` to `crates/media/src/lib.rs` (next to `pub mod export;`):

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-media stats`
Expected: compile error — `frame_allocs_on_this_thread` not found.

- [ ] **Step 3: Implement** — above the test in `crates/media/src/stats.rs`:

```rust
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
```

In `crates/media/src/lib.rs`, `RgbaFrame::black`:

```rust
    pub fn black(width: u32, height: u32) -> Self {
        let mut data = vec![0u8; (width * height * 4) as usize];
        stats::note_frame_alloc();
        data.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        RgbaFrame { width, height, data }
    }
```

In `crates/media/src/ffmpeg/frames.rs`, `FfmpegStream::next_frame` — right after `let mut data = vec![0u8; …];` add `crate::stats::note_frame_alloc();`. In `parse_pam`, replace the final line with:

```rust
    crate::stats::note_frame_alloc();
    Some(RgbaFrame { width: w, height: h, data: buf[hdr_end..hdr_end + len].to_vec() })
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p kadr-media stats`
Expected: 1 passed.

- [ ] **Step 5: Instrument the legacy preview** (`apps/editor/src/preview.rs`). This is measurement of code that M6 deletes; it has no unit test — Task 5's live run proves the numbers are non-zero and sane.

Add imports at the top: `use kadr_core::perf::{FramePerf, LayerTiming, PerfRing};` and `use std::time::Instant;`.

`PreviewController` gains two fields and the worker gets the ring:

```rust
pub struct PreviewController {
    tx: Sender<Cmd>,
    generation: Arc<AtomicU64>,
    pub quality: i32,
    /// Last 600 preview frames (MCP `get_perf`, DEV overlay later).
    pub perf: Arc<PerfRing>,
    /// Generation and start time of the pending paused-frame request.
    pub seek_started: Option<(u64, Instant)>,
}
```

```rust
    pub fn new(media: Option<Arc<dyn MediaBackend>>, clock: AudioClock) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let perf = Arc::new(PerfRing::new(600));
        if let Some(m) = media {
            let g = generation.clone();
            let p = perf.clone();
            std::thread::Builder::new().name("kadr-preview".into()).spawn(move || worker(m, rx, clock, g, p)).expect("preview thread");
        }
        PreviewController { tx, generation, quality: 1, perf, seek_started: None }
    }
```

In `App::request_frame`, right after `let generation = self.preview.next_gen();` add:

```rust
        self.preview.seek_started = Some((generation, Instant::now()));
```

Replace `on_preview_frame` with:

```rust
    pub fn on_preview_frame(&mut self, generation: u64, frame: RgbaFrame, show_loading_done: bool, mut perf: FramePerf) {
        if generation != self.preview.current_gen() {
            // Decoded for a request nobody wants any more: wasted work.
            perf.dropped = true;
            perf.total = perf.decode_total();
            self.preview.perf.push(perf);
            return;
        }
        let ui = self.ui();
        let shown = Instant::now();
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&frame.data, frame.width, frame.height);
        // clone_from_slice allocates a new frame buffer and copies into it, on the UI thread.
        perf.frame_allocs += 1;
        perf.frame_copies += 1;
        perf.bytes_copied += frame.data.len() as u64;
        ui.set_preview_frame(slint::Image::from_rgba8(buf));
        ui.set_preview_has_frame(true);
        perf.present = shown.elapsed();
        if show_loading_done {
            ui.set_preview_loading(false);
            if let Some((_, asked)) = self.preview.seek_started.take_if(|(g, _)| *g == generation) {
                perf.seek_latency = Some(asked.elapsed());
            }
        }
        perf.total = perf.decode_total() + perf.present;
        self.preview.perf.push(perf);
    }
```

`worker` takes `perf: Arc<PerfRing>` as its last parameter; its `Show` and `Play` arms become:

```rust
            Cmd::Show { generation, t, seg, w, h, rate, px_scale } => {
                let allocs = kadr_media::stats::frame_allocs_on_this_thread();
                let started = Instant::now();
                let frame = decode_one(&*media, t, &seg, w, h, rate, px_scale);
                let pf = FramePerf {
                    decode: vec![LayerTiming { layer: 0, time: started.elapsed() }],
                    frame_allocs: (kadr_media::stats::frame_allocs_on_this_thread() - allocs) as u32,
                    ..Default::default()
                };
                if let Some(f) = frame {
                    post(move |app| app.on_preview_frame(generation, f, true, pf));
                } else {
                    post(move |app| {
                        if generation == app.preview.current_gen() {
                            app.ui().set_preview_loading(false);
                        }
                    });
                }
            }
            Cmd::Play { generation, from, segs, w, h, rate, px_scale } => {
                pending = play(&*media, &rx, &clock, &current, &perf, generation, from, &segs, w, h, rate, px_scale);
            }
```

`play` gains `perf: &PerfRing` right after `current: &AtomicU64`. Its gap branch posts a measured black frame:

```rust
            None => {
                let allocs = kadr_media::stats::frame_allocs_on_this_thread();
                let black = RgbaFrame::black(w, h);
                let pf = FramePerf { frame_allocs: (kadr_media::stats::frame_allocs_on_this_thread() - allocs) as u32, ..Default::default() };
                post(move |app| app.on_preview_frame(generation, black, true, pf));
```

(the rest of the gap branch is unchanged). In the frame loop, replace the lines from `let frame = match stream.next_frame() {` through the final `post(...)` with:

```rust
                    let allocs = kadr_media::stats::frame_allocs_on_this_thread();
                    let started = Instant::now();
                    let frame = match stream.next_frame() {
                        Ok(Some(f)) => f,
                        _ => break,
                    };
                    let mut pf = FramePerf {
                        decode: vec![LayerTiming { layer: 0, time: started.elapsed() }],
                        frame_allocs: (kadr_media::stats::frame_allocs_on_this_thread() - allocs) as u32,
                        ..Default::default()
                    };
                    i += 1;
                    // Pace against the audio clock.
                    loop {
                        match now() {
                            Some(t) if t + Time::from_millis(4) >= ts => break,
                            None => return None,
                            _ => std::thread::sleep(Duration::from_millis(2)),
                        }
                    }
                    let late = now().is_some_and(|t| t > ts + Time(fd.flicks() * 2));
                    if late {
                        pf.dropped = true;
                        pf.total = pf.decode_total();
                        perf.push(pf);
                    } else {
                        post(move |app| app.on_preview_frame(generation, frame, false, pf));
                    }
```

- [ ] **Step 6: Build and run the editor's tests**

Run: `mv target/debug/kadr.exe target/debug/kadr-old-$RANDOM.exe 2>/dev/null; cargo build -p kadr-editor 2>&1 | grep -E "^(warning|error)" ; cargo test -p kadr-editor -p kadr-media 2>&1 | grep -E "test result|FAILED"`
Expected: no warnings or errors; all test results `ok`.

- [ ] **Step 7: Commit**

```bash
git add crates/media/src/stats.rs crates/media/src/lib.rs crates/media/src/ffmpeg/frames.rs apps/editor/src/preview.rs
git commit -m "media: count frame buffers per thread; preview: record decode, present, copies, allocations, drops and seek latency per frame

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
rm -f target/debug/kadr-old-*.exe
```

### Task 3: MCP `get_perf`

**Files:**
- Create: `apps/editor/src/perf_view.rs`
- Modify: `apps/editor/src/main.rs` (`mod perf_view;`), `apps/editor/src/mcp_api.rs` (`handle`), `apps/kadr-mcp/src/tools.rs`
- Test: `apps/kadr-mcp/tests/tools.rs`

**Interfaces:**
- Consumes: `PerfRing::summary/clear`, `PreviewController.perf` (Tasks 1–2).
- Produces: MCP tool `get_perf { reset?: bool }` returning `{frames, dropped, total_ms, decode_ms, composite_ms, present_ms, seek_ms (each {count,p50,p90,p99,max}), frame_allocs_per_frame, frame_copies_per_frame, mb_copied_per_frame}`; `perf_view::summary_json(&PerfSummary) -> serde_json::Value`.

- [ ] **Step 1: Write the failing tests**

`apps/editor/src/perf_view.rs` (test only for now) and `mod perf_view;` in `apps/editor/src/main.rs` (after `mod mcp_input;`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::perf::{FramePerf, PerfRing};
    use std::time::Duration;

    #[test]
    fn summary_is_reported_in_milliseconds_with_per_frame_counters() {
        let ring = PerfRing::new(10);
        ring.push(FramePerf { total: Duration::from_micros(12_345), present: Duration::from_millis(2), frame_copies: 1, bytes_copied: 2 * 1_048_576, ..Default::default() });
        ring.push(FramePerf { dropped: true, ..Default::default() });
        let v = summary_json(&ring.summary());
        assert_eq!(v["frames"], 2);
        assert_eq!(v["dropped"], 1);
        assert_eq!(v["total_ms"]["p50"], 12.345);
        assert_eq!(v["present_ms"]["max"], 2.0);
        assert_eq!(v["frame_copies_per_frame"], 0.5);
        assert_eq!(v["mb_copied_per_frame"], 1.0);
        assert_eq!(v["seek_ms"]["count"], 0);
    }
}
```

Append to `apps/kadr-mcp/tests/tools.rs`:

```rust
#[test]
fn get_perf_reports_and_can_reset() {
    let t = tool("get_perf");
    assert_eq!(prop("get_perf", "reset")["type"], "boolean");
    let d = t["description"].as_str().unwrap();
    for word in ["dropped", "seek", "reset"] {
        assert!(d.contains(word), "get_perf description lacks {word}: {d}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-editor perf_view; cargo test -p kadr-mcp --test tools get_perf`
Expected: compile error `summary_json` not found; the kadr-mcp test panics `no tool get_perf`.

- [ ] **Step 3: Implement**

`apps/editor/src/perf_view.rs`, above the tests:

```rust
//! JSON view of preview telemetry for MCP `get_perf` (render spec §10).

use kadr_core::perf::{PerfSummary, Stats};
use serde_json::{json, Value};
use std::time::Duration;

/// Milliseconds with microsecond precision.
fn ms(d: Duration) -> f64 {
    d.as_micros() as f64 / 1000.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn stats(s: &Stats) -> Value {
    json!({"count": s.count, "p50": ms(s.p50), "p90": ms(s.p90), "p99": ms(s.p99), "max": ms(s.max)})
}

pub fn summary_json(s: &PerfSummary) -> Value {
    json!({
        "frames": s.frames,
        "dropped": s.dropped,
        "total_ms": stats(&s.total),
        "decode_ms": stats(&s.decode),
        "composite_ms": stats(&s.composite),
        "present_ms": stats(&s.present),
        "seek_ms": stats(&s.seek),
        "frame_allocs_per_frame": round3(s.frame_allocs_per_frame),
        "frame_copies_per_frame": round3(s.frame_copies_per_frame),
        "mb_copied_per_frame": round3(s.bytes_copied_per_frame / 1_048_576.0),
    })
}
```

In `apps/editor/src/mcp_api.rs`, `handle`, add an arm before `"get_frame"`:

```rust
        "get_perf" => {
            let v = crate::perf_view::summary_json(&app.preview.perf.summary());
            if p.get("reset").and_then(Value::as_bool) == Some(true) {
                app.preview.perf.clear();
            }
            Reply::Now(Ok(v))
        }
```

In `apps/kadr-mcp/src/tools.rs`, add to the `kadr_tools()` vector (after `get_frame`):

```rust
        tool(
            "get_perf",
            "Preview performance over the last frames (up to 600): percentiles (ms) of frame time, decode, composite and present; dropped frames (late or superseded); seek latency from a playhead change to the frame shown; frame-sized allocations and copies per frame. `reset: true` clears the window after reading, so the next read measures only what happens afterwards.",
            json!({"reset": {"type": "boolean", "description": "Clear the window after reading."}}),
            &[],
        ),
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-editor perf_view; cargo test -p kadr-mcp`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add apps/editor/src/perf_view.rs apps/editor/src/main.rs apps/editor/src/mcp_api.rs apps/kadr-mcp/src/tools.rs apps/kadr-mcp/tests/tools.rs
git commit -m "mcp: get_perf — preview frame-time, decode, seek, drop and copy statistics

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 4: `kadr-bench` — counting allocator, test media, `baseline` scenarios

**Files:**
- Create: `apps/kadr-bench/Cargo.toml`, `apps/kadr-bench/src/main.rs`, `src/alloc.rs`, `src/media.rs`, `src/report.rs`, `src/baseline.rs`

**Interfaces:**
- Consumes: `kadr_core::perf::Stats` (Task 1); legacy `kadr_media::{MediaBackend, StreamRequest, ExportPlan, …}`.
- Produces: binary `kadr-bench` with subcommand `baseline [--quick]`; `report::{Report, Row, markdown, save}`; `baseline::seek_times(secs: u32, n: usize) -> Vec<Time>`; `media::{TestClip, ensure, bench_dir}`; `alloc::large_allocs() -> u64` (allocations ≥ 1 MiB).

- [ ] **Step 1: Create the crate skeleton with failing tests**

`apps/kadr-bench/Cargo.toml`:

```toml
[package]
name = "kadr-bench"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Kadr performance harness (not shipped)"
publish = false

[[bin]]
name = "kadr-bench"
path = "src/main.rs"

[dependencies]
kadr-core = { path = "../../crates/core" }
kadr-media = { path = "../../crates/media" }
serde_json.workspace = true
```

`apps/kadr-bench/src/main.rs`:

```rust
//! Kadr performance harness (render spec §10): measures instead of guessing.
//! Not shipped.
//!
//!   kadr-bench baseline [--quick]                  legacy decode, seek, copy and export numbers
//!   kadr-bench live --clip <file> [--seconds N]    the real app, headless, through kadr-mcp

mod alloc;
mod baseline;
mod media;
mod report;

#[global_allocator]
static ALLOC: alloc::Counting = alloc::Counting;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let result = match args.first().map(String::as_str) {
        Some("baseline") => baseline::run(flag("--quick")),
        _ => Err("usage: kadr-bench baseline [--quick] | live --clip <file> [--seconds N]".to_string()),
    };
    match result {
        Ok(r) => {
            println!("{}", report::markdown(&r));
            match report::save(&r) {
                Ok(p) => println!("saved {}", p.display()),
                Err(e) => eprintln!("kadr-bench: could not save report: {e}"),
            }
        }
        Err(e) => {
            eprintln!("kadr-bench: {e}");
            std::process::exit(1);
        }
    }
}
```

`apps/kadr-bench/src/alloc.rs` (tests only for now):

```rust
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
```

`apps/kadr-bench/src/report.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::perf::Stats;
    use std::time::Duration;

    #[test]
    fn stats_become_rows_and_rows_a_markdown_table() {
        let mut r = Report::new("t");
        r.push("decode", "h264_1080p30 full", "fps", 123.456, "fps");
        r.stats("seek", "h264_1080p30 half", &Stats::of(vec![Duration::from_millis(10), Duration::from_millis(30)]));
        assert_eq!(r.rows.len(), 5, "1 value + count, p50, p90, max");
        let md = markdown(&r);
        assert!(md.starts_with("| scenario | case | metric | value | unit |"), "{md}");
        assert!(md.contains("| decode | h264_1080p30 full | fps | 123.46 | fps |"), "{md}");
        assert!(md.contains("| seek | h264_1080p30 half | p90 | 30.00 | ms |"), "{md}");
    }
}
```

`apps/kadr-bench/src/baseline.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_times_are_deterministic_and_inside_the_clip() {
        let a = seek_times(60, 15);
        assert_eq!(a, seek_times(60, 15));
        assert_eq!(a.len(), 15);
        assert!(a.iter().all(|t| *t >= Time::ZERO && *t < Time::from_secs(58)), "{a:?}");
        assert!(a.windows(2).any(|w| w[0] != w[1]));
    }
}
```

`apps/kadr-bench/src/media.rs` — create empty for now (content in Step 3).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-bench`
Expected: compile errors (`large_allocs`, `Report`, `seek_times`, `alloc::Counting` not found).

- [ ] **Step 3: Implement**

`apps/kadr-bench/src/alloc.rs`, above the tests:

```rust
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
```

`apps/kadr-bench/src/report.rs`, above the tests:

```rust
//! Benchmark results: rows printed as a markdown table and saved as JSON.

use kadr_core::perf::Stats;
use serde_json::json;
use std::path::PathBuf;

pub struct Row {
    pub scenario: String,
    pub case: String,
    pub metric: String,
    pub value: f64,
    pub unit: &'static str,
}

pub struct Report {
    pub title: String,
    pub rows: Vec<Row>,
}

impl Report {
    pub fn new(title: &str) -> Self {
        Report { title: title.to_string(), rows: vec![] }
    }

    pub fn push(&mut self, scenario: &str, case: &str, metric: &str, value: f64, unit: &'static str) {
        self.rows.push(Row { scenario: scenario.into(), case: case.into(), metric: metric.into(), value, unit });
    }

    /// count, p50, p90 and max (ms) of a duration set.
    pub fn stats(&mut self, scenario: &str, case: &str, s: &Stats) {
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        self.push(scenario, case, "count", s.count as f64, "n");
        self.push(scenario, case, "p50", ms(s.p50), "ms");
        self.push(scenario, case, "p90", ms(s.p90), "ms");
        self.push(scenario, case, "max", ms(s.max), "ms");
    }
}

pub fn markdown(r: &Report) -> String {
    let mut s = String::from("| scenario | case | metric | value | unit |\n|---|---|---|---|---|\n");
    for row in &r.rows {
        s.push_str(&format!("| {} | {} | {} | {:.2} | {} |\n", row.scenario, row.case, row.metric, row.value, row.unit));
    }
    s
}

/// Writes `<exe dir>/bench/<title>-<unix seconds>.json`.
pub fn save(r: &Report) -> std::io::Result<PathBuf> {
    let dir = std::env::current_exe()?.parent().map(|d| d.join("bench")).unwrap_or_else(|| PathBuf::from("bench"));
    std::fs::create_dir_all(&dir)?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("{}-{secs}.json", r.title));
    let rows: Vec<_> = r.rows.iter().map(|x| json!({"scenario": x.scenario, "case": x.case, "metric": x.metric, "value": x.value, "unit": x.unit})).collect();
    std::fs::write(&path, serde_json::to_vec_pretty(&json!({"title": r.title, "rows": rows}))?)?;
    Ok(path)
}
```

`apps/kadr-bench/src/media.rs`:

```rust
//! Synthetic test media, generated once with FFmpeg (lavfi) and cached.
//! Long-GOP (2 s) like camera footage, tagged BT.709.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TestClip {
    pub name: &'static str,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub secs: u32,
}

struct Spec {
    name: &'static str,
    width: u32,
    height: u32,
    secs: u32,
    video_args: &'static [&'static str],
}

const X264: &[&str] = &["-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-pix_fmt", "yuv420p"];
const X265: &[&str] = &["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error", "-g", "60", "-pix_fmt", "yuv420p", "-tag:v", "hvc1"];

const SPECS: &[Spec] = &[
    Spec { name: "h264_1080p30", width: 1920, height: 1080, secs: 60, video_args: X264 },
    Spec { name: "h264_2160p30", width: 3840, height: 2160, secs: 20, video_args: X264 },
    Spec { name: "hevc_2160p30", width: 3840, height: 2160, secs: 20, video_args: X265 },
];

/// `<exe dir>/bench-media` (e.g. `target/release/bench-media`).
pub fn bench_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("bench-media"))).unwrap_or_else(|| PathBuf::from("bench-media"))
}

/// Generates the missing clips in `dir` and returns all of them.
pub fn ensure(dir: &Path) -> io::Result<Vec<TestClip>> {
    std::fs::create_dir_all(dir)?;
    SPECS
        .iter()
        .map(|s| {
            let path = dir.join(format!("{}.mp4", s.name));
            if !path.exists() {
                eprintln!("kadr-bench: generating {} …", path.display());
                generate(s, &path)?;
            }
            Ok(TestClip { name: s.name, path, width: s.width, height: s.height, secs: s.secs })
        })
        .collect()
}

fn generate(s: &Spec, path: &Path) -> io::Result<()> {
    let part = path.with_extension("part");
    let src = format!("testsrc2=size={}x{}:rate=30:duration={}", s.width, s.height, s.secs);
    let tone = format!("sine=frequency=440:sample_rate=48000:duration={}", s.secs);
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-f", "lavfi", "-i", &tone])
        .args(s.video_args)
        .args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-c:a", "aac", "-shortest", "-f", "mp4"])
        .arg(&part)
        .status()?;
    if !st.success() {
        return Err(io::Error::other(format!("ffmpeg failed to generate {}", s.name)));
    }
    std::fs::rename(&part, path)
}
```

`apps/kadr-bench/src/baseline.rs`, above the tests:

```rust
//! M0 baseline of the legacy pipeline: seek latency, sequential decode
//! throughput and allocations, frame buffer cost, legacy export speed.

use crate::media::{self, TestClip};
use crate::report::Report;
use crate::alloc;
use kadr_core::perf::Stats;
use kadr_core::{CancelToken, FrameRate, Time};
use kadr_media::export::{ExportVideoSource, VideoLook};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::{ExportPlan, ExportSettings, ExportTransition, ExportTransitionKind, ExportVideo, MediaBackend, StreamRequest};
use std::path::Path;
use std::time::Instant;

pub fn run(quick: bool) -> Result<Report, String> {
    let ff = FfmpegCli::locate().map_err(|e| e.to_string())?;
    let dir = media::bench_dir();
    let clips = media::ensure(&dir).map_err(|e| e.to_string())?;
    let (seeks, frames, reps) = if quick { (5, 60, 5) } else { (15, 150, 30) };
    let mut r = Report::new("m0-baseline");
    for clip in &clips {
        for (label, w, h) in [("full", clip.width, clip.height), ("half", clip.width / 2, clip.height / 2)] {
            let case = format!("{} {label}", clip.name);
            r.stats("seek", &case, &seek_latency(&ff, clip, w, h, seeks)?);
            let (fps, allocs) = decode_throughput(&ff, clip, w, h, frames)?;
            r.push("decode", &case, "fps", fps, "fps");
            r.push("decode", &case, "large_allocs_per_frame", allocs, "n");
        }
    }
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let (alloc_ms, copy_ms) = frame_memory(w, h, reps);
        let case = format!("{w}x{h} rgba");
        r.push("frame_buffer", &case, "alloc_and_first_touch", alloc_ms, "ms");
        r.push("frame_buffer", &case, "copy", copy_ms, "ms");
    }
    let clip1080 = clips.iter().find(|c| c.name == "h264_1080p30").ok_or("no 1080p clip")?;
    r.push("export_legacy", "1080p30 20 s, 10 cuts, 2 dissolves, Balanced", "fps", legacy_export(&ff, clip1080, &dir)?, "fps");
    Ok(r)
}

fn request(clip: &TestClip, start: Time, w: u32, h: u32) -> StreamRequest {
    StreamRequest { path: clip.path.clone(), start, width: w, height: h, rate: FrameRate::FPS_30, speed: 1.0, look: VideoLook::default(), px_scale: 1.0 }
}

/// Deterministic pseudo-random times in [0, secs − 2 s).
pub fn seek_times(secs: u32, n: usize) -> Vec<Time> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let span = (secs.saturating_sub(2) as u64).max(1) * 1000;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Time::from_millis((x % span) as i64)
        })
        .collect()
}

/// Request → first decoded frame for random positions (legacy: a new FFmpeg process each time).
fn seek_latency(ff: &FfmpegCli, clip: &TestClip, w: u32, h: u32, n: usize) -> Result<Stats, String> {
    let mut v = vec![];
    for t in seek_times(clip.secs, n) {
        let started = Instant::now();
        let mut s = ff.open_stream(&request(clip, t, w, h)).map_err(|e| e.to_string())?;
        s.next_frame().map_err(|e| e.to_string())?.ok_or("no frame after seek")?;
        v.push(started.elapsed());
    }
    Ok(Stats::of(v))
}

/// Frames per second of sequential decoding, and frame-sized allocations per frame.
fn decode_throughput(ff: &FfmpegCli, clip: &TestClip, w: u32, h: u32, frames: usize) -> Result<(f64, f64), String> {
    let mut s = ff.open_stream(&request(clip, Time::ZERO, w, h)).map_err(|e| e.to_string())?;
    s.next_frame().map_err(|e| e.to_string())?; // process start is measured by `seek`
    let allocs = alloc::large_allocs();
    let started = Instant::now();
    let mut n = 0usize;
    while n < frames {
        match s.next_frame().map_err(|e| e.to_string())? {
            Some(_) => n += 1,
            None => break,
        }
    }
    let secs = started.elapsed().as_secs_f64().max(1e-9);
    Ok((n as f64 / secs, (alloc::large_allocs() - allocs) as f64 / n.max(1) as f64))
}

/// Cost of a fresh frame buffer (allocation + first touch of every page) and of a full-frame copy.
fn frame_memory(w: u32, h: u32, reps: usize) -> (f64, f64) {
    let n = (w * h * 4) as usize;
    let started = Instant::now();
    for _ in 0..reps {
        let mut v = vec![0u8; n];
        for i in (0..n).step_by(4096) {
            v[i] = 1;
        }
        std::hint::black_box(v);
    }
    let alloc_ms = started.elapsed().as_secs_f64() * 1000.0 / reps as f64;
    let src = vec![7u8; n];
    let mut dst = vec![0u8; n];
    let started = Instant::now();
    for _ in 0..reps {
        dst.copy_from_slice(std::hint::black_box(&src));
        std::hint::black_box(&mut dst);
    }
    (alloc_ms, started.elapsed().as_secs_f64() * 1000.0 / reps as f64)
}

/// Legacy FFmpeg-graph export of a 20 s, 10-segment, 2-dissolve 1080p30 timeline.
fn legacy_export(ff: &FfmpegCli, clip: &TestClip, dir: &Path) -> Result<f64, String> {
    let output = dir.join("export-legacy.mp4");
    let dissolve = ExportTransition { kind: ExportTransitionKind::Dissolve, duration: Time::from_millis(500) };
    let video = (0..10)
        .map(|i| ExportVideo {
            duration: Time::from_secs(2),
            source: Some(ExportVideoSource { path: clip.path.clone(), source_start: Time::from_secs(i * 5), speed: 1.0, look: VideoLook::default() }),
            transition_in: (i == 3 || i == 7).then(|| dissolve.clone()),
        })
        .collect();
    let plan = ExportPlan {
        output: output.clone(),
        total: Time::from_secs(20),
        video,
        audio: vec![],
        settings: ExportSettings { width: 1920, height: 1080, rate: FrameRate::FPS_30, crf: 21, preset: "fast".into(), ..Default::default() },
    };
    let started = Instant::now();
    ff.export(&plan, &|_| {}, &CancelToken::new()).map_err(|e| e.to_string())?;
    let secs = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(&output);
    Ok(600.0 / secs)
}
```

If `ExportTransition` does not derive `Clone`, add `Clone` to its derive list in `crates/media/src/export.rs` (a one-word change; include it in this task's commit).

- [ ] **Step 4: Run to verify the tests pass and the quick baseline runs**

Run: `cargo test -p kadr-bench && cargo run --release -p kadr-bench -- baseline --quick`
Expected: 3 tests pass; the quick run generates `target/release/bench-media/*.mp4` on first use (minutes for 4K), prints a table with `seek`, `decode`, `frame_buffer`, `export_legacy` rows (all values > 0) and `saved …/bench/m0-baseline-<n>.json`.

- [ ] **Step 5: Commit**

```bash
git add apps/kadr-bench crates/media/src/export.rs Cargo.lock
git commit -m "bench: kadr-bench with counting allocator, cached synthetic 1080p/4K H.264/HEVC media and legacy baseline scenarios

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 5: `kadr-bench live` — the real app, headless, through `kadr-mcp`

**Files:**
- Create: `apps/kadr-bench/src/live.rs`
- Modify: `apps/kadr-bench/src/main.rs`

**Interfaces:**
- Consumes: `kadr-mcp` + `kadr` binaries in the same target dir; MCP tools `import_media`, `wait_idle`, `get_state`, `place_media`, `playback`, `set_playhead`, `get_perf` (Task 3); `baseline::seek_times`; `report::Report`.
- Produces: `kadr-bench live --clip <file> [--seconds N]`; `live::tool_result(&Value) -> Result<Value, String>`.

- [ ] **Step 1: Write the failing tests** — `apps/kadr-bench/src/live.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_results_parse_json_text_and_report_errors() {
        let ok = json!({"content": [{"type": "text", "text": "{\"idle\": true}"}]});
        assert_eq!(tool_result(&ok).unwrap(), json!({"idle": true}));
        let plain = json!({"content": [{"type": "text", "text": "done"}]});
        assert_eq!(tool_result(&plain).unwrap(), json!("done"));
        let err = json!({"isError": true, "content": [{"type": "text", "text": "not_found: x"}]});
        assert_eq!(tool_result(&err).unwrap_err(), "not_found: x");
    }

    #[test]
    fn nested_numbers_become_rows() {
        let mut r = Report::new("t");
        flatten("playback", &json!({"frames": 10, "total_ms": {"p50": 12.5}, "note": "x"}), &mut r);
        let got: Vec<(String, f64)> = r.rows.iter().map(|x| (format!("{}:{}", x.scenario, x.metric), x.value)).collect();
        assert_eq!(got, vec![("playback:frames".to_string(), 10.0), ("playback:total_ms.p50".to_string(), 12.5)]);
    }
}
```

Add `mod live;` to `main.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-bench live`
Expected: compile errors (`tool_result`, `flatten` not found).

- [ ] **Step 3: Implement** — above the tests in `live.rs`:

```rust
//! End-to-end numbers from the real editor: a headless Kadr driven through
//! kadr-mcp (stdio JSON-RPC), in a temporary data dir — never the user's.

use crate::report::Report;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(data: &Path) -> Result<Mcp, String> {
        let dir = std::env::current_exe().map_err(|e| e.to_string())?.parent().ok_or("no exe dir")?.to_path_buf();
        let exe = |n: &str| dir.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
        let mut child = Command::new(exe("kadr-mcp"))
            .env("KADR_DATA_DIR", data)
            .env("KADR_EXE", exe("kadr"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start kadr-mcp next to kadr-bench (build kadr-editor and kadr-mcp into the same target dir): {e}"))?;
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(Mcp { child, stdin, out, next: 0 })
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}).to_string();
        let w = self.stdin.as_mut().ok_or("session closed")?;
        writeln!(w, "{line}").and_then(|_| w.flush()).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        self.out.read_line(&mut reply).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&reply).map_err(|e| format!("bad reply {reply:?}: {e}"))?;
        match v.get("error") {
            Some(e) => Err(e.to_string()),
            None => Ok(v["result"].clone()),
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        tool_result(&self.rpc("tools/call", json!({"name": tool, "arguments": args}))?).map_err(|e| format!("{tool}: {e}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // Closing stdin ends the session; kadr-mcp stops the headless Kadr it launched.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// An MCP `tools/call` result: `isError` → Err(text); JSON text → parsed; other text → string.
pub fn tool_result(v: &Value) -> Result<Value, String> {
    let text = v["content"][0]["text"].as_str().unwrap_or_default();
    if v["isError"].as_bool() == Some(true) {
        return Err(text.to_string());
    }
    Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
}

/// Numeric leaves of `v` → rows `scenario = prefix`, `metric = dotted path`.
pub fn flatten(prefix: &str, v: &Value, r: &mut Report) {
    fn walk(scenario: &str, path: &str, v: &Value, r: &mut Report) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    walk(scenario, &p, x, r);
                }
            }
            Value::Number(n) => r.push(scenario, "live", path, n.as_f64().unwrap_or(0.0), ""),
            _ => {}
        }
    }
    walk(prefix, "", v, r)
}

pub fn run(clip: &Path, seconds: u64) -> Result<Report, String> {
    let clip = clip.canonicalize().map_err(|e| format!("{}: {e}", clip.display()))?;
    let data = std::env::temp_dir().join(format!("kadr-bench-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let result = drive(&clip, &data, seconds);
    let _ = std::fs::remove_dir_all(&data);
    result
}

fn drive(clip: &Path, data: &Path, seconds: u64) -> Result<Report, String> {
    let mut m = Mcp::start(data)?;
    m.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "kadr-bench", "version": "1"}}))?;
    m.call("import_media", json!({"paths": [clip]}))?;
    m.call("wait_idle", json!({"timeout_ms": 120_000}))?;
    let state = m.call("get_state", json!({}))?;
    let asset = state["project"]["assets"][0]["id"].as_str().ok_or("import produced no asset")?.to_string();
    m.call("place_media", json!({"id": asset, "at_ms": 0}))?;
    m.call("wait_idle", json!({"timeout_ms": 120_000}))?;
    let duration_ms = m.call("get_state", json!({}))?["timeline"]["duration_ms"].as_i64().ok_or("empty timeline")?;

    let mut r = Report::new("m0-live");
    m.call("get_perf", json!({"reset": true}))?;
    m.call("playback", json!({"action": "play"}))?;
    std::thread::sleep(Duration::from_secs(seconds));
    m.call("playback", json!({"action": "pause"}))?;
    flatten("playback", &m.call("get_perf", json!({"reset": true}))?, &mut r);

    for t in crate::baseline::seek_times((duration_ms / 1000) as u32, 10) {
        m.call("set_playhead", json!({"at_ms": t.as_millis()}))?;
        m.call("wait_idle", json!({"timeout_ms": 30_000}))?;
    }
    flatten("seek", &m.call("get_perf", json!({"reset": true}))?, &mut r);
    Ok(r)
}
```

In `main.rs`, extend the match:

```rust
        Some("live") => {
            let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
            match value("--clip") {
                Some(c) => live::run(std::path::Path::new(&c), value("--seconds").and_then(|s| s.parse().ok()).unwrap_or(20)),
                None => Err("live needs --clip <file>".to_string()),
            }
        }
```

- [ ] **Step 4: Run tests and one live measurement**

Run: `cargo test -p kadr-bench && cargo build --release -p kadr-editor -p kadr-mcp -p kadr-bench && target/release/kadr-bench live --clip target/release/bench-media/h264_1080p30.mp4 --seconds 10`
Expected: tests pass; the live run prints `playback` rows (frames > 0, total_ms.p50 > 0) and `seek` rows (seek_ms.count ≥ 1). Check with `tasklist //FI "IMAGENAME eq kadr.exe"` that no new headless `kadr.exe` is left running.

- [ ] **Step 5: Commit**

```bash
git add apps/kadr-bench/src/live.rs apps/kadr-bench/src/main.rs
git commit -m "bench: live scenario — headless Kadr via kadr-mcp, playback and seek statistics from get_perf

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 6: Record the M0 baseline (exit criterion M0)

**Files:**
- Create: `docs/perf/2026-09-30-m0-baseline.md`

- [ ] **Step 1: Run the full measurements** (release builds; nothing else heavy running)

```bash
cargo build --release -p kadr-editor -p kadr-mcp -p kadr-bench
target/release/kadr-bench baseline
target/release/kadr-bench live --clip target/release/bench-media/h264_1080p30.mp4 --seconds 20
target/release/kadr-bench live --clip target/release/bench-media/h264_2160p30.mp4 --seconds 20
target/release/kadr-bench live --clip target/release/bench-media/hevc_2160p30.mp4 --seconds 20
ffmpeg -version | head -1
```

- [ ] **Step 2: Write `docs/perf/2026-09-30-m0-baseline.md`** with exactly these sections, copying values from the outputs above:

1. **Machine** — CPU, GPU, RAM (from `Get-CimInstance Win32_Processor/Win32_VideoController/Win32_ComputerSystem`), FFmpeg version, build profile (`release`), preview quality in live runs (`½`, the default).
2. **Seek latency (bench, legacy: new FFmpeg process per seek)** — table: clip × {full, half} × {p50, p90, max} ms.
3. **Sequential decode** — table: clip × {full, half} × {fps, frame-sized allocations per frame}.
4. **Frame buffer cost** — 1080p and 4K: allocation + first touch ms, full copy ms.
5. **Live preview (headless Kadr)** — per clip: playback frames, dropped, total/decode/present p50/p90 ms, frame allocations and copies per frame, MB copied per frame; seek p50/p90/max ms.
6. **Legacy export** — fps.
7. **Targets for the new pipeline** — one line each: M3 seek latency must beat row 2 (same clips, same sizes); M2/M3 steady-state frame-sized allocations per frame = 0 (vs rows 3 and 5); M4 UI-thread copies per frame = 0 (vs row 5); M4 dropped frames ≤ 1 % over 60 s; M5 export fps compared to row 6.

- [ ] **Step 3: Commit**

```bash
git add docs/perf/2026-09-30-m0-baseline.md
git commit -m "perf: M0 baseline of the legacy preview and export pipeline

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

## M1 — Scene model and evaluator

### Task 7: `kadr_core::color` — explicit colour metadata

**Files:**
- Create: `crates/core/src/color.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Produces: `kadr_core::color::{ColorInfo, Primaries, Transfer, Matrix, Range, AlphaMode}` (all `Copy + Eq + Serialize + Deserialize`), `ColorInfo::{WORKING_SDR, IMAGE_SRGB, guess_video(w, h), from_ffprobe(w, h, primaries, transfer, space, range), is_supported_sdr()}`; re-exported as `kadr_core::ColorInfo`.

- [ ] **Step 1: Write the failing tests** — `crates/core/src/color.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_video_is_guessed_from_its_height() {
        let hd = ColorInfo::guess_video(1920, 1080);
        assert_eq!((hd.primaries, hd.transfer, hd.matrix, hd.range, hd.alpha), (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709, Range::Limited, AlphaMode::Opaque));
        assert_eq!(ColorInfo::guess_video(1280, 720).matrix, Matrix::Bt709);
        let pal = ColorInfo::guess_video(720, 576);
        assert_eq!((pal.primaries, pal.matrix), (Primaries::Bt601_625, Matrix::Bt601));
        let ntsc = ColorInfo::guess_video(720, 480);
        assert_eq!((ntsc.primaries, ntsc.matrix), (Primaries::Bt601_525, Matrix::Bt601));
    }

    #[test]
    fn ffprobe_tags_win_over_the_guess() {
        let c = ColorInfo::from_ffprobe(720, 480, Some("bt709"), Some("bt709"), Some("bt709"), Some("pc"));
        assert_eq!((c.primaries, c.transfer, c.matrix, c.range), (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709, Range::Full));
        let hdr = ColorInfo::from_ffprobe(3840, 2160, Some("bt2020"), Some("smpte2084"), Some("bt2020nc"), Some("tv"));
        assert_eq!((hdr.primaries, hdr.transfer, hdr.matrix), (Primaries::Bt2020, Transfer::Pq, Matrix::Bt2020Ncl));
        assert!(!hdr.is_supported_sdr());
        let sd = ColorInfo::from_ffprobe(1920, 1080, Some("smpte170m"), Some("smpte170m"), Some("smpte170m"), None);
        assert_eq!((sd.primaries, sd.transfer, sd.matrix, sd.range), (Primaries::Bt601_525, Transfer::Bt709, Matrix::Bt601, Range::Limited));
        let pal = ColorInfo::from_ffprobe(1920, 1080, Some("bt470bg"), None, Some("bt470bg"), None);
        assert_eq!((pal.primaries, pal.matrix), (Primaries::Bt601_625, Matrix::Bt601));
        assert_eq!(ColorInfo::from_ffprobe(1920, 1080, None, Some("iec61966-2-1"), Some("gbr"), None).matrix, Matrix::Rgb);
    }

    #[test]
    fn unknown_or_missing_tags_fall_back_per_field() {
        let c = ColorInfo::from_ffprobe(1920, 1080, Some("unknown"), None, Some("reserved"), Some("unknown"));
        assert_eq!(c, ColorInfo::guess_video(1920, 1080));
    }

    #[test]
    fn working_space_is_full_range_rec709_premultiplied() {
        let w = ColorInfo::WORKING_SDR;
        assert_eq!((w.primaries, w.transfer, w.matrix, w.range, w.alpha), (Primaries::Bt709, Transfer::Bt709, Matrix::Rgb, Range::Full, AlphaMode::Premultiplied));
        assert!(w.is_supported_sdr());
        assert_eq!(ColorInfo::IMAGE_SRGB.alpha, AlphaMode::Straight);
    }
}
```

Add `pub mod color;` and `pub use color::ColorInfo;` to `crates/core/src/lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-core color`
Expected: compile errors (types not found).

- [ ] **Step 3: Implement** — above the tests:

```rust
//! Colour metadata carried by every source, frame and output (render spec
//! §6). Only SDR Rec.709/601 is processed today; everything else is at least
//! described, so a frame never loses what it is.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primaries {
    Bt709,
    Bt601_625,
    Bt601_525,
    Bt2020,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    Bt709,
    Srgb,
    Linear,
    Pq,
    Hlg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Matrix {
    Rgb,
    Bt709,
    Bt601,
    Bt2020Ncl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    Limited,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaMode {
    Opaque,
    Straight,
    Premultiplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorInfo {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub matrix: Matrix,
    pub range: Range,
    pub alpha: AlphaMode,
}

impl ColorInfo {
    /// The SDR renderer's working space and output: full-range non-linear
    /// Rec.709 R'G'B' with premultiplied alpha.
    pub const WORKING_SDR: ColorInfo =
        ColorInfo { primaries: Primaries::Bt709, transfer: Transfer::Bt709, matrix: Matrix::Rgb, range: Range::Full, alpha: AlphaMode::Premultiplied };

    /// Still images (PNG, JPEG, …): sRGB, full range, straight alpha.
    pub const IMAGE_SRGB: ColorInfo =
        ColorInfo { primaries: Primaries::Bt709, transfer: Transfer::Srgb, matrix: Matrix::Rgb, range: Range::Full, alpha: AlphaMode::Straight };

    /// Untagged video: ≥ 720 lines → Rec.709; 576 → Rec.601/625; otherwise
    /// Rec.601/525. Limited range, opaque.
    pub fn guess_video(_width: u32, height: u32) -> ColorInfo {
        let (primaries, matrix) = match height {
            h if h >= 720 => (Primaries::Bt709, Matrix::Bt709),
            576 => (Primaries::Bt601_625, Matrix::Bt601),
            _ => (Primaries::Bt601_525, Matrix::Bt601),
        };
        ColorInfo { primaries, transfer: Transfer::Bt709, matrix, range: Range::Limited, alpha: AlphaMode::Opaque }
    }

    /// From ffprobe's `color_primaries`, `color_transfer`, `color_space` and
    /// `color_range`; each unknown or missing field falls back to
    /// [`ColorInfo::guess_video`].
    pub fn from_ffprobe(width: u32, height: u32, primaries: Option<&str>, transfer: Option<&str>, space: Option<&str>, range: Option<&str>) -> ColorInfo {
        let g = Self::guess_video(width, height);
        ColorInfo {
            primaries: match primaries {
                Some("bt709") => Primaries::Bt709,
                Some("bt470bg") => Primaries::Bt601_625,
                Some("smpte170m" | "smpte240m") => Primaries::Bt601_525,
                Some("bt2020") => Primaries::Bt2020,
                _ => g.primaries,
            },
            transfer: match transfer {
                // BT.601 and BT.709 share the same OETF.
                Some("bt709" | "smpte170m" | "bt470bg" | "bt2020-10" | "bt2020-12") => Transfer::Bt709,
                Some("iec61966-2-1") => Transfer::Srgb,
                Some("linear") => Transfer::Linear,
                Some("smpte2084") => Transfer::Pq,
                Some("arib-std-b67") => Transfer::Hlg,
                _ => g.transfer,
            },
            matrix: match space {
                Some("bt709") => Matrix::Bt709,
                Some("bt470bg" | "smpte170m") => Matrix::Bt601,
                Some("bt2020nc") => Matrix::Bt2020Ncl,
                Some("gbr") => Matrix::Rgb,
                _ => g.matrix,
            },
            range: match range {
                Some("pc") => Range::Full,
                Some("tv") => Range::Limited,
                _ => g.range,
            },
            alpha: g.alpha,
        }
    }

    /// Whether the SDR pipeline processes this correctly today.
    pub fn is_supported_sdr(&self) -> bool {
        matches!(self.primaries, Primaries::Bt709 | Primaries::Bt601_625 | Primaries::Bt601_525)
            && matches!(self.transfer, Transfer::Bt709 | Transfer::Srgb)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p kadr-core color`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/color.rs crates/core/src/lib.rs
git commit -m "core: explicit colour metadata (primaries, transfer, matrix, range, alpha) with ffprobe mapping and untagged-video rule

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 8: Probe reads colour tags and SAR; `VideoInfo::display_size`

**Files:**
- Modify: `crates/core/src/media_info.rs`, `crates/media/src/ffmpeg/probe.rs`
- Modify (struct literals gain the two new fields): `crates/ai/tests/assistant_flow.rs:22`, `crates/project/tests/serialization.rs:11`, `crates/timeline/tests/engine.rs:18`, `apps/editor/src/mcp_state.rs:150`

**Interfaces:**
- Consumes: `ColorInfo` (Task 7).
- Produces: `VideoInfo { …, sar: (u32, u32) /* default (1, 1) */, color: Option<ColorInfo> /* None in projects saved before this */ }`, `VideoInfo::display_size() -> (u32, u32)`, `VideoInfo::color_info() -> ColorInfo`, `MediaInfo::source_color() -> ColorInfo`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/media_info.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{AlphaMode, ColorInfo, Matrix};

    fn video(width: u32, height: u32, sar: (u32, u32), rotation: i32) -> VideoInfo {
        VideoInfo { width, height, frame_rate: None, variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation, sar, color: None }
    }

    #[test]
    fn display_size_applies_sar_then_rotation() {
        assert_eq!(video(1920, 1080, (1, 1), 0).display_size(), (1920, 1080));
        assert_eq!(video(720, 480, (8, 9), 0).display_size(), (640, 480), "anamorphic NTSC 4:3");
        assert_eq!(video(1920, 1080, (1, 1), 90).display_size(), (1080, 1920), "phone portrait");
        assert_eq!(video(1920, 1080, (1, 1), -90).display_size(), (1080, 1920));
        assert_eq!(video(1920, 1080, (1, 1), 180).display_size(), (1920, 1080));
        assert_eq!(video(1920, 1080, (0, 1), 0).display_size(), (1920, 1080), "unknown SAR = square");
    }

    #[test]
    fn projects_saved_before_colour_tags_still_load_and_get_a_guess() {
        let old = r#"{"width":720,"height":576,"frame_rate":null,"variable_frame_rate":false,"codec":"mpeg2video","pixel_format":"yuv420p","rotation":0}"#;
        let v: VideoInfo = serde_json::from_str(old).unwrap();
        assert_eq!((v.sar, v.color), ((1, 1), None));
        assert_eq!(v.color_info().matrix, Matrix::Bt601);
    }

    #[test]
    fn source_colour_of_images_has_straight_alpha() {
        let mut info = MediaInfo { kind: MediaKind::Image, duration: crate::Time::from_secs(5), container: "png_pipe".into(), size_bytes: 0, video: Some(video(10, 10, (1, 1), 0)), audio: None, timecode: None };
        assert_eq!(info.source_color(), ColorInfo::IMAGE_SRGB);
        info.kind = MediaKind::Video;
        assert_eq!(info.source_color().alpha, AlphaMode::Opaque);
    }
}
```

`crates/core/Cargo.toml` needs `serde_json` for this test: add

```toml
[dev-dependencies]
serde_json.workspace = true
```

Append to the tests in `crates/media/src/ffmpeg/probe.rs`:

```rust
    #[test]
    fn reads_colour_tags_and_sample_aspect_ratio() {
        let json = br#"{"streams":[{"codec_type":"video","codec_name":"h264","width":720,"height":480,
                        "sample_aspect_ratio":"8:9","color_range":"tv","color_space":"smpte170m",
                        "color_transfer":"smpte170m","color_primaries":"smpte170m"}],
                        "format":{"format_name":"mov","duration":"1.0"}}"#;
        let v = parse(json).unwrap().video.unwrap();
        assert_eq!(v.sar, (8, 9));
        assert_eq!(v.display_size(), (640, 480));
        let c = v.color.unwrap();
        assert_eq!((c.matrix, c.range), (kadr_core::color::Matrix::Bt601, kadr_core::color::Range::Limited));

        let untagged = br#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"sample_aspect_ratio":"0:1"}],
                            "format":{"format_name":"mov","duration":"1.0"}}"#;
        let v = parse(untagged).unwrap().video.unwrap();
        assert_eq!(v.sar, (1, 1));
        assert_eq!(v.color.unwrap().matrix, kadr_core::color::Matrix::Bt709);

        let png = br#"{"streams":[{"codec_type":"video","codec_name":"png","width":10,"height":10}],"format":{"format_name":"png_pipe"}}"#;
        assert_eq!(parse(png).unwrap().video.unwrap().color, Some(kadr_core::ColorInfo::IMAGE_SRGB));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-core media_info; cargo test -p kadr-media probe`
Expected: compile errors (`sar`, `color`, `display_size`, `source_color` not found).

- [ ] **Step 3: Implement**

In `crates/core/src/media_info.rs`: add `use crate::color::ColorInfo;`, add to `VideoInfo` after `rotation`:

```rust
    /// Sample (pixel) aspect ratio; (1, 1) for square pixels.
    #[serde(default = "square_pixels")]
    pub sar: (u32, u32),
    /// Colour metadata from probe; `None` for projects saved before it was read.
    #[serde(default)]
    pub color: Option<ColorInfo>,
```

and below the struct:

```rust
fn square_pixels() -> (u32, u32) {
    (1, 1)
}

impl VideoInfo {
    /// Size as displayed: the sample aspect ratio applied to the width, then
    /// swapped for ±90°/270° rotation (phones).
    pub fn display_size(&self) -> (u32, u32) {
        let (n, d) = if self.sar.0 > 0 && self.sar.1 > 0 { self.sar } else { (1, 1) };
        let w = ((self.width as u64 * n as u64 + d as u64 / 2) / d as u64) as u32;
        if self.rotation.rem_euclid(180) == 90 { (self.height, w) } else { (w, self.height) }
    }

    pub fn color_info(&self) -> ColorInfo {
        self.color.unwrap_or_else(|| ColorInfo::guess_video(self.width, self.height))
    }
}
```

and in `impl MediaInfo`:

```rust
    /// Colour of the decoded source (render spec §6).
    pub fn source_color(&self) -> ColorInfo {
        match (self.kind, &self.video) {
            (MediaKind::Image, v) => v.as_ref().and_then(|v| v.color).unwrap_or(ColorInfo::IMAGE_SRGB),
            (_, Some(v)) => v.color_info(),
            (_, None) => ColorInfo::WORKING_SDR,
        }
    }
```

In `crates/media/src/ffmpeg/probe.rs`, add to `struct Stream`:

```rust
    sample_aspect_ratio: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
```

add a helper:

```rust
/// "8:9" → (8, 9); "0:1", "N/A" or garbage → None.
fn parse_ratio(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once(':')?;
    let (a, b) = (a.parse().ok()?, b.parse().ok()?);
    (a > 0 && b > 0).then_some((a, b))
}
```

and in the `VideoInfo { … }` literal inside `parse`, after `rotation,`:

```rust
            sar: v.sample_aspect_ratio.as_deref().and_then(parse_ratio).unwrap_or((1, 1)),
            color: Some(if is_image {
                kadr_core::ColorInfo::IMAGE_SRGB
            } else {
                kadr_core::ColorInfo::from_ffprobe(
                    v.width.unwrap_or(0),
                    v.height.unwrap_or(0),
                    v.color_primaries.as_deref(),
                    v.color_transfer.as_deref(),
                    v.color_space.as_deref(),
                    v.color_range.as_deref(),
                )
            }),
```

In each of the four other `VideoInfo { … }` literals listed under **Files**, add `sar: (1, 1), color: None` after `rotation…`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace 2>&1 | grep -E "test result|FAILED|^error"`
Expected: all `ok`, no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/core crates/media/src/ffmpeg/probe.rs crates/ai/tests/assistant_flow.rs crates/project/tests/serialization.rs crates/timeline/tests/engine.rs apps/editor/src/mcp_state.rs
git commit -m "media: probe reads colour tags and sample aspect ratio; VideoInfo::display_size applies SAR and rotation

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 9: `kadr-scene` — scene data and aspect-correct geometry

**Files:**
- Create: `crates/scene/Cargo.toml`, `crates/scene/src/lib.rs`, `crates/scene/src/geom.rs`, `crates/scene/src/scene.rs`

**Interfaces:**
- Consumes: `kadr_core::{Time, AssetId, ClipId, TransitionId, ColorInfo}`.
- Produces (all re-exported from `kadr_scene`):
  - `SizeU { w, h }` (`SizeU::new`), `Vec2 { x, y }` (`Vec2::new`), `RectF { x0, y0, x1, y1 }` (`new, width, height, is_empty, intersect, contains_rect`), `Affine2 { a, b, c, d, tx, ty }` (`IDENTITY, translate, scale, rotate, after, apply, inverse, map_bounds`);
  - `Placement { size, anchor, position, scale, rotation }` (`fill(canvas)`, `to_canvas() -> Affine2`, `full_crop() -> RectF`, `canvas_bounds(&RectF) -> RectF`);
  - `LayerId(u128)` (`From<ClipId>`, `From<TransitionId>`), `SourceKind { Video, Image }`, `MediaRef { media, stream, kind, display_size, color }`, `Rgba { r, g, b, a }` (`BLACK`, `TRANSPARENT`), `BlendMode { Normal, Add, Multiply, Screen }`, `ColorAdjust { exposure, contrast, saturation, temperature, tint }` (`NEUTRAL`, `is_neutral`), `Effect::ColorAdjust(ColorAdjust)` (`#[non_exhaustive]`), `TransitionOp { Dissolve, DipToColor(Rgba), Wipe { angle, softness } }`, `TransitionLayer { op, progress, from, to }`, `LayerContent { Media { media, source_time }, Solid(Rgba), Transition(Box<TransitionLayer>) }`, `Layer { id, content, placement, crop, opacity, blend, effects }`, `RenderQuality { PreviewFast, PreviewHigh, Export }`, `OutputSpec { size, quality, color }` (`new(size, quality)`), `FrameScene { time, canvas, output, background, layers }` (`empty(time, canvas, output)`).

- [ ] **Step 1: Create the crate with failing geometry tests**

`crates/scene/Cargo.toml`:

```toml
[package]
name = "kadr-scene"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Kadr's evaluated scene: plain data and pure geometry (render spec §4–5)"

[dependencies]
kadr-core = { path = "../core" }
```

`crates/scene/src/lib.rs`:

```rust
//! Kadr's evaluated scene (render spec §4): what one output frame shows, as
//! plain data. Produced by the timeline's scene evaluator, consumed by
//! renderers through kadr-render. No pixels, no I/O, no timeline, no FFmpeg.

pub mod geom;
pub mod scene;

pub use geom::*;
pub use scene::*;
```

`crates/scene/src/geom.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn rect_close(a: RectF, b: RectF) -> bool {
        [(a.x0, b.x0), (a.y0, b.y0), (a.x1, b.x1), (a.y1, b.y1)].iter().all(|(p, q)| (p - q).abs() < 1e-3)
    }

    #[test]
    fn fill_placement_maps_the_canvas_onto_itself() {
        let m = Placement::fill(SizeU::new(1920, 1080)).to_canvas();
        assert!(close(m.apply(0.0, 0.0), (0.0, 0.0)));
        assert!(close(m.apply(1920.0, 1080.0), (1920.0, 1080.0)));
    }

    #[test]
    fn rotation_is_aspect_correct() {
        // A 200×100 layer rotated 90° becomes 100 wide and 200 tall, not a squashed square.
        let p = Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(500.0, 500.0), scale: Vec2::new(1.0, 1.0), rotation: std::f32::consts::FRAC_PI_2 };
        let b = p.canvas_bounds(&p.full_crop());
        assert!(rect_close(b, RectF::new(450.0, 400.0, 550.0, 600.0)), "{b:?}");
        // Positive angles turn clockwise on screen (y points down): the right edge goes down.
        assert!(close(p.to_canvas().apply(200.0, 50.0), (500.0, 600.0)));
    }

    #[test]
    fn scale_happens_about_the_anchor() {
        let p = Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.25, 0.75), position: Vec2::new(300.0, 300.0), scale: Vec2::new(0.5, 0.5), rotation: 0.0 };
        assert!(close(p.to_canvas().apply(50.0, 75.0), (300.0, 300.0)), "the anchor point lands on `position`");
        assert!(close(p.to_canvas().apply(150.0, 75.0), (350.0, 300.0)));
    }

    #[test]
    fn inverse_round_trips() {
        let p = Placement { size: Vec2::new(640.0, 360.0), anchor: Vec2::new(0.3, 0.6), position: Vec2::new(123.0, 456.0), scale: Vec2::new(1.7, 0.8), rotation: 0.7 };
        let m = p.to_canvas();
        let inv = m.inverse().unwrap();
        let (x, y) = m.apply(10.0, 20.0);
        assert!(close(inv.apply(x, y), (10.0, 20.0)));
        assert!(Affine2::scale(0.0, 1.0).inverse().is_none());
    }

    #[test]
    fn crop_keeps_the_remaining_part_in_place() {
        let p = Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(500.0, 500.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 };
        assert!(rect_close(p.canvas_bounds(&p.full_crop()), RectF::new(400.0, 450.0, 600.0, 550.0)));
        let right_half = RectF::new(100.0, 0.0, 200.0, 100.0);
        assert!(rect_close(p.canvas_bounds(&right_half), RectF::new(500.0, 450.0, 600.0, 550.0)), "no re-centring");
    }

    #[test]
    fn rect_intersection_containment_and_emptiness() {
        let a = RectF::new(0.0, 0.0, 10.0, 10.0);
        assert!(a.intersect(&RectF::new(20.0, 20.0, 30.0, 30.0)).is_empty());
        assert_eq!(a.intersect(&RectF::new(5.0, 5.0, 30.0, 30.0)), RectF::new(5.0, 5.0, 10.0, 10.0));
        assert!(RectF::new(-1.0, -1.0, 11.0, 11.0).contains_rect(&a));
        assert!(!a.contains_rect(&RectF::new(-1.0, 0.0, 5.0, 5.0)));
        assert!(RectF::new(3.0, 0.0, 3.0, 5.0).is_empty());
        assert_eq!((a.width(), a.height()), (10.0, 10.0));
    }
}
```

`crates/scene/src/scene.rs` — create empty.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-scene`
Expected: compile errors (`Placement`, `RectF`, `Affine2`, `SizeU`, `Vec2` not found).

- [ ] **Step 3: Implement geometry** — above the tests in `geom.rs`:

```rust
//! Geometry in canvas pixels (render spec §5). X and Y are the same unit
//! (square sequence pixels), so rotation, uniform scale and circles are
//! aspect-correct. Normalized coordinates appear only in `Placement::anchor`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SizeU {
    pub w: u32,
    pub h: u32,
}

impl SizeU {
    pub const fn new(w: u32, h: u32) -> Self {
        SizeU { w, h }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }
}

/// Axis-aligned rectangle by its min (x0, y0) and max (x1, y1) corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl RectF {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        RectF { x0, y0, x1, y1 }
    }
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn intersect(&self, o: &RectF) -> RectF {
        RectF::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }
    pub fn contains_rect(&self, o: &RectF) -> bool {
        self.x0 <= o.x0 && self.y0 <= o.y0 && self.x1 >= o.x1 && self.y1 >= o.y1
    }
}

/// 2-D affine map: `x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`. f64 so
/// composed transforms stay exact to well below a pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    pub fn translate(x: f64, y: f64) -> Self {
        Affine2 { tx: x, ty: y, ..Self::IDENTITY }
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine2 { a: sx, d: sy, ..Self::IDENTITY }
    }

    /// Positive angles turn clockwise on screen (the y axis points down).
    pub fn rotate(rad: f64) -> Self {
        let (s, c) = rad.sin_cos();
        Affine2 { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 }
    }

    /// `self ∘ inner`: apply `inner` first, then `self`.
    pub fn after(&self, inner: &Affine2) -> Affine2 {
        Affine2 {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            tx: self.a * inner.tx + self.c * inner.ty + self.tx,
            ty: self.b * inner.tx + self.d * inner.ty + self.ty,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.tx, self.b * x + self.d * y + self.ty)
    }

    pub fn inverse(&self) -> Option<Affine2> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine2 { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }

    /// Axis-aligned bounds of `r` after the map.
    pub fn map_bounds(&self, r: &RectF) -> RectF {
        let corners = [(r.x0, r.y0), (r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)].map(|(x, y)| self.apply(x as f64, y as f64));
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in corners {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        RectF::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
    }
}

/// Where a layer's content lands on the canvas (render spec §5):
/// `canvas = position + R(rotation) · diag(scale) · (local − anchor·size)`.
/// Local space is the content rectangle `[0, size.x] × [0, size.y]` in canvas
/// pixels at scale 1; `anchor` is normalized to that rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub size: Vec2,
    pub anchor: Vec2,
    pub position: Vec2,
    pub scale: Vec2,
    /// Radians, clockwise on screen.
    pub rotation: f32,
}

impl Placement {
    /// Content exactly covering the canvas, untransformed.
    pub fn fill(canvas: SizeU) -> Placement {
        let (w, h) = (canvas.w as f32, canvas.h as f32);
        Placement { size: Vec2::new(w, h), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(w / 2.0, h / 2.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 }
    }

    /// Local pixels → canvas pixels.
    pub fn to_canvas(&self) -> Affine2 {
        let (ax, ay) = ((self.anchor.x * self.size.x) as f64, (self.anchor.y * self.size.y) as f64);
        Affine2::translate(self.position.x as f64, self.position.y as f64)
            .after(&Affine2::rotate(self.rotation as f64))
            .after(&Affine2::scale(self.scale.x as f64, self.scale.y as f64))
            .after(&Affine2::translate(-ax, -ay))
    }

    /// The whole content rectangle, in local pixels.
    pub fn full_crop(&self) -> RectF {
        RectF::new(0.0, 0.0, self.size.x, self.size.y)
    }

    /// Canvas-space bounds of the visible (cropped) content.
    pub fn canvas_bounds(&self, crop: &RectF) -> RectF {
        self.to_canvas().map_bounds(crop)
    }
}
```

- [ ] **Step 4: Run geometry tests**

Run: `cargo test -p kadr-scene`
Expected: 6 passed.

- [ ] **Step 5: Add the scene types** — `crates/scene/src/scene.rs`:

```rust
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
    /// Edge moving along `angle` (radians, 0 = left → right); `softness` in canvas pixels.
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
```

`crates/scene/src/scene.rs` needs `uuid` only through `ClipId.0.as_u128()` — an inherent method, so no extra dependency.

- [ ] **Step 6: Build and test**

Run: `cargo test -p kadr-scene && cargo build -p kadr-scene 2>&1 | grep -E "^(warning|error)"`
Expected: 6 passed; no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/scene Cargo.lock
git commit -m "scene: kadr-scene — data-only FrameScene, layers, media refs, transitions, colour adjust and aspect-correct placement geometry

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 10: Scene evaluator — tracks to layers

**Files:**
- Create: `crates/timeline/src/scene.rs`
- Modify: `crates/timeline/src/lib.rs` (`pub mod scene;`), `crates/timeline/Cargo.toml` (`kadr-scene = { path = "../scene" }`)

**Interfaces:**
- Consumes: `kadr_scene::*` (Task 9), `VideoInfo::display_size`, `MediaInfo::source_color` (Task 8), `Clip::source_time_at`, `Track::clip_at`.
- Produces: `kadr_timeline::scene::{evaluate(project: &Project, seq: &Sequence, t: Time, out: &OutputSpec) -> FrameScene, clip_layer(project, clip, t, canvas) -> Option<Layer>, fit_contain(src: SizeU, canvas: SizeU) -> Vec2}`.

- [ ] **Step 1: Write the failing tests** — `crates/timeline/src/scene.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{ClipId, FrameRate, MediaInfo, MediaKind, TimeRange, VideoInfo};
    use kadr_project::{Clip, MediaAsset, Track, TrackKind};

    pub(crate) fn asset(kind: MediaKind, w: u32, h: u32, rotation: i32, sar: (u32, u32)) -> MediaAsset {
        let video = VideoInfo { width: w, height: h, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation, sar, color: None };
        let color = if kind == MediaKind::Image { Some(kadr_core::ColorInfo::IMAGE_SRGB) } else { None };
        MediaAsset::new("m", MediaInfo { kind, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(VideoInfo { color, ..video }), audio: None, timecode: None })
    }

    /// Adds a video track above the existing ones; returns its index.
    pub(crate) fn add_video_track(p: &mut Project) -> usize {
        let seq = p.sequence_mut();
        let at = seq.tracks.iter().take_while(|t| t.kind == TrackKind::Video).count();
        seq.tracks.insert(at, Track::new(TrackKind::Video, format!("V{}", at + 1)));
        at
    }

    /// Places `asset` on track `track` from `tl_ms` for `dur_ms`, reading the source from `src_ms`.
    pub(crate) fn place(p: &mut Project, track: usize, a: &MediaAsset, src_ms: i64, tl_ms: i64, dur_ms: i64) -> ClipId {
        if p.asset(a.id).is_none() {
            p.assets.push(a.clone());
        }
        let c = Clip::new(a.id, "c", TimeRange::new(Time::from_millis(src_ms), Time::from_millis(src_ms + dur_ms)), Time::from_millis(tl_ms));
        let id = c.id;
        let clips = &mut p.sequence_mut().tracks[track].clips;
        clips.push(c);
        clips.sort_by_key(|c| c.timeline_in);
        id
    }

    pub(crate) fn scene_at(p: &Project, ms: i64) -> FrameScene {
        evaluate(p, p.sequence(), Time::from_millis(ms), &OutputSpec::new(SizeU::new(960, 540), RenderQuality::PreviewHigh))
    }

    fn hd() -> MediaAsset {
        asset(MediaKind::Video, 1920, 1080, 0, (1, 1))
    }

    fn media(l: &Layer) -> (MediaRef, Time) {
        match &l.content {
            LayerContent::Media { media, source_time } => (*media, *source_time),
            other => panic!("not media: {other:?}"),
        }
    }

    #[test]
    fn one_clip_becomes_one_media_layer_with_exact_source_time() {
        let mut p = Project::new("t");
        let a = hd();
        let id = place(&mut p, 0, &a, 10_000, 2_000, 5_000);
        let s = scene_at(&p, 3_500);
        assert_eq!(s.canvas, SizeU::new(1920, 1080));
        assert_eq!(s.layers.len(), 1);
        let l = &s.layers[0];
        assert_eq!(l.id, LayerId::from(id));
        let (m, src) = media(l);
        assert_eq!((m.media, m.kind, m.display_size), (a.id, SourceKind::Video, SizeU::new(1920, 1080)));
        assert_eq!(src, Time::from_millis(11_500));
        assert_eq!((l.placement.size, l.placement.position), (Vec2::new(1920.0, 1080.0), Vec2::new(960.0, 540.0)));
        assert_eq!(l.crop, l.placement.full_crop());
        assert_eq!((l.opacity, l.blend), (1.0, BlendMode::Normal));
        assert!(l.effects.is_empty());
    }

    #[test]
    fn upper_video_tracks_are_drawn_above_lower_ones() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let (a, b) = (hd(), hd());
        let lower = place(&mut p, 0, &a, 0, 0, 5_000);
        let upper = place(&mut p, v2, &b, 0, 0, 5_000);
        p.sequence_mut().tracks[v2].clips[0].transform.scale = 0.5; // picture in picture
        let ids: Vec<LayerId> = scene_at(&p, 1_000).layers.iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![LayerId::from(lower), LayerId::from(upper)]);
    }

    #[test]
    fn muted_tracks_disabled_clips_and_audio_only_assets_add_nothing() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        place(&mut p, 0, &hd(), 0, 0, 5_000);
        place(&mut p, v2, &hd(), 0, 0, 5_000);
        p.sequence_mut().tracks[0].muted = true;
        p.sequence_mut().tracks[v2].clips[0].enabled = false;
        assert!(scene_at(&p, 1_000).layers.is_empty());
        let mut q = Project::new("t");
        let mut audio = hd();
        audio.info.kind = MediaKind::Audio;
        audio.info.video = None;
        place(&mut q, 0, &audio, 0, 0, 5_000);
        assert!(scene_at(&q, 1_000).layers.is_empty());
    }

    #[test]
    fn outside_the_sequence_or_in_a_gap_the_scene_is_empty_black() {
        let empty = Project::new("t");
        let s = scene_at(&empty, 0);
        assert!(s.layers.is_empty());
        assert_eq!(s.background, Rgba::BLACK);
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 1_000);
        place(&mut p, 0, &hd(), 0, 2_000, 1_000);
        for ms in [-1, 1_500, 3_000, 10_000] {
            assert!(scene_at(&p, ms).layers.is_empty(), "t = {ms} ms");
        }
    }

    #[test]
    fn speed_maps_source_time_exactly() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 10_000);
        p.sequence_mut().tracks[0].clips[0].timeline_out = Time::from_secs(5); // 2× speed
        assert_eq!(media(&scene_at(&p, 1_500).layers[0]).1, Time::from_secs(3));
    }

    #[test]
    fn transform_maps_to_placement_and_crop_in_local_space() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 5_000);
        let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
        (tr.x, tr.y, tr.scale, tr.rotation_deg, tr.crop_left, tr.opacity) = (100.0, -50.0, 0.5, 90.0, 0.25, 0.8);
        let l = &scene_at(&p, 1_000).layers[0];
        assert_eq!(l.placement.position, Vec2::new(1060.0, 490.0));
        assert_eq!(l.placement.scale, Vec2::new(0.5, 0.5));
        assert!((l.placement.rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert_eq!(l.crop, RectF::new(480.0, 0.0, 1920.0, 1080.0), "crop is local pixels; the rest stays in place");
        assert!((l.opacity - 0.8).abs() < 1e-6);
    }

    #[test]
    fn rotated_and_anamorphic_sources_fit_by_display_size() {
        let mut p = Project::new("t");
        place(&mut p, 0, &asset(MediaKind::Video, 1920, 1080, 90, (1, 1)), 0, 0, 5_000);
        let l = &scene_at(&p, 0).layers[0];
        assert_eq!(media(l).0.display_size, SizeU::new(1080, 1920));
        assert_eq!(l.placement.size, Vec2::new(607.5, 1080.0), "portrait phone video, pillarboxed");
        let mut q = Project::new("t");
        place(&mut q, 0, &asset(MediaKind::Video, 720, 480, 0, (8, 9)), 0, 0, 5_000);
        assert_eq!(scene_at(&q, 0).layers[0].placement.size, Vec2::new(1440.0, 1080.0), "4:3 anamorphic NTSC");
    }

    #[test]
    fn images_show_their_only_frame_with_image_colour() {
        let mut p = Project::new("t");
        place(&mut p, 0, &asset(MediaKind::Image, 400, 200, 0, (1, 1)), 0, 1_000, 5_000);
        let (m, src) = media(&scene_at(&p, 3_000).layers[0]);
        assert_eq!((m.kind, src), (SourceKind::Image, Time::ZERO));
        assert_eq!(m.color, kadr_core::ColorInfo::IMAGE_SRGB);
    }

    #[test]
    fn color_adjust_becomes_an_effect_only_when_not_neutral() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 5_000);
        assert!(scene_at(&p, 0).layers[0].effects.is_empty());
        p.sequence_mut().tracks[0].clips[0].color.saturation = 0.0;
        match &scene_at(&p, 0).layers[0].effects[..] {
            [Effect::ColorAdjust(c)] => assert_eq!((c.saturation, c.contrast), (0.0, 1.0)),
            other => panic!("{other:?}"),
        }
    }
}
```

Add `pub mod scene;` to `crates/timeline/src/lib.rs` and `kadr-scene = { path = "../scene" }` to `crates/timeline/Cargo.toml` `[dependencies]`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p kadr-timeline scene::`
Expected: compile errors (`evaluate` not found).

- [ ] **Step 3: Implement** — above the tests in `crates/timeline/src/scene.rs`:

```rust
//! Scene evaluator (render spec §4): the timeline at one instant as a
//! `FrameScene` — video tracks bottom to top, placements in canvas pixels,
//! references to media (not pixels). Pure: renderers and playback never see
//! the timeline, and the timeline never sees a renderer.

use kadr_core::{MediaKind, Time};
use kadr_project::{Clip, ColorAdjust as ClipColor, Project, Sequence, Track, TrackKind, Transform};
use kadr_scene::*;

pub fn evaluate(project: &Project, seq: &Sequence, t: Time, out: &OutputSpec) -> FrameScene {
    let canvas = SizeU::new(seq.width.max(1), seq.height.max(1));
    let mut scene = FrameScene::empty(t, canvas, *out);
    if t < Time::ZERO || t >= seq.duration() {
        return scene;
    }
    for track in seq.tracks.iter().filter(|tr| tr.kind == TrackKind::Video && !tr.muted) {
        if let Some(layer) = track_layer(project, seq, track, t, canvas) {
            scene.layers.push(layer);
        }
    }
    scene
}

fn track_layer(project: &Project, _seq: &Sequence, track: &Track, t: Time, canvas: SizeU) -> Option<Layer> {
    let clip = track.clip_at(t).filter(|c| c.enabled)?;
    clip_layer(project, clip, t, canvas)
}

/// One clip at timeline time `t` (which may lie outside the clip, for
/// transition handles: the source time is extrapolated, then clamped).
pub fn clip_layer(project: &Project, clip: &Clip, t: Time, canvas: SizeU) -> Option<Layer> {
    let asset = project.asset(clip.asset)?;
    let kind = match asset.info.kind {
        MediaKind::Video => SourceKind::Video,
        MediaKind::Image => SourceKind::Image,
        MediaKind::Audio => return None,
    };
    let (dw, dh) = asset.info.video.as_ref()?.display_size();
    if dw == 0 || dh == 0 {
        return None;
    }
    let display_size = SizeU::new(dw, dh);
    let source_time = match kind {
        SourceKind::Image => Time::ZERO,
        SourceKind::Video => clip.source_time_at(t).max(Time::ZERO).min(asset.info.duration),
    };
    let placement = placement_of(&clip.transform, fit_contain(display_size, canvas), canvas);
    let crop = crop_of(&clip.transform, placement.size)?;
    let mut effects = vec![];
    let color = color_of(&clip.color);
    if !color.is_neutral() {
        effects.push(Effect::ColorAdjust(color));
    }
    Some(Layer {
        id: LayerId::from(clip.id),
        content: LayerContent::Media {
            media: MediaRef { media: clip.asset, stream: 0, kind, display_size, color: asset.info.source_color() },
            source_time,
        },
        placement,
        crop,
        opacity: clip.transform.opacity.clamp(0.0, 1.0) as f32,
        blend: BlendMode::Normal,
        effects,
    })
}

/// Largest size with the source's aspect that fits the canvas (contain).
pub fn fit_contain(src: SizeU, canvas: SizeU) -> Vec2 {
    let s = (canvas.w as f64 / src.w as f64).min(canvas.h as f64 / src.h as f64);
    Vec2::new((src.w as f64 * s) as f32, (src.h as f64 * s) as f32)
}

/// The clip `Transform` (offset from centre, uniform scale, degrees) as a placement.
fn placement_of(tr: &Transform, size: Vec2, canvas: SizeU) -> Placement {
    Placement {
        size,
        anchor: Vec2::new(0.5, 0.5),
        position: Vec2::new((canvas.w as f64 / 2.0 + tr.x) as f32, (canvas.h as f64 / 2.0 + tr.y) as f32),
        scale: Vec2::new(tr.scale as f32, tr.scale as f32),
        rotation: tr.rotation_deg.to_radians() as f32,
    }
}

/// Crop fractions → rectangle in local pixels; `None` when nothing is left.
fn crop_of(tr: &Transform, size: Vec2) -> Option<RectF> {
    let f = |v: f64| v.clamp(0.0, 1.0) as f32;
    let r = RectF::new(f(tr.crop_left) * size.x, f(tr.crop_top) * size.y, (1.0 - f(tr.crop_right)) * size.x, (1.0 - f(tr.crop_bottom)) * size.y);
    (!r.is_empty()).then_some(r)
}

fn color_of(c: &ClipColor) -> ColorAdjust {
    ColorAdjust { exposure: c.exposure as f32, contrast: c.contrast as f32, saturation: c.saturation as f32, temperature: c.temperature as f32, tint: 0.0 }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p kadr-timeline scene::`
Expected: 9 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/timeline Cargo.lock
git commit -m "timeline: scene evaluator — video tracks to layers with exact source times, display-size fit, placement, local-space crop and colour adjust

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 11: Scene evaluator — transitions

**Files:**
- Modify: `crates/timeline/src/scene.rs`

**Interfaces:**
- Consumes: `kadr_project::{Transition, TransitionKind}` (`at` = cut point, `duration`, `track`), `clip_layer` (Task 10).
- Produces: a track inside a transition window yields one `LayerContent::Transition` layer (`id` from the transition, `placement = Placement::fill(canvas)`, `crop = full canvas`) whose `from`/`to` hold the outgoing/incoming clip layers; window = `[at − ⌊d/2⌋, at − ⌊d/2⌋ + d)`.

- [ ] **Step 1: Write the failing tests** — append inside `mod tests`:

```rust
    use kadr_core::TransitionId;
    use kadr_project::{Transition, TransitionKind};

    fn add_transition(p: &mut Project, track: usize, at_ms: i64, dur_ms: i64, kind: TransitionKind) -> TransitionId {
        let id = TransitionId::new();
        let track_id = p.sequence().tracks[track].id;
        p.sequence_mut().transitions.push(Transition { id, kind, track: track_id, at: Time::from_millis(at_ms), duration: Time::from_millis(dur_ms) });
        id
    }

    fn transition(l: &Layer) -> &TransitionLayer {
        match &l.content {
            LayerContent::Transition(t) => t,
            other => panic!("not a transition: {other:?}"),
        }
    }

    #[test]
    fn dissolve_window_is_centred_on_the_cut_with_exact_progress() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 2_000);
        let b = place(&mut p, 0, &hd(), 10_000, 2_000, 2_000);
        let tid = add_transition(&mut p, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        for (ms, progress) in [(1_500, 0.0), (2_000, 0.5), (2_250, 0.75)] {
            let s = scene_at(&p, ms);
            assert_eq!(s.layers.len(), 1);
            assert_eq!(s.layers[0].id, LayerId::from(tid));
            let tr = transition(&s.layers[0]);
            assert_eq!(tr.op, TransitionOp::Dissolve);
            assert!((tr.progress - progress).abs() < 1e-6, "t={ms}: {}", tr.progress);
            assert_eq!((tr.from[0].id, tr.to[0].id), (LayerId::from(a), LayerId::from(b)));
        }
        assert_eq!(scene_at(&p, 1_499).layers[0].id, LayerId::from(a));
        assert_eq!(scene_at(&p, 2_500).layers[0].id, LayerId::from(b), "the window end is exclusive");
    }

    #[test]
    fn progress_is_frame_exact_at_29_97() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 4_000);
        place(&mut p, 0, &hd(), 0, 4_000, 4_000);
        let fr = FrameRate::FPS_29_97;
        let d = fr.frame_to_time(30);
        let track = p.sequence().tracks[0].id;
        p.sequence_mut().transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, track, at: Time::from_secs(4), duration: d });
        let start = Time::from_secs(4) - Time(d.flicks() / 2);
        for k in [0, 1, 7, 15, 29] {
            let t = start + fr.frame_to_time(k);
            let s = evaluate(&p, p.sequence(), t, &OutputSpec::new(SizeU::new(960, 540), RenderQuality::Export));
            assert!((transition(&s.layers[0]).progress - k as f32 / 30.0).abs() < 1e-6, "frame {k}");
        }
    }

    #[test]
    fn transition_source_times_extend_past_the_clip_and_clamp_at_zero() {
        let mut p = Project::new("t");
        place(&mut p, 0, &hd(), 0, 0, 2_000);
        place(&mut p, 0, &hd(), 0, 2_000, 2_000); // incoming media has nothing before its in-point
        add_transition(&mut p, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        let before_cut = scene_at(&p, 1_750);
        assert_eq!(media(&transition(&before_cut.layers[0]).to[0]).1, Time::ZERO, "clamped, not negative");
        let after_cut = scene_at(&p, 2_250);
        assert_eq!(media(&transition(&after_cut.layers[0]).from[0]).1, Time::from_millis(2_250), "outgoing handle past its out-point");
    }

    #[test]
    fn missing_neighbour_or_disabled_clip_means_no_transition() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 2_000);
        place(&mut p, 0, &hd(), 0, 2_500, 2_000); // gap after the cut
        add_transition(&mut p, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        assert_eq!(scene_at(&p, 1_750).layers[0].id, LayerId::from(a));
        assert!(scene_at(&p, 2_100).layers.is_empty(), "gap stays a gap");
        let mut q = Project::new("t");
        let a = place(&mut q, 0, &hd(), 0, 0, 2_000);
        place(&mut q, 0, &hd(), 0, 2_000, 2_000);
        q.sequence_mut().tracks[0].clips[1].enabled = false;
        add_transition(&mut q, 0, 2_000, 1_000, TransitionKind::CrossDissolve);
        assert_eq!(scene_at(&q, 1_750).layers[0].id, LayerId::from(a));
    }

    #[test]
    fn overlapping_windows_pick_the_earlier_cut() {
        let mut p = Project::new("t");
        let a = place(&mut p, 0, &hd(), 0, 0, 1_000);
        let b = place(&mut p, 0, &hd(), 0, 1_000, 400);
        place(&mut p, 0, &hd(), 0, 1_400, 1_600);
        add_transition(&mut p, 0, 1_000, 1_000, TransitionKind::CrossDissolve);
        add_transition(&mut p, 0, 1_400, 1_000, TransitionKind::CrossDissolve);
        let tr = transition(&scene_at(&p, 1_200).layers[0]).clone();
        assert_eq!((tr.from[0].id, tr.to[0].id), (LayerId::from(a), LayerId::from(b)));
        assert!((tr.progress - 0.7).abs() < 1e-6);
    }

    #[test]
    fn transition_kinds_map_to_render_ops() {
        for (kind, op) in [
            (TransitionKind::DipToBlack, TransitionOp::DipToColor(Rgba::BLACK)),
            (TransitionKind::Wipe, TransitionOp::Wipe { angle: 0.0, softness: 0.0 }),
        ] {
            let mut p = Project::new("t");
            place(&mut p, 0, &hd(), 0, 0, 2_000);
            place(&mut p, 0, &hd(), 0, 2_000, 2_000);
            add_transition(&mut p, 0, 2_000, 1_000, kind);
            assert_eq!(transition(&scene_at(&p, 2_000).layers[0]).op, op);
        }
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-timeline scene::`
Expected: the six new tests fail (a plain media layer where a transition is expected); the nine from Task 10 still pass.

- [ ] **Step 3: Implement** — in `crates/timeline/src/scene.rs`, extend the imports with `kadr_core::TimeRange` and `kadr_project::{Transition, TransitionKind}`, replace `track_layer`, and add the helpers:

```rust
fn track_layer(project: &Project, seq: &Sequence, track: &Track, t: Time, canvas: SizeU) -> Option<Layer> {
    if let Some(layer) = transition_layer(project, seq, track, t, canvas) {
        return Some(layer);
    }
    let clip = track.clip_at(t).filter(|c| c.enabled)?;
    clip_layer(project, clip, t, canvas)
}

/// `[at − ⌊d/2⌋, at − ⌊d/2⌋ + d)`: centred on the cut.
fn window(tr: &Transition) -> TimeRange {
    let start = tr.at - Time(tr.duration.flicks() / 2);
    TimeRange::new(start, start + tr.duration)
}

/// The transition active on `track` at `t`, if both clips at its cut exist
/// and are enabled. Overlapping windows: the earlier cut wins.
fn transition_layer(project: &Project, seq: &Sequence, track: &Track, t: Time, canvas: SizeU) -> Option<Layer> {
    let tr = seq
        .transitions
        .iter()
        .filter(|tr| tr.track == track.id && tr.duration > Time::ZERO && window(tr).contains(t))
        .min_by_key(|tr| tr.at)?;
    let outgoing = track.clips.iter().find(|c| c.timeline_out == tr.at && c.enabled)?;
    let incoming = track.clips.iter().find(|c| c.timeline_in == tr.at && c.enabled)?;
    let w = window(tr);
    let progress = ((t - w.start).flicks() as f64 / w.duration().flicks() as f64).clamp(0.0, 1.0) as f32;
    let op = match tr.kind {
        TransitionKind::CrossDissolve => TransitionOp::Dissolve,
        TransitionKind::DipToBlack => TransitionOp::DipToColor(Rgba::BLACK),
        TransitionKind::Wipe => TransitionOp::Wipe { angle: 0.0, softness: 0.0 },
    };
    let from = clip_layer(project, outgoing, t, canvas)?;
    let to = clip_layer(project, incoming, t, canvas)?;
    let placement = Placement::fill(canvas);
    Some(Layer {
        id: LayerId::from(tr.id),
        content: LayerContent::Transition(Box::new(TransitionLayer { op, progress, from: vec![from], to: vec![to] })),
        crop: placement.full_crop(),
        placement,
        opacity: 1.0,
        blend: BlendMode::Normal,
        effects: vec![],
    })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-timeline scene::`
Expected: 15 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/timeline/src/scene.rs
git commit -m "timeline: evaluator transitions — centred windows, frame-exact progress, extrapolated handles clamped at zero, earlier cut wins on overlap

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 12: Scene evaluator — cull invisible work

**Files:**
- Modify: `crates/timeline/src/scene.rs`

**Interfaces:**
- Consumes: `Layer`, `Placement::canvas_bounds`, `RectF::{intersect, contains_rect}` (Task 9); `ColorInfo.alpha` (Task 7).
- Produces: `kadr_timeline::scene::cull(scene: &mut FrameScene)`, called at the end of `evaluate`. Rules (render spec §7.3): drop layers with opacity < 0.5/255, zero scale, empty crop or canvas bounds outside the canvas (transition layers are never dropped here); then drop every layer below the topmost opaque full-cover layer — opaque = video (alpha `Opaque`) or `Solid` with `a ≥ 1`, opacity 1, `Normal` blend, no rotation, uncropped, bounds containing the canvas.

- [ ] **Step 1: Write the failing tests** — append inside `mod tests`:

```rust
    fn ids(s: &FrameScene) -> Vec<LayerId> {
        s.layers.iter().map(|l| l.id).collect()
    }

    fn two_layers(edit_upper: impl FnOnce(&mut kadr_project::Clip)) -> (Project, ClipId, ClipId) {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let lower = place(&mut p, 0, &hd(), 0, 0, 5_000);
        let upper = place(&mut p, v2, &hd(), 0, 0, 5_000);
        edit_upper(&mut p.sequence_mut().tracks[v2].clips[0]);
        (p, lower, upper)
    }

    #[test]
    fn fully_transparent_zero_scale_and_offscreen_layers_are_dropped() {
        for edit in [
            (|c: &mut kadr_project::Clip| c.transform.opacity = 0.0) as fn(&mut kadr_project::Clip),
            |c| c.transform.scale = 0.0,
            |c| c.transform.x = 5_000.0,
            |c| c.transform.crop_left = 1.0,
        ] {
            let (p, lower, _) = two_layers(edit);
            assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(lower)]);
        }
    }

    #[test]
    fn an_opaque_full_frame_video_hides_everything_below() {
        let (p, _, upper) = two_layers(|_| {});
        assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(upper)], "nothing below is decoded");
    }

    #[test]
    fn translucent_rotated_cropped_scaled_or_letterboxed_layers_keep_what_is_below() {
        for edit in [
            (|c: &mut kadr_project::Clip| c.transform.opacity = 0.99) as fn(&mut kadr_project::Clip),
            |c| c.transform.rotation_deg = 1.0,
            |c| c.transform.crop_top = 0.1,
            |c| c.transform.scale = 0.5,
            |c| c.transform.x = 10.0,
        ] {
            let (p, lower, upper) = two_layers(edit);
            assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(lower), LayerId::from(upper)]);
        }
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let lower = place(&mut p, 0, &hd(), 0, 0, 5_000);
        let square = place(&mut p, v2, &asset(MediaKind::Video, 1080, 1080, 0, (1, 1)), 0, 0, 5_000);
        assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(lower), LayerId::from(square)], "pillarbox bars show the lower track");
    }

    #[test]
    fn png_logo_with_alpha_never_occludes() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let video = place(&mut p, 0, &hd(), 0, 0, 5_000);
        let logo = place(&mut p, v2, &asset(MediaKind::Image, 1920, 1080, 0, (1, 1)), 0, 0, 5_000);
        assert_eq!(ids(&scene_at(&p, 1_000)), vec![LayerId::from(video), LayerId::from(logo)]);
    }

    #[test]
    fn transitions_are_kept_and_do_not_occlude() {
        let mut p = Project::new("t");
        let v2 = add_video_track(&mut p);
        let below = place(&mut p, 0, &hd(), 0, 0, 4_000);
        place(&mut p, v2, &hd(), 0, 0, 2_000);
        place(&mut p, v2, &hd(), 0, 2_000, 2_000);
        let tid = add_transition(&mut p, v2, 2_000, 1_000, TransitionKind::DipToBlack);
        assert_eq!(ids(&scene_at(&p, 2_000)), vec![LayerId::from(below), LayerId::from(tid)]);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p kadr-timeline scene::`
Expected: `fully_transparent…`, `an_opaque_full_frame…` fail (nothing is culled yet); the rest pass.

- [ ] **Step 3: Implement** — in `crates/timeline/src/scene.rs` add `use kadr_core::color::AlphaMode;`, call `cull(&mut scene);` right before the final `scene` in `evaluate`, and add:

```rust
/// Below this a layer cannot change an 8-bit pixel.
const MIN_OPACITY: f32 = 0.5 / 255.0;

/// Drops layers that cannot affect the frame (render spec §7.3), so nothing
/// invisible is ever decoded or drawn.
pub fn cull(scene: &mut FrameScene) {
    let canvas = RectF::new(0.0, 0.0, scene.canvas.w as f32, scene.canvas.h as f32);
    scene.layers.retain(|l| is_visible(l, &canvas));
    if let Some(top) = scene.layers.iter().rposition(|l| covers_opaquely(l, &canvas)) {
        scene.layers.drain(..top);
    }
}

fn is_visible(l: &Layer, canvas: &RectF) -> bool {
    if matches!(l.content, LayerContent::Transition(_)) {
        return true;
    }
    l.opacity >= MIN_OPACITY
        && l.placement.scale.x != 0.0
        && l.placement.scale.y != 0.0
        && !l.crop.is_empty()
        && !l.placement.canvas_bounds(&l.crop).intersect(canvas).is_empty()
}

fn covers_opaquely(l: &Layer, canvas: &RectF) -> bool {
    let opaque_content = match &l.content {
        LayerContent::Media { media, .. } => media.kind == SourceKind::Video && media.color.alpha == AlphaMode::Opaque,
        LayerContent::Solid(c) => c.a >= 1.0,
        LayerContent::Transition(_) => false,
    };
    opaque_content
        && l.opacity >= 1.0
        && l.blend == BlendMode::Normal
        && l.placement.rotation == 0.0
        && l.crop == l.placement.full_crop()
        && l.placement.canvas_bounds(&l.crop).contains_rect(canvas)
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p kadr-timeline`
Expected: all pass (20 in `scene::` plus the existing timeline tests).

- [ ] **Step 5: Commit**

```bash
git add crates/timeline/src/scene.rs
git commit -m "timeline: cull invisible layers and everything under an opaque full-frame video before anything is decoded

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

### Task 13: Equivalence with the legacy model; evaluator cost (exit criterion M1)

**Files:**
- Create: `crates/timeline/tests/scene_equivalence.rs`
- Create: `apps/kadr-bench/src/scene_bench.rs`; Modify: `apps/kadr-bench/src/main.rs`, `apps/kadr-bench/Cargo.toml`
- Create: `docs/perf/2026-09-30-m1-scene.md`

**Interfaces:**
- Consumes: `kadr_timeline::composition::video_at`, `kadr_timeline::scene::evaluate`.
- Produces: `kadr-bench scene` subcommand (rows: `scene / 3 layers + transition / p50, p90, p99, max` in µs).

- [ ] **Step 1: Write the equivalence test** — `crates/timeline/tests/scene_equivalence.rs`:

```rust
//! For projects the legacy model could show (opaque, untransformed video),
//! the evaluator shows exactly what `video_at` showed: same clip, same
//! source time. Multi-track: the opaque upper clip hides the lower, as before.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::{Clip, MediaAsset, Project, Track, TrackKind};
use kadr_scene::{LayerContent, LayerId, OutputSpec, RenderQuality, SizeU};
use kadr_timeline::composition::video_at;
use kadr_timeline::scene::evaluate;

fn hd() -> MediaAsset {
    let video = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaAsset::new("m", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(120), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
}

fn place(p: &mut Project, track: usize, src_ms: i64, tl_ms: i64, dur_ms: i64) {
    let a = hd();
    p.assets.push(a.clone());
    let c = Clip::new(a.id, "c", TimeRange::new(Time::from_millis(src_ms), Time::from_millis(src_ms + dur_ms)), Time::from_millis(tl_ms));
    let clips = &mut p.sequence_mut().tracks[track].clips;
    clips.push(c);
    clips.sort_by_key(|c| c.timeline_in);
}

fn assert_same(p: &Project) {
    let seq = p.sequence();
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::Export);
    let step = Time::from_millis(1000).mul_ratio(1, 7);
    let mut t = Time::ZERO;
    while t < seq.duration() {
        let scene = evaluate(p, seq, t, &out);
        match video_at(seq, t) {
            None => assert!(scene.layers.is_empty(), "t={t:?}"),
            Some(v) => {
                assert_eq!(scene.layers.len(), 1, "t={t:?}");
                assert_eq!(scene.layers[0].id, LayerId::from(v.clip), "t={t:?}");
                match &scene.layers[0].content {
                    LayerContent::Media { source_time, .. } => assert_eq!(*source_time, v.source_start, "t={t:?}"),
                    other => panic!("{other:?}"),
                }
            }
        }
        t += step;
    }
}

#[test]
fn single_track_with_gaps_matches_video_at() {
    let mut p = Project::new("t");
    place(&mut p, 0, 0, 0, 3_000);
    place(&mut p, 0, 5_000, 3_000, 2_500);
    place(&mut p, 0, 20_000, 7_000, 4_000);
    place(&mut p, 0, 1_000, 11_000, 900);
    assert_same(&p);
}

#[test]
fn opaque_upper_track_hides_the_lower_like_before() {
    let mut p = Project::new("t");
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    place(&mut p, 0, 0, 0, 10_000);
    place(&mut p, 1, 30_000, 2_000, 3_000);
    place(&mut p, 1, 40_000, 7_000, 1_000);
    assert_same(&p);
}
```

(`kadr-scene` is already a normal dependency of `kadr-timeline` since Task 10; integration tests can use it directly.)

- [ ] **Step 2: Run the equivalence tests**

Run: `cargo test -p kadr-timeline --test scene_equivalence`
Expected: 2 passed. A failure here is a real divergence from the legacy model: fix the evaluator (not the test) and rerun.

- [ ] **Step 3: Add the evaluator benchmark** — `apps/kadr-bench/src/scene_bench.rs`:

```rust
//! M1: cost of `evaluate` for a realistic 3-layer scene with a transition.

use crate::report::Report;
use kadr_core::perf::Stats;
use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, TransitionId, VideoInfo};
use kadr_project::{Clip, MediaAsset, Project, Track, TrackKind, Transition, TransitionKind};
use kadr_scene::{OutputSpec, RenderQuality, SizeU};
use kadr_timeline::scene::evaluate;
use std::time::Instant;

fn asset(kind: MediaKind, w: u32, h: u32) -> MediaAsset {
    let video = VideoInfo { width: w, height: h, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaAsset::new("m", MediaInfo { kind, duration: Time::from_secs(600), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
}

fn project() -> Project {
    let mut p = Project::new("bench");
    let seq = p.sequence_mut();
    seq.tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    seq.tracks.insert(2, Track::new(TrackKind::Video, "V3"));
    let (a, b, logo) = (asset(MediaKind::Video, 1920, 1080), asset(MediaKind::Video, 3840, 2160), asset(MediaKind::Image, 512, 512));
    for x in [&a, &b, &logo] {
        p.assets.push(x.clone());
    }
    let mut clip = |track: usize, asset: &MediaAsset, tl_s: i64, dur_s: i64| {
        let mut c = Clip::new(asset.id, "c", TimeRange::new(Time::from_secs(tl_s), Time::from_secs(tl_s + dur_s)), Time::from_secs(tl_s));
        if track == 1 {
            c.transform.scale = 0.3;
            c.transform.rotation_deg = 5.0;
            c.transform.x = 600.0;
        }
        if track == 2 {
            c.transform.scale = 0.2;
            c.transform.x = -800.0;
            c.transform.y = -400.0;
        }
        p.sequence_mut().tracks[track].clips.push(c);
    };
    clip(0, &a, 0, 30);
    clip(0, &a, 30, 30);
    clip(1, &b, 0, 60);
    clip(2, &logo, 0, 60);
    let track = p.sequence().tracks[0].id;
    p.sequence_mut().transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, track, at: Time::from_secs(30), duration: Time::from_secs(2) });
    p
}

pub fn run() -> Result<Report, String> {
    let p = project();
    let seq = p.sequence();
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
    let fr = FrameRate::FPS_30;
    let mut samples = vec![];
    // Batches of 100 evaluations sweep 60 s around the transition; each sample is the mean per call.
    for batch in 0..200 {
        let started = Instant::now();
        for i in 0..100 {
            let t = fr.frame_to_time(((batch * 100 + i) % 1800) as i64);
            std::hint::black_box(evaluate(&p, seq, t, &out));
        }
        samples.push(started.elapsed() / 100);
    }
    let s = Stats::of(samples);
    let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
    let mut r = Report::new("m1-scene");
    for (metric, v) in [("p50", s.p50), ("p90", s.p90), ("p99", s.p99), ("max", s.max)] {
        r.push("scene", "3 layers + transition", metric, us(v), "µs");
    }
    Ok(r)
}
```

In `apps/kadr-bench/Cargo.toml` add to `[dependencies]`:

```toml
kadr-project = { path = "../../crates/project" }
kadr-timeline = { path = "../../crates/timeline" }
kadr-scene = { path = "../../crates/scene" }
```

In `main.rs` add `mod scene_bench;` and the match arm `Some("scene") => scene_bench::run(),`; extend the usage string with `| scene`.

- [ ] **Step 4: Run the benchmark and record M1**

Run: `cargo test -p kadr-bench && cargo run --release -p kadr-bench -- scene`
Expected: a table with `scene | 3 layers + transition | p50 … max` in µs.

Write `docs/perf/2026-09-30-m1-scene.md` with: machine line (same as M0), the scene table, the equivalence test result (2/2 green, with the two scenarios named), the full test counts (`cargo test --workspace` summary), and the verdict line: evaluator p99 vs the 50 µs target — if above, list the top suspects (allocation of `effects`/`layers` vectors per call) for M2 to address; do not optimise in M1.

- [ ] **Step 5: Full suite, then commit**

Run: `cargo test --workspace 2>&1 | grep -E "test result|FAILED|^error" ; cargo build --workspace 2>&1 | grep -E "^warning"`
Expected: every `test result: ok`; no warnings.

```bash
git add crates/timeline/tests/scene_equivalence.rs apps/kadr-bench docs/perf/2026-09-30-m1-scene.md Cargo.lock
git commit -m "timeline: evaluator matches the legacy model on single-track and opaque multi-track projects; bench: evaluator cost (M1 exit)

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

## After M1

Write `docs/superpowers/plans/<date>-render-foundation-m2.md` (CpuRenderer, `kadr_core::frame`, golden tests, render benchmarks) from the spec and the recorded M0/M1 numbers, then continue stage by stage. Each later plan keeps this plan's Global Constraints and ends with its row of the stage table.
