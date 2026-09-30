# Render Foundation M5 — Export on the new pipeline

> Continues `2026-09-30-render-foundation.md`. Same Global Constraints. Branch `feature/render-pipeline`.

**Goal:** export renders every frame with the same evaluator, resolver and `CpuRenderer` as the preview (what you see is what you get, including PiP, logos, crop, rotation, opacity, colour and transitions) and pipes raw RGBA into an FFmpeg encoder; audio still goes through the existing FFmpeg audio graph.

**Exit criterion (stage table):** PSNR ≥ 40 dB vs legacy export on a single-track reference; A/V sync test (20 min, ≥ 50 cuts, ≥ 10 transitions) within ±1 frame; export fps 1080p/4K vs M0.

## Decisions

- **Encoder in `kadr-media`:** `EncodeJob { output, width, height, rate, frames, total, audio: Vec<ExportAudio>, settings }` → `MediaBackend::start_encode(&EncodeJob) -> Box<dyn FrameEncoder>`; `FrameEncoder::{write_frame(&[u8]), finish(), abort()}`. FFmpeg reads `-f rawvideo -pix_fmt rgba -s WxH -framerate R -i -`; video goes through `[0:v]scale=in_range=pc:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]` inside the same `filter_complex` as the audio graph (the audio half of today's `build_graph`, factored out unchanged); output tagged `-colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv` (review carry-over: export colour is explicit). Writes to `<output>.part`, renamed on success, deleted on failure/cancel. Progress is counted in frames written, not parsed from FFmpeg.
- **Frames:** `N = rate.time_to_frame_round(total)` (as legacy), frame `n` at `rate.frame_to_time(n)`; output even-sized (yuv420p); the renderer letterboxes when the even size changes the aspect.
- **`ExportRunner` in `kadr-playback`:** own `Resolver` in `Mode::Export` (never drops, waits for every layer); prefetch the scene ~1 s ahead so the next clip's session opens before its cut; render frame `n+1` while a writer thread pipes frame `n` (two pooled buffers); cancel → abort; a missing/offline layer fails the export with a clear error (never silently exports MISSING red) unless the caller opted in.
- **`ProjectScenes` moves** from `apps/editor` to a small crate `crates/project-scenes` (kadr-timeline + kadr-project + kadr-playback) so the app and `kadr-bench` share one `SceneSource` over a project.
- **Transition timing** (review carry-over): export no longer computes transitions itself — the evaluator does, for preview and export alike — so the old frame-rounding differences disappear; the PSNR reference therefore uses cuts only (legacy rounds transitions differently by design).
- **Switch:** `KADR_RENDERER=legacy|cpu` selects the export path too (default `cpu`); the legacy path stays until M6.

## Tasks

1. **Encoder** (Sonnet) — `crates/media/src/encode.rs` (+ audio graph factored out of `export.rs`), trait method, tests: a 2 s RGBA gradient + tone encodes to the right frame count, size, tags and duration; colour round trip (RGB bars rendered → encoded → decoded with `open_source` BT.709 limited) within ±3; cancel deletes `.part`.
2. **`ExportRunner` + `project-scenes` crate** (Opus) — runner, progress, cancel, missing-media policy, prefetch, double buffering; tests with fake decoders (frame count, order, cancel) and one FFmpeg integration test; app switch in `apps/editor/src/export_ui.rs`.
3. **Measurements** (Sonnet) — `kadr-bench export` (the M0 scenario 1080p30 20 s 10 cuts 2 dissolves Balanced, plus a 4K row and a 3-layer 1080p row), PSNR vs legacy (single track, cuts only, BT.709-tagged source, PSNR on Y'CbCr planes with FFmpeg's `psnr` filter), the 20-minute A/V test (flash + click generator with the click on the flash frame's start sample; ≥ 50 cuts at mid-second positions, ≥ 10 short transitions away from the flashes, linked audio), `docs/perf/2026-09-30-m5-export.md`.
4. **Review** (Opus) and exit.
