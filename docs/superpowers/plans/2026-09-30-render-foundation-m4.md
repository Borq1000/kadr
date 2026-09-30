# Render Foundation M4 — Preview on the new pipeline

> Continues `2026-09-30-render-foundation.md`. Same Global Constraints (every user-visible string through `kadr_i18n` in RU and EN; tests never touch the user's running `kadr.exe` or `%LOCALAPPDATA%\Kadr`; rename `target/debug/kadr.exe` before rebuilding). Branch `feature/render-pipeline`.

**Goal:** the editor's preview shows what the timeline really is — picture-in-picture, logos, transitions, crop, rotation, opacity and colour — through Timeline → evaluator → `kadr-playback` → `CpuRenderer`, with the frame rendered straight into the display buffer.

**Exit criterion (stage table):** MCP acceptance — PiP, transitions and clip look visible in preview; dropped frames ≤ 1 % over 60 s for 1080p 3 layers at ½ and 1080p 1 layer at Full; UI-thread frame copies 0; numbers vs M0.

## Decisions

- **Switch:** `KADR_RENDERER=legacy|cpu`, default `cpu`. The legacy worker stays compiled until M6 so the two can be compared on the same project.
- **`SceneSource` over the project** (`apps/editor/src/scene_source.rs`): an immutable snapshot (`Arc<Project>` clone) taken whenever the timeline or a live inspector drag changes it; `media()` answers from the snapshot's assets (online = file exists at snapshot time). The player never sees the live `Project`.
- **Bypass ("before")**: strips the clips' effects (colour) and keeps geometry — with several layers, resetting geometry would stack every PiP full-frame. Legacy bypass also reset transforms; the badge meaning ("before grading") is what users compare.
- **Display without UI-thread copies:** the `FrameSink` hands the renderer a `slint::SharedPixelBuffer<Rgba8Pixel>` created (or reused from a small ring) on the player thread; the UI thread only wraps it in an `Image` (`from_rgba8_premultiplied`) and sets the property. Buffer reuse is measured (pointer check after `make_mut_bytes`), never assumed.
- **Gap / empty / offline:** an empty scene renders black (no more "gap" text over a stale frame — the text overlay stays for the empty timeline and gaps); offline media renders the contract's `MISSING` colour with the existing offline caption shown as a badge, not instead of the frame.
- **Clip name overlay:** the top-most media layer's clip.
- **MCP `get_frame`:** renders through the new pipeline (own resolver + renderer, `Export` mode, off the UI thread) so tools see exactly what the preview shows.
- **DEV overlay:** a small panel in the preview corner (fps, render/decode/present ms, drops, cache hits), toggled by `Ctrl+Shift+D` and the View menu; strings in RU/EN.
- **Telemetry** (carried from M0–M1 reviews): cumulative since-reset counters (frames, dropped) next to the window; atomic read-and-reset `take_summary`; one shared ring-size constant; M0 doc wording corrected; M4 drop target measured with `kadr-bench live --seconds 60` at ½ with the same clips.
- **Evaluator** (carried): a transition whose `from` and `to` both cover the canvas opaquely occludes the tracks below (no third decode during a full-frame dissolve); equivalence tests with a muted track and a disabled clip.

## Tasks

1. **Evaluator carry-overs** (Sonnet) — `crates/timeline/src/scene.rs` cull + tests; equivalence tests.
2. **Telemetry carry-overs** (Sonnet) — `crates/core/src/perf.rs` (`take_summary`, cumulative counters, `PERF_RING_FRAMES`), `apps/editor/src/perf_view.rs`, M0 doc wording.
3. **Preview integration** (Opus) — `scene_source.rs`, `PreviewController` backends, Slint sink, request/play/stop wiring, edits and live inspector drags re-snapshot, empty/offline/clip-name UI, bypass, `get_perf`/`get_frame` on the new path, DEV overlay (i18n), clean shutdown.
4. **`kadr-bench live` for M4** (Sonnet) — `--layers 1|3`, `--quality full|half`, `--renderer legacy|cpu`, 60 s runs; results doc `docs/perf/2026-09-30-m4-preview.md` vs M0.
5. **MCP acceptance** (Opus) — headless Kadr through kadr-mcp with the user's real media (`KADR_REAL_MEDIA` folder: vertical video, logo PNG with alpha, transitions): frames with PiP, logo, dissolve/wipe/dip, crop/rotation/opacity/colour visibly right; screenshots of the UI in RU and EN at a small window size.
6. **Review** (Opus) and exit.
