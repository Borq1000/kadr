# Render Foundation M6 — Remove the legacy paths

> Continues `2026-09-30-render-foundation.md`. Same Global Constraints. Branch `feature/render-pipeline`.

**Goal:** one composition path. Everything that composited video with FFmpeg filters, or chose "the one top clip", is deleted; FFmpeg only demuxes, decodes, scales to a requested size and encodes.

**Exit criterion (stage table):** full test suite green; no perf regression > 10 % vs the M4/M5 numbers.

## Also (from the M4 review)

- `EditCommand::AddTransition` snaps `at` to the cut it names: when `|at − boundary| ≤ half a frame` for exactly one adjacent clip pair on the track (their `timeline_out`/`timeline_in`), it snaps there; otherwise it stays as given (the evaluator's stale rule still applies). UI paths are already exact; this makes MCP clients with ms-rounded times work. Test both sides.
- `docs/mcp.md`: `AddTransition` documents the snap.

## Delete

- `kadr-media`: `VideoLook`, `look_filter`, the video half of `build_graph` and the legacy `export::run`, `ExportPlan`/`ExportVideo`/`ExportVideoSource`/`ExportTransition*`, `MediaBackend::export`; `StreamRequest.look` and `px_scale` (the stream keeps scaling + letterbox for video analysis, the only remaining user). Keep `ExportAudio`, `ExportSettings`, the audio graph and the encoder.
- `kadr-timeline::composition`: `video_at`, `video_segments`, `transitions_into` (and `VideoSource`/`VideoSegment`); the UI's "is there video under the playhead" checks (`timeline_ui.rs`, `multicam_ui.rs`) use the scene evaluator instead. `audio_segments` stays. `scene_equivalence.rs` (its oracle was `video_at`) is retired: its cases live on as evaluator unit tests where they add coverage.
- `apps/editor`: the legacy preview worker and `KADR_RENDERER` (a single CPU renderer; the env var and its docs go), the legacy export plan builder.
- `kadr-bench`: the legacy scenarios that can no longer run (`baseline` seek/decode via the old stream keep working without `look`; `export_legacy` goes). M0 numbers stay in their document.
- Docs: `ARCHITECTURE.md` and `docs/mcp.md` describe the new pipeline; the stage table in `2026-09-30-render-foundation.md` marks M2–M6 done with links to the per-stage docs.

## Measure

Quiet machine (no agent builds running): `kadr-bench render`, `playback`, `export`, `live` (1 and 3 layers, 60 s) — each compared with the M2–M5 documents, verdict per row (regression > 10 % = fail, investigate). Results in `docs/perf/2026-09-30-m6-final.md`, together with the before/after summary against M0.

## Tasks

1. Deletion and replacements (Sonnet), full `cargo test --workspace`, `cargo clippy --workspace --all-targets`.
2. Final measurements and doc (Sonnet).
3. Final review of the whole branch (Opus) — dead code, docs, the four stage exit criteria re-checked.
