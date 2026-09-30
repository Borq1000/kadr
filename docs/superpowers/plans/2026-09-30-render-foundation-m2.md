# Render Foundation M2 — CpuRenderer

> Continues `2026-09-30-render-foundation.md` (M0–M1). Same Global Constraints, except: **`rayon` is allowed from M2** (workspace dependency), and `png` is allowed as a dev-dependency of `kadr-render` (already in the lockfile). Branch `feature/render-pipeline`.

**Goal:** the first code that turns a `FrameScene` into pixels — `kadr-render` with the `Renderer` trait and `CpuRenderer` — exactly by the rendering contract in `crates/scene/src/lib.rs`, proven by an independent reference renderer, golden images and benchmarks.

**Exit criterion (stage table):** golden PNG tests — alpha over, multi-layer, transforms/rotation, crop-in-place, opacity, blend modes, dissolve/dip/wipe, colour reference (spec §6 list); bench — 1080p and 4K render ms for 1/2/3 layers and a transition-only row; **0 frame-sized allocations per frame after warm-up**.

## Decisions

- `kadr_core::frame`: `PixelFormat::Rgba8` only (NV12 enters with the GPU path, spec §12 — no empty code), `FramePool` (buffers by exact byte length, bounded free bytes, counts fresh allocations), `PooledBuf` (returns to the pool on drop), `CpuFrame { width, height, stride, format, color, data }`. `kadr-core` cannot see `kadr-scene`'s `SizeU`, so frames carry `width`/`height`.
- `LayerInput` is recursive: a transition layer's input is `LayerInput::Transition { from, to }` (the spec sketch had `None` for transitions and inputs "recursive for transitions"; an explicit variant keeps the two parallel lists impossible to mis-pair).
- `RenderTarget::Cpu(CpuTarget)` borrows any byte slice with a stride, so M4 can render straight into the display buffer (UI-thread copies 0) and M5 into the encoder's buffer.
- Missing input → drawn as a `Solid` of `Rgba::MISSING` (contract updated).
- Contract updates carried from the M0–M1 reviews (done before any golden image): fusing only within one layer; output stored after every top-level layer and each transition buffer before mixing; crop intersected with the decoded frame before rounding to whole texels, texel indices capped at `tw − 1`/`th − 1`; renderer premultiplies straight scene colours (spec §4.1 wording fixed).
- Cost: `layer_cost`/`scene_cost` → `Cost { class, ops }` (spec §7.2), interface and a unit test only.

## Tasks

### Task 1 — `kadr_core::frame` (done in the plan commit)
Pool reuse, byte budget, detached buffers, rows by stride — unit tests in `frame.rs`.

### Task 2 — `kadr-render` API and cost (done in the plan commit)
`lib.rs`: `Renderer`, `PreparedFrame`, `RenderInputs`, `LayerInput`, `MissingReason`, `RenderTarget`, `CpuTarget`, `RenderStats`, `RenderError`. `cost.rs` with a unit test.

### Task 3 — `CpuRenderer` (Opus)
`crates/render/src/cpu.rs`, following the contract to the letter:
- validate target size = `scene.output.size` and inputs parallel to layers (count; `Transition` input for a transition layer) → `RenderError`;
- margins opaque black, canvas area starts as the premultiplied background;
- per top-level layer: output→local inverse affine (`canvas→output` ∘ `placement.to_canvas()`), iterate only the output rows/columns of the layer's bounding box (+1 px for coverage), rows in parallel (rayon);
- bilinear sampling with the crop/texel clamping rules; straight texels premultiplied per texel before filtering; opaque frames' alpha taken as 1;
- effects (`ColorAdjust`) on unpremultiplied colour, clamp once at the end; exposure via a 256-entry LUT per layer **with linear interpolation** (or `powf`) — within ±1 LSB of the formula;
- coverage from signed distances to the four crop edges in output pixels;
- blend Normal/Add/Multiply/Screen on premultiplied values incl. alpha; store rounded (half up) after every layer;
- transitions: `from`/`to` into two pooled buffers (transparent, no background, margins untouched), stored rounded, mixed (Dissolve, DipToColor, Wipe per contract), composited pixel-for-pixel with the layer's opacity and blend;
- fast path: an opaque frame whose texel grid coincides with the output pixel grid (identity mapping, full crop, opacity 1, Normal, no effects) is a row copy (alpha forced to 255 for `Opaque`);
- no frame-sized allocation per frame after warm-up (transition buffers from the renderer's `FramePool`; no per-frame `Vec` of frame size).
Tests in `cpu.rs` and `crates/render/tests/golden.rs` (see Task 5).

### Task 4 — Independent reference renderer (Sonnet, written from the contract text only)
`crates/render/tests/reference/mod.rs`: a deliberately naive, single-threaded f64 implementation of the contract (no LUTs, no fast paths, no bounding boxes: every output pixel, every layer). `crates/render/tests/reference_parity.rs`: seeded pseudo-random scenes (own xorshift PRNG, no new crate) — 1–4 layers, random placement/rotation/scale/anchor/crop, opacity, blend, colour adjust, straight/opaque/premultiplied frames of random sizes, solids, missing inputs, transitions of all three kinds with nested layers, odd output sizes and letterboxed outputs — rendered by both; every channel of every pixel within ±1 LSB. At least 300 scenes; a failure prints seed, pixel and both values.

### Task 5 — Golden images and colour references
`crates/render/tests/golden.rs`, PNGs in `crates/render/tests/golden/` (small, e.g. 160×90 output of a 320×180 canvas). Compare ±1 LSB; `KADR_UPDATE_GOLDEN=1` rewrites; a missing golden fails the test. Cases: `alpha_over` (straight-alpha PNG-like frame over video), `multi_layer` (3 layers incl. PiP), `transform_rotation`, `crop_in_place`, `opacity`, `blend_modes` (four tiles), `dissolve_50`, `dip_to_black_25`, `wipe_40`, `letterbox_margins`, `missing_media`. Every golden is inspected visually before it is committed. Exact-value tests (no PNG): saturation 0 → grey with Rec.709 weights; 50 % red over opaque blue = (128, 0, 128) (premultiplied: 0.5·255 = 127.5 → 128, blue 0.5·255 → 128); an opaque layer at opacity 1 leaves its pixels unchanged; background premultiplied.

### Task 6 — `kadr-bench render` (Sonnet)
Synthetic in-memory frames; outputs 1920×1080 and 3840×2160; rows: `1 layer` (full-frame opaque, fast path), `1 layer scaled` (general path), `2 layers` (full + PiP 0.3 rotated), `3 layers` (+ straight-alpha logo), `transition` (dissolve between two full-frame layers only). p50/p90/max ms over ≥ 60 frames after 5 warm-up frames; `frame-sized allocations per frame` after warm-up from the counting allocator (target 0). Unit test: the bench scenes have the advertised layer counts.

### Task 7 — Review (Opus), numbers and exit
Opus review of the whole stage against the contract; fixes; `docs/perf/2026-09-30-m2-render.md` with the bench table (release), test counts, and the verdict per exit criterion. Commit.
