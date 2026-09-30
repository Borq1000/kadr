# Render Foundation M3 — Playback (`kadr-playback`)

> Continues `2026-09-30-render-foundation.md`. Same Global Constraints (rayon allowed since M2). Branch `feature/render-pipeline`.

**Goal:** real frames for scenes. Decoder sessions produce **source** frames (not composited) at the size the layer needs, with an explicit YUV matrix and range, into pooled buffers; a frame cache, generations and cancellation make scrubbing drop stale work; read-ahead and early opening of the next clip make playback smooth; `PreviewPlayer` paces rendered frames against a clock and records telemetry.

**Exit criterion (stage table):** seek latency p50/p90 vs M0 (every row of M0 §7 strictly below); scrub test proves stale requests are dropped; 0 frame-sized allocations per decoded frame in steady state; simulated-clock A/V test (≤ 1 frame drift over 10 min).

## Measurement that shaped the design (2026-09-30, same machine as M0)

One FFmpeg process per seek, first frame to a pipe, 5 (1080p) / 4 (4K) seeks:

| clip | `-hwaccel auto` | software |
|---|---:|---:|
| h264 1080p seek (full) | 399 ms | 97 ms |
| h264 4K seek (½) | 692 ms | 247 ms |
| hevc 4K seek (½) | 518 ms | 267 ms |
| h264 4K sequential decode, ½ / full | 129 / 105 fps | 236 / 152 fps |
| hevc 4K sequential decode, ½ / full | 156 / 118 fps | 198 / 127 fps |

`-hwaccel auto` spends ~300 ms initialising the GPU decoder per process and then downloads every frame to RAM; on the CLI path (frames must reach RAM through a pipe anyway, spec §12.3) it is slower on both counts. **Sessions decode in software by default**; `hwaccel` stays an option on the request (off) for a later in-process decoder.

## Decisions

- **Source decode in `kadr-media`** (`SourceRequest`, `SourceStream`, `MediaBackend::open_source`, `MediaBackend::decode_still`): FFmpeg only decodes, normalises to the source's constant frame rate and scales to the exact requested size (SAR and rotation absorbed: FFmpeg autorotates, the scale is non-uniform to the display size); `scale=…:in_color_matrix=<bt709|bt601|bt2020>:in_range=<tv|pc>:out_range=pc,format=rgba` from the source `ColorInfo` (no matrix option for `Matrix::Rgb`). Frame `n` of a stream opened at `start_frame = s` is source frame `s + n`, where source frame `k` is the one shown at `rate.frame_to_time(k)`. Seek target `max(0, frame_to_time(s) − frame/4)` (robust to container timestamp rounding). Reads go straight into caller buffers (pooled) — no allocation per frame. Stills (images) decode once to the exact size. Transfer functions are not converted in SDR mode (sRGB images and BT.709 video are both display-referred; spec §6 "not now").
- **`kadr-playback` depends on** `kadr-core`, `kadr-scene`, `kadr-render`, `kadr-media` — not on timeline or project. The app supplies scenes and media through `SceneSource` (implemented over the project in `apps/editor` in M4, and in `kadr-bench`).
- **Resolver**: per media layer, decode size = the layer's output footprint (`placement.size × |scale| × k`), capped at the display size, quantised up to a step of the display size (1, ¾, ½, ⅜, ¼, ⅛) so animated scale does not fragment the cache; even sizes not required (RGBA). Source frame = `rate.time_to_frame(source_time)`, clamped to the last frame (`source_time ≥ duration` → last frame at or before it, review carry-over); a stream that ends early serves its last frame for later indices. Offline media → `LayerInput::Missing(Offline)`; decode error → `Missing(DecodeFailed)`; not in time (playback deadline) → `Missing(NotReady)`.
- **Sessions**: a long-lived decoder per (media, size) with its own thread, current position and read-ahead; a request ahead of the position within a forward window (≤ 2 s of frames) is read forward, anything else reopens the process at the target. Several sessions per (media, size) are allowed (two layers of the same source at different times, e.g. a transition inside one file); idle sessions close after a timeout; a global cap bounds processes.
- **Generations**: every `show`/`play` increments a generation; a session always works towards the newest target and abandons older ones (a superseded seek never finishes decoding); a waiter for a superseded request returns at once.
- **Cache**: LRU by bytes, key `(media, size, frame)`, values `Arc<CpuFrame>` from a `FramePool` (evicted frames return their buffers).
- **Player**: `PreviewPlayer` thread with `show(t)`, `play(from)`, `stop()`; renders with `CpuRenderer` into a buffer supplied by a `FrameSink` (M4 plugs a Slint buffer in, so presenting is not a copy); paces against a `Clock` (audio clock in the app) with a pure pacing function; drops late frames; prefetches the scene one lookahead ahead so the next clip's session is open before the cut.
- **Telemetry**: `FramePerf` per frame — evaluate, resolve (wait), decode per layer (time the session spent on that frame), composite, present, cache hits/misses, pool allocations, dropped, seek latency — into a shared `PerfRing`.

## Tasks

### Task 1 — carry-overs in `kadr-core` (Sonnet)
- `ColorInfo::from_ffprobe`: BT.2020 primaries with an untagged matrix → `Matrix::Bt2020Ncl` (not the size guess); `is_supported_sdr` also requires `matrix ∈ {Rgb, Bt709, Bt601}`; `Matrix::Rgb` is always `Range::Full` (tagged `gbr` + `tv` included — one rule for RGB).
- `PerfRing::summary`: seek statistics only from shown (not dropped) frames.
- Tests for each.

### Task 2 — source decoding in `kadr-media` (Opus)
`SourceRequest { path, rate, start_frame, width, height, color, hwaccel }`, `trait SourceStream: Send { fn read_into(&mut self, buf: &mut [u8]) -> Result<bool> }`, `MediaBackend::open_source`, `MediaBackend::decode_still(path, width, height, out: &mut [u8])`. Integration tests (skip without FFmpeg): frame exactness — a synthetic clip whose luma encodes the frame number (e.g. `geq`, lossless x264) at 25 and 30000/1001 fps: opening at many `start_frame`s and reading on yields exactly `start_frame, start_frame + 1, …`; colour — RGB bars encoded Rec.709 limited and Rec.601 limited (tagged) decode back to the original RGB within ±2 (spec §6 tests 1–2), and decoding the 601 file with a 709 matrix differs by more than 2 (the test can fail); non-square SAR and rotation produce the requested size upright; `read_into` does not allocate per frame; stills decode to the exact size with straight alpha.

### Task 3 — `kadr-playback`: cache, sessions, resolver (Opus)
Public API (shape binding, details free): `SceneSource` (`scene_at`, `media`, `duration`, `frame_rate`, `canvas`), `MediaSource { path, kind, rate, duration, display_size, color, online }`, `Decoders` factory trait with `FfmpegDecoders` over `Arc<dyn MediaBackend>` and a fake for tests, `FrameCache`, `Resolver::prepare(scene, source, mode, perf) -> RenderInputs` with modes `Scrub { generation }` (wait until ready or superseded), `Deadline(Instant)` (playback), `Export` (wait, never drop), `Resolver::prefetch`. Tests with the fake decoder: exact source frame per layer; last-frame clamp; decode size quantisation; same media twice in one scene gets two sessions; **scrub test** — 50 rapid requests while decoding is slow: the sessions never finish a superseded target, frames decoded ≪ requests, the last request is served; offline → Missing; cache hit on repeat; steady-state sequential reads allocate no new pool buffers.

### Task 4 — `PreviewPlayer`, pacing, telemetry (Opus, same agent after Task 3)
`Clock` trait, `FrameSink` trait (buffer + present), `PreviewPlayer::{new, set_source, show, play, stop, generation}`, pure `pace(now, frame_time, frame_duration) -> Pace { Wait(Time) | Present | Drop }`. Tests: simulated clock — 10 minutes at 29.97 fps with jittery decode/render durations (deterministic PRNG), the frame presented at each clock step is never more than one frame from the clock, drops counted; show → generation superseded → no present of the stale frame; telemetry fields filled (no seek latency on dropped frames).

### Task 5 — `kadr-bench playback` and numbers (Sonnet)
Seek rows for the same clips/sizes and seek times as M0 §2 (15 samples, `seek_times`), measured as request → rendered frame through `Resolver` + `CpuRenderer` with a fresh position each time; sequential decode fps and frame-sized allocations per decoded frame after warm-up (counting allocator; target 0); `docs/perf/2026-09-30-m3-playback.md` with the table next to M0's and a verdict per row.

### Task 6 — Review (Opus) and exit
Opus review of Tasks 1–5; fixes; commit.
