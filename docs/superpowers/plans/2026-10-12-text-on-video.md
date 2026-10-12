# Text on Video (Titles) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Text clips on video tracks — typed on the preview or in the inspector, styled per word (colour, font, size), with gradients, outlines, shadows, glow, plaques, curves, perspective, colour emoji, letter/word/line animations and ~20 ready styles — rendered crisp at any output size, identical in preview and export, reachable from MCP and the AI assistant.

**Architecture:** `kadr-project` stores a `TextDoc` on the `Clip` (format 4; a text clip's asset is the nil `TEXT_ASSET`). A new pure-Rust crate `kadr-text` lays text out with `parley` (fonts from `fontique`: bundled, system, user), rasterises it with `swash` into a premultiplied raster, animates it per glyph and holds the built-in styles. The evaluator (`kadr-timeline`) turns a text clip into `LayerContent::Text(TextLayer)` whose `Arc<dyn TextPicture>` (a `kadr_text::TextFrame`) knows its bounds and draws itself; the resolver (`kadr-playback`) asks it for a raster at the output footprint and caches it; the CPU renderer samples it like a media frame. The editor adds text clips to the timeline, a «ТЕКСТ» inspector section with a rich text field, a colour picker, on-preview handles with snapping and inline typing, a «Текст» tab, MCP ops and AI commands.

**Tech Stack:** Rust 2024 workspace (windows-gnu), `parley` 0.11.1 (+ `fontique` 0.11.1, `harfrust`, `skrifa`), `swash` 0.2.10 (+ `zeno` 0.3.3) — all already in `Cargo.lock` through Slint; Slint 1.18.1 (`apps/editor/ui/*.slint`); `serde` / `serde_json`; `kadr-i18n` JSON catalogs; `png` 0.17 (goldens); `cargo test` / `cargo clippy` in Git Bash.

**Spec:** `docs/superpowers/specs/2026-10-12-text-on-video-design.md` (Russian; §0 scope, §1 behaviour, §2 model, §3 time, §4 animations, §5 rendering, §6 UI, §7 commands/MCP/AI, §8 perf, §9 tests). Binding inputs read with it: `.superpowers/sdd/2026-10-12-titles/owner-decisions.md` (owner) and `.superpowers/sdd/2026-10-12-titles/decisions.md` (controller decisions T-D1…T-D14). Where this plan's «Spec refinements» differ from T-D*, the refinement wins. Style of earlier plans: `docs/superpowers/plans/2026-10-11-effects-e3.md`. Line numbers are as of `bc1cfb9`; earlier tasks move them — find code by symbol. **Pre-flight (2026-10-12):** the API facts come from a compiled probe of parley / swash / fontique (`C:\Temp\kadr-text-probe`), and this plan's code blocks for Tasks 1–13 were applied to a scratch copy of `bc1cfb9` (`C:\Temp\kadr-full`): it builds, the tasks' tests pass with the whole workspace suite, and the text goldens were looked at. Not exercised there: the prose-only parts (Task 11's `text_data` fields, the shadow / glow / shape / tilt / loop / highlight rows and their pass-through wiring; Task 12's save-style button; Task 14). Anchor lines quoted in steps may differ from the file by one indentation level — match them by text. E3 Tasks 5–10 resume after this plan: keep `crates/timeline/src/scene.rs`, `effects_ui.rs`, `fx_thumbs.rs` changes to the small hooks given here.

## Global Constraints

- **Strings**: every user-facing string is in both `crates/i18n/locales/ru.json` and `en.json` (RU + EN); Slint `Tr.t("key")` / `Tr.t1` / `Tr.t2`, Rust `kadr_i18n::{t, tf, tn}`; new entries go before the closing `}` of each file (comma after the previous last line); history labels are `cmd.*` keys in the noun style («Добавление текста», «Текст: размер», «Стиль текста: Неон»). `crates/i18n/tests/sources.rs` scans `apps/editor/src`, `crates/ai/src`, `crates/timeline/src`; Task 12 adds `text_ui::tests::every_text_string_exists_in_both_catalogs` for keys built at run time (style names, presets).
- **Time**: all time is integer flicks (`kadr_core::Time`); f64 only for a fraction (progress, positions) or a rate (loop speed in Hz); reveal durations and staggers are `Time`. MCP arguments are integer milliseconds within ±10 000 000 000.
- **Undo**: `EditEngine` records an entry for every successful command; commands return `EditError::NoOp` when nothing changes and callers check "nothing would change" before sending; one gesture = one entry — a slider, colour drag, preview drag or typing session changes the clip live (`LiveEdit` / `TextSession`) and records one command on release / end; a typing session interrupted by any other command, undo, redo, save or selection change is **committed first, never rolled back**.
- **Slint**: interactive repeaters keep stable models (`util::sync_rows_by_key`, `util::reuse_rows`, `inspector::same_or_new`; never a `for` over an array literal that depends on changing state); icons are `Icon` paths (viewbox 24) in `global Icons`, never ◀ ▶-style glyphs; in a `PopupWindow` handler call the callback **before** `popup.close()`; a fixed-height row in a layout gets `vertical-stretch: 0`; a struct reaches Rust only if exported from `app.slint`; a text field is never bound to a struct the inspector refresh replaces (its caret would reset while typing): it binds two-way to its own `in-out` property, which Rust sets only when the shown text must change.
- **Goldens**: PNGs compared within ±1 LSB, (re)written only with `KADR_UPDATE_GOLDEN=1 cargo test -p <crate> --test <file>`, and every rewritten PNG is looked at (Read tool) before it is committed.
- **Dependencies**: pure Rust only (toolchain `x86_64-pc-windows-gnu`, no C/C++ build); the text crates are pinned to the versions in `Cargo.lock`: `parley = { version = "=0.11.1", default-features = false, features = ["std", "system", "complex-scripts"] }`, `swash = { version = "=0.2.10", default-features = false, features = ["std", "scale", "render"] }`; `fontique` and `zeno` are used through `parley::fontique` and `swash::zeno`. `kadr-render` depends on `kadr-scene` and `kadr-core` only; `kadr-playback` never on the project, the timeline or `kadr-text` (it calls `TextPicture` through the scene).
- **Fonts**: only OFL or Apache-2.0 fonts with Cyrillic, each family's licence file committed next to it in `crates/text/fonts/<dir>/`; total of `crates/text/fonts` ≤ 12 MB (test `bundled_fonts_fit_the_budget`).
- **Text raster contract**: a text raster is premultiplied RGBA8 in the working space (`ColorInfo::WORKING_SDR`); colours in a `TextDoc` are working-space values (like `Solid`), straight alpha, `"#rrggbbaa"` in JSON; sizes are canvas pixels of the sequence.
- **Tests**: `cargo test --workspace --exclude kadr-mcp` and `cargo test -p kadr-mcp`; `cargo clippy --workspace --all-targets` without new warnings in touched code. A headless Kadr may lock `target\debug\kadr.exe`: a build that fails only at replacing it is not a code failure. Use your own `CARGO_TARGET_DIR` (e.g. `C:\Temp\kadr-target-text`) if another build holds the default one.
- **Editor state**: a new `App` field is initialised in the `App { … }` literal of `app.rs` (`Default::default()` or the value given); a Slint `if` cannot wrap a `for` directly (use `visible:` on the repeated element or a wrapping element).
- **Process**: work on branch `feature/effects`. Commits end with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never print or commit API keys. The owner's media in `C:\Users\Boris\Desktop\000-Brazil` is read-only (never modified, never hard-coded in committed tests). Implementers do not run the app or the kadr MCP tools; live acceptance is the controller's (Task 14).

## Spec refinements (decided here; report them to the owner)

1. **A text layer carries its picture, not the document** (T-D5 said a doc snapshot + an injected rasteriser): `TextLayer { picture: Arc<dyn TextPicture> }` with `trait TextPicture { key(), render(scale, pool) }` in `kadr-scene`. The resolver calls it — no injection, and neither `kadr-scene`, `kadr-render` nor `kadr-playback` learns about fonts or the project model.
2. **The evaluator gets the box from `kadr-text`**: `TextFrame::bounds()` (layout box + room for outlines, shadow, glow, plaque, animation, curve, perspective) is the layer's `placement.size`; the anchor is the layout box's centre inside it, so `Transform.x/y` is the text block's centre offset and growing text stays centred; handles frame the layout box.
3. **Loops run in source time; highlight has a progress span; the seed is the document's** (T-D4 said "seeded by clip id"): a split gives its right piece a new `ClipId`, so a clip-id seed would change scatter / shake / flicker at the joint. `TextDoc.seed` is set once at creation and kept by split; loops use the clip's source time (continuous across split and trim); the highlight's `span: (from, to)` splits like an effect's progress.
4. **Reveal timing is `Time`** (flicks), loop speed Hz; reveals that do not fit the clip shrink together, like fades.
5. **`Fill` is `{ colour, gradient: Option<..> }`** (not an enum) and closed choices (`Align`, `Unit`, `TextShape`) read leniently (unknown → default): a newer file still opens; `extra` maps on `TextDoc`, `TextRun`, `RunStyle`, `TextStyle`, `Fill`, `ParagraphStyle`, `TextAnim`, `Reveal`, `LoopAnim`, `Highlight`.
6. **Fonts**: bundled in `crates/text/fonts/` (not `assets/fonts/`): Montserrat, Inter, Rubik, Oswald, Roboto Condensed, Playfair Display, PT Serif, Lobster, Comfortaa, Unbounded, Caveat, Russo One, Press Start 2P, Exo 2 (14). **Bebas Neue is left out**: the OFL Google Fonts build has no Cyrillic (Task 3's `bundled_fonts_cover_cyrillic` would fail); Oswald covers the condensed display role. A project stores the family name only — no font file paths (T-D3's "fonts referenced by a project path" dropped): a missing family renders in Montserrat with a warning and the name is kept.
7. **Colour emoji are core, not advanced**: the probe (`C:\Temp\kadr-text-probe`) showed Segoe UI Emoji renders in colour through swash's `ColorOutline` (COLR v0), rotated too; Task 3 draws them.
8. **Explicit word times for the highlight** (T-D4 "or explicit word times when present") are left to the subtitles sub-project; `Highlight.extra` keeps them if a newer file has them.
9. **Preview zoom 150 % / 250 %** magnifies the preview output (capped at 1920 px wide), as for video; "crisp at any size" covers the output size (preview quality, export resolution) and any clip scale.
10. **Rich selection uses `TextInput`'s byte offsets**: Slint 1.18.1 exposes `anchor-position-byte-offset` / `cursor-position-byte-offset` as readable `out` properties (marked internal) and `set-selection-offsets()`; the inspector's `TextField` forwards them (Task 11); Rust remembers the last non-empty selection because a focus change collapses it; the scope row says «Стиль применяется к: выделению «…»». Cargo.lock pins 1.18.1; re-check on a Slint upgrade.
11. **`AddText { clip, track: Option<TrackId> }`** chooses the track in the command (one undo entry): the given video track; else the **topmost** video track when it is unlocked and free over the clip; else a new video track above all — a new title is never hidden under a picture.
12. **Text clips**: only on video tracks (`KindMismatch` elsewhere); speed is not offered in the inspector; trim start may go before source 0.
13. **A text layer with no raster draws nothing** (never the red «missing media»); it never occludes in culling; «До» drops the clip's effects, not its text animation.
14. **Preview handles for every video clip** (T-D10: "enable it for all"); a click on the preview selects the topmost clip under the pointer, a click on empty canvas keeps the selection; double-click typing for text clips only; after «Добавить текст» inline typing starts with the text selected.
15. **`add_caption {start_ms, end_ms, text}`** keeps its existing shape and becomes a text clip of style `caption` (bottom, plaque per line).
16. **ALL CAPS** upper-cases at layout; the stored text keeps its case.
17. **Styles are 1080p-relative**: `styles.json` sizes are for a 1080-line canvas and scale with the sequence height when applied (22 built-in styles; a saved user style is stored the same way, at 1080p, in `text-styles.json` in the app's data folder, `App::dirs.data`).
18. **Inspector details** (spec §6.3 adjusted in its text): the text field grows with its text (the inspector scrolls) instead of scrolling after six lines; weight is a 100…900 slider (steps of 100) — five named segments do not fit the 290 px inspector; an animation is previewed by a «Показать» button (plays the clip from its start, at most 4 s, and puts the playhead back) rather than on every change, which would fight the user's own playhead; the plaque is drawn for straight text only.
19. **A typing session and slider drags never fight**: typing goes through `TextSession` (live, finished into one command), sliders through the existing `LiveEdit`; a slider's first live change finishes a running typing session first, and `App::execute_as` / `undo` / `redo` / `save_to` / a selection change finish it before they act.

## Review Focus

1. **Typing in a text field vs the editor's shortcuts** — while the inspector field or the preview's inline field has focus, Space, Delete, arrows and Ctrl+Z edit the text (Slint's `TextInput` consumes them; Slint 1.18.1 lets Esc, Tab and other Ctrl combinations bubble), Esc finishes typing before anything else, Ctrl+S commits the typing and then saves, Ctrl+T commits and adds a new text, and any other command, undo, redo, save or selection change commits the session instead of rolling it back (tests: Task 7 `a_typing_session_is_one_entry_and_survives_another_command`, `a_session_without_changes_records_nothing_and_a_finished_one_is_saved`; the key routing itself has no unit seam — `App` needs a window — so Task 14 item 2 checks it live).
2. **A title stays crisp at any output size** — at a 4K export of a 1080p sequence, or a clip scaled 3×, the raster is drawn at the output footprint, never upsampled from a small one (tests: Task 2 `text_rasters_follow_the_output_footprint`; Task 4 `a_4k_export_of_a_title_is_sharper_than_an_upscaled_1080p_one`).
3. **Cyrillic, emoji, symbols and missing fonts** — «Привет, мир! 🎉 ✓ 日本» in any bundled font draws no `.notdef` glyph (fallback to system fonts), the emoji in colour, and a project naming a font this machine lacks renders in Montserrat, says so, and saves the name unchanged (tests: Task 3 `cyrillic_emoji_and_symbols_never_draw_notdef`, `bundled_fonts_cover_cyrillic`, `a_missing_family_falls_back_and_says_so`; Task 4 `a_missing_font_renders_and_round_trips`).
4. **Undo granularity** — a preview drag, a slider drag, a colour drag and a typing session are one entry each; a click without movement, a double-click + Esc without typing, and a slider or colour released where it started are none (tests: Task 7 `a_release_where_the_drag_began_records_nothing`, `a_session_without_changes_records_nothing_and_a_finished_one_is_saved`; Task 8 `a_drag_is_one_entry_and_a_click_is_none`).
5. **Split and trim of an animated title** — a split in the middle keeps the loop's picture at the joint, gives «появление» to the left piece and «исчезновение» to the right, halves the highlight span; a trim moves the reveals with the edges (tests: Task 10 `a_split_keeps_the_loop_picture_at_the_joint`, `split_hands_in_to_the_left_and_out_to_the_right`, `trim_moves_the_reveals_with_the_edges`).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/project/src/text.rs` (new), `sequence.rs`, `model.rs`, `lib.rs`, `Cargo.toml`, `io.rs` (tests) | `TEXT_ASSET`, `Rgba8`, `TextDoc` and its parts, `set_plain`, `style_range`, `reset_range`, `normalise`, `split_at`, `starter`; `Clip.text`, `Clip::new_text`, `Clip::is_text`; `FORMAT_VERSION` 4 |
| `crates/timeline/src/commands.rs`, `ops.rs`, `tests/text_edits.rs` (new) | `EditCommand::AddText`, `ClipProperty::Text`, text split / trim / insert rules |
| `crates/scene/src/scene.rs`, `lib.rs` | `TextPicture`, `TextLayer`, `LayerContent::Text`, contract «Text» |
| `crates/render/src/cpu.rs`, `cost.rs`, `tests/reference/mod.rs`, `tests/reference_parity.rs` | `Text` arms, sampled like media, nothing without a raster |
| `crates/playback/src/text.rs` (new), `resolver.rs`, `lib.rs`, `tests/resolver.rs` | `TextRasters` cache, `text_scale_step`, `Resolver::text_stats` |
| `crates/project-scenes/src/lib.rs`, `tests/text_pixels.rs` (new), `tests/golden/text_*.png` | top clip names a text clip; end-to-end pixels, export = preview, 4K sharpness, missing font |
| `crates/text/` (new crate): `Cargo.toml`, `build.rs`, `src/{lib,fonts,layout,raster,effects,shape,anim,frame,styles,sample}.rs`, `styles.json`, `fonts/**`, `tests/{golden.rs,anim.rs}`, `tests/golden/*.png` | fonts (embedded by `build.rs`), layout, raster passes, shapes, perspective, animation engine, `TextFrame`, built-in styles, sample renders for thumbnails / font names |
| `crates/timeline/src/text.rs` (new), `scene.rs`, `lib.rs`, `Cargo.toml`, `tests/text_eval.rs` (new) | `text_clock`, `text_layer`; hook in `clip_layer`; `covers_opaquely` arm |
| `apps/editor/ui/color_picker.slint` (new), `apps/editor/src/color_ui.rs` (new), `app_settings.rs` | `ColorSwatch` + popup, `ColorUtil` global, recent colours |
| `apps/editor/src/timeline_ui.rs`, `text_add.rs` (new), `keys.rs`, `persistence.rs`, `ui/timeline.slint`, `ui/theme.slint`, `ui/app.slint` | purple text clips, «Т» button, Ctrl+T, menu item, `add_text_at` |
| `apps/editor/src/text_ui.rs` (new), `font_previews.rs` (new), `inspector.rs`, `app.rs`, `persistence.rs`, `ui/text_inspector.slint` (new), `ui/inspector.slint`, `ui/app.slint` | «ТЕКСТ» section, `TextField` with its selection, `TextSession`, prop edits, font picker (names drawn in their fonts), add font |
| `apps/editor/src/preview_edit.rs` (new), `preview.rs`, `ui/preview.slint`, `ui/app.slint` | handles, hit-testing, snapping, guides, inline typing |
| `apps/editor/src/text_tab.rs` (new), `effects_ui.rs` (tab index), `preview.rs` (try-on in the snapshot), `ui/text_tab.slint` (new), `ui/media.slint`, `ui/effects.slint` | «Текст» tab, gallery with thumbnails, hover try-on, click / drag, «Мои стили», «Сохранить стиль» |
| `crates/ai/src/command.rs`, `context.rs`; `apps/editor/src/mcp_api.rs`, `mcp_state.rs`, `mcp_text.rs` (new); `apps/kadr-mcp/src/tools.rs`, `tests/tools.rs`; `docs/mcp.md` | `add_text`, `set_text`, `add_caption`, `get_state` text, `list_text_styles` |
| `crates/i18n/locales/{ru,en}.json` | every new name |
| `apps/kadr-bench/src/{render_bench,scene_bench}.rs`, `docs/perf/2026-10-12-text.md` (new), `ARCHITECTURE.md` | bench rows, perf record, crate row |

**Order and parallel groups** (disjoint files; each group starts when the tasks before it are merged):
- Group A: **Task 1** (model, commands) ∥ **Task 2** (scene contract, renderer, resolver) ∥ **Task 5** (colour picker widget).
- Then **Task 3** (`kadr-text` core) → **Task 4** (evaluator, end to end).
- Group B: **Task 6 → Task 7 → Task 8** (editor: timeline, inspector, preview — they share `app.slint` / `app.rs`) ∥ **Task 9 → Task 10** (`kadr-text` looks, then animation + styles).
- Then **Task 11** (inspector advanced).
- Group C: **Task 12** (Text tab) ∥ **Task 13** (MCP / AI).
- Then **Task 14** (bench, perf, acceptance — controller).

The core is usable after Task 8: add a text (button / Ctrl+T), see it in preview and export, edit text and style in the inspector, move / scale / rotate / type on the preview.

---

### Task 1: The text model, format 4 and the text commands (`kadr-project`, `kadr-timeline`)

**Files:**
- Create: `crates/project/src/text.rs`
- Modify: `crates/project/Cargo.toml` (add `uuid.workspace = true`), `crates/project/src/lib.rs` (`pub mod text; pub use text::*;`), `crates/project/src/sequence.rs` (`Clip.text`, `Clip::new_text`, `Clip::is_text`), `crates/project/src/model.rs:11` (`FORMAT_VERSION` 4), `crates/project/src/io.rs` (tests at :383-388; doc of `migrate`)
- Modify: `crates/timeline/src/commands.rs` (`EditCommand::AddText`, `ClipProperty::Text`, `label`, `apply_one` arms, `InsertClip` check, `trim`), `crates/timeline/src/ops.rs` (`split_clip`)
- Create: `crates/timeline/tests/text_edits.rs`
- Modify: `crates/i18n/locales/ru.json`, `en.json` (`cmd.add_text`)

**Interfaces:**
- Consumes: `kadr_core::{AssetId, Time, TimeRange, TrackId}`, `Clip::new`, `ops::{insert_sorted, split_clip}`, `fx::visible_fraction`.
- Produces (later tasks rely on these exact names):
  - `kadr_project::TEXT_ASSET: AssetId` (nil UUID); `Rgba8(pub [u8; 4])` with `WHITE`, `BLACK`, `rgba(r, g, b, a)`, `hex() -> String`, `parse(&str) -> Option<Rgba8>`, `to_f32() -> [f32; 4]`.
  - `Align::{Left, Center, Right}`, `Unit::{Letter, Word, Line, Block}`, `TextShape::{Straight, Arc { bend: f32 }, Circle { radius: f32, start_deg: f32 }}`, `Perspective { tilt_x: f32, tilt_y: f32 }`.
  - `FontRef { family: String, weight: u16, italic: bool }`, `GradientStop { pos: f32, colour: Rgba8 }`, `GradientFill { stops: Vec<GradientStop>, angle: f32 }`, `Fill { colour: Rgba8, gradient: Option<GradientFill>, extra }` + `Fill::solid(Rgba8)`, `Outline { width: f32, colour: Rgba8 }`, `Shadow { colour, offset_x, offset_y, blur }`, `Glow { colour, radius, intensity }`, `Plaque { per_line: bool, colour, radius, padding_x, padding_y }`.
  - `TextStyle { font, size: f32, fill, outlines: Vec<Outline>, shadow: Option<Shadow>, glow: Option<Glow>, plaque: Option<Plaque>, extra }`; `RunStyle { family, weight, italic, size, fill, letter_spacing — all Option, extra }` + `is_empty()`, `merged(&RunStyle) -> RunStyle`; `TextRun { text: String, style: RunStyle, extra }`; `ParagraphStyle { align, line_height: f32, letter_spacing: f32, caps: bool, wrap: Option<f32>, extra }`.
  - `Reveal { preset: String, unit: Unit, duration: Time, stagger: Time, ease: String, extra }`, `LoopAnim { preset: String, unit: Unit, speed: f64, amount: f64, extra }`, `Highlight { colour: Rgba8, scale: f32, span: (f64, f64), extra }`, `TextAnim { reveal_in, reveal_out, looping, highlight: Option<..>, extra }` (JSON keys `in`, `out`, `loop`, `highlight`) + `is_empty()`.
  - `TextDoc { runs, style, paragraph, shape, perspective, anim, seed: u64, extra }` with `new(&str)`, `starter(&str, canvas_height: u32)`, `fresh_seed() -> u64`, `plain() -> String`, `label() -> String`, `set_plain(&str) -> bool`, `style_range(Range<usize>, &RunStyle) -> bool`, `reset_range(Range<usize>) -> bool`, `run_style_at(usize) -> &RunStyle`, `normalise()`, `split_at(frac: f64) -> (TextDoc, TextDoc)`, `is_animated() -> bool`.
  - `Clip.text: Option<Box<TextDoc>>`, `Clip::new_text(TextDoc, at: Time, duration: Time) -> Clip`, `Clip::is_text(&self) -> bool`; `FORMAT_VERSION == 4`.
  - `EditCommand::AddText { clip: Clip, track: Option<TrackId> }` (label key `cmd.add_text`); `ClipProperty::Text(Box<TextDoc>)`.

- [ ] **Step 1: Write the failing model tests** — create `crates/project/src/text.rs` with only this test module for now; add `pub mod text;` / `pub use text::*;` to `crates/project/src/lib.rs` (after `sequence`) and `uuid.workspace = true` under `[dependencies]` in `crates/project/Cargo.toml`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn red() -> RunStyle {
        RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 0, 0, 255))), ..Default::default() }
    }

    fn doc(runs: &[(&str, RunStyle)]) -> TextDoc {
        let mut d = TextDoc::new("");
        d.runs = runs.iter().map(|(t, s)| TextRun { text: t.to_string(), style: s.clone(), extra: Default::default() }).collect();
        d
    }

    fn texts(d: &TextDoc) -> Vec<(&str, bool)> {
        d.runs.iter().map(|r| (r.text.as_str(), r.style.fill.is_some())).collect()
    }

    #[test]
    fn colours_are_hex_strings() {
        assert_eq!(Rgba8::parse("#ff8000"), Some(Rgba8([255, 128, 0, 255])));
        assert_eq!(Rgba8::parse(" #FF800080 "), Some(Rgba8([255, 128, 0, 128])));
        for bad in ["ff8000", "#ff80", "#gg8000", "#ff8000801", "#ффф000", ""] {
            assert_eq!(Rgba8::parse(bad), None, "{bad:?}");
        }
        assert_eq!(serde_json::to_value(Rgba8([1, 2, 3, 4])).unwrap(), json!("#01020304"));
        assert!(serde_json::from_value::<Rgba8>(json!("red")).is_err());
        assert_eq!(Rgba8([255, 0, 51, 255]).to_f32(), [1.0, 0.0, 0.2, 1.0]);
    }

    #[test]
    fn a_text_doc_round_trips_and_keeps_unknown_fields() {
        let mut d = TextDoc::starter("Привет, мир", 1080);
        d.runs[0].style = red();
        d.style.outlines = vec![Outline { width: 4.0, colour: Rgba8::BLACK }];
        d.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::rgba(0, 0, 0, 160), radius: 8.0, padding_x: 16.0, padding_y: 6.0 });
        d.shape = TextShape::Arc { bend: 0.4 };
        d.perspective = Some(Perspective { tilt_x: 20.0, tilt_y: 0.0 });
        d.anim.reveal_in = Some(Reveal { preset: "rise".into(), unit: Unit::Word, duration: Time::from_millis(500), stagger: Time::from_millis(80), ease: "smooth".into(), extra: Default::default() });
        d.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.08, span: (0.25, 1.0), extra: Default::default() });
        let mut v = serde_json::to_value(&d).unwrap();
        assert_eq!(serde_json::from_value::<TextDoc>(v.clone()).unwrap(), d);
        // A newer Kadr's fields at every level that may grow survive load → save.
        v["mask"] = json!({"shape": "star"});
        v["runs"][0]["ruby"] = json!("пример");
        v["runs"][0]["style"]["underline"] = json!(true);
        v["style"]["bevel"] = json!(2);
        v["style"]["fill"]["texture"] = json!("paper");
        v["paragraph"]["indent"] = json!(12);
        v["anim"]["path"] = json!("zigzag");
        v["anim"]["in"]["bounce"] = json!(3);
        v["anim"]["highlight"]["word_times"] = json!([[0, 1]]);
        let back = serde_json::to_value(serde_json::from_value::<TextDoc>(v.clone()).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn unknown_closed_choices_read_as_their_default() {
        let mut v = serde_json::to_value(TextDoc::new("x")).unwrap();
        v["paragraph"]["align"] = json!("justify_all");
        v["shape"] = json!({"spiral": {"turns": 3}});
        v["anim"] = json!({"in": {"preset": "rise", "unit": "syllable", "duration": 1}});
        let d: TextDoc = serde_json::from_value(v).unwrap();
        assert_eq!((d.paragraph.align, d.shape, d.anim.reveal_in.unwrap().unit), (Align::Center, TextShape::Straight, Unit::Word));
    }

    #[test]
    fn set_plain_keeps_the_styles_of_untouched_text() {
        let mut d = doc(&[("Привет, ", RunStyle::default()), ("мир", red()), ("!", RunStyle::default())]);
        assert!(d.set_plain("Привет, огромный мир!"));
        assert_eq!(texts(&d), [("Привет, огромный ", false), ("мир", true), ("!", false)], "an insertion takes the style before it");
        assert!(d.set_plain("Привет, огромный миры!"));
        assert_eq!(texts(&d), [("Привет, огромный ", false), ("миры", true), ("!", false)]);
        assert!(d.set_plain("Привет, !"));
        assert_eq!(texts(&d), [("Привет, !", false)], "a deleted run goes, its neighbours merge");
        assert!(!d.set_plain("Привет, !"), "the same text is no change");
        assert!(d.set_plain(""));
        assert_eq!((d.runs.len(), d.plain()), (1, String::new()));
    }

    #[test]
    fn deleting_everything_keeps_the_first_runs_style() {
        let mut d = doc(&[("мир", red()), ("!", RunStyle::default())]);
        assert!(d.set_plain(""));
        assert_eq!(texts(&d), [("", true)], "typing again continues in that style");
        assert!(d.set_plain("ок"));
        assert_eq!(texts(&d), [("ок", true)]);
    }

    #[test]
    fn style_range_splits_and_merges_runs_on_character_boundaries() {
        let mut d = TextDoc::new("один два три");
        // "один " is 9 bytes (4 × 2 + 1), "два" 6.
        assert!(d.style_range(9..15, &red()));
        assert_eq!(texts(&d), [("один ", false), ("два", true), (" три", false)]);
        assert!(!d.style_range(9..15, &red()), "the same style again is no change");
        assert!(d.style_range(10..13, &RunStyle { size: Some(40.0), ..Default::default() }), "10 is inside «д»: floored to 9");
        assert_eq!(d.runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["один ", "дв", "а", " три"]);
        assert_eq!((d.runs[1].style.size, d.runs[2].style.size), (Some(40.0), None));
        assert_eq!(d.runs[1].style.fill, red().fill, "a patch keeps the fields it does not set");
        assert!(d.reset_range(0..d.plain().len()));
        assert_eq!(texts(&d), [("один два три", false)]);
        assert!(!d.style_range(5..5, &red()), "an empty range changes nothing");
        assert!(!d.style_range(400..900, &red()), "a range past the end is clamped to nothing");
        assert_eq!(d.run_style_at(0), &RunStyle::default());
    }

    #[test]
    fn split_at_hands_in_left_out_right_and_cuts_the_highlight() {
        let mut d = TextDoc::new("раз два");
        let rev = |p: &str| Reveal { preset: p.into(), unit: Unit::Word, duration: Time::from_millis(400), stagger: Time::ZERO, ease: "smooth".into(), extra: Default::default() };
        d.anim.reveal_in = Some(rev("fade"));
        d.anim.reveal_out = Some(rev("drop"));
        d.anim.looping = Some(LoopAnim { preset: "pulse".into(), unit: Unit::Block, speed: 1.0, amount: 0.5, extra: Default::default() });
        d.anim.highlight = Some(Highlight { colour: Rgba8::WHITE, scale: 1.1, span: (0.2, 0.6), extra: Default::default() });
        let (l, r) = d.split_at(0.5);
        assert_eq!((l.anim.reveal_in.is_some(), l.anim.reveal_out.is_some()), (true, false));
        assert_eq!((r.anim.reveal_in.is_some(), r.anim.reveal_out.is_some()), (false, true));
        assert_eq!((l.anim.looping.is_some(), r.anim.looping.is_some()), (true, true));
        let span = |d: &TextDoc| d.anim.highlight.as_ref().unwrap().span;
        assert!((span(&l).1 - 0.4).abs() < 1e-12 && span(&l).0 == 0.2);
        assert!((span(&r).0 - 0.4).abs() < 1e-12 && span(&r).1 == 0.6);
        assert_eq!((l.seed, r.seed), (d.seed, d.seed), "one seed: scatter and shake look the same on both sides");
        assert_eq!(span(&d.split_at(f64::NAN).0), (0.2, 0.2), "a non-finite cut counts as 0");
    }

    #[test]
    fn starter_scales_with_the_frame_and_labels_cut_long_lines() {
        let hd = TextDoc::starter("x", 1080);
        assert_eq!(hd.style.size, 97.0);
        assert_eq!(TextDoc::starter("x", 2160).style.size, 194.0);
        assert_eq!(hd.style.font, FontRef { family: "Montserrat".into(), weight: 700, italic: false });
        assert!(hd.style.shadow.is_some() && hd.style.outlines.is_empty() && hd.anim.is_empty());
        assert_ne!(TextDoc::new("a").seed, TextDoc::new("a").seed);
        let long = TextDoc::new(&format!("\n  {}\nвторая", "ж".repeat(80)));
        assert_eq!(long.label(), format!("{}…", "ж".repeat(60)));
        assert_eq!(TextDoc::new("").label(), "");
    }

    #[test]
    fn a_text_clip_round_trips_and_a_media_clip_writes_no_text() {
        let c = crate::Clip::new_text(TextDoc::new("Гол!"), Time::from_secs(2), Time::from_secs(5));
        assert!(c.is_text());
        assert_eq!((c.asset, c.source_in, c.source_out, c.timeline_out), (TEXT_ASSET, Time::ZERO, Time::from_secs(5), Time::from_secs(7)));
        assert_eq!(c.name, "Гол!");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["asset"], json!("00000000-0000-0000-0000-000000000000"));
        assert_eq!(serde_json::from_value::<crate::Clip>(v).unwrap(), c);
        let m = crate::Clip::new(kadr_core::AssetId::new(), "m", kadr_core::TimeRange::new(Time::ZERO, Time::from_secs(1)), Time::ZERO);
        assert!(serde_json::to_value(&m).unwrap().get("text").is_none(), "a media clip's JSON is unchanged");
        assert!(!m.is_text());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kadr-project text::tests`
Expected: FAIL to compile — `cannot find type TextDoc`, `Rgba8`, `RunStyle`, `Fill`, … in this scope.

- [ ] **Step 3: Implement the model** — put this above the test module in `crates/project/src/text.rs`:

```rust
//! Text clips (titles spec §2): a clip whose picture is a [`TextDoc`] —
//! runs of styled text, a base style, a paragraph, a shape, a
//! perspective and an animation. Plain data: layout and pixels live in
//! `kadr-text`. Fields from a newer Kadr are kept verbatim (`extra`); a
//! closed choice this build does not know reads as its default.

use kadr_core::{AssetId, Time};
use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

/// The asset of every text clip: no media (titles spec §2.1). Never in
/// `Project.assets`, so every asset lookup of a text clip finds nothing.
pub const TEXT_ASSET: AssetId = AssetId(uuid::Uuid::nil());

type Extra = BTreeMap<String, Value>;

/// A straight-alpha colour in the working space, 8 bits per channel.
/// JSON: `"#rrggbbaa"` (reads `"#rrggbb"` too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba8(pub [u8; 4]);

impl Rgba8 {
    pub const WHITE: Rgba8 = Rgba8([255, 255, 255, 255]);
    pub const BLACK: Rgba8 = Rgba8([0, 0, 0, 255]);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba8 {
        Rgba8([r, g, b, a])
    }

    pub fn hex(&self) -> String {
        let [r, g, b, a] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }

    pub fn parse(s: &str) -> Option<Rgba8> {
        let h = s.trim().strip_prefix('#')?;
        if !h.is_ascii() || !(h.len() == 6 || h.len() == 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgba8([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 255 }]))
    }

    /// Components in [0, 1], straight alpha.
    pub fn to_f32(self) -> [f32; 4] {
        self.0.map(|v| v as f32 / 255.0)
    }
}

impl Serialize for Rgba8 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgba8 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba8::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("not a colour: {s:?} (\"#rrggbb\" or \"#rrggbbaa\")")))
    }
}

/// A closed choice read leniently: a value this build does not know (a
/// newer Kadr's) reads as the default, so the project still opens.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).unwrap_or_default())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

/// What an animation moves one at a time (titles spec §4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Letter,
    #[default]
    Word,
    Line,
    Block,
}

/// The line the text runs along (titles spec §1): straight; an arc whose
/// `bend` −1..1 is its angle in half turns (positive bows upwards like a
/// rainbow); or a circle of `radius` canvas px, starting at `start_deg`
/// (0 = top, clockwise).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextShape {
    #[default]
    Straight,
    Arc { bend: f32 },
    Circle { radius: f32, start_deg: f32 },
}

fn is_straight(s: &TextShape) -> bool {
    *s == TextShape::Straight
}

/// A tilt of the finished text in degrees (−60..60): `tilt_x` about the
/// horizontal axis (positive: the top recedes), `tilt_y` about the
/// vertical one (positive: the right side recedes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Perspective {
    #[serde(default)]
    pub tilt_x: f32,
    #[serde(default)]
    pub tilt_y: f32,
}

fn default_family() -> String {
    "Montserrat".into()
}
fn w700() -> u16 {
    700
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontRef {
    #[serde(default = "default_family")]
    pub family: String,
    /// 100..900.
    #[serde(default = "w700")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
}

impl Default for FontRef {
    fn default() -> Self {
        FontRef { family: default_family(), weight: 700, italic: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// 0..1 along the gradient.
    pub pos: f32,
    pub colour: Rgba8,
}

/// A linear gradient across the text block: `angle` in degrees, 0 = left
/// to right, 90 = top to bottom.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientFill {
    pub stops: Vec<GradientStop>,
    #[serde(default)]
    pub angle: f32,
}

/// The paint of letters: `colour`, or the `gradient` when there is one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub colour: Rgba8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<GradientFill>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Fill {
    pub fn solid(colour: Rgba8) -> Fill {
        Fill { colour, gradient: None, extra: Extra::new() }
    }
}

/// A band of `width` canvas px around the letters (outside them).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub width: f32,
    pub colour: Rgba8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    pub colour: Rgba8,
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub blur: f32,
}

fn one_f32() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Glow {
    pub colour: Rgba8,
    pub radius: f32,
    /// 0..2: the glow's alpha multiplier.
    #[serde(default = "one_f32")]
    pub intensity: f32,
}

/// A rounded rectangle under the block or under each line.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plaque {
    #[serde(default)]
    pub per_line: bool,
    pub colour: Rgba8,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub padding_x: f32,
    #[serde(default)]
    pub padding_y: f32,
}

fn size96() -> f32 {
    96.0
}
fn white_fill() -> Fill {
    Fill::solid(Rgba8::WHITE)
}

/// The whole text's style (titles spec §2.2): sizes in canvas pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default)]
    pub font: FontRef,
    /// Font size, canvas px.
    #[serde(default = "size96")]
    pub size: f32,
    #[serde(default = "white_fill")]
    pub fill: Fill,
    /// At most two; the first is the inner one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<Outline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<Glow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plaque: Option<Plaque>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { font: FontRef::default(), size: 96.0, fill: white_fill(), outlines: vec![], shadow: None, glow: None, plaque: None, extra: Extra::new() }
    }
}

/// A run's own style: each set field overrides the base style.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Fill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f32>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl RunStyle {
    pub fn is_empty(&self) -> bool {
        *self == RunStyle::default()
    }

    /// `patch`'s set fields over this style's.
    pub fn merged(&self, patch: &RunStyle) -> RunStyle {
        RunStyle {
            family: patch.family.clone().or_else(|| self.family.clone()),
            weight: patch.weight.or(self.weight),
            italic: patch.italic.or(self.italic),
            size: patch.size.or(self.size),
            fill: patch.fill.clone().or_else(|| self.fill.clone()),
            letter_spacing: patch.letter_spacing.or(self.letter_spacing),
            extra: self.extra.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "RunStyle::is_empty")]
    pub style: RunStyle,
    #[serde(flatten)]
    pub extra: Extra,
}

fn lh12() -> f32 {
    1.2
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphStyle {
    #[serde(default, deserialize_with = "lenient")]
    pub align: Align,
    /// Line distance as a multiple of the font size.
    #[serde(default = "lh12")]
    pub line_height: f32,
    /// Extra advance after every letter, canvas px.
    #[serde(default)]
    pub letter_spacing: f32,
    /// ALL CAPS at layout (the stored text keeps its case).
    #[serde(default)]
    pub caps: bool,
    /// Wrap lines at this block width, canvas px; `None`: only at line breaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<f32>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        ParagraphStyle { align: Align::Center, line_height: 1.2, letter_spacing: 0.0, caps: false, wrap: None, extra: Extra::new() }
    }
}

fn smooth() -> String {
    "smooth".into()
}

/// An appearance or disappearance (titles spec §4): `preset` from
/// `kadr-text`'s list (an unknown one is kept and not run); unit `i` of
/// `N` runs over `[i·stagger, i·stagger + duration]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reveal {
    pub preset: String,
    #[serde(default, deserialize_with = "lenient")]
    pub unit: Unit,
    pub duration: Time,
    #[serde(default)]
    pub stagger: Time,
    /// A curve key: smooth, linear, ease_in, ease_out, cinematic, snap, gentle.
    #[serde(default = "smooth")]
    pub ease: String,
    #[serde(flatten)]
    pub extra: Extra,
}

fn one_f64() -> f64 {
    1.0
}
fn amount_default() -> f64 {
    0.6
}

/// A motion repeating while the text is shown, in source time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopAnim {
    pub preset: String,
    #[serde(default, deserialize_with = "lenient")]
    pub unit: Unit,
    /// Cycles per second.
    #[serde(default = "one_f64")]
    pub speed: f64,
    /// 0..1.
    #[serde(default = "amount_default")]
    pub amount: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

fn full_span() -> (f64, f64) {
    (0.0, 1.0)
}
fn is_full_span(s: &(f64, f64)) -> bool {
    *s == full_span()
}
fn highlight_colour() -> Rgba8 {
    Rgba8::rgba(255, 212, 0, 255)
}
fn highlight_scale() -> f32 {
    1.08
}

/// The current word in another colour and size: words spread evenly over
/// the progress `span` (titles spec §3), which a split cuts like an
/// effect's progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    #[serde(default = "highlight_colour")]
    pub colour: Rgba8,
    #[serde(default = "highlight_scale")]
    pub scale: f32,
    #[serde(default = "full_span", skip_serializing_if = "is_full_span")]
    pub span: (f64, f64),
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextAnim {
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub reveal_in: Option<Reveal>,
    #[serde(rename = "out", default, skip_serializing_if = "Option::is_none")]
    pub reveal_out: Option<Reveal>,
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub looping: Option<LoopAnim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<Highlight>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl TextAnim {
    pub fn is_empty(&self) -> bool {
        *self == TextAnim::default()
    }
}

fn one_empty_run() -> Vec<TextRun> {
    vec![TextRun::default()]
}

/// A text clip's picture (titles spec §2.2). The text is the runs'
/// concatenation; there is always at least one run (maybe empty).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextDoc {
    #[serde(default = "one_empty_run")]
    pub runs: Vec<TextRun>,
    #[serde(default)]
    pub style: TextStyle,
    #[serde(default)]
    pub paragraph: ParagraphStyle,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_straight")]
    pub shape: TextShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perspective: Option<Perspective>,
    #[serde(default, skip_serializing_if = "TextAnim::is_empty")]
    pub anim: TextAnim,
    /// Seed of the random-looking animations; a split keeps it.
    #[serde(default)]
    pub seed: u64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The largest char boundary of `s` at or below `i` (clamped to its length).
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

impl TextDoc {
    /// `text` in one run of the default style, a fresh seed.
    pub fn new(text: &str) -> TextDoc {
        TextDoc {
            runs: vec![TextRun { text: text.to_string(), ..Default::default() }],
            style: TextStyle::default(),
            paragraph: ParagraphStyle::default(),
            shape: TextShape::Straight,
            perspective: None,
            anim: TextAnim::default(),
            seed: Self::fresh_seed(),
            extra: Extra::new(),
        }
    }

    pub fn fresh_seed() -> u64 {
        uuid::Uuid::new_v4().as_u64_pair().0
    }

    /// A new title (titles spec §2.3): Montserrat 700 at 9 % of the frame
    /// height, white, a soft shadow, centred.
    pub fn starter(text: &str, canvas_height: u32) -> TextDoc {
        let mut d = TextDoc::new(text);
        let size = (canvas_height as f32 * 0.09).round().max(8.0);
        d.style.size = size;
        d.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 153), offset_x: 0.0, offset_y: (size * 0.03).round(), blur: (size * 0.06).round() });
        d
    }

    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// The first non-empty line, trimmed, at most 60 characters (then "…"):
    /// the clip's name on the timeline.
    pub fn label(&self) -> String {
        let text = self.plain();
        let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        if line.chars().count() > 60 { format!("{}…", line.chars().take(60).collect::<String>()) } else { line.to_string() }
    }

    pub fn is_animated(&self) -> bool {
        let a = &self.anim;
        a.reveal_in.is_some() || a.reveal_out.is_some() || a.looping.is_some() || a.highlight.is_some()
    }

    /// The style of the run holding byte `at` (the last run at the end).
    pub fn run_style_at(&self, at: usize) -> &RunStyle {
        let mut start = 0;
        for r in &self.runs {
            if at < start + r.text.len() {
                return &r.style;
            }
            start += r.text.len();
        }
        &self.runs.last().expect("a doc has a run").style
    }

    /// Makes `at` (a char boundary) a run boundary; returns the index of
    /// the run starting there (`runs.len()` at the end).
    fn boundary(&mut self, at: usize) -> usize {
        let mut start = 0;
        for i in 0..self.runs.len() {
            let len = self.runs[i].text.len();
            if at == start {
                return i;
            }
            if at < start + len {
                let tail = self.runs[i].text.split_off(at - start);
                let r = TextRun { text: tail, style: self.runs[i].style.clone(), extra: self.runs[i].extra.clone() };
                self.runs.insert(i + 1, r);
                return i + 1;
            }
            start += len;
        }
        self.runs.len()
    }

    /// Replaces the text, keeping the styles of what did not change (spec
    /// §6.3): the changed middle (after the common prefix and suffix) is
    /// replaced by the new middle in the style of the character before it.
    /// `false` when the text is the same.
    pub fn set_plain(&mut self, new: &str) -> bool {
        let old = self.plain();
        if old == new {
            return false;
        }
        let mut pre = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
            pre -= 1;
        }
        let room = (old.len() - pre).min(new.len() - pre);
        let mut suf = old.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(old.len() - suf) || !new.is_char_boundary(new.len() - suf) {
            suf -= 1;
        }
        let style = self.run_style_at(pre.saturating_sub(1)).clone();
        let first_style = self.runs[0].style.clone();
        let i = self.boundary(pre);
        let j = self.boundary(old.len() - suf);
        let inserted = &new[pre..new.len() - suf];
        self.runs.splice(i..j, (!inserted.is_empty()).then(|| TextRun { text: inserted.to_string(), style, extra: Extra::new() }));
        if self.runs.iter().all(|r| r.text.is_empty()) {
            self.runs = vec![TextRun { text: String::new(), style: first_style, extra: Extra::new() }];
        }
        self.normalise();
        true
    }

    /// Merges `patch` into the style of the text in `range` (bytes, floored
    /// to char boundaries, clamped to the text). `false` when nothing changed.
    pub fn style_range(&mut self, range: Range<usize>, patch: &RunStyle) -> bool {
        self.restyle(range, |s| s.merged(patch))
    }

    /// Gives the text in `range` the base style again.
    pub fn reset_range(&mut self, range: Range<usize>) -> bool {
        self.restyle(range, |_| RunStyle::default())
    }

    fn restyle(&mut self, range: Range<usize>, f: impl Fn(&RunStyle) -> RunStyle) -> bool {
        let text = self.plain();
        let (a, b) = (floor_boundary(&text, range.start), floor_boundary(&text, range.end));
        if a >= b {
            return false;
        }
        let before = self.runs.clone();
        let i = self.boundary(a);
        let j = self.boundary(b);
        for r in &mut self.runs[i..j] {
            r.style = f(&r.style);
        }
        self.normalise();
        self.runs != before
    }

    /// Neighbours of one style merge, empty runs go; one run always stays
    /// (an empty doc keeps its first run's style).
    pub fn normalise(&mut self) {
        let first = self.runs.first().map(|r| r.style.clone()).unwrap_or_default();
        let mut out: Vec<TextRun> = Vec::with_capacity(self.runs.len());
        for r in self.runs.drain(..) {
            if r.text.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(l) if l.style == r.style && l.extra == r.extra => l.text.push_str(&r.text),
                _ => out.push(r),
            }
        }
        if out.is_empty() {
            out.push(TextRun { text: String::new(), style: first, extra: Extra::new() });
        }
        self.runs = out;
    }

    /// The two documents of a split at `frac` of the visible length
    /// (titles spec §3): the appearance stays left, the disappearance
    /// right, the highlight span is cut at `from + (to − from)·frac` (a
    /// non-finite `frac` counts as 0); the loop and the seed go to both.
    pub fn split_at(&self, frac: f64) -> (TextDoc, TextDoc) {
        let (mut l, mut r) = (self.clone(), self.clone());
        l.anim.reveal_out = None;
        r.anim.reveal_in = None;
        if let Some(h) = &self.anim.highlight {
            let f = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
            let c = h.span.0 + (h.span.1 - h.span.0) * f;
            let c = if c.is_finite() { c } else { h.span.0 };
            if let Some(x) = l.anim.highlight.as_mut() {
                x.span = (h.span.0, c);
            }
            if let Some(x) = r.anim.highlight.as_mut() {
                x.span = (c, h.span.1);
            }
        }
        (l, r)
    }
}
```

In `crates/project/src/sequence.rs` add the field after `multicam` in `struct Clip`:

```rust
    /// The picture of a text clip (titles spec §2.1); `None` for media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Box<crate::TextDoc>>,
```

`text: None,` at the end of the literal in `Clip::new`, and after `Clip::new`:

```rust
    /// A text clip (titles spec §2.1): no media (`TEXT_ASSET`), its own
    /// time from 0 at speed 1, named after its first line.
    pub fn new_text(doc: crate::TextDoc, at: Time, duration: Time) -> Self {
        let mut c = Clip::new(crate::TEXT_ASSET, doc.label(), TimeRange::new(Time::ZERO, duration), at);
        c.text = Some(Box::new(doc));
        c
    }

    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }
```

`crates/project/src/model.rs:11`: `pub const FORMAT_VERSION: u32 = 4;`. In `io.rs` add to the doc of `migrate`: "Format 4 added text clips (titles spec §2.1): nothing to migrate — a format 3 file has none, and an older Kadr refuses a format 4 file instead of dropping its text." and, in `migrate`, before the `// E3, on every load: transitions …` line:

```rust
    // Format 4 added text clips (titles spec §2.1): nothing to migrate; the
    // upgraded document is a format-4 document in memory too.
    if from < 4
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert("format_version".into(), 4.into());
    }
```

Three existing `io.rs` tests pin the saved version to 3 — `format_2_effects_migrate_to_applied_effects` (`assert_eq!(saved["format_version"], serde_json::json!(3));`), `old_transition_kinds_become_recipes_on_their_cut` (`… json!(3), "no new format (R2)"`) and `garbage_in_a_format_2_effects_list_is_dropped_not_fatal` (`… ["format_version"], serde_json::json!(3));`): each now expects `serde_json::json!(FORMAT_VERSION)` (the R2 message becomes "the current format (E3 added none, R2; titles made it 4)"). Replace the test `a_format_3_file_is_too_new_for_a_format_2_reader` with:

```rust
    #[test]
    fn a_format_4_file_is_too_new_for_a_format_3_reader() {
        assert_eq!(FORMAT_VERSION, 4);
        let json = format!(r#"{{"format_version": {}}}"#, FORMAT_VERSION + 1);
        assert!(matches!(parse(json.as_bytes(), Path::new("x.kadr")).unwrap_err(), ProjectIoError::TooNew { .. }));
    }

    /// Format 3 needs no migration for text: it opens, and saving writes format 4.
    #[test]
    fn a_format_3_project_opens_and_saves_as_format_4() {
        let mut v = serde_json::to_value(crate::Project::new("old")).unwrap();
        v["format_version"] = serde_json::json!(3);
        let mut p = parse(&serde_json::to_vec(&v).unwrap(), Path::new("old.kadr")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        save(&mut p, &dir.path().join("p.kadr")).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(dir.path().join("p.kadr")).unwrap()).unwrap();
        assert_eq!(saved["format_version"], serde_json::json!(4));
    }
```

- [ ] **Step 4: Run the model tests**

Run: `cargo test -p kadr-project`
Expected: PASS (old tests too: `rejects_newer_format` uses `FORMAT_VERSION + 1`).

- [ ] **Step 5: Write the failing command tests** — create `crates/timeline/tests/text_edits.rs`:

```rust
//! Text clips through the edit engine (titles spec §2.1, §3, §7): where
//! `AddText` puts a title, the text property, trim before source 0, split
//! of the animations, and one undo entry per command.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_timeline::*;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

/// V1 holds a 0..10 s video, V2 above it is empty, A1 below.
fn setup() -> (Project, EditEngine) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    p.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    let v = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("m.mp4", MediaInfo { kind: MediaKind::Video, duration: s(60), container: "mp4".into(), size_bytes: 0, video: Some(v), audio: None, timecode: None });
    p.sequence_mut().tracks[0].clips.push(Clip::new(a.id, "m", TimeRange::new(Time::ZERO, s(10)), Time::ZERO));
    p.assets.push(a);
    (p, EditEngine::new())
}

fn title(at: i64, dur: i64) -> Clip {
    Clip::new_text(TextDoc::new("Гол!"), s(at), s(dur))
}

fn track_of(p: &Project, id: kadr_core::ClipId) -> String {
    let (ti, _) = p.sequence().locate_clip(id).unwrap();
    p.sequence().tracks[ti].name.clone()
}

#[test]
fn add_text_goes_on_the_topmost_video_track_or_a_new_one_above() {
    let (mut p, mut e) = setup();
    let a = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: a.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, a.id), "V2", "the topmost video track is free over 2..7 s");
    assert_eq!(e.undo_label(), Some("cmd.add_text"));
    let b = title(4, 5);
    e.execute(&mut p, EditCommand::AddText { clip: b.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, b.id), "V3", "V2 is taken at 4 s: a new track above all");
    let kinds: Vec<_> = p.sequence().tracks.iter().map(|t| (t.name.as_str(), t.kind)).collect();
    assert_eq!(kinds, [("V1", TrackKind::Video), ("V2", TrackKind::Video), ("V3", TrackKind::Video), ("A1", TrackKind::Audio)]);
    let c = title(20, 5);
    e.execute(&mut p, EditCommand::AddText { clip: c.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, c.id), "V3", "the topmost track, free at 20 s");
    e.undo(&mut p);
    e.undo(&mut p);
    assert_eq!(p.sequence().tracks.len(), 3, "one undo takes the title and its new track");
    assert!(p.sequence().clip(b.id).is_none());
}

#[test]
fn add_text_on_a_given_track_checks_it() {
    let (mut p, mut e) = setup();
    let (v1, v2, a1) = (p.sequence().tracks[0].id, p.sequence().tracks[1].id, p.sequence().tracks[2].id);
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: title(2, 5), track: Some(v1) }), Err(EditError::Overlap));
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: title(2, 5), track: Some(a1) }), Err(EditError::KindMismatch));
    p.sequence_mut().tracks[1].locked = true;
    assert!(matches!(e.execute(&mut p, EditCommand::AddText { clip: title(2, 5), track: Some(v2) }), Err(EditError::TrackLocked(_))));
    let media = p.sequence().tracks[0].clips[0].clone();
    assert_eq!(e.execute(&mut p, EditCommand::AddText { clip: media, track: None }), Err(EditError::KindMismatch), "only text clips");
    let t = title(12, 3);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: Some(v1) }).unwrap();
    assert_eq!(track_of(&p, t.id), "V1");
    let u = title(2, 3);
    e.execute(&mut p, EditCommand::AddText { clip: u.clone(), track: None }).unwrap();
    assert_eq!(track_of(&p, u.id), "V3", "the topmost (V2) is locked: a new track");
}

#[test]
fn the_text_property_changes_the_doc_once() {
    let (mut p, mut e) = setup();
    let t = title(2, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    let mut doc = TextDoc::new("Победа");
    doc.seed = 7;
    let set = |d: &TextDoc| EditCommand::SetClipProperty { clip: t.id, prop: ClipProperty::Text(Box::new(d.clone())) };
    e.execute(&mut p, set(&doc)).unwrap();
    assert_eq!(p.sequence().clip(t.id).unwrap().text.as_deref(), Some(&doc));
    assert_eq!(e.execute(&mut p, set(&doc)), Err(EditError::NoOp));
    let media = p.sequence().tracks[0].clips[0].id;
    assert_eq!(e.execute(&mut p, EditCommand::SetClipProperty { clip: media, prop: ClipProperty::Text(Box::new(doc)) }), Err(EditError::KindMismatch));
}

#[test]
fn a_text_clip_trims_before_source_zero_and_a_video_does_not() {
    let (mut p, mut e) = setup();
    let t = title(5, 5);
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    e.execute(&mut p, EditCommand::TrimClip { clip: t.id, edge: TrimEdge::Start, to: s(3), ripple: false }).unwrap();
    let c = p.sequence().clip(t.id).unwrap();
    assert_eq!((c.timeline_in, c.source_in, c.source_out), (s(3), s(-2), s(5)));
    e.execute(&mut p, EditCommand::TrimClip { clip: t.id, edge: TrimEdge::End, to: s(40), ripple: false }).unwrap();
    assert_eq!(p.sequence().clip(t.id).unwrap().timeline_out, s(40), "no source end either");
    let v = p.sequence().tracks[0].clips[0].id;
    e.execute(&mut p, EditCommand::MoveClips { clips: vec![v], delta: s(5), track_delta: 0 }).unwrap();
    assert_eq!(e.execute(&mut p, EditCommand::TrimClip { clip: v, edge: TrimEdge::Start, to: s(1), ripple: false }), Err(EditError::NoOp), "a video stops at its source start");
}

#[test]
fn split_hands_the_appearance_left_and_the_disappearance_right() {
    let (mut p, mut e) = setup();
    let mut doc = TextDoc::new("раз два три");
    let rev = |preset: &str| Reveal { preset: preset.into(), unit: Unit::Word, duration: Time::from_millis(400), stagger: Time::from_millis(100), ease: "smooth".into(), extra: Default::default() };
    doc.anim.reveal_in = Some(rev("rise"));
    doc.anim.reveal_out = Some(rev("fade"));
    doc.anim.highlight = Some(Highlight { colour: Rgba8::WHITE, scale: 1.1, span: (0.0, 1.0), extra: Default::default() });
    let t = Clip::new_text(doc.clone(), s(2), s(4));
    e.execute(&mut p, EditCommand::AddText { clip: t.clone(), track: None }).unwrap();
    e.execute(&mut p, EditCommand::Split { at: s(3), clips: Some(vec![t.id]) }).unwrap();
    let v2 = &p.sequence().tracks[1].clips;
    assert_eq!(v2.len(), 2);
    let (l, r) = (v2[0].text.as_deref().unwrap(), v2[1].text.as_deref().unwrap());
    assert_eq!((l.anim.reveal_in.is_some(), l.anim.reveal_out.is_some(), r.anim.reveal_in.is_some(), r.anim.reveal_out.is_some()), (true, false, false, true));
    assert_eq!((l.anim.highlight.as_ref().unwrap().span, r.anim.highlight.as_ref().unwrap().span), ((0.0, 0.25), (0.25, 1.0)));
    assert_eq!((l.seed, r.seed, l.plain(), r.plain()), (doc.seed, doc.seed, doc.plain(), doc.plain()));
    assert_eq!((v2[1].source_in, v2[1].timeline_in), (s(1), s(3)), "the right piece continues the text's own time");
}

#[test]
fn a_text_clip_cannot_go_on_an_audio_track() {
    let (mut p, mut e) = setup();
    let a1 = p.sequence().tracks[2].id;
    let cmd = EditCommand::InsertClip { track: a1, clip: title(1, 2), mode: InsertMode::Overwrite };
    assert_eq!(e.execute(&mut p, cmd), Err(EditError::KindMismatch));
}
```

- [ ] **Step 6: Run them to see them fail**

Run: `cargo test -p kadr-timeline --test text_edits`
Expected: FAIL to compile — `no variant named AddText`, `no variant named Text found for enum ClipProperty`.

- [ ] **Step 7: Implement the commands** — in `crates/timeline/src/commands.rs`:
  - imports: add `TextDoc` to the `kadr_project::{…}` list;
  - `ClipProperty`: after `VideoFade(EdgeFades),` add
    ```rust
        /// A text clip's document (titles spec §7): not a text clip →
        /// `KindMismatch`, the same document → `NoOp`.
        Text(Box<TextDoc>),
    ```
  - `EditCommand`: after `InsertClip { … },` add
    ```rust
        /// A text clip (titles spec §7, refinement 11): on `track` (a video
        /// track, unlocked, free over the clip's frame-snapped range), or —
        /// `None` — on the topmost video track when it is unlocked and free
        /// there, else on a new video track above all. One undo entry.
        AddText { clip: Clip, track: Option<TrackId> },
    ```
  - `label()`: `EditCommand::AddText { .. } => "cmd.add_text",` after the two `InsertClip` lines;
  - `apply_one`: `EditCommand::AddText { clip, track } => add_text(seq, clip, *track),` after the `InsertClip` arm; inside the `InsertClip` arm right after `let t = unlocked_mut(seq, *track)?;`:
    ```rust
                if clip.text.is_some() && t.kind != TrackKind::Video {
                    return Err(EditError::KindMismatch);
                }
    ```
  - `SetClipProperty`: in its `match prop` add
    ```rust
                    ClipProperty::Text(doc) => match &c.text {
                        None => return Err(EditError::KindMismatch),
                        Some(cur) if **cur == **doc => return Err(EditError::NoOp),
                        Some(_) => c.text = Some(doc.clone()),
                    },
    ```
  - in `fn trim`, `TrimEdge::Start` arm, replace the two "Can't extend before source start" lines with:
    ```rust
                // Can't extend before source start — a text clip has no
                // source to run out of (titles spec §2.1).
                if c.text.is_none() {
                    let earliest = c.timeline_time_of(Time::ZERO);
                    delta = delta.max(earliest - c.timeline_in);
                }
    ```
  - after `fn trim` add:

```rust
fn add_text(seq: &mut Sequence, clip: &Clip, track: Option<TrackId>) -> Result<(), EditError> {
    if clip.text.is_none() {
        return Err(EditError::KindMismatch);
    }
    let fr = seq.frame_rate;
    let mut clip = clip.clone();
    clip.move_to(fr.snap(clip.timeline_in).max(Time::ZERO));
    let len = fr.snap(clip.duration()).max(fr.frame_duration());
    (clip.timeline_out, clip.source_out) = (clip.timeline_in + len, clip.source_in + len);
    let range = clip.timeline_range();
    let free = |t: &Track| t.clips.iter().all(|c| !c.timeline_range().overlaps(&range));
    let ti = match track {
        Some(id) => {
            let ti = seq.tracks.iter().position(|t| t.id == id).ok_or(EditError::TrackNotFound)?;
            let t = &seq.tracks[ti];
            if t.kind != TrackKind::Video {
                return Err(EditError::KindMismatch);
            }
            if t.locked {
                return Err(EditError::TrackLocked(t.name.clone()));
            }
            if !free(t) {
                return Err(EditError::Overlap);
            }
            ti
        }
        None => {
            // Video tracks are stored V1 (bottom) first: the topmost is the last.
            let top = seq.tracks.iter().rposition(|t| t.kind == TrackKind::Video);
            match top.filter(|&i| !seq.tracks[i].locked && free(&seq.tracks[i])) {
                Some(ti) => ti,
                None => {
                    let n = seq.tracks_of(TrackKind::Video).count() + 1;
                    let at = top.map_or(0, |i| i + 1);
                    seq.tracks.insert(at, Track::new(TrackKind::Video, format!("V{n}")));
                    at
                }
            }
        }
    };
    ops::insert_sorted(&mut seq.tracks[ti], clip);
    Ok(())
}
```

In `crates/timeline/src/ops.rs::split_clip`, after the effects loop and before `right.id = ClipId::new();`:

```rust
    // Titles spec §3: the appearance stays left, the disappearance right,
    // the highlight span is cut where the cut is; both keep the seed.
    if let Some(doc) = clip.text.as_deref() {
        let (l, r) = doc.split_at(frac);
        left.text = Some(Box::new(l));
        right.text = Some(Box::new(r));
    }
```

Catalogs — `ru.json`: `"cmd.add_text": "Добавление текста"`; `en.json`: `"cmd.add_text": "Add text"`.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p kadr-timeline && cargo test -p kadr-project && cargo test -p kadr-i18n`
Expected: PASS (`all_referenced_keys_are_translated` finds `"cmd.add_text"` in `crates/timeline/src`).

- [ ] **Step 9: Workspace build, clippy, commit**

Run: `cargo build --workspace && cargo clippy -p kadr-project -p kadr-timeline --all-targets`
Expected: builds (only `commands.rs` matches `ClipProperty` exhaustively); no new warnings.

```bash
git add crates/project crates/timeline crates/i18n/locales
git commit -m "titles: TextDoc on the clip, format 4, AddText and the text property

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 2: Text layers in the scene contract, the renderers and the resolver (`kadr-scene`, `kadr-render`, `kadr-playback`)

**Files:**
- Modify: `crates/scene/src/scene.rs` (after `LayerContent`: `TextPicture`, `TextLayer`; the `Text` variant), `crates/scene/src/lib.rs` (contract: a «Text» paragraph after the Sampling paragraph, `Rgba` list untouched)
- Modify: `crates/render/src/cpu.rs` (`check_inputs` :94-116, `draw_layer` :948-958, tests), `crates/render/src/cost.rs` (`layer_cost` :110-118, test), `crates/render/tests/reference/mod.rs` (`draw_plain` :213-218), `crates/render/tests/reference_parity.rs` (new test)
- Create: `crates/playback/src/text.rs`
- Modify: `crates/playback/src/lib.rs` (`pub mod text;`, re-exports), `crates/playback/src/resolver.rs` (`Resolver.texts`, `collect` arm and parameter, `text_stats`), `crates/playback/tests/resolver.rs` (tests)
- Modify (match arms only, so the workspace builds): `crates/timeline/src/scene.rs:61-77` (`covers_opaquely`), `crates/project-scenes/src/lib.rs:129-138` (`top_media_layer`)

**Interfaces:**
- Consumes: `kadr_core::{CpuFrame, FramePool}`, the CPU `draw_frame`, resolver `draws_nothing`, `BLANK`.
- Produces:
  - `kadr_scene::TextPicture` — `trait TextPicture: Send + Sync + std::fmt::Debug { fn key(&self) -> u64; fn render(&self, scale: f32, pool: &kadr_core::FramePool) -> kadr_core::CpuFrame; }`.
  - `kadr_scene::TextLayer { pub picture: Arc<dyn TextPicture> }` (`Clone, Debug`; `PartialEq` by `picture.key()`); `LayerContent::Text(TextLayer)`.
  - `kadr_playback::text::{TextRasters, text_scale_step, step_scale, MAX_TEXT_SIDE}`: `text_scale_step(placement: &Placement, canvas: SizeU, output: SizeU) -> i32`, `step_scale(step: i32) -> f32`, `TextRasters::new(pool: FramePool, max_bytes: usize)`, `TextRasters::get(&self, picture: &dyn TextPicture, step: i32) -> Arc<CpuFrame>`, `TextRasters::stats(&self) -> (u64, u64)` (drawn, hits).
  - `Resolver::text_stats(&self) -> (u64, u64)`.

- [ ] **Step 1: Write the failing renderer tests** — in `crates/render/src/cpu.rs` `mod tests` (it has `layer`, `media`, `scene`, `frame`, `render`, `px`, `near`, `RED`, `BLUE`) add:

```rust
    /// A text picture for layers whose raster the test hands in as input.
    #[derive(Debug)]
    struct Pic;
    impl kadr_scene::TextPicture for Pic {
        fn key(&self) -> u64 {
            1
        }
        fn render(&self, _: f32, _: &kadr_core::FramePool) -> CpuFrame {
            unreachable!("renderers get rasters as inputs")
        }
    }

    fn text_layer(placement: Placement) -> Layer {
        layer(LayerContent::Text(kadr_scene::TextLayer { picture: Arc::new(Pic) }), placement)
    }

    #[test]
    fn a_text_layer_samples_its_raster_like_media_and_without_one_draws_nothing() {
        let c = SizeU::new(8, 8);
        // A 16 × 16 raster on the 8 × 8 box: half-transparent white, premultiplied.
        let raster = frame(16, 16, AlphaMode::Premultiplied, |x, _| if x < 8 { [128, 128, 128, 128] } else { [0, 0, 0, 0] });
        let s = scene((8, 8), (8, 8), BLUE, vec![text_layer(Placement::fill(c))]);
        let out = render(&s, vec![LayerInput::Cpu(raster.clone())]);
        assert!(near(px(&out, 8, 1, 3), [128, 128, 255, 255]), "{:?}", px(&out, 8, 1, 3));
        assert_eq!(px(&out, 8, 6, 3), [0, 0, 255, 255], "transparent texels leave the background");
        let as_media = render(&scene((8, 8), (8, 8), BLUE, vec![media(Placement::fill(c))]), vec![LayerInput::Cpu(raster)]);
        assert_eq!(out, as_media, "sampled exactly like a media frame of the same raster");
        let missing = render(&s, vec![LayerInput::Missing(MissingReason::DecodeFailed)]);
        assert!((0..8).all(|y| (0..8).all(|x| px(&missing, 8, x, y) == [0, 0, 255, 255])), "no raster: nothing — never the missing-media colour");
        let inputs = RenderInputs { layers: vec![LayerInput::None] };
        let mut buf = vec![0u8; 8 * 8 * 4];
        let err = CpuRenderer::new().render(&PreparedFrame { scene: &s, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(8, 8, &mut buf)));
        assert!(matches!(err, Err(RenderError::InputMismatch(ref m)) if m.contains("text layer with a None input")), "{err:?}");
    }
```

In `crates/render/src/cost.rs` tests add:

```rust
    #[test]
    fn a_text_layer_costs_like_a_media_layer() {
        #[derive(Debug)]
        struct Pic;
        impl kadr_scene::TextPicture for Pic {
            fn key(&self) -> u64 {
                1
            }
            fn render(&self, _: f32, _: &kadr_core::FramePool) -> kadr_core::CpuFrame {
                unreachable!()
            }
        }
        let canvas = SizeU::new(64, 36);
        let p = Placement::fill(canvas);
        let mk = |content| Layer { id: LayerId(1), content, crop: p.full_crop(), placement: p, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] };
        let media = mk(LayerContent::Media {
            media: MediaRef { media: kadr_core::AssetId::new(), stream: 0, kind: SourceKind::Video, display_size: canvas, color: kadr_core::ColorInfo::WORKING_SDR },
            source_time: kadr_core::Time::ZERO,
        });
        let text = mk(LayerContent::Text(kadr_scene::TextLayer { picture: std::sync::Arc::new(Pic) }));
        let scene = FrameScene::empty(kadr_core::Time::ZERO, canvas, OutputSpec::new(canvas, RenderQuality::Export));
        assert_eq!(layer_cost(&scene, &text).ops, layer_cost(&scene, &media).ops);
    }
```

(Adjust the `use` lines of the test module to what it already imports; `Cost.ops` is the field `layer_cost` returns.)

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kadr-render --lib`
Expected: FAIL to compile — `cannot find trait TextPicture in crate kadr_scene`, `TextLayer`, no variant `Text`.

- [ ] **Step 3: The scene type and contract** — in `crates/scene/src/scene.rs`, before `pub enum LayerContent`:

```rust
/// The text engine's picture of a text layer at one instant (titles spec
/// §5.2): opaque to the scene and to renderers. The resolver asks it for
/// texels at the scale the output needs (`kadr-playback`, `text`).
pub trait TextPicture: Send + Sync + std::fmt::Debug {
    /// Equal keys ⇒ pixel-identical rasters at every scale: the cache key.
    fn key(&self) -> u64;
    /// The picture's box — the layer's `placement.size`, canvas px — at
    /// `scale` raster px per canvas px: premultiplied RGBA8 in the working
    /// space, `max(1, ⌈size · scale⌉)` px each way.
    fn render(&self, scale: f32, pool: &kadr_core::FramePool) -> kadr_core::CpuFrame;
}

/// A text layer (titles spec §5.2). Layers compare by their picture's key.
#[derive(Clone, Debug)]
pub struct TextLayer {
    pub picture: std::sync::Arc<dyn TextPicture>,
}

impl PartialEq for TextLayer {
    fn eq(&self, other: &Self) -> bool {
        self.picture.key() == other.picture.key()
    }
}
```

and the variant `Text(TextLayer),` after `Generator(Generator),` in `LayerContent`. In `crates/scene/src/lib.rs`, after the Sampling sentence "…the scene itself never changes because of it." insert:

```rust
//! A [`LayerContent::Text`] layer's texels are the premultiplied raster
//! the resolver got from its [`TextPicture`] (at any resolution: texel
//! size is `placement.size / raster size`, as for a media frame); it is
//! sampled, cropped, filtered by its effects and composited exactly like a
//! `Media` layer. A text layer without a raster (a `Missing` input) draws
//! nothing at all — never `Rgba::MISSING`. A text layer never counts as
//! opaque when layers are culled.
```

- [ ] **Step 4: The renderer arms** — `crates/render/src/cpu.rs::check_inputs`: make the first two arms

```rust
                (LayerContent::Media { .. } | LayerContent::Text(_), LayerInput::Cpu(f)) => check_frame(f).map_err(|m| RenderError::InputMismatch(format!("layer {n}{within}: {m}")))?,
                (LayerContent::Media { .. } | LayerContent::Text(_), LayerInput::Missing(_)) | (LayerContent::Solid(_) | LayerContent::Generator(_), LayerInput::None) => {}
```

and add `LayerContent::Text(_) => "text",` to the kind names. `draw_layer`, before the `Media` arms:

```rust
            (LayerContent::Text(_), LayerInput::Cpu(f)) => draw_frame(data, stride, g, layer, f, passes),
            // No raster: a text layer shows nothing (contract, «Text»).
            (LayerContent::Text(_), _) => Drawn::Nothing,
```

`crates/render/src/cost.rs::layer_cost`: `LayerContent::Media { .. } | LayerContent::Solid(_) | LayerContent::Text(_) => px * per_px,` (rasterising happens before rendering). `crates/render/tests/reference/mod.rs::draw_plain`, in the `source` match before `_ =>`:

```rust
        (LayerContent::Text(_), LayerInput::Cpu(f)) => Source::Texels(f),
        // The contract: a text layer without a raster draws nothing.
        (LayerContent::Text(_), _) => return,
```

`crates/timeline/src/scene.rs::covers_opaquely`: `LayerContent::Text(_) => false,` with the comment `// Text never occludes (titles spec §5.2): its raster is not proven opaque.` `crates/project-scenes/src/lib.rs::top_media_layer`: `LayerContent::Media { .. } | LayerContent::Text(_) => Some(l.id),` (the preview's name badge names a text clip too); rename the doc of `top_clip` to "The clip of the top-most media or text layer…".

- [ ] **Step 5: Run the renderer tests**

Run: `cargo test -p kadr-render --lib && cargo test -p kadr-scene`
Expected: PASS.

- [ ] **Step 6: The parity test** — in `crates/render/tests/reference_parity.rs` (after the hand-written sanity tests):

```rust
#[derive(Debug)]
struct NoPicture;
impl TextPicture for NoPicture {
    fn key(&self) -> u64 {
        7
    }
    fn render(&self, _: f32, _: &kadr_core::FramePool) -> CpuFrame {
        unreachable!("renderers get rasters as inputs")
    }
}

/// Titles spec §5.3: a text raster — premultiplied, finer than the output,
/// on a rotated, scaled, cropped, faded placement with a colour effect —
/// renders the same in the CPU renderer and the reference; without a raster
/// it is nothing in both.
#[test]
fn cpu_renderer_matches_the_reference_on_text_layers() {
    let (w, h) = (48u32, 27u32);
    let raster: Vec<u8> = (0..60u32 * 20)
        .flat_map(|i| {
            let (x, y) = (i % 60, i / 60);
            let a = ((x * 7 + y * 13) % 256) as u8;
            let v = (a as u32 * ((x * 5) % 256) / 255) as u8;
            [v, a / 2, a, a]
        })
        .collect();
    let raster = Arc::new(CpuFrame::from_rgba8(60, 20, ColorInfo { alpha: AlphaMode::Premultiplied, ..ColorInfo::WORKING_SDR }, raster));
    let placement = Placement { size: Vec2::new(30.0, 10.0), anchor: Vec2::new(0.4, 0.6), position: Vec2::new(24.0, 13.0), scale: Vec2::new(1.3, 1.3), rotation: 0.35 };
    let text = Layer {
        id: LayerId(9),
        content: LayerContent::Text(TextLayer { picture: Arc::new(NoPicture) }),
        crop: RectF::new(2.0, 1.0, 28.0, 10.0),
        placement,
        opacity: 0.8,
        blend: BlendMode::Normal,
        effects: vec![Effect::ColorAdjust(ColorAdjust { exposure: 0.3, contrast: 1.1, saturation: 0.8, temperature: 0.0, tint: 0.0 })],
    };
    let s = scene((w, h), (w, h), rgba(0.1, 0.3, 0.6, 1.0), vec![text]);
    for input in [LayerInput::Cpu(raster), LayerInput::Missing(MissingReason::Offline)] {
        let inputs = RenderInputs { layers: vec![input] };
        let mut cpu = vec![0u8; (w * h * 4) as usize];
        CpuRenderer::new().render(&PreparedFrame { scene: &s, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(w, h, &mut cpu))).unwrap();
        let (reference, ties) = render_reference_with_ties(&s, &inputs);
        for (i, ((a, b), t)) in cpu.iter().zip(&reference).zip(&ties).enumerate() {
            assert!(a.abs_diff(*b) <= 1 + *t, "byte {i}: cpu {a}, reference {b}");
        }
    }
}
```

(If `render_reference_with_ties` returns the tie mask in another shape, compare the way `cpu_renderer_matches_the_reference_on_random_scenes` does.)

Run: `cargo test -p kadr-render --test reference_parity`
Expected: PASS.

- [ ] **Step 7: Write the failing resolver tests** — create `crates/playback/src/text.rs` with only its tests for now (add `pub mod text;` to `crates/playback/src/lib.rs` and `pub use text::{text_scale_step, step_scale, TextRasters, MAX_TEXT_SIDE};`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_scene::Vec2;

    fn placed(w: f32, h: f32, scale: f32) -> Placement {
        Placement { size: Vec2::new(w, h), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(0.0, 0.0), scale: Vec2::new(scale, -scale), rotation: 0.3 }
    }

    #[test]
    fn the_step_is_the_output_footprint_rounded_up() {
        let hd = SizeU::new(1920, 1080);
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 1.0), hd, hd), 0);
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 1.0), hd, SizeU::new(960, 540)), -8, "half quality: half scale");
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 1.0), hd, SizeU::new(3840, 2160)), 8, "a 4K export: twice");
        assert_eq!(text_scale_step(&placed(800.0, 120.0, 3.0), hd, hd), 13, "3× rounds up to 2^(13/8) ≈ 3.08 (a mirrored scale counts by size)");
        assert_eq!(step_scale(13), (13.0f32 / 8.0).exp2());
        let wide = text_scale_step(&placed(5000.0, 40.0, 1.0), hd, SizeU::new(3840, 2160));
        assert!(5000.0 * step_scale(wide) <= MAX_TEXT_SIDE && 5000.0 * step_scale(wide + 1) > MAX_TEXT_SIDE, "the largest step that fits 8192 px: {wide}");
        assert_eq!(text_scale_step(&placed(800.0, 120.0, f32::NAN), hd, hd), 0, "a non-finite scale counts as 1");
    }
}
```

and in `crates/playback/tests/resolver.rs` add (extend the imports with `kadr_core::{ColorInfo, FramePool}`, `kadr_scene::{TextLayer, TextPicture}`, `std::sync::atomic::{AtomicU64, Ordering}`):

```rust
/// A text picture that counts its renders and the scales asked for.
#[derive(Debug)]
struct Counting {
    key: u64,
    size: Vec2,
    calls: AtomicU64,
    scales: std::sync::Mutex<Vec<f32>>,
}

impl Counting {
    fn new(key: u64, w: f32, h: f32) -> Arc<Counting> {
        Arc::new(Counting { key, size: Vec2::new(w, h), calls: AtomicU64::new(0), scales: Default::default() })
    }
}

impl TextPicture for Counting {
    fn key(&self) -> u64 {
        self.key
    }
    fn render(&self, scale: f32, _: &FramePool) -> CpuFrame {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.scales.lock().unwrap().push(scale);
        let (w, h) = (((self.size.x * scale).ceil() as u32).max(1), ((self.size.y * scale).ceil() as u32).max(1));
        CpuFrame::from_rgba8(w, h, ColorInfo::WORKING_SDR, vec![255; (w * h * 4) as usize])
    }
}

fn text_layer(pic: &Arc<Counting>, scale: f32, canvas: SizeU) -> Layer {
    let placement = Placement { size: pic.size, anchor: Vec2::new(0.5, 0.5), position: Vec2::new(canvas.w as f32 / 2.0, canvas.h as f32 / 2.0), scale: Vec2::new(scale, scale), rotation: 0.0 };
    Layer { id: LayerId(77), content: LayerContent::Text(TextLayer { picture: pic.clone() }), crop: placement.full_crop(), placement, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
}

fn text_scene(canvas: SizeU, out: SizeU, layers: Vec<Layer>) -> FrameScene {
    FrameScene { layers, ..FrameScene::empty(Time::ZERO, canvas, OutputSpec::new(out, RenderQuality::Export)) }
}

#[test]
fn a_static_text_is_drawn_once_by_its_picture_and_never_decoded() {
    let fake = fake(0, 0);
    let r = resolver(&fake, ResolverConfig::default());
    let pic = Counting::new(5, 20.0, 8.0);
    let s = text_scene(CANVAS, CANVAS, vec![text_layer(&pic, 1.0, CANVAS)]);
    for _ in 0..3 {
        let (inputs, _) = prepare(&r, &s, &source(vec![]), Mode::Export);
        let f = frame_of(&inputs.layers[0]);
        assert_eq!((f.width, f.height), (20, 8));
    }
    assert_eq!(pic.calls.load(Ordering::Relaxed), 1);
    assert_eq!(r.text_stats(), (1, 2));
    assert_eq!(fake.open_calls(), 0);
    let other = Counting::new(6, 20.0, 8.0);
    prepare(&r, &text_scene(CANVAS, CANVAS, vec![text_layer(&other, 1.0, CANVAS)]), &source(vec![]), Mode::Export);
    assert_eq!(other.calls.load(Ordering::Relaxed), 1, "another key (an animation frame) is drawn anew");
}

#[test]
fn text_rasters_follow_the_output_footprint() {
    let fake = fake(0, 0);
    let r = resolver(&fake, ResolverConfig::default());
    let hd = SizeU::new(1920, 1080);
    let cases = [(hd, SizeU::new(960, 540), 1.0, 0.5), (hd, hd, 1.0, 1.0), (hd, SizeU::new(3840, 2160), 1.0, 2.0), (hd, hd, 3.0, (13.0f32 / 8.0).exp2())];
    for (i, (canvas, out, scale, want)) in cases.into_iter().enumerate() {
        let pic = Counting::new(100 + i as u64, 600.0, 90.0);
        let (inputs, _) = prepare(&r, &text_scene(canvas, out, vec![text_layer(&pic, scale, canvas)]), &source(vec![]), Mode::Export);
        assert_eq!(pic.scales.lock().unwrap().as_slice(), [want], "case {i}");
        assert_eq!(frame_of(&inputs.layers[0]).width, (600.0 * want).ceil() as u32, "case {i}: never upsampled from a smaller raster");
    }
}

#[test]
fn a_text_layer_that_draws_nothing_and_prefetch_never_draw_text() {
    let fake = fake(0, 0);
    let r = resolver(&fake, ResolverConfig::default());
    let pic = Counting::new(9, 20.0, 8.0);
    let mut hidden = text_layer(&pic, 1.0, CANVAS);
    hidden.opacity = 0.0;
    let (inputs, _) = prepare(&r, &text_scene(CANVAS, CANVAS, vec![hidden]), &source(vec![]), Mode::Export);
    assert_eq!((frame_of(&inputs.layers[0]).width, pic.calls.load(Ordering::Relaxed)), (1, 0), "the blank frame, no render");
    r.prefetch(&text_scene(CANVAS, CANVAS, vec![text_layer(&pic, 1.0, CANVAS)]), &source(vec![]));
    assert_eq!(pic.calls.load(Ordering::Relaxed), 0, "prefetch is for decoders only");
}
```

- [ ] **Step 8: Run them to see them fail**

Run: `cargo test -p kadr-playback`
Expected: FAIL to compile — `cannot find function text_scale_step`, `no method named text_stats`.

- [ ] **Step 9: Implement the rasters and the resolver arm** — top of `crates/playback/src/text.rs`:

```rust
//! Rasters of text layers (titles spec §5.3): drawn by the layer's own
//! picture at the scale the output needs, kept by (key, scale step) in an
//! LRU by bytes. A static text is drawn once; an animated one once per
//! frame of its animation. Drawing runs on the calling thread (the player
//! or the export), synchronously: a text is never "not ready".

use kadr_core::{CpuFrame, FramePool};
use kadr_scene::{Placement, SizeU, TextPicture};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// The largest raster side: a title magnified past it is sampled up from this.
pub const MAX_TEXT_SIDE: f32 = 8192.0;
/// Scale steps per doubling: a raster is at most 2^(1/8) ≈ 9 % finer than the output needs.
const STEPS_PER_OCTAVE: f32 = 8.0;

/// Raster px per canvas px of a step.
pub fn step_scale(step: i32) -> f32 {
    (step as f32 / STEPS_PER_OCTAVE).exp2()
}

/// The scale step of a text layer's raster: output px per canvas px times
/// the layer's larger |scale|, rounded up to a step; lowered until the
/// box's longer side fits [`MAX_TEXT_SIDE`]. A non-finite or zero scale
/// counts as 1; steps stay within ±64 (2⁻⁸ … 2⁸).
pub fn text_scale_step(placement: &Placement, canvas: SizeU, output: SizeU) -> i32 {
    let k = if canvas.w == 0 || canvas.h == 0 { 1.0 } else { (output.w as f32 / canvas.w as f32).min(output.h as f32 / canvas.h as f32) };
    let s = placement.scale.x.abs().max(placement.scale.y.abs());
    let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
    let mut step = ((k * s).log2() * STEPS_PER_OCTAVE - 1e-4).ceil() as i32;
    let side = placement.size.x.abs().max(placement.size.y.abs()).max(1.0);
    while step > -64 && side * step_scale(step) > MAX_TEXT_SIDE {
        step -= 1;
    }
    step.clamp(-64, 64)
}

struct Inner {
    map: HashMap<(u64, i32), (Arc<CpuFrame>, u64)>,
    bytes: usize,
    clock: u64,
    drawn: u64,
    hits: u64,
}

/// The text raster cache of one resolver.
pub struct TextRasters {
    pool: FramePool,
    max_bytes: usize,
    inner: Mutex<Inner>,
}

impl TextRasters {
    pub fn new(pool: FramePool, max_bytes: usize) -> Self {
        TextRasters { pool, max_bytes, inner: Mutex::new(Inner { map: HashMap::new(), bytes: 0, clock: 0, drawn: 0, hits: 0 }) }
    }

    /// The raster of `picture` at `step`: cached, or drawn now.
    pub fn get(&self, picture: &dyn TextPicture, step: i32) -> Arc<CpuFrame> {
        let key = (picture.key(), step);
        {
            let mut g = self.inner.lock();
            g.clock += 1;
            let now = g.clock;
            if let Some((f, used)) = g.map.get_mut(&key) {
                *used = now;
                let f = f.clone();
                g.hits += 1;
                return f;
            }
        }
        let frame = Arc::new(picture.render(step_scale(step), &self.pool));
        let mut g = self.inner.lock();
        g.drawn += 1;
        let now = g.clock;
        g.bytes += frame.byte_len();
        if let Some((old, _)) = g.map.insert(key, (frame.clone(), now)) {
            g.bytes -= old.byte_len();
        }
        while g.bytes > self.max_bytes && g.map.len() > 1 {
            let Some(oldest) = g.map.iter().filter(|(k, _)| **k != key).min_by_key(|(_, (_, used))| *used).map(|(k, _)| *k) else { break };
            if let Some((f, _)) = g.map.remove(&oldest) {
                g.bytes -= f.byte_len();
            }
        }
        frame
    }

    /// (rasters drawn, cache hits) since the start.
    pub fn stats(&self) -> (u64, u64) {
        let g = self.inner.lock();
        (g.drawn, g.hits)
    }
}
```

In `crates/playback/src/resolver.rs`:
  - field `texts: crate::text::TextRasters,` in `Resolver`, built in `new` as `texts: crate::text::TextRasters::new(env.pool.clone(), TEXT_CACHE_BYTES),` (create `env` first, as now) with `/// Text rasters kept per resolver (titles spec §5.3). const TEXT_CACHE_BYTES: usize = 128 << 20;` near `REASSERT`;
  - `pub fn text_stats(&self) -> (u64, u64) { self.texts.stats() }` after `stats()`;
  - `collect` gets a last parameter `texts: Option<&crate::text::TextRasters>` (passed down in the `Transition` recursion); `collect_scene` gets `texts: Option<&crate::text::TextRasters>` and passes it; `prepare` calls `self.collect_scene(scene, source, Some(&self.texts))`, `prefetch` calls `self.collect_scene(scene, source, None)`;
  - the arms, before the `Media` arms of `collect`:

```rust
                LayerContent::Text(_) if draws_nothing(l) => Slot::Input(LayerInput::Cpu(BLANK.clone())),
                // Titles spec §5.3: drawn by its picture at the output footprint
                // (prefetch, `None`, is for decoders: text waits for nothing).
                LayerContent::Text(t) => match texts {
                    Some(texts) => {
                        let step = crate::text::text_scale_step(&l.placement, scene.canvas, scene.output.size);
                        Slot::Input(LayerInput::Cpu(texts.get(t.picture.as_ref(), step)))
                    }
                    None => Slot::Input(LayerInput::None),
                },
```

  - module doc of `lib.rs`: add "- Text layers: drawn by their picture (`text::TextRasters`) at the output footprint, synchronously, cached by key and scale step."

- [ ] **Step 10: Run the tests**

Run: `cargo test -p kadr-playback && cargo test -p kadr-timeline && cargo test -p kadr-project-scenes`
Expected: PASS.

- [ ] **Step 11: Workspace build, clippy, commit**

Run: `cargo build --workspace && cargo clippy -p kadr-scene -p kadr-render -p kadr-playback --all-targets`
Expected: builds; no new warnings. (`fx_thumbs::render`'s sample path maps every non-media layer to `None`: its one-clip sample projects never hold text — leave it.)

```bash
git add crates/scene crates/render crates/playback crates/timeline/src/scene.rs crates/project-scenes/src/lib.rs
git commit -m "titles: text layers in the scene contract, renderers and resolver

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 3: `kadr-text` — fonts, layout, the raster (fill, outlines, plaque, colour emoji) and `TextFrame`

**Files:**
- Create: `crates/text/Cargo.toml`, `crates/text/build.rs`, `crates/text/src/{lib,fonts,layout,anim,shape,raster,frame}.rs`, `crates/text/tests/golden.rs`, `crates/text/tests/golden/*.png`, `crates/text/fonts/<dir>/*.ttf` + `OFL.txt` (14 families)
- Modify: `ARCHITECTURE.md` (§3 crate table: a `kadr-text` row after `kadr-effects`)

**Interfaces:**
- Consumes: Task 1 `kadr_project::{TextDoc, Rgba8, Fill, Align, …}`; Task 2 `kadr_scene::TextPicture`; `kadr_core::{CpuFrame, FramePool, ColorInfo, Time}`.
- Produces:
  - `kadr_text::fonts() -> &'static Fonts`; `Fonts::{families(&self) -> Vec<Family>, has(&self, &str) -> bool, add_font_bytes(&self, Vec<u8>) -> Vec<String>, install_user_font(&self, &Path) -> Result<Vec<String>, String>, load_user_dir(&self, &Path) -> usize, generation(&self) -> u64}`; `Family { name: String, source: FontSource }`; `FontSource::{Bundled, User, System}`; `DEFAULT_FAMILY = "Montserrat"`; `BUNDLED_FAMILIES: &[&str]`.
  - `kadr_text::layout::{layout(&TextDoc) -> Arc<Laid>, Laid { faces, glyphs, lines, width, height, letters, words, missing, max_size }, Glyph { face, id: u16, x, y, size, advance, run, letter, word, line, visible }, LineBox { top, bottom, baseline, left, right }, Face}`.
  - `kadr_text::anim::{Clock { since_in: Time, until_out: Time, source: Time, highlight: f64 }, Clock::REST, GlyphAnim { dx, dy, scale, rotation, opacity, blur: f32, colour: Option<Rgba8> }, GlyphAnim::REST, FrameAnim { glyphs: Vec<GlyphAnim>, block: GlyphAnim, plaque_opacity: f32, cursor: Option<usize> }, FrameAnim::{rest(n), key() -> u64}, animate(&TextDoc, &Laid, Clock) -> FrameAnim}` (Task 3: always rest; Task 10 fills it).
  - `kadr_text::shape::{Placed { x, y, rotation: f32 }, place(&TextDoc, &Laid) -> Vec<Placed>}` (Task 3: straight; Task 9 adds arcs and circles).
  - `kadr_text::frame_size(&TextDoc) -> (f32, f32)` (the box the preview's handles frame, centred on the anchor);
  - `kadr_text::{TextFrame, Bounds}`: `TextFrame::new(&TextDoc, Clock) -> TextFrame`, `bounds() -> Bounds` (canvas px about the layout box centre), `layout_size() -> (f32, f32)`, `missing_fonts() -> &[String]`, `laid() -> &Laid`, `raster(scale: f32) -> CpuFrame`; `impl TextPicture for TextFrame`. `Bounds { x0, y0, x1, y1 }` with `width()`, `height()`, `grow(f32)`, `union(Bounds)`.
  - crate-private for Tasks 9–10: `raster::{Canvas, Mask, Final, finals, bounds, render, glyph_image, plaque_rects}`.

- [ ] **Step 1: Fetch the bundled fonts** (network; OFL from github.com/google/fonts). In Git Bash from the repo root:

```bash
mkdir -p crates/text/fonts
for d in montserrat inter rubik oswald robotocondensed playfairdisplay ptserif lobster comfortaa unbounded caveat russoone pressstart2p exo2; do
  mkdir -p "crates/text/fonts/$d"
  curl -sL "https://api.github.com/repos/google/fonts/contents/ofl/$d" \
    | grep -o '"download_url": *"[^"]*"' | sed 's/.*"\(https[^"]*\)"/\1/' \
    | grep -E '\.ttf$|/OFL\.txt$' \
    | while read -r url; do
        name=$(basename "$url" | sed 's/%5B/[/g; s/%5D/]/g; s/%2C/,/g')
        curl -sL -o "crates/text/fonts/$d/$name" "$url"
      done
done
du -sh crates/text/fonts; ls crates/text/fonts/*
```

Expected (this exact script was run on 2026-10-12: 9.3 MB, 23 font files, 14 `OFL.txt`): each directory holds its `.ttf` files (variable `Family[wght].ttf`, plus `-Italic[wght]` for Montserrat, Inter, Rubik, Roboto Condensed, Playfair Display, Exo 2; four static PT Serif `PT_Serif-Web-*.ttf`; static Lobster, Russo One, Press Start 2P) and `OFL.txt`. If the total is over 12 MB, delete the `*-Italic*` files of `inter`, `rubik`, `robotocondensed` and `exo2` (fake italic covers them) and check again. If a directory is empty (the API rate limit: 60 requests / hour), wait and rerun for that directory only. If a family is missing from `ofl/` (Roboto Condensed may live under `apache/`), use `apache/<dir>` and its `LICENSE.txt`. Never add a font without its licence file.

- [ ] **Step 2: The crate skeleton and the failing font tests** — `crates/text/Cargo.toml`:

```toml
[package]
name = "kadr-text"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Kadr's text engine: fonts, layout, rasterisation, animation and styles of text clips (titles spec §5)"
build = "build.rs"

[dependencies]
kadr-core = { path = "../core" }
kadr-project = { path = "../project" }
kadr-scene = { path = "../scene" }
parley = { version = "=0.11.1", default-features = false, features = ["std", "system", "complex-scripts"] }
swash = { version = "=0.2.10", default-features = false, features = ["std", "scale", "render"] }
parking_lot.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true

[dev-dependencies]
png = "0.17"
```

`crates/text/build.rs` (embeds whatever font files Step 1 fetched — no hand-kept file list):

```rust
//! Embeds every .ttf / .otf under `fonts/` (titles spec §5.1) as
//! `BUNDLED_FILES: &[(&str, &[u8])]` in `$OUT_DIR/bundled.rs`.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("fonts");
    println!("cargo:rerun-if-changed=fonts");
    let mut files = vec![];
    for dir in std::fs::read_dir(&root).expect("crates/text/fonts (Task 3, Step 1)") {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            let ext = f.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("ttf" | "otf")) {
                files.push(f);
            }
        }
    }
    files.sort();
    let mut out = String::from("/// Every bundled font file: (path under crates/text/fonts, bytes).\npub static BUNDLED_FILES: &[(&str, &[u8])] = &[\n");
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        writeln!(out, "    ({rel:?}, include_bytes!({:?})),", f.to_string_lossy()).unwrap();
    }
    out.push_str("];\n");
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("bundled.rs"), out).unwrap();
}
```

`crates/text/src/lib.rs`:

```rust
//! Kadr's text engine (titles spec §5.1): the fonts, layout, pixels,
//! animation and built-in styles of text clips. Pure Rust: parley (with
//! fontique and harfrust) lays text out, swash (with zeno) rasterises it.
//! The evaluator builds a [`TextFrame`] per text clip and instant; the
//! resolver asks it for a raster through `kadr_scene::TextPicture`.

pub mod anim;
pub mod fonts;
pub mod frame;
pub mod layout;
pub(crate) mod raster;
pub mod shape;

pub use anim::{Clock, FrameAnim, GlyphAnim};
pub use fonts::{fonts, Family, FontSource, BUNDLED_FAMILIES, DEFAULT_FAMILY};
pub use frame::{frame_size, Bounds, TextFrame};
pub use layout::{layout, Laid};

/// Tests that compare layouts or keys across calls hold this while the
/// font test adds a font (which bumps the font generation in between).
#[cfg(test)]
pub(crate) static TEST_FONTS: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// A 64-bit hash of a value's JSON (in-process cache keys only).
pub(crate) fn hash_of(v: &impl serde::Serialize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(v).unwrap_or_default().hash(&mut h);
    h.finish()
}
```

In `crates/text/src/fonts.rs` write the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_cover_cyrillic() {
        assert!(BUNDLED_FILES.len() >= 14, "Step 1 fetched the fonts: {}", BUNDLED_FILES.len());
        for (name, bytes) in BUNDLED_FILES {
            let font = swash::FontRef::from_index(bytes, 0).unwrap_or_else(|| panic!("{name} is a font"));
            let cmap = font.charmap();
            for ch in ['Ж', 'я', 'Ё', 'A', 'z', '0', '!'] {
                assert_ne!(cmap.map(ch), 0, "{name} lacks {ch:?}");
            }
        }
    }

    #[test]
    fn bundled_fonts_fit_the_budget_and_carry_their_licences() {
        let total: usize = BUNDLED_FILES.iter().map(|(_, b)| b.len()).sum();
        assert!(total <= 12 << 20, "{} MB of fonts", total >> 20);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
        for dir in std::fs::read_dir(&root).unwrap().map(|d| d.unwrap().path()).filter(|d| d.is_dir()) {
            assert!(dir.join("OFL.txt").exists() || dir.join("LICENSE.txt").exists(), "{} has no licence file", dir.display());
        }
    }

    #[test]
    fn every_bundled_family_is_known_by_name_and_listed_first() {
        let f = fonts();
        for name in BUNDLED_FAMILIES {
            assert!(f.has(name), "{name}");
        }
        let all = f.families();
        let firsts: Vec<&str> = all.iter().take(BUNDLED_FAMILIES.len()).map(|x| x.name.as_str()).collect();
        assert_eq!(firsts, BUNDLED_FAMILIES);
        assert!(all.iter().take(BUNDLED_FAMILIES.len()).all(|x| x.source == FontSource::Bundled));
        assert!(all.iter().any(|x| x.source == FontSource::System && x.name == "Segoe UI"), "Windows' own fonts are listed");
        let mut names: Vec<String> = all.iter().map(|x| x.name.to_lowercase()).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "each family once");
        assert!(!f.has("No Such Font Anywhere"));
    }

    #[test]
    fn a_font_file_can_be_added_and_garbage_cannot() {
        let _serial = crate::TEST_FONTS.lock();
        let f = fonts();
        let g0 = f.generation();
        let (_, lobster) = BUNDLED_FILES.iter().find(|(n, _)| n.starts_with("lobster/")).expect("Lobster");
        assert!(f.add_font_bytes(lobster.to_vec()).iter().any(|n| n == "Lobster"), "registering a font returns its families");
        assert!(f.generation() > g0, "layouts made before are stale");
        assert!(f.add_font_bytes(b"not a font".to_vec()).is_empty());
        let dir = std::env::temp_dir().join(format!("kadr-text-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.ttf");
        std::fs::write(&junk, b"nope").unwrap();
        assert!(f.install_user_font(&junk).is_err(), "a file that is not a font is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

Run: `cargo test -p kadr-text fonts`
Expected: FAIL to compile — `cannot find value BUNDLED_FILES`, `fonts`, `FontSource` (and `anim`, `frame`, … are not written yet: create those files empty except for `//!` lines so the crate builds as far as `fonts`).

- [ ] **Step 3: Implement the fonts** — above the tests in `crates/text/src/fonts.rs`:

```rust
//! Fonts (titles spec §2.4, §5.1): one collection per process — the
//! bundled families (embedded), Windows' fonts (DirectWrite: the system's
//! and the user's own installs) and the fonts added to Kadr's data folder.
//! A family the collection lacks is drawn in [`DEFAULT_FAMILY`]; characters
//! a font lacks come from a system font for their script (parley and
//! fontique choose it), emoji from Segoe UI Emoji.

use crate::layout::RunIx;
use parking_lot::Mutex;
use parley::fontique::{Blob, Collection, CollectionOptions, SourceCache};
use parley::{FontContext, LayoutContext};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

/// The family of a new text and the stand-in for a missing one.
pub const DEFAULT_FAMILY: &str = "Montserrat";

/// The bundled families in the picker's order (their names as the fonts
/// declare them; Task 3's tests check each resolves).
pub const BUNDLED_FAMILIES: &[&str] = &[
    "Montserrat", "Inter", "Rubik", "Oswald", "Roboto Condensed", "Playfair Display", "PT Serif",
    "Lobster", "Comfortaa", "Unbounded", "Caveat", "Russo One", "Press Start 2P", "Exo 2",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontSource {
    Bundled,
    User,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub source: FontSource,
}

pub(crate) struct Cx {
    pub fonts: FontContext,
    pub layout: LayoutContext<RunIx>,
}

pub struct Fonts {
    cx: Mutex<Cx>,
    user: Mutex<Vec<String>>,
    user_dir: Mutex<Option<PathBuf>>,
    generation: AtomicU64,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// The process's fonts; the first call builds the collection (about 1 ms
/// warm, 20 ms cold — the editor warms it on a background thread).
pub fn fonts() -> &'static Fonts {
    FONTS.get_or_init(Fonts::load)
}

impl Fonts {
    fn load() -> Fonts {
        let mut collection = Collection::new(CollectionOptions { shared: false, system_fonts: true });
        for (_, bytes) in BUNDLED_FILES {
            collection.register_fonts(Blob::new(Arc::new(*bytes)), None);
        }
        Fonts {
            cx: Mutex::new(Cx { fonts: FontContext { collection, source_cache: SourceCache::default() }, layout: LayoutContext::new() }),
            user: Mutex::new(vec![]),
            user_dir: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }

    pub(crate) fn with_cx<R>(&self, f: impl FnOnce(&mut Cx) -> R) -> R {
        f(&mut self.cx.lock())
    }

    /// Bumped whenever a font is added: cached layouts made before are stale.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn has(&self, family: &str) -> bool {
        self.cx.lock().fonts.collection.family_id(family).is_some()
    }

    /// Every family to pick from: the bundled ones (in [`BUNDLED_FAMILIES`]
    /// order), the user's, then the system's (sorted), each once.
    pub fn families(&self) -> Vec<Family> {
        let names: BTreeSet<String> = {
            let mut cx = self.cx.lock();
            let c = &mut cx.fonts.collection;
            let all: Vec<String> = c.family_names().map(str::to_owned).collect();
            let mut canon = BTreeSet::new();
            for n in &all {
                if let Some(id) = c.family_id(n)
                    && let Some(name) = c.family_name(id)
                {
                    canon.insert(name.to_owned());
                }
            }
            canon
        };
        let user = self.user.lock().clone();
        let taken = |n: &str| BUNDLED_FAMILIES.iter().any(|b| b.eq_ignore_ascii_case(n)) || user.iter().any(|u| u.eq_ignore_ascii_case(n));
        let mut out: Vec<Family> = BUNDLED_FAMILIES.iter().map(|n| Family { name: n.to_string(), source: FontSource::Bundled }).collect();
        out.extend(user.iter().map(|n| Family { name: n.clone(), source: FontSource::User }));
        out.extend(names.into_iter().filter(|n| !taken(n) && !n.starts_with('@')).map(|name| Family { name, source: FontSource::System }));
        out
    }

    /// Registers a font file's bytes; returns the families it holds (none:
    /// not a font). Layouts are refreshed (`generation`).
    pub fn add_font_bytes(&self, bytes: Vec<u8>) -> Vec<String> {
        let names: Vec<String> = {
            let mut cx = self.cx.lock();
            let c = &mut cx.fonts.collection;
            let ids: Vec<_> = c.register_fonts(Blob::from(bytes), None).into_iter().map(|(id, _)| id).collect();
            ids.into_iter().filter_map(|id| c.family_name(id).map(str::to_owned)).collect()
        };
        if !names.is_empty() {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        names
    }

    /// Registers every .ttf / .otf / .ttc in `dir` as the user's fonts and
    /// remembers `dir` for [`Fonts::install_user_font`]; returns how many
    /// files were fonts.
    pub fn load_user_dir(&self, dir: &Path) -> usize {
        *self.user_dir.lock() = Some(dir.to_path_buf());
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        let mut n = 0;
        for f in entries.flatten().map(|e| e.path()) {
            let ext = f.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&f) else { continue };
            let names = self.add_font_bytes(bytes);
            if !names.is_empty() {
                n += 1;
                self.remember_user(names);
            } else {
                tracing::warn!(file = %f.display(), "not a font: skipped");
            }
        }
        n
    }

    /// «Добавить шрифт…»: registers the file and copies it into the user
    /// font folder (when one was loaded), so it is there after a restart.
    pub fn install_user_font(&self, file: &Path) -> Result<Vec<String>, String> {
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        let names = self.add_font_bytes(bytes);
        if names.is_empty() {
            return Err("not a font".into());
        }
        if let Some(dir) = self.user_dir.lock().clone() {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let name = file.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "font.ttf".into());
            let target = dir.join(name);
            if !target.exists() {
                std::fs::copy(file, &target).map_err(|e| e.to_string())?;
            }
        }
        self.remember_user(names.clone());
        Ok(names)
    }

    fn remember_user(&self, names: Vec<String>) {
        let mut user = self.user.lock();
        for n in names {
            if !user.iter().any(|u| u.eq_ignore_ascii_case(&n)) && !BUNDLED_FAMILIES.iter().any(|b| b.eq_ignore_ascii_case(&n)) {
                user.push(n);
            }
        }
        user.sort_by_key(|n| n.to_lowercase());
    }
}
```

Run: `cargo test -p kadr-text fonts`
Expected: PASS. If `every_bundled_family_is_known_by_name_and_listed_first` names a family that does not resolve, print `fonts().families()` (bundled files' own names) and correct that entry of `BUNDLED_FAMILIES` to the name the font declares.

- [ ] **Step 4: Write the failing layout tests** — in `crates/text/src/layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{Fill, Rgba8, RunStyle, TextDoc};

    fn doc(text: &str) -> TextDoc {
        let mut d = TextDoc::new(text);
        d.style.size = 50.0;
        d
    }

    #[test]
    fn glyphs_map_back_to_runs_letters_words_and_lines() {
        let mut d = doc("ab cd\nэф");
        d.style_range(3..5, &RunStyle { fill: Some(Fill::solid(Rgba8::BLACK)), ..Default::default() });
        let l = layout(&d);
        let vis: Vec<(usize, usize, usize, usize)> = l.glyphs.iter().filter(|g| g.visible).map(|g| (g.run, g.letter, g.word, g.line)).collect();
        assert_eq!(vis, [(0, 0, 0, 0), (0, 1, 0, 0), (1, 2, 1, 0), (1, 3, 1, 0), (2, 4, 2, 1), (2, 5, 2, 1)]);
        assert_eq!((l.letters, l.words, l.lines.len()), (6, 3, 2));
        // Line boxes may overlap by a pixel (parley rounds each one outwards).
        assert!(l.lines[1].top >= l.lines[0].bottom - 1.5 && l.lines[1].baseline > l.lines[0].baseline);
        assert!(l.glyphs.iter().all(|g| g.size == 50.0));
        assert!(l.glyphs.iter().filter(|g| g.visible).all(|g| g.id != 0));
    }

    #[test]
    fn wrapping_and_alignment_shape_the_box() {
        let mut d = doc("один два три четыре пять шесть");
        let one_line = layout(&d);
        assert_eq!(one_line.lines.len(), 1);
        d.paragraph.wrap = Some(300.0);
        let wrapped = layout(&d);
        assert!(wrapped.lines.len() >= 3, "{}", wrapped.lines.len());
        assert_eq!(wrapped.width, 300.0, "a wrapped block is as wide as its wrap");
        // Tolerances of 0.3 em: a line's trailing space may count in its alignment.
        for line in &wrapped.lines {
            assert!(line.right <= 300.5 && line.left >= -0.5);
            assert!(((line.left + line.right) / 2.0 - 150.0).abs() < 15.0, "centred: {line:?}");
        }
        d.paragraph.align = kadr_project::Align::Left;
        assert!(layout(&d).lines.iter().all(|l| l.left.abs() < 1.0));
        d.paragraph.align = kadr_project::Align::Right;
        assert!(layout(&d).lines.iter().all(|l| (l.right - 300.0).abs() < 15.0));
        let empty = layout(&doc(""));
        assert!(empty.width > 0.0 && empty.height > 0.0, "an empty text still has a box to grab");
    }

    #[test]
    fn caps_upper_case_at_layout_only() {
        let mut d = doc("мяч");
        let lower: Vec<u16> = layout(&d).glyphs.iter().map(|g| g.id).collect();
        d.paragraph.caps = true;
        let upper: Vec<u16> = layout(&d).glyphs.iter().map(|g| g.id).collect();
        assert_ne!(lower, upper);
        assert_eq!(upper, layout(&doc("МЯЧ")).glyphs.iter().map(|g| g.id).collect::<Vec<_>>());
        assert_eq!(d.plain(), "мяч");
    }

    #[test]
    fn cyrillic_emoji_and_symbols_never_draw_notdef() {
        for family in crate::BUNDLED_FAMILIES {
            // ✓ after the Han text, as the probe verified: a Common-script symbol takes its neighbour's script.
            let mut d = doc("Привет, мир! 🎉⚽ 日本 ✓");
            d.style.font.family = family.to_string();
            let l = layout(&d);
            assert!(l.glyphs.iter().filter(|g| g.visible).all(|g| g.id != 0), "{family}: a .notdef glyph");
            assert!(l.missing.is_empty());
            let faces: std::collections::BTreeSet<usize> = l.glyphs.iter().filter(|g| g.visible).map(|g| g.face).collect();
            assert!(faces.len() >= 2, "{family}: the emoji and the CJK come from fallback fonts");
        }
    }

    #[test]
    fn a_missing_family_falls_back_and_says_so() {
        let mut d = doc("Матч");
        d.style.font.family = "Кадр Несуществующий".into();
        d.runs[0].style.family = Some("Also Missing".into());
        let l = layout(&d);
        assert_eq!(l.missing, ["Кадр Несуществующий", "Also Missing"]);
        assert!(l.glyphs.iter().all(|g| g.id != 0));
        let mut m = doc("Матч");
        m.style.font.family = crate::DEFAULT_FAMILY.into();
        let ids = |l: &Laid| l.glyphs.iter().map(|g| (g.id, g.x)).collect::<Vec<_>>();
        assert_eq!(ids(&l), ids(&layout(&m)), "drawn exactly as Montserrat");
    }

    #[test]
    fn layouts_are_cached_by_what_shapes_them() {
        let _serial = crate::TEST_FONTS.lock();
        let d = doc("кэш");
        let a = layout(&d);
        assert!(Arc::ptr_eq(&a, &layout(&d.clone())));
        let mut recoloured = d.clone();
        recoloured.style.fill = Fill::solid(Rgba8::BLACK);
        recoloured.anim.looping = Some(kadr_project::LoopAnim { preset: "pulse".into(), unit: kadr_project::Unit::Block, speed: 1.0, amount: 0.5, extra: Default::default() });
        assert!(Arc::ptr_eq(&a, &layout(&recoloured)), "paint and animation do not change the layout");
        let mut bigger = d.clone();
        bigger.style.size = 60.0;
        assert!(!Arc::ptr_eq(&a, &layout(&bigger)));
    }
}
```

Run: `cargo test -p kadr-text layout`
Expected: FAIL to compile — `cannot find function layout`.

- [ ] **Step 5: Implement the layout** — above the tests in `crates/text/src/layout.rs`:

```rust
//! Layout (titles spec §5.1): a document's runs shaped and broken into
//! lines by parley — fallback fonts, kerning, wrapping, alignment, line
//! and letter spacing, ALL CAPS — as glyphs in canvas px (layout box
//! coordinates: origin top left, y down, `y` on the baseline) with their
//! face, document run and animation units. Cached by what shapes it.

use crate::fonts::{fonts, DEFAULT_FAMILY};
use kadr_project::{Align, TextDoc};
use parking_lot::Mutex;
use parley::fontique::Blob;
use parley::{Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, LineHeight, PositionedLayoutItem, StyleProperty};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

/// The brush of laid-out text: the index of the document run it comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunIx(pub u16);

/// A face as a run uses it: the font file, its index, the variation
/// coordinates (synthesis variations included) and the synthesis left to
/// the rasteriser (fake bold, fake italic skew in degrees).
#[derive(Clone, Debug)]
pub struct Face {
    pub data: Blob<u8>,
    pub index: u32,
    pub coords: Vec<i16>,
    pub embolden: bool,
    pub skew: Option<f32>,
}

impl Face {
    fn same(&self, o: &Face) -> bool {
        self.data.id() == o.data.id() && self.index == o.index && self.coords == o.coords && self.embolden == o.embolden && self.skew == o.skew
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub face: usize,
    pub id: u16,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub advance: f32,
    /// The document run (its fill).
    pub run: usize,
    pub letter: usize,
    pub word: usize,
    pub line: usize,
    /// False for whitespace: never drawn, not a letter.
    pub visible: bool,
}

/// A line's box: `top`..`bottom` its line box, `left`..`right` its visible glyphs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineBox {
    pub top: f32,
    pub bottom: f32,
    pub baseline: f32,
    pub left: f32,
    pub right: f32,
}

#[derive(Debug)]
pub struct Laid {
    pub faces: Vec<Face>,
    pub glyphs: Vec<Glyph>,
    pub lines: Vec<LineBox>,
    /// The layout box: the wrap width (or the widest line) × the lines' height.
    pub width: f32,
    pub height: f32,
    pub letters: usize,
    pub words: usize,
    /// Families the document names that this machine lacks (drawn in DEFAULT_FAMILY).
    pub missing: Vec<String>,
    /// The largest font size in use.
    pub max_size: f32,
}

static CACHE: LazyLock<Mutex<HashMap<u64, Arc<Laid>>>> = LazyLock::new(Default::default);

fn clamp_size(s: f32) -> f32 {
    if s.is_finite() { s.clamp(1.0, 2000.0) } else { 96.0 }
}

fn weight(w: u16) -> FontWeight {
    FontWeight::new(w.clamp(100, 900) as f32)
}

fn style(italic: bool) -> FontStyle {
    if italic { FontStyle::Italic } else { FontStyle::Normal }
}

/// The document's layout, from the cache when the runs, the base font and
/// size, the paragraph and the font set are unchanged.
pub fn layout(doc: &TextDoc) -> Arc<Laid> {
    let key = crate::hash_of(&(&doc.runs, &doc.style.font, doc.style.size.to_bits(), &doc.paragraph, fonts().generation()));
    if let Some(l) = CACHE.lock().get(&key) {
        return l.clone();
    }
    let laid = Arc::new(build(doc));
    let mut c = CACHE.lock();
    if c.len() >= 256 {
        c.clear();
    }
    c.insert(key, laid.clone());
    laid
}

fn build(doc: &TextDoc) -> Laid {
    let f = fonts();
    let mut missing: Vec<String> = vec![];
    let mut family = |name: &str| -> String {
        if f.has(name) {
            name.to_string()
        } else {
            if !missing.iter().any(|m| m == name) {
                missing.push(name.to_string());
            }
            DEFAULT_FAMILY.to_string()
        }
    };
    let base_family = family(&doc.style.font.family);
    let run_families: Vec<Option<String>> = doc.runs.iter().map(|r| r.style.family.as_deref().map(&mut family)).collect();
    // The laid-out text (ALL CAPS applied) and each run's bytes in it.
    let mut text = String::new();
    let ranges: Vec<Range<usize>> = doc
        .runs
        .iter()
        .map(|r| {
            let start = text.len();
            text.push_str(&if doc.paragraph.caps { r.text.to_uppercase() } else { r.text.clone() });
            start..text.len()
        })
        .collect();
    let size = clamp_size(doc.style.size);
    let line_height = if doc.paragraph.line_height.is_finite() { doc.paragraph.line_height.clamp(0.5, 4.0) } else { 1.2 };
    let spacing = |v: f32| if v.is_finite() { v.clamp(-200.0, 2000.0) } else { 0.0 };
    let wrap = doc.paragraph.wrap.filter(|w| w.is_finite() && *w > 0.0);
    let layout = f.with_cx(|cx| {
        let mut b = cx.layout.ranged_builder(&mut cx.fonts, &text, 1.0, false);
        b.push_default(StyleProperty::FontFamily(FontFamily::named(&base_family)));
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::FontWeight(weight(doc.style.font.weight)));
        b.push_default(StyleProperty::FontStyle(style(doc.style.font.italic)));
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(line_height)));
        b.push_default(StyleProperty::LetterSpacing(spacing(doc.paragraph.letter_spacing)));
        b.push_default(StyleProperty::Brush(RunIx(0)));
        for (i, (r, range)) in doc.runs.iter().zip(&ranges).enumerate() {
            if range.is_empty() {
                continue;
            }
            b.push(StyleProperty::Brush(RunIx(i as u16)), range.clone());
            if let Some(name) = &run_families[i] {
                b.push(StyleProperty::FontFamily(FontFamily::named(name)), range.clone());
            }
            if let Some(s) = r.style.size {
                b.push(StyleProperty::FontSize(clamp_size(s)), range.clone());
            }
            if let Some(w) = r.style.weight {
                b.push(StyleProperty::FontWeight(weight(w)), range.clone());
            }
            if let Some(it) = r.style.italic {
                b.push(StyleProperty::FontStyle(style(it)), range.clone());
            }
            if let Some(ls) = r.style.letter_spacing {
                b.push(StyleProperty::LetterSpacing(spacing(ls)), range.clone());
            }
        }
        let mut layout = b.build(&text);
        layout.break_all_lines(wrap);
        let align = match doc.paragraph.align {
            Align::Left => Alignment::Left,
            Align::Center => Alignment::Center,
            Align::Right => Alignment::Right,
        };
        layout.align(align, AlignmentOptions { align_when_overflowing: false });
        layout
    });

    // Per byte of `text`: the letter and the word it belongs to.
    let (mut letter_at, mut word_at) = (vec![0usize; text.len() + 1], vec![0usize; text.len() + 1]);
    let (mut letters, mut words, mut in_word) = (0usize, 0usize, false);
    for (i, ch) in text.char_indices() {
        let ws = ch.is_whitespace();
        if !ws && !in_word {
            words += 1;
        }
        if !ws {
            letters += 1;
        }
        in_word = !ws;
        for b in i..i + ch.len_utf8() {
            (letter_at[b], word_at[b]) = (letters.saturating_sub(1), words.saturating_sub(1));
        }
    }

    let mut faces: Vec<Face> = vec![];
    let mut glyphs = vec![];
    let mut lines = vec![];
    for (li, line) in layout.lines().enumerate() {
        let m = line.metrics();
        let (mut left, mut right) = (f32::MAX, f32::MIN);
        let (mut last_run, mut cursor) = (usize::MAX, 0usize);
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else { continue };
            let run = gr.run();
            if run.index() != last_run {
                (last_run, cursor) = (run.index(), 0);
            }
            let n = gr.glyphs().count();
            // Glyph → its cluster's bytes, in the order of `positioned_glyphs`.
            let spans: Vec<Range<usize>> = run
                .visual_clusters()
                .flat_map(|c| {
                    let r = c.text_range();
                    c.glyphs().map(move |_| r.clone())
                })
                .skip(cursor)
                .take(n)
                .collect();
            cursor += n;
            let font = run.font();
            let syn = run.synthesis();
            let face = Face { data: font.data.clone(), index: font.index, coords: run.normalized_coords().to_vec(), embolden: syn.embolden(), skew: syn.skew() };
            let fi = match faces.iter().position(|f| f.same(&face)) {
                Some(i) => i,
                None => {
                    faces.push(face);
                    faces.len() - 1
                }
            };
            let doc_run = gr.style().brush.0 as usize;
            let size = run.font_size();
            for (g, span) in gr.positioned_glyphs().zip(spans) {
                let visible = !text[span.clone()].chars().all(char::is_whitespace);
                if visible {
                    left = left.min(g.x);
                    right = right.max(g.x + g.advance);
                }
                glyphs.push(Glyph { face: fi, id: g.id as u16, x: g.x, y: g.y, size, advance: g.advance, run: doc_run, letter: letter_at[span.start], word: word_at[span.start], line: li, visible });
            }
        }
        if left > right {
            (left, right) = (m.offset, m.offset);
        }
        lines.push(LineBox { top: m.block_min_coord, bottom: m.block_max_coord, baseline: m.baseline, left, right });
    }
    let width = wrap.unwrap_or(layout.width()).max(size * 0.3);
    let height = layout.height().max(size * line_height);
    let max_size = glyphs.iter().map(|g| g.size).fold(size, f32::max);
    Laid { faces, glyphs, lines, width, height, letters, words, missing, max_size }
}
```

Run: `cargo test -p kadr-text layout`
Expected: PASS. If `glyphs_map_back_to_runs_letters_words_and_lines` shows the second run starting one glyph late, check that `style_range(3..5, …)` covers "cd" (bytes 3..5) — it does for ASCII.

- [ ] **Step 6: Animation and shape skeletons** (filled by Tasks 9 and 10) — `crates/text/src/anim.rs`:

```rust
//! Text animation (titles spec §4): per-glyph state at an instant. This
//! task has the types and the resting state; Task 10 adds the presets.

use crate::layout::Laid;
use kadr_core::Time;
use kadr_project::{Rgba8, TextDoc};
use serde::Serialize;

/// Where a text clip's frame is in time (titles spec §3): since its
/// visible start, until its visible end (both clip-bound), its source time
/// (loops) and the highlight progress (0..1 over the words).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    pub since_in: Time,
    pub until_out: Time,
    pub source: Time,
    pub highlight: f64,
}

impl Clock {
    /// Long after the appearance and before the disappearance, loops at 0.
    pub const REST: Clock = Clock { since_in: Time(i64::MAX / 4), until_out: Time(i64::MAX / 4), source: Time::ZERO, highlight: 0.0 };
}

/// One glyph's (or the whole block's) animated state: an offset (canvas
/// px), a scale and a rotation (radians, clockwise) about its centre, an
/// opacity, a blur (canvas px) and a colour that replaces its fill.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GlyphAnim {
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub rotation: f32,
    pub opacity: f32,
    pub blur: f32,
    pub colour: Option<Rgba8>,
}

impl GlyphAnim {
    pub const REST: GlyphAnim = GlyphAnim { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0, opacity: 1.0, blur: 0.0, colour: None };
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrameAnim {
    /// One per glyph of the layout.
    pub glyphs: Vec<GlyphAnim>,
    /// The whole block, about the layout box centre (pulse, float).
    pub block: GlyphAnim,
    pub plaque_opacity: f32,
    /// The typewriter cursor: after this glyph (`None`: no cursor).
    pub cursor: Option<usize>,
}

impl FrameAnim {
    pub fn rest(n: usize) -> FrameAnim {
        FrameAnim { glyphs: vec![GlyphAnim::REST; n], block: GlyphAnim::REST, plaque_opacity: 1.0, cursor: None }
    }

    pub fn is_rest(&self) -> bool {
        self.block == GlyphAnim::REST && self.plaque_opacity == 1.0 && self.cursor.is_none() && self.glyphs.iter().all(|g| *g == GlyphAnim::REST)
    }

    /// 0 at rest, else a hash of the state quantised to 1/64 px and 1/1024
    /// of the rest: equal keys draw equal pixels.
    pub fn key(&self) -> u64 {
        if self.is_rest() {
            return 0;
        }
        let q = |g: &GlyphAnim| {
            let r = |v: f32, k: f32| (v * k).round() as i64;
            (r(g.dx, 64.0), r(g.dy, 64.0), r(g.scale, 1024.0), r(g.rotation, 1024.0), r(g.opacity, 1024.0), r(g.blur, 64.0), g.colour)
        };
        let glyphs: Vec<_> = self.glyphs.iter().map(q).collect();
        crate::hash_of(&(glyphs, q(&self.block), (self.plaque_opacity * 1024.0).round() as i64, self.cursor)).max(1)
    }
}

/// The animated state of every glyph at `clock` (Task 10: the presets).
pub fn animate(_doc: &TextDoc, laid: &Laid, _clock: Clock) -> FrameAnim {
    FrameAnim::rest(laid.glyphs.len())
}
```

`crates/text/src/shape.rs`:

```rust
//! The line text runs along (titles spec §1): straight, an arc, a circle.
//! This task places glyphs straight; Task 9 adds arcs and circles.

use crate::layout::Laid;
use kadr_project::TextDoc;

/// A glyph's pen origin in layout px and its rotation (radians, clockwise).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
}

pub fn place(_doc: &TextDoc, laid: &Laid) -> Vec<Placed> {
    laid.glyphs.iter().map(|g| Placed { x: g.x, y: g.y, rotation: 0.0 }).collect()
}
```

- [ ] **Step 7: Write the failing frame and raster tests** — `crates/text/src/frame.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{Outline, Plaque, Rgba8, TextDoc};

    fn styled() -> TextDoc {
        let mut d = TextDoc::starter("Йога\nкурсивом", 1080);
        d.style.font.italic = true;
        d.style.outlines = vec![Outline { width: 6.0, colour: Rgba8::BLACK }, Outline { width: 4.0, colour: Rgba8::rgba(255, 0, 0, 255) }];
        d.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::rgba(0, 0, 0, 160), radius: 10.0, padding_x: 20.0, padding_y: 8.0 });
        d
    }

    fn alpha_at(f: &kadr_core::CpuFrame, x: u32, y: u32) -> u8 {
        f.row(y)[x as usize * 4 + 3]
    }

    #[test]
    fn bounds_hold_everything_drawn() {
        for d in [styled(), TextDoc::starter("🎉 Гол! ⚽", 1080), TextDoc::starter("", 1080)] {
            let f = TextFrame::new(&d, Clock::REST);
            let r = f.raster(1.0);
            let b = f.bounds();
            assert_eq!((r.width, r.height), (b.width().ceil() as u32, b.height().ceil() as u32));
            let (w, h) = (r.width, r.height);
            let edge = (0..w).all(|x| alpha_at(&r, x, 0) == 0 && alpha_at(&r, x, h - 1) == 0) && (0..h).all(|y| alpha_at(&r, 0, y) == 0 && alpha_at(&r, w - 1, y) == 0);
            assert!(edge, "{:?}: something touches the raster's edge", d.plain());
            let (lw, lh) = f.layout_size();
            assert!(b.x0 <= -lw / 2.0 && b.x1 >= lw / 2.0 && b.y0 <= -lh / 2.0 && b.y1 >= lh / 2.0, "the layout box is inside");
        }
    }

    #[test]
    fn a_raster_is_its_box_at_its_scale_and_premultiplied() {
        let f = TextFrame::new(&styled(), Clock::REST);
        let (one, two) = (f.raster(1.0), f.raster(2.0));
        assert_eq!((two.width, two.height), ((f.bounds().width() * 2.0).ceil() as u32, (f.bounds().height() * 2.0).ceil() as u32));
        assert!(one.width > 100 && two.width > one.width);
        assert_eq!(one.color, kadr_core::ColorInfo::WORKING_SDR);
        assert!((0..one.height).all(|y| one.row(y).as_chunks::<4>().0.iter().all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])), "premultiplied");
    }

    #[test]
    fn the_key_follows_the_picture() {
        let _serial = crate::TEST_FONTS.lock();
        let d = styled();
        let k = |d: &TextDoc| kadr_scene::TextPicture::key(&TextFrame::new(d, Clock::REST));
        assert_eq!(k(&d), k(&d.clone()));
        let mut red = d.clone();
        red.style.fill.colour = Rgba8::rgba(255, 0, 0, 255);
        assert_ne!(k(&d), k(&red));
        let later = Clock { since_in: kadr_core::Time::from_secs(9), ..Clock::REST };
        assert_eq!(kadr_scene::TextPicture::key(&TextFrame::new(&d, later)), k(&d), "a still text is the same picture at every instant");
    }

    #[test]
    fn a_missing_font_is_reported_by_the_frame() {
        let mut d = TextDoc::new("x");
        d.style.font.family = "Nope Sans".into();
        assert_eq!(TextFrame::new(&d, Clock::REST).missing_fonts(), ["Nope Sans"]);
    }
}
```

and `crates/text/tests/golden.rs`:

```rust
//! Golden images of text rasters (titles spec §9): each composited over an
//! opaque #303040 so it can be looked at, compared within ±1 LSB with
//! `tests/golden/<name>.png`. `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-text
//! --test golden` (re)writes them; look at every rewritten image before
//! committing it. Fallback glyphs (emoji, CJK) come from this machine's
//! system fonts (Windows 11, Segoe UI Emoji).

use kadr_project::*;
use kadr_text::{Clock, TextFrame};

const BG: [f32; 3] = [48.0, 48.0, 64.0];

pub fn picture(frame: &TextFrame, scale: f32) -> (u32, u32, Vec<u8>) {
    let r = frame.raster(scale);
    let mut out = Vec::with_capacity((r.width * r.height * 4) as usize);
    for y in 0..r.height {
        for p in r.row(y).as_chunks::<4>().0 {
            let a = p[3] as f32 / 255.0;
            out.extend((0..3).map(|c| (p[c] as f32 + BG[c] * (1.0 - a)).round().clamp(0.0, 255.0) as u8));
            out.push(255);
        }
    }
    (r.width, r.height, out)
}

pub fn check(name: &str, doc: &TextDoc, clock: Clock, scale: f32) {
    let (w, h, px) = picture(&TextFrame::new(doc, clock), scale);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.png"));
    if std::env::var("KADR_UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        return;
    }
    let file = std::fs::File::open(&path).unwrap_or_else(|_| panic!("{} missing: run with KADR_UPDATE_GOLDEN=1 and look at it", path.display()));
    let mut dec = png::Decoder::new(file).read_info().unwrap();
    let mut want = vec![0u8; dec.output_buffer_size()];
    let info = dec.next_frame(&mut want).unwrap();
    assert_eq!((info.width, info.height), (w, h), "{name}: size");
    let bad = px.iter().zip(&want).filter(|(a, b)| a.abs_diff(**b) > 1).count();
    assert_eq!(bad, 0, "{name}: {bad} bytes differ by more than 1");
}

/// A starter title for a 540-line canvas, without its shadow (Task 9 draws
/// shadows; these goldens stay valid through it).
fn base(text: &str) -> TextDoc {
    let mut d = TextDoc::starter(text, 540);
    d.style.shadow = None;
    d.seed = 1;
    d
}

#[test]
fn text_goldens() {
    check("title_plain", &base("Привет, мир!"), Clock::REST, 1.0);
    let mut rich = base("Победа нашей команды");
    rich.style_range(13..23, &RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 200, 0, 255))), family: Some("Playfair Display".into()), size: Some(60.0), italic: Some(true), ..Default::default() });
    check("rich_runs", &rich, Clock::REST, 1.0);
    let mut outlined = base("ОБВОДКА");
    outlined.style.outlines = vec![Outline { width: 4.0, colour: Rgba8::BLACK }, Outline { width: 3.0, colour: Rgba8::rgba(255, 60, 60, 255) }];
    outlined.style.shadow = None;
    check("outlines_two", &outlined, Clock::REST, 1.0);
    let mut block = base("Плашка под блоком\nвторая строка");
    block.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::rgba(20, 20, 20, 200), radius: 12.0, padding_x: 24.0, padding_y: 10.0 });
    check("plaque_block", &block, Clock::REST, 1.0);
    let mut lines = block.clone();
    lines.style.plaque = Some(Plaque { per_line: true, colour: Rgba8::rgba(255, 220, 0, 255), radius: 6.0, padding_x: 14.0, padding_y: 4.0 });
    lines.style.fill = Fill::solid(Rgba8::BLACK);
    lines.paragraph.align = Align::Left;
    check("plaque_lines", &lines, Clock::REST, 1.0);
    let mut wrapped = base("Длинный текст переносится по ширине блока и выравнивается по центру");
    wrapped.paragraph.wrap = Some(420.0);
    wrapped.style.size = 32.0;
    check("wrap_center", &wrapped, Clock::REST, 1.0);
    check("cyrillic_emoji", &base("Гол! ⚽🔥 Ура 🎉"), Clock::REST, 1.0);
    let mut caps = base("разрядка");
    caps.paragraph.caps = true;
    caps.paragraph.letter_spacing = 12.0;
    caps.style.font = FontRef { family: "Oswald".into(), weight: 500, italic: false };
    check("caps_spacing", &caps, Clock::REST, 1.0);
    check("title_plain_x2", &base("Привет, мир!"), Clock::REST, 2.0);
}
```

Run: `cargo test -p kadr-text`
Expected: FAIL to compile — `cannot find type TextFrame`, `Bounds`, module `raster` has no items.

- [ ] **Step 8: Implement the raster** — `crates/text/src/raster.rs`:

```rust
//! Pixels (titles spec §5.1): glyphs through swash into coverage masks and
//! colour images, composed bottom to top into a premultiplied float
//! buffer, stored as RGBA8 (contract rounding). Passes: plaque, (Task 9:
//! shadow, glow), outline 2, outline 1, fill. Colour emoji (COLR / colour
//! bitmaps) draw their own colours.

use crate::anim::FrameAnim;
use crate::frame::Bounds;
use crate::layout::{Face, Glyph, Laid};
use crate::shape::Placed;
use kadr_core::{ColorInfo, CpuFrame, FramePool};
use kadr_project::{Fill, Rgba8, TextDoc};
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use swash::scale::image::{Content, Image};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Join, Stroke, Transform, Vector};
use swash::FontRef;

/// A premultiplied RGBA float buffer.
pub(crate) struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 4]>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        let (w, h) = (w.max(1), h.max(1));
        Canvas { w, h, px: vec![[0.0; 4]; w * h] }
    }

    /// Source-over of premultiplied `c` scaled by coverage `cov`.
    #[inline]
    pub fn over(&mut self, i: usize, c: [f32; 4], cov: f32) {
        let d = &mut self.px[i];
        let k = 1.0 - c[3] * cov;
        for ch in 0..4 {
            d[ch] = c[ch] * cov + d[ch] * k;
        }
    }

    pub fn over_mask(&mut self, m: &Mask, c: [f32; 4]) {
        for (i, &cov) in m.a.iter().enumerate() {
            if cov > 0.0 {
                self.over(i, c, cov);
            }
        }
    }

    /// RGBA8 in the working space: `floor(clamp(v)·255 + 0.5)`.
    pub fn into_frame(self, pool: &FramePool) -> CpuFrame {
        let mut f = CpuFrame::rgba8(pool, self.w as u32, self.h as u32, ColorInfo::WORKING_SDR);
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        for y in 0..self.h {
            let row = f.row_mut(y as u32);
            for x in 0..self.w {
                let p = self.px[y * self.w + x];
                row[x * 4..x * 4 + 4].copy_from_slice(&[q(p[0]), q(p[1]), q(p[2]), q(p[3])]);
            }
        }
        f
    }
}

/// A coverage buffer (0..1).
pub(crate) struct Mask {
    pub w: usize,
    pub h: usize,
    pub a: Vec<f32>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Mask {
        let (w, h) = (w.max(1), h.max(1));
        Mask { w, h, a: vec![0.0; w * h] }
    }

    /// Max-union of a glyph image's coverage (a colour image's alpha) at
    /// `(x0, y0)`, times `k`.
    pub fn union(&mut self, img: &Image, x0: i32, y0: i32, k: f32) {
        let p = img.placement;
        for row in 0..p.height as i32 {
            let y = y0 + row;
            if y < 0 || y >= self.h as i32 {
                continue;
            }
            for col in 0..p.width as i32 {
                let x = x0 + col;
                if x < 0 || x >= self.w as i32 {
                    continue;
                }
                let i = (row * p.width as i32 + col) as usize;
                let cov = match img.content {
                    Content::Mask => img.data[i] as f32 / 255.0,
                    Content::Color => img.data[i * 4 + 3] as f32 / 255.0,
                    Content::SubpixelMask => 0.0,
                } * k;
                let a = &mut self.a[y as usize * self.w + x as usize];
                *a = a.max(cov);
            }
        }
    }

    /// Max-union of a rounded rectangle (raster px), anti-aliased by its
    /// signed distance.
    pub fn round_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) {
        if !(x1 > x0 && y1 > y0) {
            return;
        }
        let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
        let (cx, cy, hx, hy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let (xa, xb) = ((x0.floor().max(0.0)) as usize, (x1.ceil().min(self.w as f32)) as usize);
        let (ya, yb) = ((y0.floor().max(0.0)) as usize, (y1.ceil().min(self.h as f32)) as usize);
        for y in ya..yb {
            for x in xa..xb {
                let qx = (x as f32 + 0.5 - cx).abs() - (hx - r);
                let qy = (y as f32 + 0.5 - cy).abs() - (hy - r);
                let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
                let a = &mut self.a[y * self.w + x];
                *a = a.max((0.5 - d).clamp(0.0, 1.0));
            }
        }
    }
}

/// A glyph as drawn: pen origin (layout px), rotation (radians, clockwise),
/// scale, opacity, blur (canvas px) and the colour replacing its fill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Final {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub scale: f32,
    pub opacity: f32,
    pub blur: f32,
    pub colour: Option<Rgba8>,
}

/// The glyph's centre relative to its pen origin, unrotated: half its
/// advance along the baseline, 0.35 em above it.
fn centre_offset(g: &Glyph) -> (f32, f32) {
    (g.advance / 2.0, -0.35 * g.size)
}

fn rot((x, y): (f32, f32), a: f32) -> (f32, f32) {
    let (s, c) = a.sin_cos();
    (x * c - y * s, x * s + y * c)
}

/// The glyphs along the shape, moved by the animation (titles spec §4):
/// each turns and scales about its own centre and shifts; then the whole
/// block turns, scales and shifts about the layout box centre.
pub(crate) fn finals(laid: &Laid, placed: &[Placed], anim: &FrameAnim) -> Vec<Final> {
    let (bx, by) = (laid.width / 2.0, laid.height / 2.0);
    let b = &anim.block;
    laid.glyphs
        .iter()
        .zip(placed)
        .zip(&anim.glyphs)
        .map(|((g, p), a)| {
            let u = centre_offset(g);
            let c0 = rot(u, p.rotation);
            let centre = (p.x + c0.0 + a.dx, p.y + c0.1 + a.dy);
            let r = p.rotation + a.rotation;
            let c1 = rot(u, r);
            let origin = (centre.0 - a.scale * c1.0, centre.1 - a.scale * c1.1);
            let o = rot((origin.0 - bx, origin.1 - by), b.rotation);
            Final {
                x: bx + b.scale * o.0 + b.dx,
                y: by + b.scale * o.1 + b.dy,
                rotation: r + b.rotation,
                scale: a.scale * b.scale,
                opacity: (a.opacity * b.opacity).clamp(0.0, 1.0),
                blur: a.blur.max(b.blur),
                colour: a.colour,
            }
        })
        .collect()
}

pub(crate) fn fill_of(doc: &TextDoc, run: usize) -> &Fill {
    doc.runs.get(run).and_then(|r| r.style.fill.as_ref()).unwrap_or(&doc.style.fill)
}

pub(crate) fn premul(c: Rgba8, opacity: f32) -> [f32; 4] {
    let [r, g, b, a] = c.to_f32();
    let a = a * opacity;
    [r * a, g * a, b * a, a]
}

/// The plaque's rectangles in layout px: one per line with glyphs, or one
/// round the block; padding added. None for a curved text (titles spec §1).
pub(crate) fn plaque_rects(doc: &TextDoc, laid: &Laid) -> Vec<(f32, f32, f32, f32)> {
    let Some(p) = doc.style.plaque else { return vec![] };
    if doc.shape != kadr_project::TextShape::Straight {
        return vec![];
    }
    let (px, py) = (p.padding_x.max(0.0), p.padding_y.max(0.0));
    let lines: Vec<_> = laid.lines.iter().filter(|l| l.right > l.left).collect();
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else { return vec![] };
    if p.per_line {
        lines.iter().map(|l| (l.left - px, l.top - py, l.right + px, l.bottom + py)).collect()
    } else {
        let (l, r) = lines.iter().fold((f32::MAX, f32::MIN), |(a, b), l| (a.min(l.left), b.max(l.right)));
        vec![(l - px, first.top - py, r + px, last.bottom + py)]
    }
}

/// The outlines' total width (at most two, each ≤ 200 px).
pub(crate) fn outline_reach(doc: &TextDoc) -> f32 {
    doc.style.outlines.iter().take(2).map(|o| if o.width.is_finite() { o.width.clamp(0.0, 200.0) } else { 0.0 }).sum()
}

/// What the picture may cover, in canvas px about the layout box centre:
/// the layout box, every glyph's em box as placed, the plaque, and room
/// for the outlines and ink beyond the em box (Task 9 adds shadow, glow
/// and perspective).
pub(crate) fn bounds(doc: &TextDoc, laid: &Laid, finals: &[Final]) -> Bounds {
    let mut b = Bounds { x0: 0.0, y0: 0.0, x1: laid.width, y1: laid.height };
    for (g, f) in laid.glyphs.iter().zip(finals).filter(|(g, _)| g.visible) {
        for corner in [(0.0, -g.size), (g.advance, -g.size), (g.advance, 0.35 * g.size), (0.0, 0.35 * g.size)] {
            let (x, y) = rot((corner.0 * f.scale, corner.1 * f.scale), f.rotation);
            b.add_point(f.x + x, f.y + y);
        }
    }
    for (x0, y0, x1, y1) in plaque_rects(doc, laid) {
        b = b.union(Bounds { x0, y0, x1, y1 });
    }
    let margin = outline_reach(doc) + 0.15 * laid.max_size + 2.0;
    let b = b.grow(margin);
    Bounds { x0: b.x0 - laid.width / 2.0, y0: b.y0 - laid.height / 2.0, x1: b.x1 - laid.width / 2.0, y1: b.y1 - laid.height / 2.0 }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    blob: u64,
    index: u32,
    coords: u64,
    id: u16,
    size: i32,
    frac: u8,
    stroke: i32,
    embolden: bool,
    skew: u32,
    rotation: i32,
    scale: i32,
}

static GLYPHS: LazyLock<Mutex<HashMap<GlyphKey, Option<Arc<Image>>>>> = LazyLock::new(Default::default);

thread_local! {
    static SCALE: RefCell<ScaleContext> = RefCell::new(ScaleContext::new());
}

/// A glyph's image from swash, cached: `size` px; a centred stroke of
/// `stroke` px with round joins when `stroke > 0` (outlines only); turned
/// by `rotation` (radians, clockwise on screen) and scaled by `scale`
/// about the pen origin; shifted right by `frac` (0..1, in quarters).
pub(crate) fn glyph_image(face: &Face, id: u16, size: f32, frac: f32, stroke: f32, rotation: f32, scale: f32) -> Option<Arc<Image>> {
    let q = |v: f32| (v * 64.0).round() as i32;
    let frac = ((frac * 4.0).floor() as i32).clamp(0, 3) as u8;
    let key = GlyphKey {
        blob: face.data.id(),
        index: face.index,
        coords: crate::hash_of(&face.coords),
        id,
        size: q(size),
        frac,
        stroke: q(stroke),
        embolden: face.embolden,
        skew: face.skew.map_or(0, f32::to_bits),
        rotation: q(rotation * 16.0),
        scale: q(scale * 16.0),
    };
    if let Some(hit) = GLYPHS.lock().get(&key) {
        return hit.clone();
    }
    let img = SCALE.with(|scx| {
        let mut scx = scx.borrow_mut();
        let font = FontRef::from_index(face.data.as_ref(), face.index as usize)?;
        let mut scaler = scx.builder_with_id(font, [face.data.id(), face.index as u64]).size(size).hint(false).normalized_coords(&face.coords).build();
        let colour = [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
        let plain = [Source::Outline];
        let mut r = Render::new(if stroke > 0.0 { &plain } else { &colour });
        r.format(Format::Alpha).offset(Vector::new(frac as f32 / 4.0, 0.0)).default_color([255, 255, 255, 255]);
        if face.embolden {
            r.embolden(size / 48.0);
        }
        let mut t = face.skew.map(|deg| Transform::skew(Angle::from_degrees(deg), Angle::ZERO));
        if rotation != 0.0 || scale != 1.0 {
            let m = Transform::scale(scale, scale).then(&Transform::rotation(Angle::from_radians(-rotation)));
            t = Some(t.map_or(m, |s| s.then(&m)));
        }
        r.transform(t);
        if stroke > 0.0 {
            let mut st = Stroke::new(stroke);
            st.join(Join::Round);
            r.style(st);
        }
        r.render(&mut scaler, id)
    })
    .map(Arc::new);
    let mut cache = GLYPHS.lock();
    if cache.len() >= 8192 {
        cache.clear();
    }
    cache.insert(key, img.clone());
    img
}

/// Where a glyph's image goes: its pen origin in raster px.
pub(crate) struct Place {
    pub s: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Place {
    fn pen(&self, f: &Final) -> (f32, f32) {
        ((f.x - self.ox) * self.s, (f.y - self.oy) * self.s)
    }

    /// The glyph's image and its top-left in raster px.
    pub fn image(&self, laid: &Laid, g: &Glyph, f: &Final, stroke: f32) -> Option<(Arc<Image>, i32, i32)> {
        let (px, py) = self.pen(f);
        let ix = px.floor();
        let img = glyph_image(&laid.faces[g.face], g.id, g.size * self.s, px - ix, stroke * self.s, f.rotation, f.scale)?;
        let (x0, y0) = (ix as i32 + img.placement.left, py.round() as i32 - img.placement.top);
        Some((img, x0, y0))
    }
}

/// The union of every visible glyph's coverage, stroked by `stroke` canvas px when > 0.
pub(crate) fn glyph_mask(laid: &Laid, finals: &[Final], at: &Place, w: usize, h: usize, stroke: f32) -> Mask {
    let mut m = Mask::new(w, h);
    for (g, f) in laid.glyphs.iter().zip(finals).filter(|(g, f)| g.visible && f.opacity > 0.0) {
        if let Some((img, x0, y0)) = at.image(laid, g, f, stroke) {
            m.union(&img, x0, y0, f.opacity);
        }
    }
    m
}

/// Draws one glyph's fill: its coverage in `colour` (premultiplied), or a
/// colour glyph's own colours, times its opacity.
fn paint_glyph(c: &mut Canvas, img: &Image, x0: i32, y0: i32, colour: impl Fn(usize, usize) -> [f32; 4], opacity: f32) {
    let p = img.placement;
    for row in 0..p.height as i32 {
        let y = y0 + row;
        if y < 0 || y >= c.h as i32 {
            continue;
        }
        for col in 0..p.width as i32 {
            let x = x0 + col;
            if x < 0 || x >= c.w as i32 {
                continue;
            }
            let i = (row * p.width as i32 + col) as usize;
            let at = y as usize * c.w + x as usize;
            match img.content {
                Content::Mask => {
                    let cov = img.data[i] as f32 / 255.0 * opacity;
                    if cov > 0.0 {
                        c.over(at, colour(x as usize, y as usize), cov);
                    }
                }
                Content::Color => {
                    let s = &img.data[i * 4..i * 4 + 4];
                    let f = |v: u8| v as f32 / 255.0;
                    // COLR output is premultiplied; a colour bitmap is straight.
                    let k = if matches!(img.source, Source::ColorBitmap(_)) { f(s[3]) } else { 1.0 };
                    c.over(at, [f(s[0]) * k, f(s[1]) * k, f(s[2]) * k, f(s[3])], opacity);
                }
                Content::SubpixelMask => {}
            }
        }
    }
}

/// The raster of a text at `scale` (titles spec §5.1): `bounds` × scale.
pub(crate) fn render(doc: &TextDoc, laid: &Laid, placed: &[Placed], anim: &FrameAnim, bounds: Bounds, scale: f32, pool: &FramePool) -> CpuFrame {
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let (w, h) = (((bounds.width() * s).ceil() as usize).max(1), ((bounds.height() * s).ceil() as usize).max(1));
    let mut canvas = Canvas::new(w, h);
    let at = Place { s, ox: laid.width / 2.0 + bounds.x0, oy: laid.height / 2.0 + bounds.y0 };
    let finals = finals(laid, placed, anim);
    // 1. The plaque.
    if let Some(p) = doc.style.plaque.filter(|p| p.colour.0[3] > 0) {
        let mut m = Mask::new(w, h);
        for (x0, y0, x1, y1) in plaque_rects(doc, laid) {
            m.round_rect((x0 - at.ox) * s, (y0 - at.oy) * s, (x1 - at.ox) * s, (y1 - at.oy) * s, p.radius.max(0.0) * s);
        }
        canvas.over_mask(&m, premul(p.colour, anim.plaque_opacity));
    }
    // 2–3. Shadow and glow: Task 9.
    // 4. Outlines, the outer one first: each a union of stroked glyphs in one colour.
    let mut reach = outline_reach(doc);
    for o in doc.style.outlines.iter().take(2).rev() {
        let width = if o.width.is_finite() { o.width.clamp(0.0, 200.0) } else { 0.0 };
        if width > 0.0 {
            let m = glyph_mask(laid, &finals, &at, w, h, 2.0 * reach);
            canvas.over_mask(&m, premul(o.colour, 1.0));
        }
        reach -= width;
    }
    // 5. The fill.
    for (g, f) in laid.glyphs.iter().zip(&finals).filter(|(g, f)| g.visible && f.opacity > 0.0) {
        if let Some((img, x0, y0)) = at.image(laid, g, f, 0.0) {
            let colour = premul(f.colour.unwrap_or(fill_of(doc, g.run).colour), 1.0);
            paint_glyph(&mut canvas, &img, x0, y0, |_, _| colour, f.opacity);
        }
    }
    canvas.into_frame(pool)
}
```

`crates/text/src/frame.rs` (above its tests):

```rust
//! A text clip's picture at one instant (titles spec §5.1–5.2): the
//! document, its layout, the glyphs' places and animated state, the bounds
//! the evaluator sizes the layer by, and the key the resolver caches by.

use crate::anim::{Clock, FrameAnim};
use crate::layout::Laid;
use crate::shape::Placed;
use kadr_core::{CpuFrame, FramePool};
use kadr_project::TextDoc;
use std::sync::Arc;

/// A rectangle in canvas px about the layout box centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Bounds {
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
    pub fn grow(self, m: f32) -> Bounds {
        Bounds { x0: self.x0 - m, y0: self.y0 - m, x1: self.x1 + m, y1: self.y1 + m }
    }
    pub fn union(self, o: Bounds) -> Bounds {
        Bounds { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    pub fn add_point(&mut self, x: f32, y: f32) {
        if x.is_finite() && y.is_finite() {
            *self = self.union(Bounds { x0: x, y0: y, x1: x, y1: y });
        }
    }
}

#[derive(Debug)]
pub struct TextFrame {
    doc: TextDoc,
    laid: Arc<Laid>,
    placed: Vec<Placed>,
    anim: FrameAnim,
    bounds: Bounds,
    key: u64,
}

impl TextFrame {
    pub fn new(doc: &TextDoc, clock: Clock) -> TextFrame {
        let laid = crate::layout::layout(doc);
        let placed = crate::shape::place(doc, &laid);
        let anim = crate::anim::animate(doc, &laid, clock);
        let finals = crate::raster::finals(&laid, &placed, &anim);
        let bounds = crate::raster::bounds(doc, &laid, &finals);
        let key = crate::hash_of(&(doc, anim.key(), crate::fonts::fonts().generation()));
        TextFrame { doc: doc.clone(), laid, placed, anim, bounds, key }
    }

    /// What the picture may cover, canvas px about the layout box centre.
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// The text block (what the preview's handles frame), canvas px.
    pub fn layout_size(&self) -> (f32, f32) {
        (self.laid.width, self.laid.height)
    }

    pub fn missing_fonts(&self) -> &[String] {
        &self.laid.missing
    }

    pub fn laid(&self) -> &Laid {
        &self.laid
    }

    pub fn anim(&self) -> &FrameAnim {
        &self.anim
    }

    /// The raster at `scale`, not pooled (tests, thumbnails).
    pub fn raster(&self, scale: f32) -> CpuFrame {
        kadr_scene::TextPicture::render(self, scale, &FramePool::new(0))
    }
}

/// The block the preview's handles frame (canvas px, centred on the
/// layout box centre): the layout box (Task 9 widens it to a curved or
/// tilted text's extent).
pub fn frame_size(doc: &TextDoc) -> (f32, f32) {
    let l = crate::layout::layout(doc);
    (l.width, l.height)
}

impl kadr_scene::TextPicture for TextFrame {
    fn key(&self) -> u64 {
        self.key
    }

    fn render(&self, scale: f32, pool: &FramePool) -> CpuFrame {
        crate::raster::render(&self.doc, &self.laid, &self.placed, &self.anim, self.bounds, scale, pool)
    }
}
```

- [ ] **Step 9: Run the tests, bless and look at the goldens**

Run: `cargo test -p kadr-text --lib`
Expected: PASS.
Run: `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-text --test golden && cargo test -p kadr-text --test golden`
Expected: 9 PNGs in `crates/text/tests/golden/`, then PASS. Open every one with the Read tool and check: Cyrillic letters correct and kerned; «Победа … команды» in Montserrat white with «нашей» yellow, larger, Playfair italic; two outlines (black inner, red outer) fully around each letter with round corners; a dark rounded plaque round both lines vs a yellow plaque per line with black left-aligned text; the wrapped paragraph centred in three or four lines; ⚽🔥🎉 in colour; «РАЗРЯДКА» upper case, widely spaced, in Oswald; `title_plain_x2` twice as large and as crisp. Anything clipped at an edge, a box (tofu) instead of a letter or a blurred `x2` is a bug to fix before committing.

- [ ] **Step 10: The architecture row, clippy, commit** — `ARCHITECTURE.md` §3 table, after the `kadr-effects` row:

```
| `kadr-text` | текст клипов (титры): шрифты (встроенные `crates/text/fonts`, Windows, свои), вёрстка `parley`, растр `swash` (заливка, обводки, плашка, тень, свечение, цветные эмодзи, дуга/круг, перспектива), анимации, встроенные стили; `TextFrame` — картинка слоя для резолвера |
```

Run: `cargo clippy -p kadr-text --all-targets && cargo test -p kadr-text`
Expected: no warnings; PASS.

```bash
git add crates/text ARCHITECTURE.md Cargo.lock
git commit -m "titles: kadr-text — bundled fonts, parley layout, swash raster, TextFrame

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 4: Text clips in the evaluator; preview = export, end to end (`kadr-timeline`, `kadr-project-scenes`)

**Files:**
- Create: `crates/timeline/src/text.rs`, `crates/timeline/tests/text_eval.rs`, `crates/project-scenes/tests/text_pixels.rs`, `crates/project-scenes/tests/golden/text_title_quarter.png`, `text_rich_full.png`
- Modify: `crates/timeline/Cargo.toml` (`kadr-text = { path = "../text" }`), `crates/timeline/src/lib.rs` (`pub mod text;`), `crates/timeline/src/scene.rs` (`clip_layer` hook; `placement_of`, `crop_of`, `color_of` → `pub(crate)`), `crates/project-scenes/Cargo.toml` (dev-dependency `kadr-text = { path = "../text" }`)

**Interfaces:**
- Consumes: Task 1 `Clip::{new_text, is_text}`, `TextDoc`; Task 2 `LayerContent::Text`, `TextLayer`, `Resolver::text_stats`; Task 3 `kadr_text::{TextFrame, Clock, Bounds, layout}`; `scene::{clip_state_at, placement_of, crop_of, color_of}`, `fx::{apply_effects, crop_window, visible_fraction}`.
- Produces: `kadr_timeline::text::text_clock(clip: &Clip, doc: &TextDoc, t: Time) -> kadr_text::Clock`; `pub(crate) text::text_layer(clip, doc, t, canvas, frame) -> Option<Layer>`; a text clip's layer: `id = LayerId::from(clip.id)`, `placement.size = bounds size`, `placement.anchor = (−x0/w, −y0/h)` (the text block's centre), `position = canvas centre + (transform.x, transform.y)`.

- [ ] **Step 1: Write the failing evaluator tests** — `crates/timeline/tests/text_eval.rs`:

```rust
//! Text clips in the evaluator (titles spec §5.2): a text layer sized by
//! the text's bounds, anchored at its block's centre, placed, faded,
//! animated by keyframes and filtered by effects like a media clip; never
//! occluding; the same picture key while nothing changes.

use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_project::*;
use kadr_scene::*;
use kadr_timeline::scene::evaluate;

fn s(x: i64) -> Time {
    Time::from_secs(x)
}

fn project(doc: TextDoc) -> (Project, kadr_core::ClipId) {
    let mut p = Project::new("t");
    p.sequence_mut().frame_rate = FrameRate::FPS_25;
    let c = Clip::new_text(doc, s(0), s(5));
    let id = c.id;
    p.sequence_mut().tracks[0].clips.push(c);
    (p, id)
}

fn layer_at(p: &Project, t: Time) -> Vec<Layer> {
    evaluate(p, p.sequence(), t, &OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh)).layers
}

fn text_of(l: &Layer) -> &TextLayer {
    match &l.content {
        LayerContent::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn a_text_clip_is_a_text_layer_sized_by_its_bounds_and_anchored_at_its_centre() {
    let doc = TextDoc::starter("Гол!", 1080);
    let (mut p, id) = project(doc.clone());
    let tr = &mut p.sequence_mut().tracks[0].clips[0].transform;
    (tr.x, tr.y, tr.scale, tr.rotation_deg) = (100.0, -200.0, 1.5, 10.0);
    let layers = layer_at(&p, s(1));
    assert_eq!(layers.len(), 1);
    let l = &layers[0];
    text_of(l);
    assert_eq!(l.id, LayerId::from(id));
    let b = kadr_text::TextFrame::new(&doc, kadr_text::Clock::REST).bounds();
    assert_eq!(l.placement.size, Vec2::new(b.width(), b.height()));
    assert_eq!(l.placement.anchor, Vec2::new(-b.x0 / b.width(), -b.y0 / b.height()));
    assert_eq!((l.placement.position, l.placement.scale), (Vec2::new(1060.0, 340.0), Vec2::new(1.5, 1.5)));
    assert!((l.placement.rotation - 10f32.to_radians()).abs() < 1e-6);
    // The layout box centre is local (−x0, −y0): it lands on the position.
    let (cx, cy) = l.placement.to_canvas().apply(-b.x0 as f64, -b.y0 as f64);
    assert!((cx - 1060.0).abs() < 1e-3 && (cy - 340.0).abs() < 1e-3);
    assert_eq!((l.opacity, l.blend, l.crop), (1.0, BlendMode::Normal, l.placement.full_crop()));
    assert!(layer_at(&p, s(6)).is_empty(), "outside the clip: nothing");
}

#[test]
fn growing_text_keeps_its_centre() {
    let centre = |text: &str| {
        let (p, _) = project(TextDoc::starter(text, 1080));
        let l = layer_at(&p, s(1)).remove(0);
        let a = l.placement.anchor;
        l.placement.to_canvas().apply((a.x * l.placement.size.x) as f64, (a.y * l.placement.size.y) as f64)
    };
    let (a, b) = (centre("Гол"), centre("Гол!!!!!!!!\nи ещё строка"));
    assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3, "{a:?} vs {b:?}");
}

#[test]
fn keyframes_fades_colour_and_effects_apply_as_to_media() {
    let (mut p, _) = project(TextDoc::starter("Матч", 1080));
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    c.video_fade = EdgeFades { fade_in: s(1), fade_out: Time::ZERO };
    c.color.exposure = 0.5;
    c.animation.set_key(AnimParam::Scale, SourceTime(s(0)), 1.0, Some(Ease::Linear));
    c.animation.set_key(AnimParam::Scale, SourceTime(s(4)), 2.0, None);
    let mut zoom = AppliedEffect::new("smooth_zoom", 1, Default::default());
    zoom.params.insert("strength".into(), ParamValue::Number(1.0));
    c.effects.push(zoom);
    let l = layer_at(&p, Time::from_millis(500)).remove(0);
    assert!((l.opacity - 0.5).abs() < 0.02, "half way through the fade-in: {}", l.opacity);
    assert!(matches!(l.effects.first(), Some(Effect::ColorAdjust(_))), "the inspector's correction");
    assert!(l.placement.scale.x > 1.125, "keyframes (1.125 at 0.5 s) times the zoom: {}", l.placement.scale.x);
}

#[test]
fn text_never_hides_the_video_below() {
    let (mut p, _) = project(TextDoc::starter("Поверх", 1080));
    let v = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_25), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("v.mp4", MediaInfo { kind: MediaKind::Video, duration: s(60), container: "mp4".into(), size_bytes: 0, video: Some(v), audio: None, timecode: None });
    let seq = p.sequence_mut();
    seq.tracks.insert(0, Track::new(TrackKind::Video, "V0"));
    seq.tracks[0].clips.push(Clip::new(a.id, "v", TimeRange::new(Time::ZERO, s(5)), Time::ZERO));
    p.assets.push(a);
    p.sequence_mut().tracks[1].clips[0].transform.scale = 40.0; // the text box covers the canvas
    let layers = layer_at(&p, s(1));
    assert_eq!(layers.len(), 2, "the opaque video stays under the text");
    assert!(matches!(layers[0].content, LayerContent::Media { .. }));
}

#[test]
fn a_still_text_is_one_picture_and_an_edit_is_another() {
    let (mut p, _) = project(TextDoc::starter("Ключ", 1080));
    let (a, b) = (layer_at(&p, s(1)).remove(0), layer_at(&p, s(3)).remove(0));
    assert_eq!(text_of(&a), text_of(&b), "no animation: the same key at every instant");
    p.sequence_mut().tracks[0].clips[0].text.as_mut().unwrap().style.fill.colour = Rgba8::rgba(255, 0, 0, 255);
    assert_ne!(text_of(&layer_at(&p, s(1))[0]), text_of(&a));
}

#[test]
fn the_clock_runs_from_the_visible_edges_and_the_source() {
    let (mut p, _) = project(TextDoc::new("x"));
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (s(2), s(6), s(-1), s(3));
    let doc = c.text.as_deref().unwrap().clone();
    let k = kadr_timeline::text::text_clock(c, &doc, s(3));
    assert_eq!((k.since_in, k.until_out, k.source), (s(1), s(3), s(0)));
    let edge = kadr_timeline::text::text_clock(c, &doc, s(9));
    assert_eq!((edge.since_in, edge.until_out), (s(4), Time::ZERO), "past the end: the end's state");
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kadr-timeline --test text_eval`
Expected: FAIL — `could not find text in kadr_timeline` (and, once that exists, a text clip draws nothing: `project.asset(TEXT_ASSET)` is `None`).

- [ ] **Step 3: Implement** — `crates/timeline/Cargo.toml`: `kadr-text = { path = "../text" }` under `[dependencies]`; `crates/timeline/src/lib.rs`: `pub mod text;` (after `snap`); in `crates/timeline/src/scene.rs` make `placement_of`, `crop_of` and `color_of` `pub(crate)`, and at the top of `clip_layer`, before `let asset = …`:

```rust
    // Titles spec §5.2: a text clip draws its text, not media.
    if let Some(doc) = clip.text.as_deref() {
        return crate::text::text_layer(clip, doc, t, canvas, frame);
    }
```

`crates/timeline/src/text.rs`:

```rust
//! Text clips in the evaluator (titles spec §3, §5.2): the clock of a
//! clip's text animation at `t`, and its layer — sized by the text's
//! bounds, anchored at the text block's centre (so `Transform.x/y` place
//! that centre and growing text stays centred), placed, faded, keyframed,
//! corrected and filtered by effects exactly like a media clip.

use kadr_core::Time;
use kadr_project::{Clip, TextDoc};
use kadr_scene::*;
use std::sync::Arc;

/// Titles spec §3: time since the visible start and until the visible end
/// (`t` clamped into the clip: a transition handle beyond an edge gets the
/// edge's state), the source time (loops; extrapolated like keyframes) and
/// the highlight's progress over its span.
pub fn text_clock(clip: &Clip, doc: &TextDoc, t: Time) -> kadr_text::Clock {
    let tt = t.max(clip.timeline_in).min(clip.timeline_out);
    let frac = crate::fx::visible_fraction(clip, tt).clamp(0.0, 1.0);
    let highlight = doc.anim.highlight.as_ref().map_or(0.0, |h| h.span.0 + (h.span.1 - h.span.0) * frac);
    kadr_text::Clock { since_in: tt - clip.timeline_in, until_out: clip.timeline_out - tt, source: clip.source_time_at(t), highlight }
}

/// The layer of text clip `clip` (its document `doc`) at `t`.
pub(crate) fn text_layer(clip: &Clip, doc: &TextDoc, t: Time, canvas: SizeU, frame: Time) -> Option<Layer> {
    let st = crate::scene::clip_state_at(clip, t, frame);
    if st.transform.scale <= 0.0 {
        return None;
    }
    let picture = kadr_text::TextFrame::new(doc, text_clock(clip, doc, t));
    let b = picture.bounds();
    let (w, h) = (b.width(), b.height());
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let mut placement = crate::scene::placement_of(&st.transform, Vec2::new(w, h), canvas);
    placement.anchor = Vec2::new(-b.x0 / w, -b.y0 / h);
    let crop = crate::scene::crop_of(&st.transform, placement.size)?;
    let (placement, colour) = crate::fx::apply_effects(clip, t, canvas, placement, crate::fx::crop_window(&st.transform));
    let mut effects = vec![];
    let color = crate::scene::color_of(&st.color);
    if !color.is_neutral() {
        effects.push(Effect::ColorAdjust(color));
    }
    effects.extend(colour);
    Some(Layer {
        id: LayerId::from(clip.id),
        content: LayerContent::Text(TextLayer { picture: Arc::new(picture) }),
        placement,
        crop,
        opacity: (st.transform.opacity * st.fade) as f32,
        blend: BlendMode::Normal,
        effects,
    })
}
```

- [ ] **Step 4: Run the evaluator tests**

Run: `cargo test -p kadr-timeline`
Expected: PASS (all earlier evaluator tests too: media clips never reach the hook).

- [ ] **Step 5: Write the end-to-end tests** — add `kadr-text = { path = "../text" }` to `[dev-dependencies]` of `crates/project-scenes/Cargo.toml`, then `crates/project-scenes/tests/text_pixels.rs`:

```rust
//! Text clips end to end (titles spec §5, §9): project → evaluator →
//! resolver (the text picture draws itself) → CPU renderer. The preview's
//! frame equals the export's; a 4K export is drawn at 4K, not upscaled; a
//! font this machine lacks still renders and is kept.

use kadr_core::perf::FramePerf;
use kadr_core::{FrameRate, Time};
use kadr_playback::testing::FakeDecoders;
use kadr_playback::{Mode, Resolver, ResolverConfig, SceneSource};
use kadr_project::*;
use kadr_project_scenes::ProjectScenes;
use kadr_render::{CpuRenderer, CpuTarget, PreparedFrame, RenderTarget, Renderer};
use kadr_scene::{LayerContent, OutputSpec, RenderQuality, SizeU};
use std::sync::Arc;
use std::time::Duration;

fn project(doc: TextDoc, w: u32, h: u32) -> Project {
    let mut p = Project::new("text");
    let seq = p.sequence_mut();
    (seq.frame_rate, seq.width, seq.height) = (FrameRate::FPS_25, w, h);
    seq.tracks[0].clips.push(Clip::new_text(doc, Time::ZERO, Time::from_secs(5)));
    p
}

fn render(p: &Project, bypass: bool, out: SizeU, mode: Mode) -> Vec<u8> {
    let scenes = ProjectScenes::new(p, bypass);
    let r = Resolver::new(Arc::new(FakeDecoders::new(Duration::ZERO, Duration::ZERO)), ResolverConfig::default());
    let quality = if mode == Mode::Export { RenderQuality::Export } else { RenderQuality::PreviewHigh };
    let scene = scenes.scene_at(Time::from_secs(1), &OutputSpec::new(out, quality));
    let inputs = r.prepare(&scene, &scenes, mode, &mut FramePerf::default());
    let mut buf = vec![0u8; (out.w * out.h * 4) as usize];
    CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut RenderTarget::Cpu(CpuTarget::packed(out.w, out.h, &mut buf))).unwrap();
    buf
}

fn lit(px: &[u8]) -> usize {
    px.as_chunks::<4>().0.iter().filter(|p| p[0] > 128).count()
}

/// Σ of squared horizontal steps of the red channel: a sharper edge of
/// the same height scores higher (a step of H over one pixel: H²; over two: H²/2).
fn edge_energy(px: &[u8], w: u32) -> f64 {
    px.chunks_exact(w as usize * 4).map(|row| row.as_chunks::<4>().0.windows(2).map(|p| (p[1][0] as f64 - p[0][0] as f64).powi(2)).sum::<f64>()).sum()
}

/// 2× bilinear upscale (texel centres), RGBA.
fn upscale2(px: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w2, h2) = (w * 2, h * 2);
    let mut out = vec![0u8; (w2 * h2 * 4) as usize];
    let at = |x: i64, y: i64, c: usize| px[((y.clamp(0, h as i64 - 1) as u32 * w + x.clamp(0, w as i64 - 1) as u32) * 4) as usize + c] as f64;
    for y in 0..h2 {
        for x in 0..w2 {
            let (u, v) = ((x as f64 + 0.5) / 2.0 - 0.5, (y as f64 + 0.5) / 2.0 - 0.5);
            let (i, j) = (u.floor() as i64, v.floor() as i64);
            let (f, g) = (u - i as f64, v - j as f64);
            for c in 0..4 {
                let val = at(i, j, c) * (1.0 - f) * (1.0 - g) + at(i + 1, j, c) * f * (1.0 - g) + at(i, j + 1, c) * (1.0 - f) * g + at(i + 1, j + 1, c) * f * g;
                out[((y * w2 + x) * 4) as usize + c] = val.round() as u8;
            }
        }
    }
    out
}

#[test]
fn a_title_renders_and_the_preview_matches_the_export() {
    let p = project(TextDoc::starter("Привет, мир! 🎉", 1080), 1920, 1080);
    let out = SizeU::new(480, 270);
    let preview = render(&p, false, out, Mode::Scrub { generation: 1 });
    assert!(lit(&preview) > 200, "white letters on black");
    assert_eq!(preview, render(&p, false, out, Mode::Export), "one picture: preview and export");
    golden::check("text_title_quarter", &preview, out);
}

#[test]
fn a_4k_export_of_a_title_is_sharper_than_an_upscaled_1080p_one() {
    let mut doc = TextDoc::starter("Чёткость", 360);
    doc.style.shadow = None;
    let p = project(doc, 640, 360);
    let native = render(&p, false, SizeU::new(1280, 720), Mode::Export);
    let up = upscale2(&render(&p, false, SizeU::new(640, 360), Mode::Export), 640, 360);
    let (n, u) = (edge_energy(&native, 1280), edge_energy(&up, 1280));
    assert!(n > 1.4 * u, "drawn at the output size, not stretched: {n:.0} vs {u:.0}");
}

#[test]
fn a_missing_font_renders_and_round_trips() {
    let mut doc = TextDoc::starter("Шрифт", 1080);
    doc.style.font.family = "Nope Sans".into();
    let p = project(doc.clone(), 1920, 1080);
    let json = kadr_project::io::to_json(&p);
    let back = kadr_project::io::parse(&json, std::path::Path::new("m.kadr")).unwrap();
    assert!(lit(&render(&back, false, SizeU::new(480, 270), Mode::Export)) > 100, "drawn in the default font");
    assert_eq!(kadr_text::layout(&doc).missing, ["Nope Sans"], "the inspector can say so");
    let saved = String::from_utf8(kadr_project::io::to_json(&back)).unwrap();
    assert!(saved.contains("\"family\": \"Nope Sans\"") || saved.contains("\"family\":\"Nope Sans\""), "the name is kept");
}

#[test]
fn before_view_keeps_the_text_and_drops_its_effects() {
    let mut p = project(TextDoc::starter("До", 1080), 1920, 1080);
    let c = &mut p.sequence_mut().tracks[0].clips[0];
    c.color.exposure = 1.0;
    let mut look = AppliedEffect::new("look", 1, Default::default());
    look.params.insert("preset".into(), ParamValue::Choice("vintage".into()));
    c.effects.push(look);
    let scenes = ProjectScenes::new(&p, true);
    let scene = scenes.scene_at(Time::from_secs(1), &OutputSpec::new(SizeU::new(480, 270), RenderQuality::PreviewHigh));
    assert_eq!(scene.layers.len(), 1);
    assert!(matches!(scene.layers[0].content, LayerContent::Text(_)) && scene.layers[0].effects.is_empty());
    assert_eq!(scenes.top_clip(&scene), Some(p.sequence().tracks[0].clips[0].id), "the preview names the text clip");
}

#[test]
fn rich_text_full_resolution_golden() {
    let mut doc = TextDoc::starter("Счёт 2:1 — победа!", 1080);
    doc.style_range(9..12, &RunStyle { fill: Some(Fill::solid(Rgba8::rgba(255, 210, 0, 255))), size: Some(130.0), ..Default::default() });
    doc.style.outlines = vec![Outline { width: 5.0, colour: Rgba8::BLACK }];
    let p = project(doc, 1920, 1080);
    let out = SizeU::new(960, 540);
    golden::check("text_rich_full", &render(&p, false, out, Mode::Export), out);
}

mod golden {
    use kadr_scene::SizeU;

    /// `tests/golden/<name>.png` within ±1 LSB; `KADR_UPDATE_GOLDEN=1` rewrites it (look at it).
    pub fn check(name: &str, px: &[u8], size: SizeU) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.png"));
        if std::env::var("KADR_UPDATE_GOLDEN").as_deref() == Ok("1") {
            let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()), size.w, size.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(px).unwrap();
            return;
        }
        let file = std::fs::File::open(&path).unwrap_or_else(|_| panic!("{} missing: KADR_UPDATE_GOLDEN=1, then look at it", path.display()));
        let mut dec = png::Decoder::new(file).read_info().unwrap();
        let mut want = vec![0u8; dec.output_buffer_size()];
        dec.next_frame(&mut want).unwrap();
        let bad = px.iter().zip(&want).filter(|(a, b)| a.abs_diff(**b) > 1).count();
        assert_eq!(bad, 0, "{name}: {bad} bytes off by more than 1");
    }
}
```

(Byte range 9..12 of «Счёт 2:1 — победа!» is «2:1»: «Счёт » is 4 × 2 + 1 = 9 bytes.)

- [ ] **Step 6: Run, bless, look**

Run: `cargo test -p kadr-project-scenes --test text_pixels`
Expected: FAIL only in the two goldens (missing PNGs).
Run: `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-project-scenes --test text_pixels && cargo test -p kadr-project-scenes --test text_pixels`
Expected: PASS. Look at both PNGs (Read tool): `text_title_quarter` — «Привет, мир! 🎉» white and centred on black, emoji in colour, no blur beyond the quarter-size output; `text_rich_full` — «2:1» yellow and larger, a black outline round every letter, centred.

- [ ] **Step 7: Full suite and commit**

Run: `cargo test -p kadr-timeline -p kadr-project-scenes -p kadr-playback -p kadr-render && cargo clippy -p kadr-timeline -p kadr-project-scenes --all-targets`
Expected: PASS; no new warnings.

```bash
git add crates/timeline crates/project-scenes
git commit -m "titles: text clips in the evaluator; preview = export end to end

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 5: The colour picker widget (`kadr-editor`)

**Files:**
- Create: `apps/editor/ui/color_picker.slint`, `apps/editor/src/color_ui.rs`
- Modify: `apps/editor/ui/app.slint` (import `ColorUtil` and add it to the `export { … }` list), `apps/editor/src/main.rs` (`mod color_ui;`), `apps/editor/src/app.rs` (`wire_callbacks`: the `ColorUtil` callbacks; set `recent` at start), `apps/editor/src/app_settings.rs` (`recent_colors`), `crates/i18n/locales/{ru,en}.json`

**Interfaces:**
- Consumes: `AppSettings::save`, `App::{ui, settings, dirs}`.
- Produces:
  - Slint `ColorSwatch` (`in value: color`, `in alpha: bool` (default true), `in enabled: bool`, `in tooltip: string`; `callback live(color)`, `callback committed(color)`), exported from `color_picker.slint`; Slint global `ColorUtil { pure callback parse(string) -> color; pure callback valid(string) -> bool; pure callback hex(color, bool) -> string; in property <[color]> recent; }` exported from `app.slint`.
  - `color_ui::{parse_hex(&str) -> Option<[u8; 4]>, to_hex([u8; 4], alpha: bool) -> String, push_recent(&mut Vec<String>, &str), to_slint([u8; 4]) -> slint::Color, from_slint(slint::Color) -> [u8; 4]}`; `App::colour_committed(&mut self, c: slint::Color)` (records a recent colour, saves the settings, updates `ColorUtil.recent`).

This task runs beside Tasks 1–4 and does not use `kadr_project::Rgba8` (Task 1); Task 7 converts with `from_slint` / `to_slint`.

- [ ] **Step 1: Write the failing tests** — `apps/editor/src/color_ui.rs` (add `mod color_ui;` to `main.rs` in the module list):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_reads_six_or_eight_digits_and_writes_lower_case() {
        assert_eq!(parse_hex("#FF8000"), Some([255, 128, 0, 255]));
        assert_eq!(parse_hex(" #ff800080"), Some([255, 128, 0, 128]));
        assert_eq!(parse_hex("ff8000"), Some([255, 128, 0, 255]), "the # is optional when typing");
        for bad in ["#ff80", "#zz0000", "#ff800080ff", "", "#ффф000"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        assert_eq!(to_hex([255, 128, 0, 255], false), "#ff8000");
        assert_eq!(to_hex([255, 128, 0, 64], true), "#ff800040");
        assert_eq!(to_hex([255, 128, 0, 255], true), "#ff8000", "an opaque colour needs no alpha digits");
    }

    #[test]
    fn colours_round_trip_through_slint() {
        for c in [[0, 0, 0, 0], [255, 255, 255, 255], [12, 200, 7, 99]] {
            assert_eq!(from_slint(to_slint(c)), c);
        }
    }

    #[test]
    fn recent_colours_are_newest_first_unique_and_at_most_eight() {
        let mut r = vec![];
        for h in ["#111111", "#222222", "#111111"] {
            push_recent(&mut r, h);
        }
        assert_eq!(r, ["#111111", "#222222"], "a repeat moves to the front");
        for i in 0..10 {
            push_recent(&mut r, &format!("#0000{i:02}"));
        }
        assert_eq!(r.len(), 8);
        assert_eq!(r[0], "#000009");
        push_recent(&mut r, "#00000A");
        assert_eq!(r.iter().filter(|h| h.eq_ignore_ascii_case("#00000a")).count(), 1, "case-insensitive");
    }
}
```

Run: `cargo test -p kadr-editor color_ui`
Expected: FAIL to compile — `cannot find function parse_hex`.

- [ ] **Step 2: Implement the Rust side** — top of `apps/editor/src/color_ui.rs`:

```rust
//! The colour picker's Rust side (titles spec §6.4): hex ↔ colour for the
//! `ColorUtil` global, and the recent colours (newest first, at most 8,
//! kept in the settings).

use crate::app::App;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

/// "#rrggbb" or "#rrggbbaa" (the # optional, surrounding spaces ignored).
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let t = s.trim();
    let h = t.strip_prefix('#').unwrap_or(t);
    if !h.is_ascii() || !(h.len() == 6 || h.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 255 }])
}

/// Lower-case hex; the alpha digits only when `alpha` and not opaque.
pub fn to_hex(c: [u8; 4], alpha: bool) -> String {
    if alpha && c[3] != 255 { format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]) } else { format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]) }
}

pub fn to_slint(c: [u8; 4]) -> slint::Color {
    slint::Color::from_argb_u8(c[3], c[0], c[1], c[2])
}

pub fn from_slint(c: slint::Color) -> [u8; 4] {
    [c.red(), c.green(), c.blue(), c.alpha()]
}

/// Puts `hex` first (once, case-insensitively), keeping at most 8.
pub fn push_recent(recent: &mut Vec<String>, hex: &str) {
    recent.retain(|h| !h.eq_ignore_ascii_case(hex));
    recent.insert(0, hex.to_ascii_lowercase());
    recent.truncate(8);
}

/// The `ColorUtil` callbacks (pure: no app state).
pub fn wire(ui: &crate::AppWindow) {
    let cu = ui.global::<crate::ColorUtil>();
    cu.on_parse(|s| parse_hex(&s).map(to_slint).unwrap_or(slint::Color::from_argb_u8(0, 0, 0, 0)));
    cu.on_valid(|s| parse_hex(&s).is_some());
    cu.on_hex(|c, alpha| to_hex(from_slint(c), alpha).into());
}

impl App {
    /// Shows the settings' recent colours in every picker.
    pub fn show_recent_colours(&self) {
        let colours: Vec<slint::Color> = self.settings.recent_colors.iter().filter_map(|h| parse_hex(h)).map(to_slint).collect();
        let ui = self.ui();
        let cu = ui.global::<crate::ColorUtil>();
        let old = cu.get_recent();
        if old.row_count() != colours.len() || old.iter().zip(&colours).any(|(a, b)| a != *b) {
            cu.set_recent(ModelRc::new(VecModel::from(colours)));
        }
    }

    /// A colour was committed in a picker: it becomes the newest recent one.
    pub fn colour_committed(&mut self, c: slint::Color) {
        push_recent(&mut self.settings.recent_colors, &to_hex(from_slint(c), true));
        self.settings.save(&self.dirs.settings_file());
        self.show_recent_colours();
    }
}
```

`apps/editor/src/app_settings.rs`: in `AppSettings` add `/// Recently committed picker colours, newest first (titles spec §6.4). #[serde(default)] pub recent_colors: Vec<String>,` (and `recent_colors: vec![]` in its `Default` if it has a hand-written one). In `app.rs::wire_callbacks` add `crate::color_ui::wire(ui);`, and where the app first fills the UI after loading settings (where `settings` is applied to `ui`) call `app.show_recent_colours();`. If `self.settings.save` returns a `Result`, log its error with `tracing::warn!` as the other call sites do.

Run: `cargo test -p kadr-editor color_ui`
Expected: FAIL to compile until Step 3 adds `ColorUtil` to the UI (`no ColorUtil in crate`).

- [ ] **Step 3: The widget** — `apps/editor/ui/color_picker.slint`:

```slint
import { Theme } from "theme.slint";
import { Tr } from "i18n.slint";
import { LineEdit } from "std-widgets.slint";

/// Hex ↔ colour, in Rust (`color_ui.rs`), and the recent colours.
export global ColorUtil {
    /// "#rrggbb" / "#rrggbbaa" → the colour (transparent when not a colour: ask `valid`).
    pure callback parse(string) -> color;
    pure callback valid(string) -> bool;
    /// "#rrggbb", or "#rrggbbaa" when `with-alpha` and not opaque.
    pure callback hex(color, bool) -> string;
    /// Newest first, at most 8.
    in property <[color]> recent;
}

/// A colour swatch that opens the picker (titles spec §6.4): a saturation ×
/// value square, a hue bar, an alpha bar (`alpha`), a hex field, 16 preset
/// colours and the recent ones. `live` while the square or a bar is
/// dragged; `committed` once — on release, on a preset or recent swatch,
/// on an accepted hex. The popup closes on a click outside it.
export component ColorSwatch inherits Rectangle {
    in property <color> value;
    in property <bool> alpha: true;
    in property <bool> enabled: true;
    in property <string> tooltip;
    callback live(color);
    callback committed(color);
    property <float> h;
    property <float> s;
    property <float> v;
    property <float> a: 1;
    property <color> current: hsv(root.h, root.s, root.v, root.alpha ? root.a : 1);
    property <[color]> presets: [#ffffff, #000000, #8e8e93, #ff3b30, #ff9500, #ffd400, #34c759, #00c7be, #00e5ff, #30b0ff, #007aff, #5856d6, #af52de, #ff2d55, #ff6ad5, #a2845e];
    width: 30px;
    height: 20px;
    border-radius: 4px;
    background: @linear-gradient(135deg, #bdbdbd 0%, #bdbdbd 50%, #7a7a7a 50%, #7a7a7a 100%);
    border-width: 1px;
    border-color: sw.has-hover && root.enabled ? Theme.border-strong : Theme.border;
    opacity: root.enabled ? 1 : 0.4;
    accessible-role: button;
    accessible-label: root.tooltip;

    function set-colour(c: color) {
        root.h = c.to-hsv().hue;
        root.s = c.to-hsv().saturation;
        root.v = c.to-hsv().value;
        root.a = c.to-hsv().alpha;
    }
    function pick-sv(x: float, y: float) {
        root.s = clamp(x, 0, 1);
        root.v = 1 - clamp(y, 0, 1);
        root.live(root.current);
    }
    function pick-hue(x: float) {
        root.h = clamp(x, 0, 1) * 360;
        root.live(root.current);
    }
    function pick-alpha(x: float) {
        root.a = clamp(x, 0, 1);
        root.live(root.current);
    }

    Rectangle {
        border-radius: 3px;
        background: root.value;
    }
    sw := TouchArea {
        enabled: root.enabled;
        mouse-cursor: pointer;
        clicked => {
            root.set-colour(root.value);
            popup.show();
        }
    }
    popup := PopupWindow {
        x: root.width - self.width;
        y: root.height + 4px;
        width: 236px;
        height: root.alpha ? 316px : 296px;
        close-policy: close-on-click-outside;
        Rectangle {
            background: Theme.bg2;
            border-radius: 6px;
            border-width: 1px;
            border-color: Theme.border-strong;
            drop-shadow-blur: 12px;
            drop-shadow-color: #00000080;
        }
        VerticalLayout {
            // The hex field follows the colour (a typed value breaks a plain binding).
            property <color> shown: root.current;
            changed shown => { hexed.text = ColorUtil.hex(self.shown, root.alpha); }
            padding: 10px;
            spacing: 8px;
            Rectangle {
                height: 120px;
                vertical-stretch: 0;
                border-radius: 4px;
                clip: true;
                background: hsv(root.h, 1, 1);
                Rectangle { background: @linear-gradient(90deg, #ffffff 0%, #ffffff00 100%); }
                Rectangle { background: @linear-gradient(0deg, #000000 0%, #00000000 100%); }
                Rectangle {
                    x: root.s * parent.width - 6px;
                    y: (1 - root.v) * parent.height - 6px;
                    width: 12px;
                    height: 12px;
                    border-radius: 6px;
                    border-width: 2px;
                    border-color: #ffffff;
                    drop-shadow-blur: 2px;
                    drop-shadow-color: #000000a0;
                }
                TouchArea {
                    mouse-cursor: crosshair;
                    pointer-event(e) => {
                        if (e.button == PointerEventButton.left && e.kind == PointerEventKind.down) { root.pick-sv(self.mouse-x / self.width, self.mouse-y / self.height); }
                        if (e.button == PointerEventButton.left && e.kind == PointerEventKind.up) { root.committed(root.current); }
                    }
                    moved => { if (self.pressed) { root.pick-sv(self.mouse-x / self.width, self.mouse-y / self.height); } }
                }
            }
            Rectangle {
                height: 12px;
                vertical-stretch: 0;
                border-radius: 6px;
                background: @linear-gradient(90deg, #ff0000 0%, #ffff00 16.67%, #00ff00 33.33%, #00ffff 50%, #0000ff 66.67%, #ff00ff 83.33%, #ff0000 100%);
                Rectangle {
                    x: root.h / 360 * parent.width - 5px;
                    y: -1px;
                    width: 10px;
                    height: 14px;
                    border-radius: 3px;
                    border-width: 2px;
                    border-color: #ffffff;
                    background: hsv(root.h, 1, 1);
                }
                TouchArea {
                    pointer-event(e) => {
                        if (e.button == PointerEventButton.left && e.kind == PointerEventKind.down) { root.pick-hue(self.mouse-x / self.width); }
                        if (e.button == PointerEventButton.left && e.kind == PointerEventKind.up) { root.committed(root.current); }
                    }
                    moved => { if (self.pressed) { root.pick-hue(self.mouse-x / self.width); } }
                }
            }
            if root.alpha: Rectangle {
                height: 12px;
                vertical-stretch: 0;
                border-radius: 6px;
                clip: true;
                background: @linear-gradient(135deg, #bdbdbd 0%, #bdbdbd 50%, #7a7a7a 50%, #7a7a7a 100%);
                Rectangle { background: @linear-gradient(90deg, hsv(root.h, root.s, root.v, 0) 0%, hsv(root.h, root.s, root.v, 1) 100%); }
                Rectangle {
                    x: root.a * parent.width - 5px;
                    width: 10px;
                    border-radius: 3px;
                    border-width: 2px;
                    border-color: #ffffff;
                }
                TouchArea {
                    pointer-event(e) => {
                        if (e.button == PointerEventButton.left && e.kind == PointerEventKind.down) { root.pick-alpha(self.mouse-x / self.width); }
                        if (e.button == PointerEventButton.left && e.kind == PointerEventKind.up) { root.committed(root.current); }
                    }
                    moved => { if (self.pressed) { root.pick-alpha(self.mouse-x / self.width); } }
                }
            }
            HorizontalLayout {
                height: 28px;
                vertical-stretch: 0;
                spacing: 6px;
                Rectangle {
                    width: 28px;
                    border-radius: 4px;
                    border-width: 1px;
                    border-color: Theme.border;
                    background: root.current;
                }
                hexed := LineEdit {
                    text: ColorUtil.hex(root.current, root.alpha);
                    accepted(t) => {
                        if (ColorUtil.valid(t)) {
                            root.set-colour(ColorUtil.parse(t));
                            root.committed(root.current);
                        }
                    }
                }
            }
            // 16 presets, two rows of eight.
            Rectangle {
                height: 48px;
                vertical-stretch: 0;
                for c[i] in root.presets: Rectangle {
                    x: mod(i, 8) * 27px;
                    y: floor(i / 8) * 25px;
                    width: 22px;
                    height: 22px;
                    border-radius: 4px;
                    background: c;
                    border-width: 1px;
                    border-color: pa.has-hover ? Theme.text : Theme.border;
                    pa := TouchArea {
                        mouse-cursor: pointer;
                        clicked => {
                            root.set-colour(c);
                            root.committed(root.current);
                        }
                    }
                }
            }
            Text { text: Tr.t("color.recent"); font-size: Theme.font-xs; color: Theme.text-faint; vertical-stretch: 0; }
            Rectangle {
                height: 22px;
                vertical-stretch: 0;
                for c[i] in ColorUtil.recent: Rectangle {
                    x: i * 27px;
                    width: 22px;
                    height: 22px;
                    border-radius: 4px;
                    background: c;
                    border-width: 1px;
                    border-color: ra.has-hover ? Theme.text : Theme.border;
                    ra := TouchArea {
                        mouse-cursor: pointer;
                        clicked => {
                            root.set-colour(c);
                            root.committed(root.current);
                        }
                    }
                }
            }
        }
    }
}
```

`apps/editor/ui/app.slint`: `import { ColorUtil } from "color_picker.slint";` with the other imports, and `ColorUtil` added to the `export { … }` list. Catalogs — `ru.json`: `"color.recent": "Недавние"`; `en.json`: `"color.recent": "Recent"`.

- [ ] **Step 4: Build and run the tests**

Run: `cargo build -p kadr-editor && cargo test -p kadr-editor color_ui && cargo test -p kadr-i18n`
Expected: the Slint compiles (fix any reported `.slint` line by the compiler's message); tests PASS (`all_referenced_keys_are_translated` sees `color.recent`).

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p kadr-editor --all-targets`
Expected: no new warnings but one: `colour_committed` is unused until Task 7 calls it (leave it; Task 7 removes the warning).

```bash
git add apps/editor/ui/color_picker.slint apps/editor/ui/app.slint apps/editor/src/color_ui.rs apps/editor/src/main.rs apps/editor/src/app.rs apps/editor/src/app_settings.rs crates/i18n/locales
git commit -m "titles: a reusable colour picker (square, hue, alpha, hex, presets, recent)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 6: Text clips on the timeline; «Добавить текст» (button, Ctrl+T, menu) (`kadr-editor`)

**Files:**
- Create: `apps/editor/src/text_add.rs`
- Modify: `apps/editor/ui/theme.slint` (`Theme.clip-text`, `clip-text-top`; `Icons.text`), `apps/editor/ui/timeline.slint` (`ClipView.is-text`; `ClipRect` colours and «Т» mark; toolbar button), `apps/editor/ui/app.slint` (Edit menu entry), `apps/editor/src/timeline_ui.rs` (`ClipView` fields, `tl_tool("add-text")`), `apps/editor/src/keys.rs` (Ctrl+T, shortcut row), `apps/editor/src/persistence.rs` (menu id `add-text`), `apps/editor/src/main.rs` (`mod text_add;`), `crates/i18n/locales/{ru,en}.json`

**Interfaces:**
- Consumes: Task 1 `Clip::new_text`, `TextDoc::starter`, `EditCommand::AddText`; Task 4 (the clip renders).
- Produces: `text_add::new_text_clip(seq: &Sequence, at: Time, text: &str) -> Clip` (5 s, frame-snapped, at ≥ 0, the starter doc for the sequence height); `App::add_text(&mut self, at: Time, track: Option<TrackId>, doc: Option<TextDoc>, pos: Option<(f64, f64)>) -> Option<ClipId>` (`pos`: the block centre's `Transform.x / y`, canvas px; one undo entry «Добавление текста»; selects the clip, then calls `self.text_added(id)` — a no-op hook here that Task 8 turns into inline typing); `App::add_text_at_playhead(&mut self)`.

- [ ] **Step 1: Write the failing test** — `apps/editor/src/text_add.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::FrameRate;
    use kadr_project::Project;

    #[test]
    fn a_new_text_is_five_seconds_of_the_starter_at_a_frame() {
        let mut p = Project::new("t");
        (p.sequence_mut().frame_rate, p.sequence_mut().height) = (FrameRate::FPS_25, 720);
        let c = new_text_clip(p.sequence(), Time::from_millis(1_013), "Ваш текст");
        assert_eq!((c.timeline_in, c.duration()), (Time::from_millis(1_000), Time::from_secs(5)), "snapped to the 25 fps grid");
        let doc = c.text.as_deref().unwrap();
        assert_eq!((doc.plain(), doc.style.size), ("Ваш текст".to_string(), 65.0), "9 % of 720 lines");
        assert_eq!(new_text_clip(p.sequence(), Time::from_secs(-3), "x").timeline_in, Time::ZERO);
    }
}
```

Run: `cargo test -p kadr-editor text_add`
Expected: FAIL to compile — `cannot find function new_text_clip` (add `mod text_add;` to `main.rs` first).

- [ ] **Step 2: Implement adding** — top of `apps/editor/src/text_add.rs`:

```rust
//! Adding a text (titles spec §1, §7): «Т» on the timeline, Ctrl+T, Edit →
//! «Добавить текст», the «Текст» tab. A 5 s title at the playhead (or a
//! drop point) on the topmost video track when free there, else on a new
//! track above (`EditCommand::AddText`), one undo entry; the clip is
//! selected and inline typing starts (`text_added`, Task 8).

use crate::app::App;
use kadr_core::{ClipId, Time, TrackId};
use kadr_i18n::t;
use kadr_project::{Clip, Sequence, TextDoc};
use kadr_timeline::EditCommand;

/// A 5 s text clip of the starter style for `seq`, at `at` snapped to a frame (never before 0).
pub fn new_text_clip(seq: &Sequence, at: Time, text: &str) -> Clip {
    Clip::new_text(TextDoc::starter(text, seq.height), seq.frame_rate.snap(at.max(Time::ZERO)), Time::from_secs(5))
}

impl App {
    pub fn add_text_at_playhead(&mut self) {
        self.add_text(self.playhead, None, None, None);
    }

    /// Adds a text clip at `at` (`doc`: a style's; `None`: the starter) on
    /// `track` (`None`: the command chooses), its block centred at `pos`
    /// (`Transform.x / y`; `None`: the frame's centre). Returns its id.
    pub fn add_text(&mut self, at: Time, track: Option<TrackId>, doc: Option<TextDoc>, pos: Option<(f64, f64)>) -> Option<ClipId> {
        let seq = self.project.sequence();
        let mut clip = new_text_clip(seq, at, &t("text.default"));
        if let Some(d) = doc {
            clip.name = d.label();
            clip.text = Some(Box::new(d));
        }
        if let Some((x, y)) = pos {
            (clip.transform.x, clip.transform.y) = (x, y);
        }
        let id = clip.id;
        if !self.execute(EditCommand::AddText { clip, track }) {
            return None;
        }
        self.tl.selection = vec![id];
        self.refresh_timeline();
        self.refresh_inspector();
        self.refresh_status();
        self.text_added(id);
        Some(id)
    }

    /// After a text was added (Task 8: starts inline typing with the text selected).
    pub fn text_added(&mut self, _id: ClipId) {}
}
```

- [ ] **Step 3: The timeline look and the entry points** —
  - `theme.slint`, `Theme`: `out property <color> clip-text: #5b3f8f;` and `out property <color> clip-text-top: #6d4dab;` next to `clip-video`; `Icons`: `out property <string> text: "M5 7 V5 H19 V7 M12 5 V19 M9 19 H15";`.
  - `timeline.slint`, `ClipView`: add `is-text: bool,` after `is-audio`. In `ClipRect`: `background: root.cv.offline ? #5a2a2a : !root.cv.enabled ? Theme.clip-disabled : root.cv.is-text ? Theme.clip-text : root.cv.is-audio ? Theme.clip-audio : Theme.clip-video;`, the top bar likewise with `Theme.clip-text-top`; in the name `HorizontalLayout` before the link icon: `if root.cv.is-text: Icon { y: 2px; icon: Icons.text; width: 11px; height: 11px; color: #ffffffd0; }`; and in the body, after the thumbnail `Image`:
    ```slint
    if root.cv.is-text && root.cv.width > 40px: Text {
        x: 6px;
        y: 18px;
        width: parent.width - 12px;
        height: parent.height - 20px;
        text: root.cv.name;
        font-size: Theme.font-sm;
        color: #ffffffb0;
        wrap: word-wrap;
        overflow: elide;
        vertical-alignment: top;
    }
    ```
    Toolbar, after the marker `IconButton`: `VerticalLayout { alignment: center; IconButton { icon: Icons.text; tooltip: Tr.t("tl.tip.add_text"); clicked => { root.tool("add-text"); } } }`.
  - `app.slint`, Edit menu, before `add-video-track`: `{ id: "add-text", label: Tr.t("menu.edit.add_text"), shortcut: "Ctrl+T", enabled: true },`.
  - `timeline_ui.rs::refresh_timeline`, the `ClipView { … }` literal: `name: c.text.as_deref().map_or_else(|| c.name.clone(), |d| d.label()).into(),` and `is_text: c.is_text(),`; `tl_tool`: `"add-text" => self.add_text_at_playhead(),`.
  - `keys.rs::on_key`, Ctrl table: `("t", false) => self.add_text_at_playhead(),`; `shortcut_rows`, in the editing group after `r(".", "keys.overwrite")`: `r("Ctrl+T", "keys.add_text"),`.
  - `persistence.rs::menu` (the `match` with `"add-video-track"`): `"add-text" => self.add_text_at_playhead(),`.
  - Catalogs — `ru.json`: `"text.default": "Ваш текст"`, `"tl.tip.add_text": "Добавить текст (Ctrl+T)"`, `"menu.edit.add_text": "Добавить текст"`, `"keys.add_text": "Добавить текст"`; `en.json`: `"text.default": "Your text"`, `"tl.tip.add_text": "Add text (Ctrl+T)"`, `"menu.edit.add_text": "Add text"`, `"keys.add_text": "Add text"`.

- [ ] **Step 4: Run the tests and build**

Run: `cargo test -p kadr-editor && cargo test -p kadr-i18n`
Expected: PASS (`text_add` and every earlier editor test; the catalog scan finds the new keys).

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p kadr-editor --all-targets`
Expected: no new warnings.

```bash
git add apps/editor crates/i18n/locales
git commit -m "titles: text clips on the timeline, «Т» / Ctrl+T / Edit menu add a title

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 7: The inspector's «ТЕКСТ» section — text field, font, size, weight, colour, outlines, plaque, paragraph (`kadr-editor`)

**Files:**
- Create: `apps/editor/src/text_ui.rs`, `apps/editor/ui/text_inspector.slint`
- Modify: `apps/editor/ui/inspector.slint` (`InspectorData.is-text`; the section before «ТРАНСФОРМАЦИЯ»; speed hidden for text; the badge), `apps/editor/ui/app.slint` (properties, callbacks, exports `TextInspectorData`, `FontRow`), `apps/editor/ui/theme.slint` (icons `align-left`, `align-center`, `align-right`, `caps`), `apps/editor/src/inspector.rs` (`d.is_text`, `refresh_text_inspector` call, session end on selection change), `apps/editor/src/app.rs` (`App.text_session`, callbacks; `execute_as`, `undo`, `redo` commit the session first; user fonts loaded at start), `apps/editor/src/persistence.rs` (`save_to` commits the session first), `apps/editor/src/main.rs` (`mod text_ui;`), `apps/editor/Cargo.toml` (`kadr-text = { path = "../../crates/text" }`), `crates/i18n/locales/{ru,en}.json`

**Interfaces:**
- Consumes: Task 1 `TextDoc::{set_plain, style_range, plain}`, `Rgba8`, `ClipProperty::Text`; Task 3 `kadr_text::{fonts, layout, FontSource, DEFAULT_FAMILY}`; Task 5 `ColorSwatch`, `color_ui::{to_slint, from_slint}`, `App::colour_committed`; `inspector::LiveEdit`, `App::{inspected, show_rolled_back, execute}`.
- Produces:
  - `text_ui::TextValue::{Num(f64), Flag(bool), Name(String), Colour(Rgba8)}`; `text_ui::apply_text_prop(doc: &mut TextDoc, scope: Option<Range<usize>>, prop: &str, v: &TextValue) -> bool` (run props `family size weight italic fill letter-spacing` — to `scope` when given, else the base with the runs' overrides cleared; block props `outline1 outline1-width outline1-color outline2 outline2-width outline2-color plaque plaque-per-line plaque-color plaque-radius plaque-pad-x plaque-pad-y align line-height caps wrap wrap-width`; Task 11 adds more); `text_ui::prop_label_key(prop: &str) -> &'static str`; `text_ui::text_data(doc: &TextDoc, scope: Option<&Range<usize>>, locked: bool) -> TextInspectorData`; `text_ui::font_rows(filter: &str, current: &str) -> Vec<FontRow>`.
  - `text_ui::TextSession` with `clip() -> Option<ClipId>`, `type_text(&mut self, &mut Project, ClipId, &str) -> bool`, `finish(&mut self, &mut Project) -> Option<EditCommand>`; `App.text_session: TextSession`; `App::{text_typed, text_field_focus, commit_text_session, text_live, text_commit, text_set, text_color_live, text_color_commit, font_rows_refresh, refresh_text_inspector}`; free fn `text_ui::add_font_dialog()`.
  - `App.text_scope: Option<Range<usize>>` (always `None` here; Task 11 fills it from the field's selection).
  - Slint: `TextInspectorData`, `FontRow { name, header: bool, current: bool, preview: image, has-preview: bool }`, `TextField` (`in-out text`, `in read-only`, `callback edited(string)`, `callback focus-changed(bool)`), `TextSection`.

- [ ] **Step 1: Write the failing tests** — `apps/editor/src/text_ui.rs` (add `mod text_ui;` to `main.rs` and the `kadr-text` dependency to `apps/editor/Cargo.toml`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;
    use kadr_project::{Clip, Project, RunStyle};
    use kadr_timeline::EditEngine;

    fn red() -> Rgba8 {
        Rgba8::rgba(255, 0, 0, 255)
    }

    #[test]
    fn whole_text_edits_set_the_base_and_clear_the_runs_overrides() {
        let mut d = TextDoc::new("раз два");
        d.style_range(7..13, &RunStyle { size: Some(40.0), fill: Some(Fill::solid(red())), ..Default::default() });
        assert!(apply_text_prop(&mut d, None, "size", &TextValue::Num(120.0)));
        assert_eq!((d.style.size, d.runs.iter().any(|r| r.style.size.is_some())), (120.0, false));
        assert!(d.runs.iter().any(|r| r.style.fill.is_some()), "only the edited field's overrides go");
        assert!(!apply_text_prop(&mut d, None, "size", &TextValue::Num(120.0)), "the same value: no change");
        assert!(apply_text_prop(&mut d, None, "weight", &TextValue::Num(640.0)));
        assert_eq!(d.style.font.weight, 600, "weights land on hundreds");
        assert!(apply_text_prop(&mut d, None, "fill", &TextValue::Colour(Rgba8::BLACK)));
        assert_eq!((d.style.fill.colour, d.runs.len()), (Rgba8::BLACK, 1), "no overrides left: one run");
        assert!(apply_text_prop(&mut d, None, "family", &TextValue::Name("Oswald".into())));
        assert_eq!(d.style.font.family, "Oswald");
        assert!(apply_text_prop(&mut d, None, "letter-spacing", &TextValue::Num(6.0)));
        assert_eq!(d.paragraph.letter_spacing, 6.0);
    }

    #[test]
    fn scoped_edits_style_only_the_selection() {
        let mut d = TextDoc::new("раз два");
        assert!(apply_text_prop(&mut d, Some(7..13), "fill", &TextValue::Colour(red())));
        assert_eq!(d.runs.len(), 2);
        assert_eq!((d.runs[1].text.as_str(), d.runs[1].style.fill.as_ref().map(|f| f.colour)), ("два", Some(red())));
        assert!(apply_text_prop(&mut d, Some(7..13), "letter-spacing", &TextValue::Num(4.0)));
        assert_eq!(d.runs[1].style.letter_spacing, Some(4.0));
        assert!(!apply_text_prop(&mut d, Some(7..13), "outline1", &TextValue::Flag(false)), "block props ignore the scope: already off");
    }

    #[test]
    fn outlines_and_the_plaque_switch_on_with_defaults() {
        let mut d = TextDoc::starter("x", 1080);
        assert!(apply_text_prop(&mut d, None, "outline1", &TextValue::Flag(true)));
        assert_eq!(d.style.outlines, [Outline { width: 4.0, colour: Rgba8::BLACK }]);
        assert!(apply_text_prop(&mut d, None, "outline2", &TextValue::Flag(true)));
        assert_eq!(d.style.outlines.len(), 2);
        assert!(apply_text_prop(&mut d, None, "outline2-width", &TextValue::Num(9.0)));
        assert!(apply_text_prop(&mut d, None, "outline1-color", &TextValue::Colour(red())));
        assert_eq!((d.style.outlines[1].width, d.style.outlines[0].colour), (9.0, red()));
        assert!(apply_text_prop(&mut d, None, "outline1", &TextValue::Flag(false)));
        assert!(d.style.outlines.is_empty(), "the inner outline off takes the outer one too");
        assert!(!apply_text_prop(&mut d, None, "outline2-width", &TextValue::Num(3.0)), "no outline to size");
        assert!(apply_text_prop(&mut d, None, "plaque", &TextValue::Flag(true)));
        let p = d.style.plaque.unwrap();
        assert!(!p.per_line && p.colour.0[3] < 255 && p.radius > 0.0);
        assert!(apply_text_prop(&mut d, None, "plaque-per-line", &TextValue::Flag(true)));
        assert!(apply_text_prop(&mut d, None, "plaque-pad-x", &TextValue::Num(30.0)));
        assert_eq!((d.style.plaque.unwrap().per_line, d.style.plaque.unwrap().padding_x), (true, 30.0));
    }

    #[test]
    fn paragraph_edits() {
        let mut d = TextDoc::new("x");
        assert!(apply_text_prop(&mut d, None, "align", &TextValue::Num(0.0)));
        assert_eq!(d.paragraph.align, Align::Left);
        assert!(apply_text_prop(&mut d, None, "caps", &TextValue::Flag(true)));
        assert!(apply_text_prop(&mut d, None, "wrap", &TextValue::Flag(true)));
        assert_eq!(d.paragraph.wrap, Some(800.0));
        assert!(apply_text_prop(&mut d, None, "wrap-width", &TextValue::Num(500.0)));
        assert!(apply_text_prop(&mut d, None, "wrap", &TextValue::Flag(false)));
        assert_eq!(d.paragraph.wrap, None);
        assert!(apply_text_prop(&mut d, None, "line-height", &TextValue::Num(1.5)));
        assert!(!apply_text_prop(&mut d, None, "no-such-prop", &TextValue::Num(1.0)));
    }

    #[test]
    fn a_release_where_the_drag_began_records_nothing() {
        let d = TextDoc::starter("x", 1080);
        let mut same = d.clone();
        assert!(!apply_text_prop(&mut same, None, "size", &TextValue::Num(d.style.size as f64)));
        assert!(!apply_text_prop(&mut same, None, "fill", &TextValue::Colour(d.style.fill.colour)), "a colour picked again: nothing");
    }

    #[test]
    fn the_data_shows_the_doc() {
        let mut d = TextDoc::starter("x", 1080);
        d.style.font.weight = 650;
        d.style.outlines = vec![Outline { width: 3.0, colour: red() }];
        let v = text_data(&d, None, false);
        assert!(v.has && v.outline1 && !v.outline2 && !v.locked);
        assert_eq!((v.size, v.weight, v.outline1_width), (97.0, 650.0, 3.0));
        assert_eq!(crate::color_ui::from_slint(v.outline1_color), [255, 0, 0, 255]);
        assert_eq!(v.family.as_str(), "Montserrat");
        let mut m = d.clone();
        m.style.font.family = "Nope Sans".into();
        assert!(text_data(&m, None, true).family_missing);
    }

    #[test]
    fn font_rows_group_bundled_mine_system_and_filter() {
        let rows = font_rows("", "Inter");
        assert!(rows[0].header);
        assert!(rows.iter().any(|r| !r.header && r.name == "Montserrat"));
        assert!(rows.iter().filter(|r| r.current).all(|r| r.name == "Inter"));
        let some = font_rows("MONT", "x");
        assert!(some.iter().filter(|r| !r.header).all(|r| r.name.to_lowercase().contains("mont")));
        assert!(font_rows("zzzz-nothing", "x").is_empty(), "no match: no headers either");
    }

    fn project_with_text() -> (Project, kadr_core::ClipId) {
        let mut p = Project::new("t");
        let c = Clip::new_text(TextDoc::new("Гол"), Time::ZERO, Time::from_secs(5));
        let id = c.id;
        p.sequence_mut().tracks[0].clips.push(c);
        (p, id)
    }

    fn text_of(p: &Project, id: kadr_core::ClipId) -> String {
        p.sequence().clip(id).unwrap().text.as_deref().unwrap().plain()
    }

    #[test]
    fn a_typing_session_is_one_entry_and_survives_another_command() {
        let (mut p, id) = project_with_text();
        let mut e = EditEngine::new();
        let mut s = TextSession::default();
        for t in ["Гол!", "Гол!!", "Гол!!!"] {
            assert!(s.type_text(&mut p, id, t));
        }
        assert_eq!((s.clip(), text_of(&p, id)), (Some(id), "Гол!!!".to_string()), "live while typing");
        assert!(!e.can_undo(), "nothing recorded yet");
        // Another command arrives (App::execute_as finishes the session first).
        let cmd = s.finish(&mut p).expect("the session changed the text");
        assert_eq!(text_of(&p, id), "Гол", "finish puts the recorded state back…");
        e.execute(&mut p, cmd).unwrap();
        assert_eq!(text_of(&p, id), "Гол!!!", "…and the one command brings the typing back");
        e.execute(&mut p, kadr_timeline::EditCommand::MoveClips { clips: vec![id], delta: Time::from_secs(1), track_delta: 0 }).unwrap();
        assert_eq!(text_of(&p, id), "Гол!!!", "the other command keeps the typing");
        e.undo(&mut p);
        e.undo(&mut p);
        assert_eq!(text_of(&p, id), "Гол", "two entries: the move, the whole typing");
        assert!(s.clip().is_none() && s.finish(&mut p).is_none());
    }

    #[test]
    fn a_session_without_changes_records_nothing_and_a_finished_one_is_saved() {
        let (mut p, id) = project_with_text();
        let mut s = TextSession::default();
        assert!(!s.type_text(&mut p, id, "Гол"), "the same text starts nothing");
        assert!(s.finish(&mut p).is_none());
        s.type_text(&mut p, id, "Гол!");
        s.type_text(&mut p, id, "Гол");
        assert!(s.finish(&mut p).is_none(), "typed and typed back: no entry");
        s.type_text(&mut p, id, "Счёт");
        let mut e = EditEngine::new();
        let cmd = s.finish(&mut p).unwrap();
        e.execute(&mut p, cmd).unwrap();
        let json = String::from_utf8(kadr_project::io::to_json(&p)).unwrap();
        assert!(json.contains("Счёт"), "what is saved after finishing is the typed text");
    }
}
```

Run: `cargo test -p kadr-editor text_ui`
Expected: FAIL to compile — `cannot find function apply_text_prop`, `TextSession`, `TextInspectorData`.

- [ ] **Step 2: The model edits and the session** — top of `apps/editor/src/text_ui.rs`:

```rust
//! The inspector's «ТЕКСТ» section (titles spec §6.3): the text field (a
//! typing session — live, one undo entry when it ends), and the style
//! controls (slider drags live through `LiveEdit`, one entry on release;
//! switches and choices one entry each). Run properties (font, size,
//! weight, italic, colour, letter spacing) go to the selection when there
//! is one (`App.text_scope`, Task 11), else to the whole text; block
//! properties (outlines, plaque, paragraph) always to the whole text.

use crate::app::App;
use crate::color_ui::{from_slint, to_slint};
use crate::{FontRow, TextInspectorData};
use kadr_core::ClipId;
use kadr_i18n::{t, tf};
use kadr_project::{Align, Fill, Outline, Plaque, Project, Rgba8, RunStyle, TextDoc};
use kadr_timeline::{ClipProperty, EditCommand};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub enum TextValue {
    Num(f64),
    Flag(bool),
    Name(String),
    Colour(Rgba8),
}

impl TextValue {
    fn num(&self) -> Option<f64> {
        match self {
            TextValue::Num(v) if v.is_finite() => Some(*v),
            _ => None,
        }
    }
    fn flag(&self) -> Option<bool> {
        match self {
            TextValue::Flag(b) => Some(*b),
            _ => None,
        }
    }
    fn colour(&self) -> Option<Rgba8> {
        match self {
            TextValue::Colour(c) => Some(*c),
            _ => None,
        }
    }
}

/// Sets `*field` to `v`; `true` when it changed.
fn put<T: PartialEq>(field: &mut T, v: T) -> bool {
    if *field == v {
        false
    } else {
        *field = v;
        true
    }
}

/// One run property over the whole text: the base gets it, the runs' own
/// values of it go (so the whole text shows it).
fn whole<T: PartialEq>(doc: &mut TextDoc, base: impl Fn(&mut TextDoc) -> &mut T, v: T, clear: impl Fn(&mut RunStyle) -> bool) -> bool {
    let mut changed = put(base(doc), v);
    for r in &mut doc.runs {
        changed |= clear(&mut r.style);
    }
    if changed {
        doc.normalise();
    }
    changed
}

fn take<T>(o: &mut Option<T>) -> bool {
    o.take().is_some()
}

/// Applies one inspector edit; `false` when nothing changes (no undo entry).
pub fn apply_text_prop(doc: &mut TextDoc, scope: Option<Range<usize>>, prop: &str, v: &TextValue) -> bool {
    let scoped = scope.filter(|r| r.start < r.end);
    let run = |patch: RunStyle, doc: &mut TextDoc| match &scoped {
        Some(r) => Some(doc.style_range(r.clone(), &patch)),
        None => None,
    };
    match prop {
        "family" => {
            let TextValue::Name(n) = v else { return false };
            if n.trim().is_empty() {
                return false;
            }
            run(RunStyle { family: Some(n.clone()), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.font.family, n.clone(), |s| take(&mut s.family)))
        }
        "size" => {
            let Some(x) = v.num() else { return false };
            let x = x.clamp(1.0, 2000.0).round() as f32;
            run(RunStyle { size: Some(x), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.size, x, |s| take(&mut s.size)))
        }
        "weight" => {
            let Some(x) = v.num() else { return false };
            let w = ((x / 100.0).round() * 100.0).clamp(100.0, 900.0) as u16;
            run(RunStyle { weight: Some(w), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.font.weight, w, |s| take(&mut s.weight)))
        }
        "italic" => {
            let Some(b) = v.flag() else { return false };
            run(RunStyle { italic: Some(b), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.font.italic, b, |s| take(&mut s.italic)))
        }
        "fill" => {
            let Some(c) = v.colour() else { return false };
            let fill = Fill::solid(c);
            run(RunStyle { fill: Some(fill.clone()), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.style.fill, fill, |s| take(&mut s.fill)))
        }
        "letter-spacing" => {
            let Some(x) = v.num() else { return false };
            let x = x.clamp(-200.0, 2000.0) as f32;
            run(RunStyle { letter_spacing: Some(x), ..Default::default() }, doc).unwrap_or_else(|| whole(doc, |d| &mut d.paragraph.letter_spacing, x, |s| take(&mut s.letter_spacing)))
        }
        "outline1" | "outline2" => {
            let Some(on) = v.flag() else { return false };
            let n = if prop == "outline1" { 1 } else { 2 };
            let o = &mut doc.style.outlines;
            match (on, o.len() >= n) {
                (true, true) | (false, false) => false,
                (true, false) => {
                    while o.len() < n {
                        o.push(Outline { width: 4.0, colour: if o.is_empty() { Rgba8::BLACK } else { Rgba8::WHITE } });
                    }
                    true
                }
                (false, true) => {
                    o.truncate(n - 1);
                    true
                }
            }
        }
        "outline1-width" | "outline2-width" | "outline1-color" | "outline2-color" => {
            let i = if prop.starts_with("outline1") { 0 } else { 1 };
            let Some(o) = doc.style.outlines.get_mut(i) else { return false };
            match v {
                TextValue::Num(x) if x.is_finite() => put(&mut o.width, x.clamp(0.0, 200.0) as f32),
                TextValue::Colour(c) => put(&mut o.colour, *c),
                _ => false,
            }
        }
        "plaque" => {
            let Some(on) = v.flag() else { return false };
            let size = doc.style.size;
            match (on, doc.style.plaque.is_some()) {
                (true, false) => {
                    doc.style.plaque = Some(Plaque { per_line: false, colour: Rgba8::rgba(0, 0, 0, 180), radius: (size * 0.12).round(), padding_x: (size * 0.3).round(), padding_y: (size * 0.12).round() });
                    true
                }
                (false, true) => take(&mut doc.style.plaque),
                _ => false,
            }
        }
        "plaque-per-line" | "plaque-color" | "plaque-radius" | "plaque-pad-x" | "plaque-pad-y" => {
            let Some(p) = doc.style.plaque.as_mut() else { return false };
            match (prop, v) {
                ("plaque-per-line", TextValue::Flag(b)) => put(&mut p.per_line, *b),
                ("plaque-color", TextValue::Colour(c)) => put(&mut p.colour, *c),
                ("plaque-radius", TextValue::Num(x)) if x.is_finite() => put(&mut p.radius, x.clamp(0.0, 500.0) as f32),
                ("plaque-pad-x", TextValue::Num(x)) if x.is_finite() => put(&mut p.padding_x, x.clamp(0.0, 500.0) as f32),
                ("plaque-pad-y", TextValue::Num(x)) if x.is_finite() => put(&mut p.padding_y, x.clamp(0.0, 500.0) as f32),
                _ => false,
            }
        }
        "align" => {
            let Some(x) = v.num() else { return false };
            let a = match x.round() as i64 {
                0 => Align::Left,
                2 => Align::Right,
                _ => Align::Center,
            };
            put(&mut doc.paragraph.align, a)
        }
        "line-height" => v.num().is_some_and(|x| put(&mut doc.paragraph.line_height, x.clamp(0.5, 4.0) as f32)),
        "caps" => v.flag().is_some_and(|b| put(&mut doc.paragraph.caps, b)),
        "wrap" => {
            let Some(on) = v.flag() else { return false };
            match (on, doc.paragraph.wrap.is_some()) {
                (true, false) => {
                    doc.paragraph.wrap = Some(800.0);
                    true
                }
                (false, true) => take(&mut doc.paragraph.wrap),
                _ => false,
            }
        }
        "wrap-width" => match (v.num(), doc.paragraph.wrap.as_mut()) {
            (Some(x), Some(w)) => put(w, x.clamp(20.0, 20000.0).round() as f32),
            _ => false,
        },
        _ => false,
    }
}

/// The i18n key naming `prop` in history labels («Текст: размер»).
pub fn prop_label_key(prop: &str) -> &'static str {
    match prop {
        "family" => "text.font",
        "size" => "text.size",
        "weight" => "text.weight",
        "italic" => "text.italic",
        "fill" => "text.fill",
        "letter-spacing" => "text.letter_spacing",
        p if p.starts_with("outline") => "text.outline",
        p if p.starts_with("plaque") => "text.plaque",
        "align" => "text.align",
        "line-height" => "text.line_height",
        "caps" => "text.caps",
        "wrap" | "wrap-width" => "text.wrap",
        _ => "inspector.text",
    }
}

/// What the section shows: the style of the scope's first character
/// (over the base), or the base; `locked` makes the field read-only.
pub fn text_data(doc: &TextDoc, scope: Option<&Range<usize>>, locked: bool) -> TextInspectorData {
    let base = &doc.style;
    let rs = scope.map(|r| doc.run_style_at(r.start).clone()).unwrap_or_default();
    let family = rs.family.clone().unwrap_or_else(|| base.font.family.clone());
    let fill = rs.fill.as_ref().unwrap_or(&base.fill).colour;
    let o = |i: usize| base.outlines.get(i);
    let p = base.plaque;
    TextInspectorData {
        has: true,
        locked,
        family_missing: !kadr_text::fonts().has(&family),
        family: family.into(),
        size: rs.size.unwrap_or(base.size),
        weight: rs.weight.unwrap_or(base.font.weight) as f32,
        italic: rs.italic.unwrap_or(base.font.italic),
        fill: to_slint(fill.0),
        letter_spacing: rs.letter_spacing.unwrap_or(doc.paragraph.letter_spacing),
        outline1: o(0).is_some(),
        outline1_width: o(0).map_or(4.0, |x| x.width),
        outline1_color: to_slint(o(0).map_or(Rgba8::BLACK, |x| x.colour).0),
        outline2: o(1).is_some(),
        outline2_width: o(1).map_or(4.0, |x| x.width),
        outline2_color: to_slint(o(1).map_or(Rgba8::WHITE, |x| x.colour).0),
        plaque: p.is_some(),
        plaque_per_line: p.is_some_and(|p| p.per_line),
        plaque_color: to_slint(p.map_or(Rgba8::rgba(0, 0, 0, 180), |p| p.colour).0),
        plaque_radius: p.map_or(0.0, |p| p.radius),
        plaque_pad_x: p.map_or(0.0, |p| p.padding_x),
        plaque_pad_y: p.map_or(0.0, |p| p.padding_y),
        align: match doc.paragraph.align {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        },
        line_height: doc.paragraph.line_height,
        caps: doc.paragraph.caps,
        wrap: doc.paragraph.wrap.is_some(),
        wrap_width: doc.paragraph.wrap.unwrap_or(800.0),
        ..Default::default()
    }
}

/// The font picker's rows: «Встроенные», «Мои», «Системные», each with its
/// header, filtered by `filter` (case-insensitive); `current` marked.
pub fn font_rows(filter: &str, current: &str) -> Vec<FontRow> {
    let f = filter.trim().to_lowercase();
    let mut out = vec![];
    for (source, key) in [(kadr_text::FontSource::Bundled, "text.fonts.bundled"), (kadr_text::FontSource::User, "text.fonts.mine"), (kadr_text::FontSource::System, "text.fonts.system")] {
        let names: Vec<String> = kadr_text::fonts().families().into_iter().filter(|x| x.source == source && x.name.to_lowercase().contains(&f)).map(|x| x.name).collect();
        if names.is_empty() {
            continue;
        }
        out.push(FontRow { name: t(key).into(), header: true, ..Default::default() });
        out.extend(names.into_iter().map(|n| FontRow { current: n.eq_ignore_ascii_case(current), name: n.into(), ..Default::default() }));
    }
    out
}

/// Typing into a text clip (titles spec §6.3): changes go into the project
/// live; `finish` puts the recorded state back and returns the one command
/// that records the whole session (`None`: nothing changed).
#[derive(Default)]
pub struct TextSession(Option<(ClipId, TextDoc)>);

impl TextSession {
    pub fn clip(&self) -> Option<ClipId> {
        self.0.as_ref().map(|(id, _)| *id)
    }

    /// `text` into clip `id`, live; the session starts (remembering the
    /// document) at its first change. `true` when the text changed.
    pub fn type_text(&mut self, project: &mut Project, id: ClipId, text: &str) -> bool {
        let Some((ti, ci)) = project.sequence().locate_clip(id) else { return false };
        let Some(doc) = project.sequence_mut().tracks[ti].clips[ci].text.as_deref_mut() else { return false };
        if doc.plain() == text {
            return false;
        }
        if self.clip() != Some(id) {
            self.0 = Some((id, doc.clone()));
        }
        doc.set_plain(text)
    }

    pub fn finish(&mut self, project: &mut Project) -> Option<EditCommand> {
        let (id, before) = self.0.take()?;
        let (ti, ci) = project.sequence().locate_clip(id)?;
        let c = &mut project.sequence_mut().tracks[ti].clips[ci];
        let now = c.text.as_deref()?.clone();
        c.text = Some(Box::new(before.clone()));
        (now != before).then(|| EditCommand::Batch { label: t("cmd.text_edit"), commands: vec![EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Text(Box::new(now)) }] })
    }
}
```

(A `TextSession` changed after `finish` by someone else's text edit is not a case: `App` finishes the session before every command, undo, redo, save and selection change.)

Run: `cargo test -p kadr-editor text_ui`
Expected: still FAIL to compile — `TextInspectorData` and `FontRow` come from the Slint of Step 3.

- [ ] **Step 3: The section's Slint** — icons in `theme.slint` `Icons`: `align-left: "M4 6 H20 M4 10 H14 M4 14 H20 M4 18 H14";`, `align-center: "M4 6 H20 M7 10 H17 M4 14 H20 M7 18 H17";`, `align-right: "M4 6 H20 M10 10 H20 M4 14 H20 M10 18 H20";`, `caps: "M3 18 L7.5 6 L12 18 M4.7 14 H10.3 M13 18 L17 8 L21 18 M14.4 15 H19.6";`. Create `apps/editor/ui/text_inspector.slint`:

```slint
import { Theme, Icons } from "theme.slint";
import { Tr } from "i18n.slint";
import { PropSlider, SectionTitle, Segmented, BoundSwitch, Button, IconButton, Icon } from "widgets.slint";
import { ColorSwatch } from "color_picker.slint";
import { LineEdit, ScrollView } from "std-widgets.slint";

export struct TextInspectorData {
    has: bool,
    locked: bool,
    scope: string,          // Task 11: "" whole text, else the selected fragment
    family: string,
    family-missing: bool,
    size: float,
    weight: float,
    italic: bool,
    fill: color,
    letter-spacing: float,
    outline1: bool,
    outline1-width: float,
    outline1-color: color,
    outline2: bool,
    outline2-width: float,
    outline2-color: color,
    plaque: bool,
    plaque-per-line: bool,
    plaque-color: color,
    plaque-radius: float,
    plaque-pad-x: float,
    plaque-pad-y: float,
    align: int,
    line-height: float,
    caps: bool,
    wrap: bool,
    wrap-width: float,
}

export struct FontRow {
    name: string,
    header: bool,
    current: bool,
    preview: image,
    has-preview: bool,
}

/// A label and controls on one line.
component Row inherits HorizontalLayout {
    in property <string> label;
    spacing: 8px;
    height: 26px;
    vertical-stretch: 0;
    Text { text: root.label; width: 96px; font-size: Theme.font-sm; color: Theme.text-dim; vertical-alignment: center; overflow: elide; }
    @children
}

/// The text of a text clip (titles spec §6.3): multi-line, wrapping,
/// growing with its text. Its `text` binds two-way to a property the
/// inspector refresh never replaces while typing (Global Constraints).
export component TextField inherits Rectangle {
    in-out property <string> text;
    in property <bool> read-only;
    callback edited(string);
    callback focus-changed(bool);
    height: max(32px, ti.preferred-height + 12px);
    vertical-stretch: 0;
    border-radius: 4px;
    background: Theme.bg0;
    border-width: 1px;
    border-color: ti.has-focus ? Theme.accent : Theme.border;
    ti := TextInput {
        x: 6px;
        y: 6px;
        width: parent.width - 12px;
        text <=> root.text;
        read-only: root.read-only;
        single-line: false;
        wrap: word-wrap;
        font-size: Theme.font;
        color: Theme.text;
        selection-background-color: Theme.accent.with-alpha(0.45);
        edited => { root.edited(self.text); }
        changed has-focus => { root.focus-changed(self.has-focus); }
    }
}

/// «ТЕКСТ»: the field and the style of a text clip (titles spec §6.3).
export component TextSection inherits VerticalLayout {
    in property <TextInspectorData> d;
    in-out property <string> field-text;
    in property <[FontRow]> fonts;
    in-out property <string> font-search;
    callback typed(string);
    callback field-focus(bool);
    callback live(string, float);
    callback commit(string, float);
    callback set(string, string);
    callback color-live(string, color);
    callback color-commit(string, color);
    callback font-open();
    callback font-search-changed(string);
    callback font-chosen(string);
    callback font-add();
    spacing: 4px;

    SectionTitle { text: Tr.t("inspector.text"); }
    TextField {
        text <=> root.field-text;
        read-only: root.d.locked;
        edited(t) => { root.typed(t); }
        focus-changed(f) => { root.field-focus(f); }
    }
    Row {
        label: Tr.t("text.font");
        font-button := Rectangle {
            horizontal-stretch: 1;
            border-radius: 4px;
            background: fa.has-hover ? Theme.bg3 : Theme.bg2;
            border-width: 1px;
            border-color: Theme.border;
            Text { x: 8px; width: parent.width - 30px; text: root.d.family; font-size: Theme.font-sm; color: Theme.text; vertical-alignment: center; overflow: elide; }
            Icon { x: parent.width - 20px; icon: Icons.chevron-down; width: 12px; height: 12px; }
            fa := TouchArea {
                mouse-cursor: pointer;
                clicked => {
                    root.font-open();
                    picker.show();
                }
            }
        }
    }
    if root.d.family-missing: Text { text: Tr.t1("text.font_missing", root.d.family); font-size: Theme.font-xs; color: Theme.warning; wrap: word-wrap; vertical-stretch: 0; }
    PropSlider { label: Tr.t("text.size"); value: root.d.size; minimum: 8; maximum: 400; default-value: 97; decimals: 0; unit: " px"; changed(v) => { root.live("size", v); } committed(v) => { root.commit("size", v); } }
    PropSlider { label: Tr.t("text.weight"); value: root.d.weight; minimum: 100; maximum: 900; default-value: 700; decimals: 0; changed(v) => { root.live("weight", v); } committed(v) => { root.commit("weight", v); } }
    Row {
        label: Tr.t("text.italic");
        BoundSwitch { checked: root.d.italic; toggled => { root.set("italic", root.d.italic ? "0" : "1"); } }
        Rectangle { horizontal-stretch: 1; }
    }
    Row {
        label: Tr.t("text.fill");
        ColorSwatch { value: root.d.fill; tooltip: Tr.t("text.fill"); live(c) => { root.color-live("fill", c); } committed(c) => { root.color-commit("fill", c); } }
        Rectangle { horizontal-stretch: 1; }
    }
    Row {
        label: Tr.t("text.outline");
        BoundSwitch { checked: root.d.outline1; toggled => { root.set("outline1", root.d.outline1 ? "0" : "1"); } }
        ColorSwatch { enabled: root.d.outline1; value: root.d.outline1-color; tooltip: Tr.t("text.outline"); live(c) => { root.color-live("outline1-color", c); } committed(c) => { root.color-commit("outline1-color", c); } }
        Rectangle { horizontal-stretch: 1; }
    }
    if root.d.outline1: PropSlider { label: Tr.t("text.thickness"); value: root.d.outline1-width; minimum: 0; maximum: 40; default-value: 4; decimals: 0; unit: " px"; changed(v) => { root.live("outline1-width", v); } committed(v) => { root.commit("outline1-width", v); } }
    if root.d.outline1: Row {
        label: Tr.t("text.outline2");
        BoundSwitch { checked: root.d.outline2; toggled => { root.set("outline2", root.d.outline2 ? "0" : "1"); } }
        ColorSwatch { enabled: root.d.outline2; value: root.d.outline2-color; tooltip: Tr.t("text.outline2"); live(c) => { root.color-live("outline2-color", c); } committed(c) => { root.color-commit("outline2-color", c); } }
        Rectangle { horizontal-stretch: 1; }
    }
    if root.d.outline1 && root.d.outline2: PropSlider { label: Tr.t("text.thickness"); value: root.d.outline2-width; minimum: 0; maximum: 40; default-value: 4; decimals: 0; unit: " px"; changed(v) => { root.live("outline2-width", v); } committed(v) => { root.commit("outline2-width", v); } }
    Row {
        label: Tr.t("text.plaque");
        BoundSwitch { checked: root.d.plaque; toggled => { root.set("plaque", root.d.plaque ? "0" : "1"); } }
        ColorSwatch { enabled: root.d.plaque; value: root.d.plaque-color; tooltip: Tr.t("text.plaque"); live(c) => { root.color-live("plaque-color", c); } committed(c) => { root.color-commit("plaque-color", c); } }
        if root.d.plaque: Segmented {
            options: [Tr.t("text.plaque.block"), Tr.t("text.plaque.lines")];
            current: root.d.plaque-per-line ? 1 : 0;
            selected(i) => { root.set("plaque-per-line", i == 1 ? "1" : "0"); }
        }
        Rectangle { horizontal-stretch: 1; }
    }
    if root.d.plaque: PropSlider { label: Tr.t("text.plaque.radius"); value: root.d.plaque-radius; minimum: 0; maximum: 100; default-value: 12; decimals: 0; unit: " px"; changed(v) => { root.live("plaque-radius", v); } committed(v) => { root.commit("plaque-radius", v); } }
    if root.d.plaque: PropSlider { label: Tr.t("text.plaque.pad_x"); value: root.d.plaque-pad-x; minimum: 0; maximum: 200; default-value: 29; decimals: 0; unit: " px"; changed(v) => { root.live("plaque-pad-x", v); } committed(v) => { root.commit("plaque-pad-x", v); } }
    if root.d.plaque: PropSlider { label: Tr.t("text.plaque.pad_y"); value: root.d.plaque-pad-y; minimum: 0; maximum: 200; default-value: 12; decimals: 0; unit: " px"; changed(v) => { root.live("plaque-pad-y", v); } committed(v) => { root.commit("plaque-pad-y", v); } }
    Row {
        label: Tr.t("text.align");
        IconButton { icon: Icons.align-left; tooltip: Tr.t("text.align.left"); active: root.d.align == 0; clicked => { root.set("align", "0"); } }
        IconButton { icon: Icons.align-center; tooltip: Tr.t("text.align.center"); active: root.d.align == 1; clicked => { root.set("align", "1"); } }
        IconButton { icon: Icons.align-right; tooltip: Tr.t("text.align.right"); active: root.d.align == 2; clicked => { root.set("align", "2"); } }
        Rectangle { width: 8px; }
        IconButton { icon: Icons.caps; tooltip: Tr.t("text.caps"); active: root.d.caps; clicked => { root.set("caps", root.d.caps ? "0" : "1"); } }
        Rectangle { horizontal-stretch: 1; }
    }
    PropSlider { label: Tr.t("text.line_height"); value: root.d.line-height; minimum: 0.5; maximum: 3; default-value: 1.2; decimals: 2; changed(v) => { root.live("line-height", v); } committed(v) => { root.commit("line-height", v); } }
    PropSlider { label: Tr.t("text.letter_spacing"); value: root.d.letter-spacing; minimum: -20; maximum: 100; default-value: 0; decimals: 0; unit: " px"; changed(v) => { root.live("letter-spacing", v); } committed(v) => { root.commit("letter-spacing", v); } }
    Row {
        label: Tr.t("text.wrap");
        BoundSwitch { checked: root.d.wrap; toggled => { root.set("wrap", root.d.wrap ? "0" : "1"); } }
        Rectangle { horizontal-stretch: 1; }
    }
    if root.d.wrap: PropSlider { label: Tr.t("text.wrap_width"); value: root.d.wrap-width; minimum: 100; maximum: 3840; default-value: 800; decimals: 0; unit: " px"; changed(v) => { root.live("wrap-width", v); } committed(v) => { root.commit("wrap-width", v); } }

    picker := PopupWindow {
        x: 0;
        y: 56px;
        width: root.width;
        height: 380px;
        close-policy: close-on-click-outside;
        Rectangle {
            background: Theme.bg2;
            border-radius: 6px;
            border-width: 1px;
            border-color: Theme.border-strong;
            drop-shadow-blur: 12px;
            drop-shadow-color: #00000080;
        }
        VerticalLayout {
            padding: 8px;
            spacing: 6px;
            LineEdit {
                vertical-stretch: 0;
                placeholder-text: Tr.t("text.font_search");
                text <=> root.font-search;
                edited(t) => { root.font-search-changed(t); }
            }
            ScrollView {
                vertical-stretch: 1;
                VerticalLayout {
                    alignment: start;
                    for f in root.fonts: Rectangle {
                        height: f.header ? 22px : 30px;
                        background: !f.header && fr.has-hover ? Theme.bg3 : transparent;
                        border-radius: 4px;
                        if f.header: Text { x: 6px; text: f.name; font-size: Theme.font-xs; font-weight: 700; color: Theme.text-faint; vertical-alignment: center; }
                        if !f.header && f.has-preview: Image { x: 8px; height: parent.height - 6px; source: f.preview; image-fit: contain; }
                        if !f.header && !f.has-preview: Text { x: 8px; text: f.name; font-size: Theme.font; color: f.current ? Theme.accent : Theme.text; vertical-alignment: center; }
                        if f.current: Icon { x: parent.width - 20px; icon: Icons.check; width: 12px; height: 12px; color: Theme.accent; }
                        fr := TouchArea {
                            enabled: !f.header;
                            mouse-cursor: pointer;
                            clicked => {
                                root.font-chosen(f.name);
                                picker.close();
                            }
                        }
                    }
                }
            }
            Button {
                vertical-stretch: 0;
                icon: Icons.plus;
                text: Tr.t("text.add_font");
                clicked => {
                    root.font-add();
                    picker.close();
                }
            }
        }
    }
}
```

(If `Icons.check` does not exist, add `check: "M5 12.5 L10 17.5 L19 7";` to `Icons`.)

`inspector.slint`: import `TextSection, TextInspectorData, FontRow` from `"text_inspector.slint"`; `InspectorData` gets `is-text: bool,`; `Inspector` gets

```slint
    in property <TextInspectorData> text;
    in-out property <string> text-field;
    in property <[FontRow]> fonts;
    in-out property <string> font-search;
    callback text-typed(string);
    callback text-focus(bool);
    callback text-live(string, float);
    callback text-commit(string, float);
    callback text-set(string, string);
    callback text-color-live(string, color);
    callback text-color-commit(string, color);
    callback font-open();
    callback font-search-changed(string);
    callback font-chosen(string);
    callback font-add();
```

and in the clip `ScrollView`, right before `if root.d.has-video-props: SectionTitle { text: Tr.t("inspector.transform"); … }`:

```slint
                if root.d.is-text: TextSection {
                    d: root.text;
                    field-text <=> root.text-field;
                    fonts: root.fonts;
                    font-search <=> root.font-search;
                    typed(t) => { root.text-typed(t); }
                    field-focus(f) => { root.text-focus(f); }
                    live(p, v) => { root.text-live(p, v); }
                    commit(p, v) => { root.text-commit(p, v); }
                    set(p, v) => { root.text-set(p, v); }
                    color-live(p, c) => { root.text-color-live(p, c); }
                    color-commit(p, c) => { root.text-color-commit(p, c); }
                    font-open => { root.font-open(); }
                    font-search-changed(s) => { root.font-search-changed(s); }
                    font-chosen(n) => { root.font-chosen(n); }
                    font-add => { root.font-add(); }
                }
```

The speed `HorizontalLayout { … PropSlider { label: Tr.t("inspector.speed"); … } }` becomes `if !root.d.is-text: HorizontalLayout { … }`; the header `Badge`: `text: root.d.is-text ? Tr.t("inspector.badge.text") : root.d.is-audio ? … ; bg: root.d.is-text ? Theme.clip-text-top : …`.

`app.slint`: `import { TextInspectorData, FontRow } from "text_inspector.slint";`, both added to `export { … }`; on `AppWindow`:

```slint
    in property <TextInspectorData> text-insp;
    in-out property <string> text-field;
    in property <[FontRow]> font-rows;
    in-out property <string> font-search;
    callback text-typed(string);
    callback text-focus(bool);
    callback text-live(string, float);
    callback text-commit(string, float);
    callback text-set(string, string);
    callback text-color-live(string, color);
    callback text-color-commit(string, color);
    callback font-open();
    callback font-search-changed(string);
    callback font-chosen(string);
    callback font-add();
```

and on the `Inspector { … }` element:

```slint
                    text: root.text-insp;
                    text-field <=> root.text-field;
                    fonts: root.font-rows;
                    font-search <=> root.font-search;
                    text-typed(t) => { root.text-typed(t); }
                    text-focus(f) => { root.text-focus(f); }
                    text-live(p, v) => { root.text-live(p, v); }
                    text-commit(p, v) => { root.text-commit(p, v); root.focus-editor(); }
                    text-set(p, v) => { root.text-set(p, v); root.focus-editor(); }
                    text-color-live(p, c) => { root.text-color-live(p, c); }
                    text-color-commit(p, c) => { root.text-color-commit(p, c); }
                    font-open => { root.font-open(); }
                    font-search-changed(s) => { root.font-search-changed(s); }
                    font-chosen(n) => { root.font-chosen(n); root.focus-editor(); }
                    font-add => { root.font-add(); }
```

(`text-typed` and `text-focus` never call `focus-editor`: the field keeps the keyboard.)

- [ ] **Step 4: The App side** — in `text_ui.rs` add:

```rust
impl App {
    /// Ends the typing session: records it as one entry (titles spec §6.3).
    /// Called before every other command, undo, redo, save and selection change.
    pub fn commit_text_session(&mut self) {
        if let Some(cmd) = self.text_session.finish(&mut self.project) {
            self.execute(cmd);
        }
    }

    pub fn text_typed(&mut self, text: &str) {
        let Some(id) = self.inspected() else { return };
        let locked = self.project.sequence().locate_clip(id).is_some_and(|(ti, _)| self.project.sequence().tracks[ti].locked);
        if locked {
            return;
        }
        if self.text_session.clip().is_some_and(|c| c != id) {
            self.commit_text_session();
        }
        if self.text_session.type_text(&mut self.project, id, text) {
            self.preview.invalidate();
            self.refresh_timeline();
            self.request_frame();
        }
    }

    pub fn text_field_focus(&mut self, focused: bool) {
        if !focused {
            self.commit_text_session();
        }
    }

    fn text_edit_live(&mut self, prop: &str, v: TextValue) {
        let Some(id) = self.inspected() else { return };
        let Some((ti, ci)) = self.project.sequence().locate_clip(id) else { return };
        if self.project.sequence().tracks[ti].locked {
            return;
        }
        self.commit_text_session();
        self.live_edit.arm(self.project.sequence_mut(), id);
        let scope = self.text_scope.clone();
        let Some(doc) = self.project.sequence_mut().tracks[ti].clips[ci].text.as_deref_mut() else { return };
        if apply_text_prop(doc, scope, prop, &v) {
            self.preview.invalidate();
            self.refresh_text_inspector();
            self.request_frame();
        }
    }

    fn text_edit_commit(&mut self, prop: &str, v: TextValue) {
        let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
        self.commit_text_session();
        let Some(id) = self.inspected() else {
            if rolled_back {
                self.show_rolled_back();
            }
            return;
        };
        let Some(mut doc) = self.project.sequence().clip(id).and_then(|c| c.text.as_deref()).cloned() else { return };
        if !apply_text_prop(&mut doc, self.text_scope.clone(), prop, &v) {
            return self.show_rolled_back(); // nothing changed: no undo entry
        }
        let label = tf("cmd.text_named", &[("name", &t(prop_label_key(prop)))]);
        let set = EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Text(Box::new(doc)) };
        if !self.execute(EditCommand::Batch { label, commands: vec![set] }) {
            self.show_rolled_back();
        }
    }

    pub fn text_live(&mut self, prop: &str, v: f32) {
        self.text_edit_live(prop, TextValue::Num(v as f64));
    }

    pub fn text_commit(&mut self, prop: &str, v: f32) {
        self.text_edit_commit(prop, TextValue::Num(v as f64));
    }

    /// A switch, a choice or a font: "1" / "0", a number, or a name.
    pub fn text_set(&mut self, prop: &str, value: &str) {
        let v = match prop {
            "family" => TextValue::Name(value.to_string()),
            "italic" | "outline1" | "outline2" | "plaque" | "plaque-per-line" | "caps" | "wrap" => TextValue::Flag(value == "1"),
            _ => match value.parse::<f64>() {
                Ok(x) => TextValue::Num(x),
                Err(_) => return,
            },
        };
        self.text_edit_commit(prop, v);
    }

    pub fn text_color_live(&mut self, prop: &str, c: slint::Color) {
        self.text_edit_live(prop, TextValue::Colour(Rgba8(from_slint(c))));
    }

    pub fn text_color_commit(&mut self, prop: &str, c: slint::Color) {
        self.colour_committed(c);
        self.text_edit_commit(prop, TextValue::Colour(Rgba8(from_slint(c))));
    }

    /// The «ТЕКСТ» section for the inspected clip (called by `refresh_inspector`).
    pub fn refresh_text_inspector(&mut self) {
        let ui = self.ui();
        let Some(id) = self.inspected() else { return };
        let seq = self.project.sequence();
        let Some((ti, ci)) = seq.locate_clip(id) else { return };
        let Some(doc) = seq.tracks[ti].clips[ci].text.as_deref() else { return };
        ui.set_text_insp(text_data(doc, self.text_scope.as_ref(), seq.tracks[ti].locked));
        // The field shows the document unless it is the one being typed in.
        let plain = doc.plain();
        if self.text_session.clip() != Some(id) && ui.get_text_field().as_str() != plain {
            ui.set_text_field(plain.into());
        }
    }

    pub fn font_rows_refresh(&mut self) {
        let ui = self.ui();
        let current = ui.get_text_insp().family.to_string();
        let rows = font_rows(ui.get_font_search().as_str(), &current);
        crate::util::sync_rows_by_key(ui.get_font_rows(), rows, |r| (r.name.clone(), r.header), |m| ui.set_font_rows(m));
    }
}

/// «Добавить шрифт…»: a native dialog (outside the app borrow); each font
/// is installed into the user's font folder; the first family is applied
/// to the inspected text.
pub fn add_font_dialog() {
    let Some(files) = rfd::FileDialog::new().set_title(t("text.add_font")).add_filter(t("dlg.filter.fonts"), &["ttf", "otf", "ttc"]).pick_files() else { return };
    let mut added: Vec<String> = vec![];
    let mut failed: Vec<String> = vec![];
    for f in files {
        match kadr_text::fonts().install_user_font(&f) {
            Ok(names) => added.extend(names),
            Err(_) => failed.push(f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        }
    }
    crate::app::with_app(|app| {
        if let Some(first) = added.first().cloned() {
            app.toast(tf("text.font_added", &[("name", &added.join(", "))]));
            app.text_set("family", &first);
        }
        for name in failed {
            app.toast_warn(tf("text.font_not_a_font", &[("file", &name)]));
        }
        app.font_rows_refresh();
    });
}
```

In `app.rs`: field `pub text_session: crate::text_ui::TextSession,` and `pub text_scope: Option<std::ops::Range<usize>>,` on `App` (defaults); `execute_as` starts with `self.commit_text_session();` (before `self.end_live_edit();`); `undo` and `redo` start with `self.commit_text_session();`; in `wire_callbacks`:

```rust
    cb!(ui.on_text_typed, |s| |app: &mut App| app.text_typed(&s));
    cb!(ui.on_text_focus, |f| |app: &mut App| app.text_field_focus(f));
    cb!(ui.on_text_live, |p, v| |app: &mut App| app.text_live(&p, v));
    cb!(ui.on_text_commit, |p, v| |app: &mut App| app.text_commit(&p, v));
    cb!(ui.on_text_set, |p, v| |app: &mut App| app.text_set(&p, &v));
    cb!(ui.on_text_color_live, |p, c| |app: &mut App| app.text_color_live(&p, c));
    cb!(ui.on_text_color_commit, |p, c| |app: &mut App| app.text_color_commit(&p, c));
    cb!(ui.on_font_open, | | |app: &mut App| app.font_rows_refresh());
    cb!(ui.on_font_search_changed, |_s| |app: &mut App| app.font_rows_refresh());
    cb!(ui.on_font_chosen, |n| |app: &mut App| app.text_set("family", &n));
    ui.on_font_add(crate::text_ui::add_font_dialog);
```

and, where the app starts (after `AppDirs` is known), warm the fonts and load the user's on a background thread: `let fonts_dir = dirs.data.join("fonts"); std::thread::Builder::new().name("kadr-fonts".into()).spawn(move || { kadr_text::fonts().load_user_dir(&fonts_dir); }).ok();`. `persistence.rs::save_to` starts with `self.commit_text_session();`. `inspector.rs::refresh_inspector`: at its top, `if self.text_session.clip().is_some() && self.text_session.clip() != self.inspected() { self.commit_text_session(); }` (a selection change ends typing; `commit_text_session` → `execute` → `after_edit` → a nested refresh, which finds no session); in the clip branch `d.is_text = c.is_text();`; after `ui.set_inspector(d);`: `self.refresh_text_inspector();`. Also clear `self.text_scope` when the inspected clip changes (keep the last inspected id in `App.text_scope_clip: Option<ClipId>`; if it differs from `self.inspected()`, set `text_scope = None`).

Catalogs (RU / EN): `inspector.text` «ТЕКСТ» / "TEXT"; `inspector.badge.text` «Текст» / "Text"; `text.font` «Шрифт» / "Font"; `text.font_missing` «Шрифт «{0}» не найден — показан Montserrat» / "Font “{0}” not found — showing Montserrat"; `text.font_search` «Найти шрифт» / "Find a font"; `text.fonts.bundled` «Встроенные» / "Built-in"; `text.fonts.mine` «Мои» / "Mine"; `text.fonts.system` «Системные» / "System"; `text.add_font` «Добавить шрифт…» / "Add font…"; `text.font_added` «Добавлен шрифт: {name}» / "Font added: {name}"; `text.font_not_a_font` «{file} — не шрифт» / "{file} is not a font"; `dlg.filter.fonts` «Шрифты» / "Fonts"; `text.size` «Размер» / "Size"; `text.weight` «Насыщенность» / "Weight"; `text.italic` «Курсив» / "Italic"; `text.fill` «Цвет» / "Colour"; `text.outline` «Обводка» / "Outline"; `text.outline2` «Обводка 2» / "Outline 2"; `text.thickness` «Толщина» / "Thickness"; `text.plaque` «Плашка» / "Plaque"; `text.plaque.block` «Блок» / "Block"; `text.plaque.lines` «Строки» / "Lines"; `text.plaque.radius` «Скругление» / "Rounding"; `text.plaque.pad_x` «Отступ по X» / "Padding X"; `text.plaque.pad_y` «Отступ по Y» / "Padding Y"; `text.align` «Абзац» / "Paragraph"; `text.align.left` «По левому краю» / "Align left"; `text.align.center` «По центру» / "Centre"; `text.align.right` «По правому краю» / "Align right"; `text.caps` «ВСЕ ЗАГЛАВНЫЕ» / "ALL CAPS"; `text.line_height` «Межстрочный» / "Line height"; `text.letter_spacing` «Межбуквенный» / "Letter spacing"; `text.wrap` «Перенос по ширине» / "Wrap"; `text.wrap_width` «Ширина» / "Width"; `cmd.text_named` «Текст: {name}» / "Text: {name}"; `cmd.text_edit` «Правка текста» / "Edit text". (`text.font_missing` uses Slint's `{0}`: check `placeholders_match_between_languages`.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p kadr-editor && cargo test -p kadr-i18n`
Expected: PASS — the `text_ui` tests, every earlier editor test, the catalog scans.

- [ ] **Step 6: Clippy and commit**

Run: `cargo clippy -p kadr-editor --all-targets`
Expected: no new warnings.

```bash
git add apps/editor crates/i18n/locales
git commit -m "titles: the inspector's ТЕКСТ section — field, font, size, colour, outlines, plaque, paragraph

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 8: Editing on the preview — frame and handles, move / scale / rotate with snapping, inline typing (`kadr-editor`)

**Files:**
- Create: `apps/editor/src/preview_edit.rs`
- Modify: `apps/editor/ui/preview.slint` (structs `HandleView`, `GuideView`; the frame, handles, guides, the editing `TouchArea`, the inline field), `apps/editor/ui/app.slint` (properties, callbacks, exports), `apps/editor/src/preview.rs` (`request_frame` → `refresh_preview_frame`; a text clip's name in the badge), `apps/editor/src/inspector.rs` (`fn apply` → `pub(crate)`), `apps/editor/src/keys.rs` (Esc and Ctrl+Enter finish inline typing first), `apps/editor/src/text_add.rs` (`text_added` starts inline typing), `apps/editor/src/app.rs` (`App.preview_edit`, callbacks), `apps/editor/src/main.rs` (`mod preview_edit;`), `crates/i18n/locales/{ru,en}.json`

**Interfaces:**
- Consumes: Task 4 text layers (`LayerContent::Text`, anchor at the block centre); Task 7 `TextSession`, `App::commit_text_session`; `effects_ui::layer_of`; `anim_ui::{track_of, edit_time, key_here}`; `inspector::{apply, LiveEdit}`.
- Produces:
  - Pure: `preview_edit::{Frame { corners: [(f64, f64); 4], centre: (f64, f64), rotate: (f64, f64) }, local_box(&Layer, Option<(f32, f32)>) -> RectF, frame_of(&Layer, Option<(f32, f32)>, gap: f64) -> Frame, Frame::contains((f64, f64)) -> bool, Frame::bounds() -> [f64; 4], Grab::{Move, Corner(usize), Rotate}, grab_at(&Frame, (f64, f64), tol: f64) -> Option<Grab>, Guide::{V(f64), H(f64)}, snap_move([f64; 4], canvas: (f64, f64), others: &[[f64; 4]], tol: f64) -> ((f64, f64), Vec<Guide>), snap_angle(f64) -> f64, DragStart { grab, at, x, y, scale, rotation, frame }, drag_to(&DragStart, p, canvas, others, tol, free: bool) -> (Vec<(&'static str, f64)>, Vec<Guide>), transform_command(&Clip, &[(&str, f64)], playhead: Time, frame: Time, label: String) -> Option<EditCommand>}`.
  - `App::{refresh_preview_frame(Option<&FrameScene>), preview_pointer(kind: i32, fx: f32, fy: f32, shift: bool, view_w: f32), preview_double(fx: f32, fy: f32, view_w: f32), begin_inline_typing(ClipId), inline_typed(&str), finish_inline_typing()}`; `App.preview_edit: preview_edit::PreviewEdit`.

- [ ] **Step 1: Write the failing geometry tests** — `apps/editor/src/preview_edit.rs` (add `mod preview_edit;` to `main.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;
    use kadr_project::{AnimParam, Clip, SourceTime, TextDoc};
    use kadr_scene::{BlendMode, LayerContent, LayerId, Placement, RectF, Vec2};

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn solid_layer(p: Placement) -> Layer {
        Layer { id: LayerId(1), content: LayerContent::Solid(kadr_scene::Rgba::BLACK), crop: p.full_crop(), placement: p, opacity: 1.0, blend: BlendMode::Normal, effects: vec![] }
    }

    #[test]
    fn a_text_frame_is_its_layout_box_turned_and_scaled_about_the_anchor() {
        let p = Placement { size: Vec2::new(300.0, 120.0), anchor: Vec2::new(0.4, 0.5), position: Vec2::new(960.0, 540.0), scale: Vec2::new(2.0, 2.0), rotation: std::f32::consts::FRAC_PI_2 };
        let l = solid_layer(p);
        assert_eq!(local_box(&l, Some((200.0, 80.0))), RectF::new(20.0, 20.0, 220.0, 100.0), "the 200 × 80 block round the anchor (120, 60)");
        let f = frame_of(&l, Some((200.0, 80.0)), 24.0);
        assert!(near(f.centre, (960.0, 540.0)));
        // Turned 90° clockwise and doubled: the block's top-left (−100, −40) from the centre lands at (+80, −200).
        assert!(near(f.corners[0], (1040.0, 340.0)), "{:?}", f.corners[0]);
        assert!(near(f.corners[2], (880.0, 740.0)));
        assert!(f.contains((960.0, 540.0)) && !f.contains((960.0, 300.0)));
        let b = f.bounds();
        assert!(b.iter().zip([880.0, 340.0, 1040.0, 740.0]).all(|(a, e)| (a - e).abs() < 1e-3), "{b:?}");
        let media = frame_of(&solid_layer(Placement::fill(kadr_scene::SizeU::new(1920, 1080))), None, 24.0);
        assert!(near(media.corners[0], (0.0, 0.0)) && near(media.corners[2], (1920.0, 1080.0)), "a media layer is framed by its crop");
        assert!(near(media.rotate, (960.0, -24.0)), "the rotation handle sits above the top edge's middle");
    }

    #[test]
    fn grabbing_prefers_the_rotation_handle_then_a_corner_then_the_inside() {
        let f = frame_of(&solid_layer(Placement::fill(kadr_scene::SizeU::new(400, 200))), None, 24.0);
        assert_eq!(grab_at(&f, (200.0, -22.0), 8.0), Some(Grab::Rotate));
        assert_eq!(grab_at(&f, (396.0, 197.0), 8.0), Some(Grab::Corner(2)));
        assert_eq!(grab_at(&f, (100.0, 100.0), 8.0), Some(Grab::Move));
        assert_eq!(grab_at(&f, (500.0, 100.0), 8.0), None);
    }

    #[test]
    fn moves_snap_to_centre_edges_safe_areas_and_other_texts() {
        let canvas = (1920.0, 1080.0);
        // A 200 × 100 box whose centre is 4 px right of the canvas centre.
        let ((dx, dy), guides) = snap_move([864.0, 300.0, 1064.0, 400.0], canvas, &[], 6.0);
        assert_eq!((dx, dy), (-4.0, 0.0));
        assert_eq!(guides, [Guide::V(960.0)]);
        let ((dx, _), g) = snap_move([1720.0, 0.0, 1827.0, 50.0], canvas, &[], 6.0);
        assert_eq!((dx, g[0]), (-3.0, Guide::V(1824.0)), "the right edge to the 90 % action-safe line (1920 · 0.95)");
        let ((_, dy), g) = snap_move([400.0, 497.0, 600.0, 597.0], canvas, &[[1000.0, 600.0, 1200.0, 700.0]], 6.0);
        assert_eq!((dy, g[0]), (3.0, Guide::H(600.0)), "the bottom edge to another text's top");
        let ((dx, dy), g) = snap_move([300.0, 300.0, 400.0, 400.0], canvas, &[], 6.0);
        assert!(dx == 0.0 && dy == 0.0 && g.is_empty(), "nothing within reach: no snap");
    }

    #[test]
    fn corner_drags_scale_about_the_centre_and_rotation_snaps_to_15_degrees() {
        let f = frame_of(&solid_layer(Placement { size: Vec2::new(200.0, 100.0), anchor: Vec2::new(0.5, 0.5), position: Vec2::new(500.0, 500.0), scale: Vec2::new(1.0, 1.0), rotation: 0.0 }), None, 24.0);
        let start = DragStart { grab: Grab::Corner(2), at: (600.0, 550.0), x: 0.0, y: 0.0, scale: 1.0, rotation: 0.0, frame: f };
        let (v, _) = drag_to(&start, (700.0, 600.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, "scale");
        assert!((v[0].1 - 2.0).abs() < 1e-9, "twice as far from the centre: twice the scale");
        let rot = DragStart { grab: Grab::Rotate, at: (500.0, 400.0), ..start };
        let (v, _) = drag_to(&rot, (600.0, 503.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v, [("rotation", 90.0)], "91.7° snaps to 90°");
        let (v, _) = drag_to(&rot, (600.0, 503.0), (1920.0, 1080.0), &[], 6.0, true);
        assert!((v[0].1 - 91.7184).abs() < 1e-3, "Shift: free, {}", v[0].1);
        assert_eq!(snap_angle(46.0), 45.0);
        assert_eq!(snap_angle(52.0), 52.0);
        let mv = DragStart { grab: Grab::Move, at: (500.0, 500.0), x: 10.0, y: 20.0, ..start };
        let (v, guides) = drag_to(&mv, (958.0, 500.0), (1920.0, 1080.0), &[], 6.0, false);
        assert_eq!(v, [("x", 470.0), ("y", 20.0)], "the box centre lands on the canvas centre (dx 458 + 2)");
        assert_eq!(guides, [Guide::V(960.0)]);
    }

    #[test]
    fn a_drag_is_one_entry_and_a_click_is_none() {
        let mut c = Clip::new_text(TextDoc::new("x"), Time::ZERO, Time::from_secs(5));
        let frame = Time::from_millis(40);
        assert!(transform_command(&c, &[("x", 0.0), ("y", 0.0)], Time::from_secs(1), frame, "m".into()).is_none(), "released where it started: nothing");
        let cmd = transform_command(&c, &[("x", 30.0), ("y", -5.0)], Time::from_secs(1), frame, "m".into()).unwrap();
        let EditCommand::Batch { commands, .. } = &cmd else { panic!() };
        assert_eq!(commands.len(), 1, "both static values in one SetClipProperty");
        c.animation.set_key(AnimParam::X, SourceTime(Time::ZERO), 0.0, None);
        let EditCommand::Batch { commands, .. } = transform_command(&c, &[("x", 30.0), ("y", -5.0)], Time::from_secs(1), frame, "m".into()).unwrap() else { panic!() };
        assert!(matches!(commands[0], EditCommand::SetKeyframe { .. }) && matches!(commands[1], EditCommand::SetClipProperty { .. }), "an animated x gets a keyframe at the playhead, y stays static — one entry");
        assert!(transform_command(&c, &[("x", 30.0)], Time::from_secs(9), frame, "m".into()).is_none(), "the playhead off the clip: no keyframe there");
    }
}
```

Run: `cargo test -p kadr-editor preview_edit`
Expected: FAIL to compile — `cannot find function local_box`.

- [ ] **Step 2: The geometry** — top of `apps/editor/src/preview_edit.rs`:

```rust
//! Editing on the preview (titles spec §1, §6.2): the selected clip's frame
//! (a text clip's layout box, any other clip's crop) with four corner
//! handles and a rotation handle; a drag moves (snapping to the canvas
//! centre and edges, the 90 % / 93 % safe areas and other texts; Shift
//! frees it), scales about the centre or rotates (15° steps), live, and is
//! one undo entry on release; a click selects the topmost clip under the
//! pointer; a double-click on a text types into it on the frame. Geometry
//! here is pure, in canvas px.

use crate::app::App;
use kadr_core::{ClipId, Time};
use kadr_i18n::t;
use kadr_project::{Clip, SourceTime};
use kadr_scene::{FrameScene, Layer, LayerContent, RectF};
use kadr_timeline::{ClipProperty, EditCommand};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Top-left, top-right, bottom-right, bottom-left of the box, canvas px.
    pub corners: [(f64, f64); 4],
    /// The point scaling and rotation turn about: the placement's position.
    pub centre: (f64, f64),
    /// The rotation handle.
    pub rotate: (f64, f64),
}

impl Frame {
    /// Inside the (convex) box.
    pub fn contains(&self, p: (f64, f64)) -> bool {
        let c = &self.corners;
        let side = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let s: Vec<f64> = (0..4).map(|i| side(c[i], c[(i + 1) % 4])).collect();
        s.iter().all(|v| *v >= 0.0) || s.iter().all(|v| *v <= 0.0)
    }

    /// Axis-aligned bounds `[x0, y0, x1, y1]`.
    pub fn bounds(&self) -> [f64; 4] {
        let xs = self.corners.map(|c| c.0);
        let ys = self.corners.map(|c| c.1);
        [xs.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max), ys.iter().cloned().fold(f64::MIN, f64::max)]
    }
}

/// The box the handles frame, in the layer's local px: a text layer's
/// layout block (`text_size`, centred on its anchor — titles refinement 2),
/// any other layer's crop.
pub fn local_box(layer: &Layer, text_size: Option<(f32, f32)>) -> RectF {
    match text_size {
        Some((w, h)) => {
            let (ax, ay) = (layer.placement.anchor.x * layer.placement.size.x, layer.placement.anchor.y * layer.placement.size.y);
            RectF::new(ax - w / 2.0, ay - h / 2.0, ax + w / 2.0, ay + h / 2.0)
        }
        None => layer.crop,
    }
}

/// The layer's frame on the canvas; the rotation handle `gap` canvas px
/// beyond the top edge's middle, away from the centre.
pub fn frame_of(layer: &Layer, text_size: Option<(f32, f32)>, gap: f64) -> Frame {
    let b = local_box(layer, text_size);
    let m = layer.placement.to_canvas();
    let corners = [(b.x0, b.y0), (b.x1, b.y0), (b.x1, b.y1), (b.x0, b.y1)].map(|(x, y)| m.apply(x as f64, y as f64));
    let centre = (layer.placement.position.x as f64, layer.placement.position.y as f64);
    let top = ((corners[0].0 + corners[1].0) / 2.0, (corners[0].1 + corners[1].1) / 2.0);
    let bottom = ((corners[2].0 + corners[3].0) / 2.0, (corners[2].1 + corners[3].1) / 2.0);
    let (ux, uy) = (top.0 - bottom.0, top.1 - bottom.1);
    let len = ux.hypot(uy).max(1e-9);
    Frame { corners, centre, rotate: (top.0 + ux / len * gap, top.1 + uy / len * gap) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grab {
    Move,
    Corner(usize),
    Rotate,
}

pub fn grab_at(f: &Frame, p: (f64, f64), tol: f64) -> Option<Grab> {
    let d = |q: (f64, f64)| (q.0 - p.0).hypot(q.1 - p.1);
    if d(f.rotate) <= tol {
        return Some(Grab::Rotate);
    }
    if let Some(i) = (0..4).find(|&i| d(f.corners[i]) <= tol) {
        return Some(Grab::Corner(i));
    }
    f.contains(p).then_some(Grab::Move)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Guide {
    /// A vertical line at x (canvas px).
    V(f64),
    /// A horizontal line at y.
    H(f64),
}

/// The shift that snaps a moved box (`[x0, y0, x1, y1]`) — its edges or
/// centre — to the nearest line within `tol`: the canvas centre and edges,
/// the 90 % action-safe and 93 % title-safe lines, other boxes' edges and
/// centres; each axis on its own. With the guide lines hit.
pub fn snap_move(b: [f64; 4], canvas: (f64, f64), others: &[[f64; 4]], tol: f64) -> ((f64, f64), Vec<Guide>) {
    let lines = |len: f64, pick: fn(&[f64; 4]) -> [f64; 3]| {
        let mut v = vec![0.0, len / 2.0, len, len * 0.05, len * 0.95, len * 0.035, len * 0.965];
        for o in others {
            v.extend(pick(o));
        }
        v
    };
    let axis = |feats: [f64; 3], lines: Vec<f64>| -> Option<(f64, f64)> {
        let mut best: Option<(f64, f64)> = None;
        for f in feats {
            for &l in &lines {
                let d = l - f;
                if d.abs() <= tol && best.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    best = Some((d, l));
                }
            }
        }
        best
    };
    let xs = axis([b[0], (b[0] + b[2]) / 2.0, b[2]], lines(canvas.0, |o| [o[0], (o[0] + o[2]) / 2.0, o[2]]));
    let ys = axis([b[1], (b[1] + b[3]) / 2.0, b[3]], lines(canvas.1, |o| [o[1], (o[1] + o[3]) / 2.0, o[3]]));
    // Rounded to 1e-9 px: 1920 · 0.95 is 1824 on screen, whatever f64 makes of it.
    let round = |v: f64| (v * 1e9).round() / 1e9;
    let mut guides = vec![];
    if let Some((_, l)) = xs {
        guides.push(Guide::V(round(l)));
    }
    if let Some((_, l)) = ys {
        guides.push(Guide::H(round(l)));
    }
    ((round(xs.map_or(0.0, |s| s.0)), round(ys.map_or(0.0, |s| s.0))), guides)
}

/// Rotation lands on a multiple of 15° within 3°.
pub fn snap_angle(deg: f64) -> f64 {
    let k = (deg / 15.0).round() * 15.0;
    if (deg - k).abs() <= 3.0 { k } else { deg }
}

/// A drag's start: what was grabbed, where, the clip's values then.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragStart {
    pub grab: Grab,
    pub at: (f64, f64),
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub rotation: f64,
    pub frame: Frame,
}

/// The values a drag to `p` gives: `x`, `y` (moved, snapped unless `free`),
/// `scale` (about the centre, ≥ 0.01) or `rotation` (degrees, clockwise,
/// 15° steps unless `free`); with the guides to draw.
pub fn drag_to(s: &DragStart, p: (f64, f64), canvas: (f64, f64), others: &[[f64; 4]], tol: f64, free: bool) -> (Vec<(&'static str, f64)>, Vec<Guide>) {
    let c = s.frame.centre;
    match s.grab {
        Grab::Move => {
            let (mut dx, mut dy) = (p.0 - s.at.0, p.1 - s.at.1);
            let mut guides = vec![];
            if !free {
                let b = s.frame.bounds();
                let ((sx, sy), g) = snap_move([b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy], canvas, others, tol);
                (dx, dy, guides) = (dx + sx, dy + sy, g);
            }
            (vec![("x", s.x + dx), ("y", s.y + dy)], guides)
        }
        Grab::Corner(_) => {
            let r0 = (s.at.0 - c.0).hypot(s.at.1 - c.1).max(1e-6);
            let r1 = (p.0 - c.0).hypot(p.1 - c.1);
            (vec![("scale", (s.scale * r1 / r0).max(0.01))], vec![])
        }
        Grab::Rotate => {
            let a0 = (s.at.1 - c.1).atan2(s.at.0 - c.0);
            let a1 = (p.1 - c.1).atan2(p.0 - c.0);
            let deg = s.rotation + (a1 - a0).to_degrees();
            (vec![("rotation", if free { deg } else { snap_angle(deg) })], vec![])
        }
    }
}

/// The one command a drag records (titles spec §1, Review Focus 4): an
/// animated property gets its keyframe at the playhead (none when the
/// playhead is off the clip), the static ones one `SetClipProperty`; all in
/// one batch. `None` when nothing would change.
pub fn transform_command(c: &Clip, changes: &[(&str, f64)], playhead: Time, frame: Time, label: String) -> Option<EditCommand> {
    let mut cmds = vec![];
    let mut statics = c.clone();
    for &(prop, v) in changes {
        let k = crate::anim_ui::track_of(prop)?;
        if c.animation.is_animated(&k) {
            let tt = crate::anim_ui::edit_time(c, playhead, frame)?;
            let here = crate::anim_ui::key_here(c, &k, playhead, frame);
            if here.is_some_and(|key| key.value == v) {
                continue;
            }
            let at = here.map_or(SourceTime(c.source_time_at(tt)), |key| key.at);
            cmds.push(EditCommand::SetKeyframe { clip: c.id, param: k, at, value: v, ease: None });
        } else {
            crate::inspector::apply(&mut statics, prop, v, frame);
        }
    }
    if statics.transform != c.transform {
        cmds.push(EditCommand::SetClipProperty { clip: c.id, prop: ClipProperty::Transform(statics.transform) });
    }
    (!cmds.is_empty()).then_some(EditCommand::Batch { label, commands: cmds })
}
```

In `inspector.rs` make `fn apply` `pub(crate) fn apply`. (`key_here` is called as the inspector calls it, with `&k`.)

Run: `cargo test -p kadr-editor preview_edit`
Expected: PASS.

- [ ] **Step 3: The App side** — append to `preview_edit.rs`:

```rust
/// The preview editing state: a drag in progress and the clip being typed into.
#[derive(Default)]
pub struct PreviewEdit {
    drag: Option<(ClipId, DragStart, bool)>,
    pub typing: Option<ClipId>,
}

/// Handle and guide tolerances in view px.
const GRAB_PX: f64 = 8.0;
const SNAP_PX: f64 = 6.0;
const ROTATE_GAP_PX: f64 = 24.0;

impl App {
    /// The block a text clip's handles frame (`None` for media): `kadr_text::frame_size`.
    fn text_size(&self, id: ClipId) -> Option<(f32, f32)> {
        Some(kadr_text::frame_size(self.project.sequence().clip(id)?.text.as_deref()?))
    }

    fn scene_now(&self) -> FrameScene {
        let seq = self.project.sequence();
        let t = self.playhead.min((seq.duration() - seq.frame_rate.frame_duration()).max(Time::ZERO));
        kadr_timeline::scene::evaluate(&self.project, seq, t, &self.preview.output(&self.project))
    }

    /// Frames of the scene's clip layers, topmost first.
    fn frames(&self, scene: &FrameScene, px: f64) -> Vec<(ClipId, Frame, bool)> {
        let seq = self.project.sequence();
        scene
            .layers
            .iter()
            .rev()
            .filter_map(|l| {
                let c = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| kadr_scene::LayerId::from(c.id) == l.id)?;
                let is_text = matches!(l.content, LayerContent::Text(_));
                Some((c.id, frame_of(l, if is_text { self.text_size(c.id) } else { None }, ROTATE_GAP_PX * px), is_text))
            })
            .collect()
    }

    /// The selected clip's frame, handles and the guides on the preview.
    pub fn refresh_preview_frame(&mut self, scene: Option<&FrameScene>) {
        let ui = self.ui();
        let own;
        let scene = match scene {
            Some(s) => s,
            None => {
                own = self.scene_now();
                &own
            }
        };
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let sel = self.inspected();
        let f = sel.and_then(|id| crate::effects_ui::layer_of(&scene.layers, id).map(|l| (id, l))).map(|(id, l)| {
            let ts = if matches!(l.content, LayerContent::Text(_)) { self.text_size(id) } else { None };
            frame_of(l, ts, ROTATE_GAP_PX * cw / ui.get_preview_canvas_width().max(1.0) as f64)
        });
        let k = |x: f64, len: f64| (x / len * 1000.0) as f32;
        match f {
            Some(f) if self.preview_edit.typing.is_none() => {
                let path = format!("M{} {} L{} {} L{} {} L{} {} Z", k(f.corners[0].0, cw), k(f.corners[0].1, ch), k(f.corners[1].0, cw), k(f.corners[1].1, ch), k(f.corners[2].0, cw), k(f.corners[2].1, ch), k(f.corners[3].0, cw), k(f.corners[3].1, ch));
                ui.set_frame_path(path.into());
                let mut rows: Vec<crate::HandleView> = f.corners.iter().map(|c| crate::HandleView { x: (c.0 / cw) as f32, y: (c.1 / ch) as f32, rotate: false }).collect();
                rows.push(crate::HandleView { x: (f.rotate.0 / cw) as f32, y: (f.rotate.1 / ch) as f32, rotate: true });
                ui.set_handles(crate::util::reuse_rows(Some(ui.get_handles()), rows));
                ui.set_frame_shown(true);
            }
            _ => ui.set_frame_shown(false),
        }
    }

    /// A pointer event on the preview (canvas fractions; `view_w` = the
    /// canvas's width in view px). 0 down, 1 move, 2 up.
    pub fn preview_pointer(&mut self, kind: i32, fx: f32, fy: f32, shift: bool, view_w: f32) {
        let scene = self.scene_now();
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let px = cw / (view_w.max(1.0) as f64); // canvas px per view px
        let p = (fx as f64 * cw, fy as f64 * ch);
        if self.preview_edit.typing.is_some() {
            if kind == 0 {
                self.finish_inline_typing(); // a click outside the field
            }
            return;
        }
        match kind {
            0 => {
                let frames = self.frames(&scene, px);
                let sel = self.inspected();
                let hit = frames.iter().find(|(id, f, _)| Some(*id) == sel && grab_at(f, p, GRAB_PX * px).is_some()).or_else(|| frames.iter().find(|(_, f, _)| f.contains(p)));
                let Some(&(id, frame, _)) = hit else { return };
                let grab = if Some(id) == sel { grab_at(&frame, p, GRAB_PX * px).unwrap_or(Grab::Move) } else { Grab::Move };
                if Some(id) != sel {
                    self.tl.selection = vec![id];
                    self.refresh_timeline();
                    self.refresh_inspector();
                    self.refresh_status();
                }
                let Some(c) = self.project.sequence().clip(id) else { return };
                let frame_t = self.project.sequence().frame_rate.frame_duration();
                let at = crate::anim_ui::shown_time(c, self.playhead, frame_t);
                use kadr_project::AnimParam as P;
                let v = |q| c.anim_value(q, at);
                self.preview_edit.drag = Some((id, DragStart { grab, at: p, x: v(P::X), y: v(P::Y), scale: v(P::Scale), rotation: v(P::Rotation), frame }, false));
                self.request_frame();
            }
            1 => {
                let Some((id, start, _)) = self.preview_edit.drag else { return };
                let others: Vec<[f64; 4]> = self.frames(&scene, px).into_iter().filter(|(o, _, text)| *o != id && *text).map(|(_, f, _)| f.bounds()).collect();
                let (values, guides) = drag_to(&start, p, (cw, ch), &others, SNAP_PX * px, shift);
                if let Some(d) = self.preview_edit.drag.as_mut() {
                    d.2 = true;
                }
                self.preview_live(id, &values);
                let ui = self.ui();
                let rows: Vec<crate::GuideView> = guides.iter().map(|g| match g {
                    Guide::V(x) => crate::GuideView { vertical: true, at: (x / cw) as f32 },
                    Guide::H(y) => crate::GuideView { vertical: false, at: (y / ch) as f32 },
                }).collect();
                ui.set_guides(crate::util::reuse_rows(Some(ui.get_guides()), rows));
            }
            _ => {
                let ui = self.ui();
                ui.set_guides(Default::default());
                let Some((id, start, moved)) = self.preview_edit.drag.take() else { return };
                // The values last shown (snapped, with the Shift state of the last move).
                let last = std::mem::take(&mut self.preview_edit_last);
                let rolled_back = self.live_edit.roll_back(self.project.sequence_mut());
                if !moved || last.is_empty() {
                    if rolled_back {
                        self.show_rolled_back();
                    }
                    return; // a click: selection only
                }
                let label = t(match start.grab {
                    Grab::Move => "cmd.preview_move",
                    Grab::Corner(_) => "cmd.preview_scale",
                    Grab::Rotate => "cmd.preview_rotate",
                });
                let frame_t = self.project.sequence().frame_rate.frame_duration();
                let cmd = self.project.sequence().clip(id).and_then(|c| transform_command(c, &last, self.playhead, frame_t, label));
                match cmd {
                    Some(cmd) => {
                        if !self.execute(cmd) {
                            self.show_rolled_back();
                        }
                    }
                    None => self.show_rolled_back(),
                }
            }
        }
    }
}
```

The release records exactly the values last shown: add `pub preview_edit_last: Vec<(&'static str, f64)>` to `App` (default empty); `preview_live` sets it. `preview_live` and the rest:


```rust
impl App {
    /// The drag's values, live (no history): like an inspector slider.
    fn preview_live(&mut self, id: ClipId, values: &[(&'static str, f64)]) {
        let Some((ti, ci)) = self.project.sequence().locate_clip(id) else { return };
        if self.project.sequence().tracks[ti].locked {
            return;
        }
        self.commit_text_session();
        self.live_edit.arm(self.project.sequence_mut(), id);
        let frame = self.project.sequence().frame_rate.frame_duration();
        let playhead = self.playhead;
        let c = &mut self.project.sequence_mut().tracks[ti].clips[ci];
        for &(prop, v) in values {
            match crate::anim_ui::track_of(prop) {
                Some(k) if c.animation.is_animated(&k) => {
                    let Some(tt) = crate::anim_ui::edit_time(c, playhead, frame) else { continue };
                    let at = crate::anim_ui::key_here(c, &k, playhead, frame).map_or(SourceTime(c.source_time_at(tt)), |key| key.at);
                    c.animation.set_key(&k, at, v, None);
                }
                _ => crate::inspector::apply(c, prop, v, frame),
            }
        }
        self.preview_edit_last = values.to_vec();
        self.preview.invalidate();
        self.refresh_inspector();
        self.request_frame();
    }

    /// A double-click: on a text clip, type into it on the frame.
    pub fn preview_double(&mut self, fx: f32, fy: f32, view_w: f32) {
        let scene = self.scene_now();
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let p = (fx as f64 * cw, fy as f64 * ch);
        let px = cw / (view_w.max(1.0) as f64);
        if let Some((id, _, _)) = self.frames(&scene, px).into_iter().find(|(_, f, text)| *text && f.contains(p)) {
            self.begin_inline_typing(id);
        }
    }

    /// Opens the field over the text block (its axis-aligned bounds), the text selected.
    pub fn begin_inline_typing(&mut self, id: ClipId) {
        let scene = self.scene_now();
        let (cw, ch) = (scene.canvas.w as f64, scene.canvas.h as f64);
        let Some(l) = crate::effects_ui::layer_of(&scene.layers, id) else { return };
        let Some(c) = self.project.sequence().clip(id) else { return };
        let Some(doc) = c.text.as_deref() else { return };
        let b = frame_of(l, self.text_size(id), 0.0).bounds();
        let ui = self.ui();
        ui.set_type_x((b[0] / cw) as f32);
        ui.set_type_y((b[1] / ch) as f32);
        ui.set_type_w(((b[2] - b[0]) / cw) as f32);
        ui.set_type_font((doc.style.size as f64 * l.placement.scale.x.abs() as f64 / ch).clamp(0.0, 0.2) as f32);
        ui.set_type_text(doc.plain().into());
        self.preview_edit.typing = Some(id);
        ui.set_typing(true);
        ui.set_frame_shown(false);
    }

    pub fn inline_typed(&mut self, text: &str) {
        let Some(id) = self.preview_edit.typing else { return };
        if self.text_session.type_text(&mut self.project, id, text) {
            self.preview.invalidate();
            self.refresh_timeline();
            self.request_frame();
        }
    }

    /// Esc, Ctrl+Enter, a click outside, focus elsewhere: one entry for the typing (none without changes).
    pub fn finish_inline_typing(&mut self) {
        if self.preview_edit.typing.take().is_none() {
            return;
        }
        let ui = self.ui();
        ui.set_typing(false);
        self.commit_text_session();
        self.refresh_inspector();
        self.request_frame();
        ui.invoke_focus_editor();
    }
}
```

`text_add.rs`: `text_added` becomes `pub fn text_added(&mut self, id: ClipId) { self.begin_inline_typing(id); }` (after `request_frame` has run in `execute`'s `after_edit`). `keys.rs::on_key`: at the very start of the `Key::Escape` branch: `if self.preview_edit.typing.is_some() { self.finish_inline_typing(); return true; }`; in the `if ctrl {` branch before the table: `if self.preview_edit.typing.is_some() && (is(text, Key::Return) || text == "\r") { self.finish_inline_typing(); return true; }`. `preview.rs::request_frame`: after `self.fx_update_marker_in(Some(&scene));` add `self.refresh_preview_frame(Some(&scene));`, and the clip name: `.map(|c| c.text.as_deref().map_or_else(|| c.name.clone(), |d| d.label()))`.

- [ ] **Step 4: The overlay's Slint** — in `preview.slint`:

```slint
/// A handle of the selected clip's frame: canvas fractions.
export struct HandleView {
    x: float,
    y: float,
    rotate: bool,
}

/// A snapping guide: a canvas fraction across the whole canvas.
export struct GuideView {
    vertical: bool,
    at: float,
}
```

`Preview` gets:

```slint
    // Editing on the preview (titles spec §1, §6.2).
    in property <bool> frame-shown;
    in property <string> frame-path;          // "M x y L … Z" in a 1000 × 1000 canvas box
    in property <[HandleView]> handles;
    in property <[GuideView]> guides;
    in property <bool> typing;
    in property <float> type-x;
    in property <float> type-y;
    in property <float> type-w;
    in property <float> type-font;            // the text's size, a fraction of the canvas height
    in-out property <string> type-text;
    out property <length> canvas-width: canvas.width;
    callback edit-pointer(int, float, float, bool, float);
    callback edit-double(float, float, float);
    callback type-edited(string);
    callback type-done();
    changed typing => {
        if (root.typing) {
            tin.focus();
            tin.select-all();
        }
    }
```

inside `canvas := Rectangle { … }`, after the point marker:

```slint
                if root.frame-shown && !root.playing: Path {
                    width: parent.width;
                    height: parent.height;
                    viewbox-width: 1000;
                    viewbox-height: 1000;
                    fit: fill;
                    commands: root.frame-path;
                    stroke: #ffffff;
                    stroke-width: 1px;
                    fill: transparent;
                }
                for h in root.handles: Rectangle {
                    visible: root.frame-shown && !root.playing;
                    x: parent.width * h.x - self.width / 2;
                    y: parent.height * h.y - self.height / 2;
                    width: h.rotate ? 10px : 8px;
                    height: h.rotate ? 10px : 8px;
                    border-radius: h.rotate ? 5px : 1px;
                    background: #ffffff;
                    border-width: 1px;
                    border-color: #00000090;
                }
                for g in root.guides: Rectangle {
                    visible: !root.playing;
                    x: g.vertical ? parent.width * g.at : 0;
                    y: g.vertical ? 0 : parent.height * g.at;
                    width: g.vertical ? 1px : parent.width;
                    height: g.vertical ? parent.height : 1px;
                    background: #ff4fd8;
                }
```

after the pick-mode `TouchArea` (still inside `viewport`):

```slint
            if !root.picking && !root.playing && root.has-frame: TouchArea {
                mouse-cursor: root.frame-shown ? move : default;
                pointer-event(e) => {
                    if (e.button == PointerEventButton.left && e.kind == PointerEventKind.down) { root.edit-pointer(0, (self.mouse-x - canvas.x) / canvas.width, (self.mouse-y - canvas.y) / canvas.height, e.modifiers.shift, canvas.width / 1px); }
                    if (e.kind == PointerEventKind.move && self.pressed) { root.edit-pointer(1, (self.mouse-x - canvas.x) / canvas.width, (self.mouse-y - canvas.y) / canvas.height, e.modifiers.shift, canvas.width / 1px); }
                    if (e.button == PointerEventButton.left && e.kind == PointerEventKind.up) { root.edit-pointer(2, (self.mouse-x - canvas.x) / canvas.width, (self.mouse-y - canvas.y) / canvas.height, e.modifiers.shift, canvas.width / 1px); }
                }
                double-clicked => { root.edit-double((self.mouse-x - canvas.x) / canvas.width, (self.mouse-y - canvas.y) / canvas.height, canvas.width / 1px); }
            }
            // Typing on the frame: the field over the text block; the real text redraws beneath it.
            Rectangle {
                visible: root.typing;
                x: canvas.x + root.type-x * canvas.width;
                y: canvas.y + root.type-y * canvas.height;
                width: max(80px, root.type-w * canvas.width);
                height: max(30px, tin.preferred-height + 10px);
                border-radius: 4px;
                background: #000000a8;
                border-width: 1px;
                border-color: Theme.accent;
                tin := TextInput {
                    x: 5px;
                    y: 5px;
                    width: parent.width - 10px;
                    text <=> root.type-text;
                    single-line: false;
                    wrap: word-wrap;
                    horizontal-alignment: center;
                    color: #ffffff;
                    font-size: clamp(root.type-font * canvas.height, 13px, 64px);
                    selection-background-color: Theme.accent.with-alpha(0.5);
                    edited => { root.type-edited(self.text); }
                    changed has-focus => { if (!self.has-focus && root.typing) { root.type-done(); } }
                }
            }
```

`app.slint`: export `HandleView`, `GuideView` (import them from `"preview.slint"` with `AngleView, DevRow`); `AppWindow` gets `in property <bool> frame-shown; in property <string> frame-path; in property <[HandleView]> handles; in property <[GuideView]> guides; in property <bool> typing; in property <float> type-x; in property <float> type-y; in property <float> type-w; in property <float> type-font; in-out property <string> type-text; out property <length> preview-canvas-width;` and callbacks `edit-pointer(int, float, float, bool, float)`, `edit-double(float, float, float)`, `type-edited(string)`, `type-done()`; on the `Preview { … }` element:

```slint
                    frame-shown: root.frame-shown;
                    frame-path: root.frame-path;
                    handles: root.handles;
                    guides: root.guides;
                    typing: root.typing;
                    type-x: root.type-x;
                    type-y: root.type-y;
                    type-w: root.type-w;
                    type-font: root.type-font;
                    type-text <=> root.type-text;
                    edit-pointer(k, x, y, s, w) => { root.preview-canvas-width = self.canvas-width; root.edit-pointer(k, x, y, s, w); }
                    edit-double(x, y, w) => { root.edit-double(x, y, w); }
                    type-edited(t) => { root.type-edited(t); }
                    type-done => { root.type-done(); }
```

(`preview-canvas-width` lets `refresh_preview_frame` size the rotation gap; it is a plain `in-out property <length> preview-canvas-width;` — declare it `in-out` on `AppWindow`, and read it in Rust as `ui.get_preview_canvas_width()` in px.) In `app.rs::wire_callbacks`:

```rust
    cb!(ui.on_edit_pointer, |k, x, y, s, w| |app: &mut App| app.preview_pointer(k, x, y, s, w));
    cb!(ui.on_edit_double, |x, y, w| |app: &mut App| app.preview_double(x, y, w));
    cb!(ui.on_type_edited, |t| |app: &mut App| app.inline_typed(&t));
    cb!(ui.on_type_done, | | |app: &mut App| app.finish_inline_typing());
```

`App` gets `pub preview_edit: crate::preview_edit::PreviewEdit,` and `pub preview_edit_last: Vec<(&'static str, f64)>,` (defaults). Catalogs (RU / EN): `cmd.preview_move` «Перемещение» / "Move"; `cmd.preview_scale` «Масштаб» / "Scale"; `cmd.preview_rotate` «Поворот» / "Rotate".

- [ ] **Step 5: Build and run the tests**

Run: `cargo build -p kadr-editor && cargo test -p kadr-editor && cargo test -p kadr-i18n`
Expected: PASS (geometry tests, every earlier test; the new `cmd.preview_*` keys are found by the catalog scan through `t("cmd.…")` — the scan reads `"cmd.` literals).

- [ ] **Step 6: Clippy and commit**

Run: `cargo clippy -p kadr-editor --all-targets`
Expected: no new warnings.

```bash
git add apps/editor crates/i18n/locales
git commit -m "titles: edit on the preview — frame, handles, snapping guides, inline typing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 9: `kadr-text` looks — gradient, shadow, glow, arc and circle, perspective

**Files:**
- Create: `crates/text/src/effects.rs`
- Modify: `crates/text/src/lib.rs` (`pub(crate) mod effects;`), `crates/text/src/raster.rs` (`render` passes 2–3, gradient paint, `Canvas::over_mask_at`, `bounds` room for shadow and glow), `crates/text/src/shape.rs` (`place`: arcs and circles), `crates/text/src/frame.rs` (flat vs final bounds, perspective; `frame_size` for curved and tilted text), `crates/text/tests/golden.rs` (new goldens), `crates/text/tests/golden/*.png`

**Interfaces:**
- Consumes: Task 3 `raster::{Canvas, Mask, Final, finals, glyph_mask, Place, premul, fill_of, outline_reach, plaque_rects}`, `layout::Laid`, `shape::Placed`.
- Produces: `effects::{blur(&mut Mask, f32), Paint::{of(&Fill, [f32; 4]) -> Paint, at(usize, usize) -> [f32; 4]}, homography(Perspective, d: f32) -> [[f32; 3]; 3], project(&[[f32; 3]; 3], x, y) -> Option<(f32, f32)>, project_bounds(Bounds, Perspective, d) -> Bounds, warp(&Canvas, flat: Bounds, out: Bounds, Perspective, d, s) -> Canvas, camera_distance(&Laid) -> f32}`; `shape::place` handles `TextShape::{Arc, Circle}`; `TextFrame` keeps `flat: Bounds` (before perspective) and `bounds` (after); `frame_size` covers curved and tilted text. No public signature of Task 3 changes.

- [ ] **Step 1: Write the failing shape and effect tests** — `crates/text/src/shape.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{TextDoc, TextShape};

    fn doc(shape: TextShape) -> TextDoc {
        let mut d = TextDoc::new("радуга текста");
        d.style.size = 60.0;
        d.shape = shape;
        d
    }

    fn visible(d: &TextDoc) -> Vec<(Placed, f32)> {
        let l = crate::layout(d);
        place(d, &l).into_iter().zip(l.glyphs.iter()).filter(|(_, g)| g.visible).map(|(p, g)| (p, g.advance)).collect()
    }

    #[test]
    fn a_rainbow_turns_its_ends_down_and_a_smile_up() {
        let flat = visible(&doc(TextShape::Straight));
        assert!(flat.iter().all(|(p, _)| p.rotation == 0.0));
        let up = visible(&doc(TextShape::Arc { bend: 0.5 }));
        let (first, mid, last) = (up[0].0, up[up.len() / 2].0, up[up.len() - 1].0);
        assert!(first.rotation < -0.3 && last.rotation > 0.3, "the ends lean out: {} {}", first.rotation, last.rotation);
        assert!(mid.rotation.abs() < 0.15);
        assert!(first.y > mid.y + 10.0 && last.y > mid.y + 10.0, "a rainbow's ends drop");
        let down = visible(&doc(TextShape::Arc { bend: -0.5 }));
        assert!(down[0].0.y < down[down.len() / 2].0.y - 10.0 && down[0].0.rotation > 0.3, "a smile's ends rise and lean in");
        let almost = visible(&doc(TextShape::Arc { bend: 0.0 }));
        assert!(almost.iter().zip(&flat).all(|(a, b)| (a.0.x - b.0.x).abs() < 1e-3 && (a.0.y - b.0.y).abs() < 1e-3), "bend 0 is straight");
    }

    #[test]
    fn a_circle_puts_the_glyphs_on_its_radius_clockwise_from_its_start() {
        let d = doc(TextShape::Circle { radius: 200.0, start_deg: 0.0 });
        let l = crate::layout(&d);
        let (cx, cy) = (l.width / 2.0, l.height / 2.0);
        let g = visible(&d);
        for (p, adv) in &g {
            // The baseline's middle lies on the circle.
            let (mx, my) = (p.x + adv / 2.0 * p.rotation.cos(), p.y + adv / 2.0 * p.rotation.sin());
            assert!(((mx - cx).hypot(my - cy) - 200.0).abs() < 1.0, "{mx} {my}");
        }
        assert!(g[0].0.rotation >= 0.0 && g[0].0.rotation < 0.2, "starts at the top");
        assert!(g.windows(2).all(|w| w[1].0.rotation > w[0].0.rotation), "runs clockwise");
    }
}
```

`crates/text/src/effects.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_project::{Fill, GradientFill, GradientStop, Perspective, Rgba8};

    #[test]
    fn blur_spreads_and_keeps_the_mass() {
        let mut m = Mask::new(41, 41);
        m.a[20 * 41 + 20] = 1.0;
        blur(&mut m, 4.0);
        let total: f32 = m.a.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "{total}");
        assert!(m.a[20 * 41 + 20] < 0.1 && m.a[20 * 41 + 26] > 0.0, "spread over the radius");
        let mut none = Mask::new(5, 5);
        none.a[12] = 1.0;
        blur(&mut none, 0.2);
        assert_eq!(none.a[12], 1.0, "under half a pixel: untouched");
    }

    #[test]
    fn a_gradient_runs_across_the_box_at_its_angle() {
        let red = Rgba8::rgba(255, 0, 0, 255);
        let blue = Rgba8::rgba(0, 0, 255, 255);
        let fill = Fill { colour: red, gradient: Some(GradientFill { stops: vec![GradientStop { pos: 0.0, colour: red }, GradientStop { pos: 1.0, colour: blue }], angle: 0.0 }), extra: Default::default() };
        let p = Paint::of(&fill, [0.0, 0.0, 100.0, 10.0]);
        assert!((p.at(0, 5)[0] - 0.995).abs() < 1e-4, "{:?}", p.at(0, 5));
        assert!((p.at(49, 5)[2] - 0.5).abs() < 0.02 && (p.at(99, 5)[2] - 0.995).abs() < 1e-3);
        let down = Fill { gradient: Some(GradientFill { angle: 90.0, ..fill.gradient.clone().unwrap() }), ..fill.clone() };
        assert!(Paint::of(&down, [0.0, 0.0, 100.0, 10.0]).at(50, 9)[2] > 0.9, "90°: top to bottom");
        assert_eq!(Paint::of(&Fill::solid(red), [0.0; 4]).at(3, 3), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_tilt_recedes_the_named_side() {
        let none = homography(Perspective { tilt_x: 0.0, tilt_y: 0.0 }, 1000.0);
        assert_eq!(project(&none, 100.0, -50.0), Some((100.0, -50.0)));
        let top_back = homography(Perspective { tilt_x: 30.0, tilt_y: 0.0 }, 1000.0);
        let (top, bottom) = (project(&top_back, 100.0, -100.0).unwrap(), project(&top_back, 100.0, 100.0).unwrap());
        assert!(top.0 < bottom.0, "the top edge is narrower: {top:?} {bottom:?}");
        let right_back = homography(Perspective { tilt_x: 0.0, tilt_y: 30.0 }, 1000.0);
        assert!(project(&right_back, 100.0, -100.0).unwrap().1 > -100.0, "the right side shrinks towards the middle");
        let b = project_bounds(Bounds { x0: -100.0, y0: -50.0, x1: 100.0, y1: 50.0 }, Perspective { tilt_x: 30.0, tilt_y: 0.0 }, 1000.0);
        assert!(b.width() > 200.0 && b.height() < 100.0, "the near edge widens, the height shrinks: {b:?}");
    }
}
```

Run: `cargo test -p kadr-text`
Expected: FAIL to compile — module `effects` missing, `place` straight only (and then the arc tests fail).

- [ ] **Step 2: Implement the effects** — `crates/text/src/effects.rs` (and `pub(crate) mod effects;` in `lib.rs`):

```rust
//! Raster effects of text (titles spec §5.1): a Gaussian-like blur (three
//! box blurs) for shadows, glows and blur-in; the fill paint (solid or a
//! linear gradient across the text block); the perspective tilt (a
//! homography of the finished flat picture).

use crate::frame::Bounds;
use crate::raster::{premul, Canvas, Mask};
use kadr_project::{Fill, Perspective};

/// Three box blurs of radius `b` ≈ a Gaussian of σ = `r` raster px
/// (`(2b + 1)² = 4r² + 1`); edges read as transparent; under half a pixel
/// nothing happens.
pub(crate) fn blur(m: &mut Mask, r: f32) {
    if !r.is_finite() || r <= 0.5 {
        return;
    }
    let b = (((4.0 * r * r + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as usize;
    let mut tmp = vec![0.0f32; m.w.max(m.h)];
    for _ in 0..3 {
        for y in 0..m.h {
            box_line(&mut m.a[y * m.w..(y + 1) * m.w], b, &mut tmp);
        }
        for x in 0..m.w {
            let mut col: Vec<f32> = (0..m.h).map(|y| m.a[y * m.w + x]).collect();
            box_line(&mut col, b, &mut tmp);
            for (y, v) in col.into_iter().enumerate() {
                m.a[y * m.w + x] = v;
            }
        }
    }
}

/// One box blur of width `2b + 1` along a line (zero outside it).
fn box_line(line: &mut [f32], b: usize, tmp: &mut [f32]) {
    let n = line.len();
    let k = 1.0 / (2 * b + 1) as f32;
    let mut sum: f32 = line.iter().take(b + 1).sum();
    for i in 0..n {
        tmp[i] = sum * k;
        if i + b + 1 < n {
            sum += line[i + b + 1];
        }
        if i >= b {
            sum -= line[i - b];
        }
    }
    line.copy_from_slice(&tmp[..n]);
}

/// The paint of a fill at a raster pixel: its colour, or its gradient
/// across the text block `b` (raster px `[x0, y0, x1, y1]`) at its angle
/// (0° left → right, 90° top → bottom), stops mixed premultiplied.
pub(crate) enum Paint {
    Solid([f32; 4]),
    Gradient { stops: Vec<(f32, [f32; 4])>, cx: f32, cy: f32, ux: f32, uy: f32, len: f32 },
}

impl Paint {
    pub fn of(fill: &Fill, b: [f32; 4]) -> Paint {
        match &fill.gradient {
            Some(g) if !g.stops.is_empty() => {
                let a = if g.angle.is_finite() { g.angle.to_radians() } else { 0.0 };
                let (ux, uy) = (a.cos(), a.sin());
                let len = ((b[2] - b[0]) * ux).abs() + ((b[3] - b[1]) * uy).abs();
                let mut stops: Vec<(f32, [f32; 4])> = g.stops.iter().filter(|s| s.pos.is_finite()).map(|s| (s.pos.clamp(0.0, 1.0), premul(s.colour, 1.0))).collect();
                stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                Paint::Gradient { stops, cx: (b[0] + b[2]) / 2.0, cy: (b[1] + b[3]) / 2.0, ux, uy, len: len.max(1e-3) }
            }
            _ => Paint::Solid(premul(fill.colour, 1.0)),
        }
    }

    pub fn at(&self, x: usize, y: usize) -> [f32; 4] {
        match self {
            Paint::Solid(c) => *c,
            Paint::Gradient { stops, cx, cy, ux, uy, len } => {
                let t = (0.5 + ((x as f32 + 0.5 - cx) * ux + (y as f32 + 0.5 - cy) * uy) / len).clamp(0.0, 1.0);
                let first = stops[0];
                let last = stops[stops.len() - 1];
                if t <= first.0 {
                    return first.1;
                }
                if t >= last.0 {
                    return last.1;
                }
                let i = stops.iter().rposition(|s| s.0 <= t).unwrap_or(0);
                let (a, b) = (stops[i], stops[(i + 1).min(stops.len() - 1)]);
                let f = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 1.0 };
                [0, 1, 2, 3].map(|c| a.1[c] + (b.1[c] - a.1[c]) * f)
            }
        }
    }
}

/// The camera distance of a tilt: 2.5 × the text block's larger side.
pub(crate) fn camera_distance(laid: &crate::layout::Laid) -> f32 {
    2.5 * laid.width.max(laid.height).max(1.0)
}

/// The tilt as a homography of centred canvas px: rotate the flat text
/// about its vertical axis by `tilt_y` (the right side recedes) and its
/// horizontal axis by `tilt_x` (the top recedes), each clamped to ±60°,
/// then project from distance `d`: `[X·w, Y·w, w] = M · [x, y, 1]`.
pub(crate) fn homography(p: Perspective, d: f32) -> [[f32; 3]; 3] {
    let c = |deg: f32| if deg.is_finite() { deg.clamp(-60.0, 60.0).to_radians() } else { 0.0 };
    let (tx, ty) = (c(p.tilt_x), c(p.tilt_y));
    [[ty.cos(), 0.0, 0.0], [0.0, tx.cos(), 0.0], [ty.sin() / d, -tx.sin() / d, 1.0]]
}

pub(crate) fn project(m: &[[f32; 3]; 3], x: f32, y: f32) -> Option<(f32, f32)> {
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    (w > 0.05).then(|| ((m[0][0] * x + m[0][1] * y + m[0][2]) / w, (m[1][0] * x + m[1][1] * y + m[1][2]) / w))
}

fn invert(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let k = 1.0 / det;
    Some([
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ])
}

/// The tilted picture's bounds: the flat bounds' corners projected.
pub(crate) fn project_bounds(flat: Bounds, p: Perspective, d: f32) -> Bounds {
    let m = homography(p, d);
    let mut out: Option<Bounds> = None;
    for (x, y) in [(flat.x0, flat.y0), (flat.x1, flat.y0), (flat.x1, flat.y1), (flat.x0, flat.y1)] {
        if let Some((px, py)) = project(&m, x, y) {
            let pt = Bounds { x0: px, y0: py, x1: px, y1: py };
            out = Some(out.map_or(pt, |b| b.union(pt)));
        }
    }
    out.unwrap_or(flat).grow(1.0)
}

/// The flat raster `src` (bounds `flat` at `s` raster px per canvas px)
/// seen tilted, as a raster of bounds `out`: each output pixel centre maps
/// back through the inverse homography, sampled bilinearly (premultiplied).
pub(crate) fn warp(src: &Canvas, flat: Bounds, out: Bounds, p: Perspective, d: f32, s: f32) -> Canvas {
    let (w, h) = (((out.width() * s).ceil() as usize).max(1), ((out.height() * s).ceil() as usize).max(1));
    let mut dst = Canvas::new(w, h);
    let Some(inv) = invert(&homography(p, d)) else { return dst };
    let texel = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || y < 0 || x >= src.w as i64 || y >= src.h as i64 { [0.0; 4] } else { src.px[y as usize * src.w + x as usize] }
    };
    for y in 0..h {
        for x in 0..w {
            let (cx, cy) = (out.x0 + (x as f32 + 0.5) / s, out.y0 + (y as f32 + 0.5) / s);
            let Some((fx, fy)) = project(&inv, cx, cy) else { continue };
            let (u, v) = ((fx - flat.x0) * s - 0.5, (fy - flat.y0) * s - 0.5);
            let (i, j) = (u.floor() as i64, v.floor() as i64);
            let (a, b) = (u - i as f32, v - j as f32);
            let (p00, p10, p01, p11) = (texel(i, j), texel(i + 1, j), texel(i, j + 1), texel(i + 1, j + 1));
            dst.px[y * w + x] = [0, 1, 2, 3].map(|c| p00[c] * (1.0 - a) * (1.0 - b) + p10[c] * a * (1.0 - b) + p01[c] * (1.0 - a) * b + p11[c] * a * b);
        }
    }
    dst
}
```

(`project` uses `w > 0.05`: a corner behind the camera has no image; the clamp to ±60° and `d = 2.5 ×` the block keep every corner in front. The inverse of `M` is a homography too, so `project(&inv, …)` maps back.)

- [ ] **Step 3: Arcs and circles** — `crates/text/src/shape.rs::place`:

```rust
pub fn place(doc: &TextDoc, laid: &Laid) -> Vec<Placed> {
    let straight = || laid.glyphs.iter().map(|g| Placed { x: g.x, y: g.y, rotation: 0.0 }).collect();
    let (w, h) = (laid.width, laid.height);
    match doc.shape {
        TextShape::Straight => straight(),
        TextShape::Arc { bend } => {
            let bend = if bend.is_finite() { bend.clamp(-1.0, 1.0) } else { 0.0 };
            let angle = bend * std::f32::consts::PI;
            if angle.abs() < 1e-3 {
                return straight();
            }
            // The block's width spans `angle` on a circle of radius R; its
            // centre lies R below the block centre (a rainbow, s = 1) or above
            // it (a smile, s = −1). Each glyph keeps its arc length from the
            // middle on the circle through its own baseline.
            let s = angle.signum();
            let r = w / angle.abs();
            let (cx, cy) = (w / 2.0, h / 2.0 + s * r);
            laid.glyphs
                .iter()
                .map(|g| {
                    let rr = (s * (cy - g.y)).max(1.0);
                    let alpha = (g.x + g.advance / 2.0 - cx) / rr;
                    let (mx, my) = (cx + rr * alpha.sin(), cy - s * rr * alpha.cos());
                    let rot = s * alpha;
                    Placed { x: mx - g.advance / 2.0 * rot.cos(), y: my - g.advance / 2.0 * rot.sin(), rotation: rot }
                })
                .collect()
        }
        TextShape::Circle { radius, start_deg } => {
            // Clockwise round the outside of a circle about the block centre,
            // from `start_deg` (0 = top); each further line one line inwards.
            let radius = if radius.is_finite() { radius.clamp(10.0, 20000.0) } else { 200.0 };
            let start = if start_deg.is_finite() { start_deg.to_radians() } else { 0.0 };
            let (cx, cy) = (w / 2.0, h / 2.0);
            let first_baseline = laid.lines.first().map_or(0.0, |l| l.baseline);
            laid.glyphs
                .iter()
                .map(|g| {
                    let line = &laid.lines[g.line.min(laid.lines.len().saturating_sub(1))];
                    let rr = (radius - (g.y - first_baseline)).max(1.0);
                    let phi = start + (g.x + g.advance / 2.0 - line.left) / rr;
                    let (mx, my) = (cx + rr * phi.sin(), cy - rr * phi.cos());
                    Placed { x: mx - g.advance / 2.0 * phi.cos(), y: my - g.advance / 2.0 * phi.sin(), rotation: phi }
                })
                .collect()
        }
    }
}
```

(Import `kadr_project::TextShape`. The pen origin is the baseline's middle moved back half an advance along the glyph's own baseline direction `(cos rot, sin rot)` — the circle test checks exactly that.)

- [ ] **Step 4: Shadow, glow, gradient, perspective in the raster** — in `crates/text/src/raster.rs`:
  - `Canvas` gets:
    ```rust
    /// A mask composited shifted by whole pixels `(dx, dy)`.
    pub fn over_mask_at(&mut self, m: &Mask, dx: i32, dy: i32, c: [f32; 4]) {
        for y in 0..self.h as i32 {
            let sy = y - dy;
            if sy < 0 || sy >= m.h as i32 {
                continue;
            }
            for x in 0..self.w as i32 {
                let sx = x - dx;
                if sx < 0 || sx >= m.w as i32 {
                    continue;
                }
                let cov = m.a[sy as usize * m.w + sx as usize];
                if cov > 0.0 {
                    self.over(y as usize * self.w + x as usize, c, cov);
                }
            }
        }
    }
    ```
  - split `render` into `compose(doc, laid, placed, anim, flat: Bounds, s) -> Canvas` (the old body up to the fill pass) and `render(doc, laid, placed, anim, flat: Bounds, bounds: Bounds, scale, pool) -> CpuFrame`:
    ```rust
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(doc: &TextDoc, laid: &Laid, placed: &[Placed], anim: &FrameAnim, flat: Bounds, bounds: Bounds, scale: f32, pool: &FramePool) -> CpuFrame {
        let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let canvas = compose(doc, laid, placed, anim, flat, s);
        let canvas = match doc.perspective.filter(|p| p.tilt_x != 0.0 || p.tilt_y != 0.0) {
            Some(p) => crate::effects::warp(&canvas, flat, bounds, p, crate::effects::camera_distance(laid), s),
            None => canvas,
        };
        canvas.into_frame(pool)
    }
    ```
  - in `compose`, between the plaque and the outlines:
    ```rust
        // 2. The shadow: the silhouette with its outlines, blurred, shifted.
        let reach = outline_reach(doc);
        if let Some(sh) = doc.style.shadow.filter(|x| x.colour.0[3] > 0) {
            let mut m = glyph_mask(laid, &finals, &at, w, h, 2.0 * reach);
            crate::effects::blur(&mut m, sh.blur.max(0.0) * s);
            let off = |v: f32| if v.is_finite() { (v * s).round() as i32 } else { 0 };
            canvas.over_mask_at(&m, off(sh.offset_x), off(sh.offset_y), premul(sh.colour, 1.0));
        }
        // 3. The glow (neon): the silhouette blurred by its radius, times its intensity.
        if let Some(gl) = doc.style.glow.filter(|x| x.colour.0[3] > 0 && x.radius > 0.0) {
            let mut m = glyph_mask(laid, &finals, &at, w, h, 2.0 * reach);
            crate::effects::blur(&mut m, gl.radius.min(500.0) * s);
            let k = if gl.intensity.is_finite() { gl.intensity.clamp(0.0, 2.0) } else { 1.0 };
            m.a.iter_mut().for_each(|a| *a = (*a * k).min(1.0));
            canvas.over_mask(&m, premul(gl.colour, 1.0));
        }
    ```
    and the outline loop starts from that `reach` (delete its own `let mut reach = outline_reach(doc);`, use `let mut reach = reach;`);
  - the fill pass paints through `Paint` across the layout box (raster px):
    ```rust
        let block = [(0.0 - at.ox) * s, (0.0 - at.oy) * s, (laid.width - at.ox) * s, (laid.height - at.oy) * s];
        for (g, f) in laid.glyphs.iter().zip(&finals).filter(|(g, f)| g.visible && f.opacity > 0.0) {
            if let Some((img, x0, y0)) = at.image(laid, g, f, 0.0) {
                let paint = match f.colour {
                    Some(c) => crate::effects::Paint::Solid(premul(c, 1.0)),
                    None => crate::effects::Paint::of(fill_of(doc, g.run), block),
                };
                paint_glyph(&mut canvas, &img, x0, y0, |x, y| paint.at(x, y), f.opacity);
            }
        }
    ```
  - `bounds` makes room for the shadow and the glow, before converting to centred coordinates:
    ```rust
        let mut b = b.grow(margin);
        if let Some(sh) = doc.style.shadow {
            let blur = if sh.blur.is_finite() { sh.blur.max(0.0) } else { 0.0 };
            let (ox, oy) = (if sh.offset_x.is_finite() { sh.offset_x } else { 0.0 }, if sh.offset_y.is_finite() { sh.offset_y } else { 0.0 });
            b = b.union(Bounds { x0: b.x0 + ox, y0: b.y0 + oy, x1: b.x1 + ox, y1: b.y1 + oy }.grow(3.0 * blur + 1.0));
        }
        if let Some(gl) = doc.style.glow {
            b = b.grow(3.0 * gl.radius.clamp(0.0, 500.0) + 1.0);
        }
    ```
- `crates/text/src/frame.rs`: `TextFrame` gets a `flat: Bounds` field; in `new`, `let flat = crate::raster::bounds(doc, &laid, &finals); let bounds = match doc.perspective.filter(|p| p.tilt_x != 0.0 || p.tilt_y != 0.0) { Some(p) => crate::effects::project_bounds(flat, p, crate::effects::camera_distance(&laid)), None => flat };`; `render` passes `self.flat, self.bounds`. `frame_size` becomes:
    ```rust
    pub fn frame_size(doc: &TextDoc) -> (f32, f32) {
        let laid = crate::layout::layout(doc);
        let (bx, by) = (laid.width / 2.0, laid.height / 2.0);
        let (mut hw, mut hh) = (bx, by);
        if doc.shape != kadr_project::TextShape::Straight {
            let placed = crate::shape::place(doc, &laid);
            let finals = crate::raster::finals(&laid, &placed, &crate::anim::FrameAnim::rest(laid.glyphs.len()));
            for (g, f) in laid.glyphs.iter().zip(&finals).filter(|(g, _)| g.visible) {
                for (x, y) in [(0.0, -g.size), (g.advance, -g.size), (g.advance, 0.3 * g.size), (0.0, 0.3 * g.size)] {
                    let (s, c) = f.rotation.sin_cos();
                    let (px, py) = (f.x + x * c - y * s, f.y + x * s + y * c);
                    (hw, hh) = (hw.max((px - bx).abs()), hh.max((py - by).abs()));
                }
            }
        }
        if let Some(p) = doc.perspective.filter(|p| p.tilt_x != 0.0 || p.tilt_y != 0.0) {
            let m = crate::effects::homography(p, crate::effects::camera_distance(&laid));
            let (mut w2, mut h2) = (0.0f32, 0.0f32);
            for (x, y) in [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)] {
                if let Some((px, py)) = crate::effects::project(&m, x, y) {
                    (w2, h2) = (w2.max(px.abs()), h2.max(py.abs()));
                }
            }
            (hw, hh) = (w2, h2);
        }
        (2.0 * hw, 2.0 * hh)
    }
    ```

Run: `cargo test -p kadr-text --lib`
Expected: PASS (Task 3's `bounds_hold_everything_drawn` included — its starter docs now draw their shadow, inside the grown bounds).

- [ ] **Step 5: Goldens** — append to `crates/text/tests/golden.rs`:

```rust
#[test]
fn look_goldens() {
    let mut grad = base("ГРАДИЕНТ");
    grad.style.font = FontRef { family: "Unbounded".into(), weight: 800, italic: false };
    grad.style.fill = Fill { colour: Rgba8::WHITE, gradient: Some(GradientFill { stops: vec![GradientStop { pos: 0.0, colour: Rgba8::rgba(255, 140, 0, 255) }, GradientStop { pos: 1.0, colour: Rgba8::rgba(255, 0, 128, 255) }], angle: 0.0 }), extra: Default::default() };
    check("gradient_fill", &grad, Clock::REST, 1.0);
    let mut soft = base("Мягкая тень");
    soft.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 200), offset_x: 4.0, offset_y: 6.0, blur: 8.0 });
    check("shadow_soft", &soft, Clock::REST, 1.0);
    let mut hard = base("ЖЁСТКАЯ");
    hard.style.font = FontRef { family: "Russo One".into(), weight: 400, italic: false };
    hard.style.outlines = vec![Outline { width: 3.0, colour: Rgba8::BLACK }];
    hard.style.shadow = Some(Shadow { colour: Rgba8::rgba(255, 0, 80, 255), offset_x: 6.0, offset_y: 6.0, blur: 0.0 });
    check("shadow_hard_outline", &hard, Clock::REST, 1.0);
    let mut neon = base("НЕОН");
    neon.style.font = FontRef { family: "Comfortaa".into(), weight: 700, italic: false };
    neon.style.fill = Fill::solid(Rgba8::rgba(255, 230, 255, 255));
    neon.style.glow = Some(Glow { colour: Rgba8::rgba(255, 40, 200, 255), radius: 10.0, intensity: 1.4 });
    check("glow_neon", &neon, Clock::REST, 1.0);
    let mut rainbow = base("Текст по дуге вверх");
    rainbow.shape = TextShape::Arc { bend: 0.6 };
    check("arc_rainbow", &rainbow, Clock::REST, 1.0);
    let mut smile = rainbow.clone();
    smile.shape = TextShape::Arc { bend: -0.6 };
    check("arc_smile", &smile, Clock::REST, 1.0);
    let mut circle = base("ПО КРУГУ • ПО КРУГУ • ПО КРУГУ •");
    circle.style.size = 28.0;
    circle.shape = TextShape::Circle { radius: 110.0, start_deg: 0.0 };
    check("circle", &circle, Clock::REST, 1.0);
    let mut tilt = base("Перспектива");
    tilt.perspective = Some(Perspective { tilt_x: 35.0, tilt_y: 0.0 });
    check("tilt_x", &tilt, Clock::REST, 1.0);
    tilt.perspective = Some(Perspective { tilt_x: 0.0, tilt_y: -30.0 });
    check("tilt_y", &tilt, Clock::REST, 1.0);
    let mut emoji = base("Гол! ⚽🔥");
    emoji.style.outlines = vec![Outline { width: 3.0, colour: Rgba8::BLACK }];
    emoji.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 160), offset_x: 3.0, offset_y: 4.0, blur: 4.0 });
    check("emoji_styled", &emoji, Clock::REST, 1.0);
}
```

and in `crates/text/src/frame.rs` tests extend `bounds_hold_everything_drawn`'s list with a glow (radius 12), a hard shadow (offset 20, 30), an arc (bend 0.8), a circle and a tilt (both axes 40°) — each with nothing on the raster's edge.

Run: `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-text --test golden look_goldens && cargo test -p kadr-text`
Expected: 10 new PNGs, then PASS. Look at every one: the gradient orange → pink left to right inside the letters only; the soft shadow down-right, blurred, the text still crisp; the hard magenta shadow offset with no blur, behind a black outline; the pink neon halo round light letters; the rainbow's ends dropping, the smile's rising, letters upright to the curve; the circle closing on itself, starting at the top, clockwise; `tilt_x` a trapezoid narrowing upwards, `tilt_y` narrowing to the left; the emoji in colour with the outline and the shadow. Task 3's goldens must still pass untouched (their `base` has no shadow).

- [ ] **Step 6: Clippy and commit**

Run: `cargo clippy -p kadr-text --all-targets && cargo test -p kadr-timeline --test text_eval`
Expected: no warnings; PASS.

```bash
git add crates/text
git commit -m "titles: gradient, shadow, glow, arc and circle, perspective in kadr-text

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 10: Text animation — reveals, loops, highlight; built-in styles (`kadr-text`, `kadr-timeline` tests)

**Files:**
- Modify: `crates/text/src/anim.rs` (presets and `animate`), `crates/text/src/raster.rs` (per-glyph blur, typewriter cursor, blur room in `bounds`), `crates/text/src/lib.rs` (`pub mod styles;`, re-exports)
- Create: `crates/text/src/styles.rs`, `crates/text/styles.json`, `crates/text/tests/anim.rs`
- Modify: `crates/text/tests/golden.rs` (animation strips, the styles sheet), `crates/timeline/tests/text_eval.rs` (split / trim of animated titles)

**Interfaces:**
- Consumes: Task 3 `Clock`, `GlyphAnim`, `FrameAnim`, `Laid` (`letter`, `word`, `line`, `letters`, `words`); `kadr_core::ease::Curve`; `kadr_core::hash::{pcg32_hash, fmix64}`; Task 4 `text_clock`.
- Produces:
  - `kadr_text::anim::{REVEALS: &[RevealDef], LOOPS: &[LoopDef], EASES: &[&str], reveal(&str) -> Option<&'static RevealDef>, looping(&str) -> Option<&'static LoopDef>, curve(&str) -> Option<Curve>}`; `RevealDef { id, unit, duration_ms, stagger_ms, ease }` with `look(u, size, rnd) -> GlyphAnim` and `fresh(&self) -> kadr_project::Reveal`; `LoopDef { id, unit, speed, amount, block }` with `fresh(&self) -> kadr_project::LoopAnim`; `animate` real.
  - `kadr_text::styles::{StyleDef { id, category, sample, style, paragraph, shape, perspective, anim, at }, builtin() -> &'static [StyleDef], by_id(&str) -> Option<&'static StyleDef>, CATEGORIES: &[&str]}`; `StyleDef::{name_key() -> String, apply(&self, &mut TextDoc, canvas_h: u32), new_doc(&self, text: &str, canvas_h: u32) -> TextDoc, position(&self, w: u32, h: u32) -> (f64, f64), from_doc(id: &str, doc: &TextDoc, canvas_h: u32) -> StyleDef}`.
  - i18n key families (Task 11 / 12 use them): `text.anim.<id>` for every reveal and loop id, `text.anim.highlight`, `text.ease.<key>`, `text.unit.<letter|word|line|block>`, `text.style.<id>`, `text.cat.<category>`, `text.sample.<key>`.

- [ ] **Step 1: Write the failing animation tests** — `crates/text/tests/anim.rs`:

```rust
//! The animation engine (titles spec §3–4): reveals per unit with stagger
//! and curve, shrunk to fit the clip; the out mirrors the in from the end;
//! loops in source time; the highlighted word; deterministic keys.

use kadr_core::Time;
use kadr_project::*;
use kadr_text::anim::*;
use kadr_text::{layout, Clock};

fn ms(x: i64) -> Time {
    Time::from_millis(x)
}

fn clock(since: i64, until: i64) -> Clock {
    Clock { since_in: ms(since), until_out: ms(until), source: ms(since), highlight: 0.0 }
}

fn reveal(preset: &str, unit: Unit, d: i64, st: i64, ease: &str) -> Reveal {
    Reveal { preset: preset.into(), unit, duration: ms(d), stagger: ms(st), ease: ease.into(), extra: Default::default() }
}

fn three_words() -> TextDoc {
    let mut d = TextDoc::new("раз два три");
    d.style.size = 40.0;
    d
}

/// The opacity of the first visible glyph of each word.
fn word_opacities(d: &TextDoc, c: Clock) -> Vec<f32> {
    let l = layout(d);
    let a = animate(d, &l, c);
    // Rounded to 1e-4: a stagger of 0.05 s × 2 is 0.1 s only up to f64 dust.
    (0..3).map(|w| (l.glyphs.iter().zip(&a.glyphs).find(|(g, _)| g.visible && g.word == w).unwrap().1.opacity * 1e4).round() / 1e4).collect()
}

#[test]
fn reveals_run_per_unit_with_stagger_and_rest_between() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    assert_eq!(word_opacities(&d, clock(150, 5_000)), [0.375, 0.125, 0.0]);
    assert_eq!(word_opacities(&d, clock(0, 5_000)), [0.0, 0.0, 0.0], "hidden at the clip's first frame");
    assert!(animate(&d, &layout(&d), clock(2_000, 3_000)).is_rest(), "after the reveal: at rest");
}

#[test]
fn the_out_mirrors_the_in_from_the_end() {
    let mut d = three_words();
    d.anim.reveal_out = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    // Out total 0.6 s: word 0 leaves first, from 0.6 s before the end.
    assert_eq!(word_opacities(&d, clock(5_000, 450)), [0.625, 0.875, 1.0]);
    assert_eq!(word_opacities(&d, clock(5_000, 0)), [0.0, 0.0, 0.0]);
}

#[test]
fn reveals_that_do_not_fit_the_clip_shrink_together() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    d.anim.reveal_out = Some(reveal("fade", Unit::Word, 400, 100, "linear"));
    // In and out take 1.2 s; the clip is 0.6 s: both at half speed (0.2 s per word, 0.05 s apart).
    assert_eq!(word_opacities(&d, clock(100, 500)), [0.5, 0.25, 0.0]);
    assert_eq!(word_opacities(&d, clock(300, 300)), [1.0, 1.0, 1.0], "the in just ended, the out not begun");
}

#[test]
fn typewriter_reveals_letters_and_shows_a_cursor_while_typing() {
    let mut d = TextDoc::new("абв");
    d.anim.reveal_in = Some(reveal("typewriter", Unit::Letter, 1, 50, "linear"));
    let l = layout(&d);
    let a = animate(&d, &l, clock(60, 5_000));
    let shown: Vec<f32> = a.glyphs.iter().map(|g| g.opacity).collect();
    assert_eq!(shown, [1.0, 1.0, 0.0]);
    assert_eq!(a.cursor, Some(1));
    assert_eq!(animate(&d, &l, clock(500, 5_000)).cursor, None, "typed out: no cursor");
}

#[test]
fn unknown_presets_are_kept_and_do_nothing() {
    let mut d = three_words();
    d.anim.reveal_in = Some(reveal("teleport", Unit::Word, 400, 100, "linear"));
    d.anim.looping = Some(LoopAnim { preset: "spin3d".into(), unit: Unit::Block, speed: 1.0, amount: 1.0, extra: Default::default() });
    assert!(animate(&d, &layout(&d), clock(0, 5_000)).is_rest());
}

#[test]
fn loops_run_in_source_time() {
    let mut d = three_words();
    d.anim.looping = Some(LoopAnim { preset: "wave".into(), unit: Unit::Letter, speed: 1.0, amount: 1.0, extra: Default::default() });
    let l = layout(&d);
    let at = |since: i64, source: i64| animate(&d, &l, Clock { since_in: ms(since), until_out: ms(9_000), source: ms(source), highlight: 0.0 });
    assert_eq!(at(1_000, 2_250), at(4_000, 2_250), "only the source time moves a loop");
    assert_ne!(at(1_000, 2_250), at(1_000, 2_500));
    let pulse = TextDoc { anim: TextAnim { looping: Some(LoopAnim { preset: "pulse".into(), unit: Unit::Block, speed: 1.0, amount: 1.0, extra: Default::default() }), ..Default::default() }, ..three_words() };
    let a = animate(&pulse, &layout(&pulse), Clock { source: ms(250), ..Clock::REST });
    assert!((a.block.scale - 1.06).abs() < 1e-4, "a quarter cycle: the top of the pulse");
}

#[test]
fn the_highlight_walks_the_words() {
    let mut d = three_words();
    d.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.1, span: (0.0, 1.0), extra: Default::default() });
    let l = layout(&d);
    let lit = |p: f64| -> Vec<usize> {
        let a = animate(&d, &l, Clock { highlight: p, ..Clock::REST });
        l.glyphs.iter().zip(&a.glyphs).filter(|(g, x)| g.visible && x.colour.is_some()).map(|(g, _)| g.word).collect::<std::collections::BTreeSet<_>>().into_iter().collect()
    };
    assert_eq!((lit(0.1), lit(0.5), lit(0.99), lit(1.0)), (vec![0], vec![1], vec![2], vec![2]));
}

#[test]
fn every_preset_has_a_unique_id_and_rests_at_one() {
    let mut ids: Vec<&str> = REVEALS.iter().map(|r| r.id).chain(LOOPS.iter().map(|l| l.id)).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!((ids.len(), REVEALS.len(), LOOPS.len()), (n, 11, 6));
    for r in REVEALS {
        assert_eq!(r.look(1.0, 50.0, [0.3, -0.7, 0.9]), kadr_text::GlyphAnim::REST, "{} at rest", r.id);
        assert!(r.look(0.0, 50.0, [0.3, -0.7, 0.9]).opacity < 0.01 || r.id == "pop", "{} hidden at 0", r.id);
    }
    assert!(EASES.iter().all(|e| curve(e).is_some()));
}
```

Run: `cargo test -p kadr-text --test anim`
Expected: FAIL to compile — `cannot find value REVEALS`, `LOOPS`, `EASES`, `curve`.

- [ ] **Step 2: Implement the presets and `animate`** — in `crates/text/src/anim.rs` replace the placeholder `animate` with:

```rust
use crate::layout::Glyph;
use kadr_core::ease::Curve;
use kadr_core::hash::{fmix64, pcg32_hash};
use kadr_project::{LoopAnim, Reveal, Unit};
use std::f64::consts::TAU;

/// The reveal curves' keys (spec §4) and their `kadr_core::ease` curves.
pub const EASES: &[&str] = &["smooth", "linear", "ease_in", "ease_out", "cinematic", "snap", "gentle"];

pub fn curve(key: &str) -> Option<Curve> {
    Some(match key {
        "smooth" => Curve::EaseInOut,
        "linear" => Curve::Linear,
        "ease_in" => Curve::EaseIn,
        "ease_out" => Curve::EaseOut,
        "cinematic" => Curve::Cinematic,
        "snap" => Curve::Snap,
        "gentle" => Curve::Gentle,
        _ => return None,
    })
}

/// A unit's look at reveal progress `u` (0 hidden … 1 at rest) for a glyph
/// of `size` px; `rnd`: three seeded numbers in −1..1 for this unit.
type RevealLook = fn(f32, f32, [f32; 3]) -> GlyphAnim;

pub struct RevealDef {
    pub id: &'static str,
    pub unit: Unit,
    pub duration_ms: i64,
    pub stagger_ms: i64,
    pub ease: &'static str,
    look: RevealLook,
}

impl RevealDef {
    pub fn look(&self, u: f32, size: f32, rnd: [f32; 3]) -> GlyphAnim {
        (self.look)(u.clamp(0.0, 1.0), size, rnd)
    }

    /// This preset with its defaults, for a document.
    pub fn fresh(&self) -> Reveal {
        Reveal { preset: self.id.into(), unit: self.unit, duration: Time::from_millis(self.duration_ms), stagger: Time::from_millis(self.stagger_ms), ease: self.ease.into(), extra: Default::default() }
    }
}

/// Back-out (overshoot 1.70158): 0 → 1 passing about 1.1.
fn back_out(u: f32) -> f32 {
    let (c1, v) = (1.70158f32, u - 1.0);
    1.0 + (c1 + 1.0) * v * v * v + c1 * v * v
}

const R: GlyphAnim = GlyphAnim::REST;

pub const REVEALS: &[RevealDef] = &[
    RevealDef { id: "typewriter", unit: Unit::Letter, duration_ms: 1, stagger_ms: 50, ease: "linear", look: |u, _, _| GlyphAnim { opacity: if u > 0.0 { 1.0 } else { 0.0 }, ..R } },
    RevealDef { id: "fade", unit: Unit::Word, duration_ms: 400, stagger_ms: 80, ease: "smooth", look: |u, _, _| GlyphAnim { opacity: u, ..R } },
    RevealDef { id: "rise", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "snap", look: |u, s, _| GlyphAnim { dy: (1.0 - u) * 0.5 * s, opacity: u, ..R } },
    RevealDef { id: "drop", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "snap", look: |u, s, _| GlyphAnim { dy: -(1.0 - u) * 0.5 * s, opacity: u, ..R } },
    RevealDef { id: "slide_left", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "cinematic", look: |u, s, _| GlyphAnim { dx: (1.0 - u) * s, opacity: u, ..R } },
    RevealDef { id: "slide_right", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "cinematic", look: |u, s, _| GlyphAnim { dx: -(1.0 - u) * s, opacity: u, ..R } },
    RevealDef { id: "pop", unit: Unit::Letter, duration_ms: 350, stagger_ms: 30, ease: "linear", look: |u, _, _| GlyphAnim { scale: back_out(u).max(0.0), opacity: (3.0 * u).min(1.0), ..R } },
    RevealDef { id: "wave_in", unit: Unit::Letter, duration_ms: 450, stagger_ms: 40, ease: "linear", look: |u, s, _| GlyphAnim { dy: (1.0 - back_out(u)) * 0.6 * s, opacity: (2.0 * u).min(1.0), ..R } },
    RevealDef { id: "scatter", unit: Unit::Letter, duration_ms: 600, stagger_ms: 20, ease: "smooth", look: |u, s, r| GlyphAnim { dx: (1.0 - u) * r[0] * 2.0 * s, dy: (1.0 - u) * r[1] * 2.0 * s, rotation: (1.0 - u) * r[2] * 0.7, opacity: u, ..R } },
    RevealDef { id: "zoom", unit: Unit::Word, duration_ms: 450, stagger_ms: 80, ease: "cinematic", look: |u, _, _| GlyphAnim { scale: 2.0 - u, opacity: u, ..R } },
    RevealDef { id: "blur_in", unit: Unit::Word, duration_ms: 500, stagger_ms: 80, ease: "smooth", look: |u, s, _| GlyphAnim { blur: (1.0 - u) * 0.3 * s, opacity: u, ..R } },
];

/// A loop's look at `phase` (cycles of source time × speed) for unit `i`,
/// `amount` 0..1, glyph `size`, the document's `seed`.
type LoopLook = fn(f64, usize, f32, f32, u64) -> GlyphAnim;

pub struct LoopDef {
    pub id: &'static str,
    pub unit: Unit,
    pub speed: f64,
    pub amount: f64,
    /// Moves the whole block (`FrameAnim::block`) rather than each unit.
    pub block: bool,
    look: LoopLook,
}

impl LoopDef {
    pub fn look(&self, phase: f64, i: usize, amount: f32, size: f32, seed: u64) -> GlyphAnim {
        (self.look)(phase, i, amount.clamp(0.0, 1.0), size, seed)
    }

    pub fn fresh(&self) -> LoopAnim {
        LoopAnim { preset: self.id.into(), unit: self.unit, speed: self.speed, amount: self.amount, extra: Default::default() }
    }
}

/// Smooth value noise in −1..1 over `t`, one stream per (seed, unit).
fn noise(seed: u64, i: usize, t: f64) -> f32 {
    let s = seed ^ fmix64(i as u64 + 1);
    let h = |k: f64| pcg32_hash(s, k as i64 as u64) as f64 / u32::MAX as f64 * 2.0 - 1.0;
    let (k, f) = (t.floor(), t - t.floor());
    let w = f * f * (3.0 - 2.0 * f);
    (h(k) + (h(k + 1.0) - h(k)) * w) as f32
}

fn sin(x: f64) -> f32 {
    x.sin() as f32
}

pub const LOOPS: &[LoopDef] = &[
    LoopDef { id: "pulse", unit: Unit::Block, speed: 1.0, amount: 0.6, block: true, look: |p, _, a, _, _| GlyphAnim { scale: 1.0 + 0.06 * a * sin(TAU * p), ..R } },
    LoopDef { id: "sway", unit: Unit::Word, speed: 0.5, amount: 0.6, block: false, look: |p, i, a, _, _| GlyphAnim { rotation: (4.0f32).to_radians() * a * sin(TAU * (p - 0.15 * i as f64)), ..R } },
    LoopDef { id: "shake", unit: Unit::Letter, speed: 1.0, amount: 0.5, block: false, look: |p, i, a, s, seed| GlyphAnim { dx: 0.04 * a * s * noise(seed, 2 * i, p * 12.0), dy: 0.04 * a * s * noise(seed, 2 * i + 1, p * 12.0), ..R } },
    LoopDef { id: "flicker", unit: Unit::Letter, speed: 1.0, amount: 0.6, block: false, look: |p, i, a, _, seed| GlyphAnim { opacity: 1.0 - 0.5 * a * noise(seed, i, p * 8.0).max(0.0), ..R } },
    LoopDef { id: "wave", unit: Unit::Letter, speed: 1.0, amount: 0.6, block: false, look: |p, i, a, s, _| GlyphAnim { dy: -0.15 * a * s * sin(TAU * (p - 0.1 * i as f64)), ..R } },
    LoopDef { id: "float", unit: Unit::Block, speed: 0.5, amount: 0.6, block: true, look: |p, _, a, s, _| GlyphAnim { dy: 0.05 * a * s * sin(TAU * p), ..R } },
];

pub fn reveal(id: &str) -> Option<&'static RevealDef> {
    REVEALS.iter().find(|r| r.id == id)
}

pub fn looping(id: &str) -> Option<&'static LoopDef> {
    LOOPS.iter().find(|l| l.id == id)
}

/// Both states at once: offsets add, scales multiply, turns add,
/// opacities multiply, the larger blur, the later colour.
fn combine(a: GlyphAnim, b: GlyphAnim) -> GlyphAnim {
    GlyphAnim { dx: a.dx + b.dx, dy: a.dy + b.dy, scale: a.scale * b.scale, rotation: a.rotation + b.rotation, opacity: a.opacity * b.opacity, blur: a.blur.max(b.blur), colour: b.colour.or(a.colour) }
}

fn unit_index(g: &Glyph, u: Unit) -> usize {
    match u {
        Unit::Letter => g.letter,
        Unit::Word => g.word,
        Unit::Line => g.line,
        Unit::Block => 0,
    }
}

fn units(laid: &Laid, u: Unit) -> usize {
    match u {
        Unit::Letter => laid.letters,
        Unit::Word => laid.words,
        Unit::Line => laid.lines.len(),
        Unit::Block => 1,
    }
    .max(1)
}

/// Progress of `t` seconds into a span of `d` seconds (a zero span is a step at 0).
fn progress(t: f64, d: f64) -> f32 {
    if d <= 1e-9 { if t >= 0.0 { 1.0 } else { 0.0 } } else { (t / d).clamp(0.0, 1.0) as f32 }
}

/// Three seeded numbers in −1..1 for unit `i`.
fn rnds(seed: u64, i: usize) -> [f32; 3] {
    [0u64, 1, 2].map(|k| pcg32_hash(seed ^ fmix64(k + 17), i as u64) as f32 / u32::MAX as f32 * 2.0 - 1.0)
}

/// The animated state of every glyph at `clock` (titles spec §3–4).
pub fn animate(doc: &TextDoc, laid: &Laid, clock: Clock) -> FrameAnim {
    let mut out = FrameAnim::rest(laid.glyphs.len());
    let secs = |t: Time| t.as_secs_f64();
    let (since, until) = (secs(clock.since_in).max(0.0), secs(clock.until_out).max(0.0));
    let len = since + until;
    // A reveal this build runs: (its settings, its preset, duration, stagger, units).
    fn plan<'a>(r: &'a Option<Reveal>, laid: &Laid) -> Option<(&'a Reveal, &'static RevealDef, f64, f64, usize)> {
        let r = r.as_ref()?;
        Some((r, reveal(&r.preset)?, r.duration.as_secs_f64().max(0.0), r.stagger.as_secs_f64().max(0.0), units(laid, r.unit)))
    }
    let (pin, pout) = (plan(&doc.anim.reveal_in, laid), plan(&doc.anim.reveal_out, laid));
    let total = |p: &Option<(&Reveal, &RevealDef, f64, f64, usize)>| p.map_or(0.0, |(_, _, d, st, n)| d + st * (n - 1) as f64);
    let (tin, tout) = (total(&pin), total(&pout));
    // Reveals longer than the clip shrink together (titles spec §3).
    let k = if tin + tout > len && len > 0.0 { len / (tin + tout) } else { 1.0 };
    let eased = |r: &Reveal, def: &RevealDef, u: f32| curve(&r.ease).or_else(|| curve(def.ease)).unwrap_or(Curve::Linear).apply(u as f64) as f32;
    let lp = doc.anim.looping.as_ref().and_then(|l| Some((l, looping(&l.preset)?)));
    let phase = secs(clock.source) * lp.map_or(1.0, |(l, _)| if l.speed.is_finite() { l.speed.clamp(0.0, 20.0) } else { 1.0 });
    let words = laid.words.max(1);
    let lit = doc.anim.highlight.as_ref().map(|h| (h, ((clock.highlight.clamp(0.0, 1.0) * words as f64).floor() as usize).min(words - 1)));
    for (gi, g) in laid.glyphs.iter().enumerate() {
        let mut a = GlyphAnim::REST;
        if let Some((r, def, d, st, _)) = pin {
            let i = unit_index(g, r.unit);
            a = combine(a, def.look(eased(r, def, progress(since - i as f64 * st * k, d * k)), g.size, rnds(doc.seed, i)));
        }
        if let Some((r, def, d, st, _)) = pout {
            let i = unit_index(g, r.unit);
            let w = progress(tout * k - until - i as f64 * st * k, d * k);
            a = combine(a, def.look(eased(r, def, 1.0 - w), g.size, rnds(doc.seed ^ 0x5eed, i)));
        }
        if let Some((l, def)) = lp.filter(|(_, def)| !def.block) {
            a = combine(a, def.look(phase, unit_index(g, l.unit), l.amount as f32, g.size, doc.seed));
        }
        if let Some((h, word)) = lit
            && g.visible
            && g.word == word
        {
            a = combine(a, GlyphAnim { scale: if h.scale.is_finite() { h.scale.clamp(0.2, 5.0) } else { 1.0 }, colour: Some(h.colour), ..GlyphAnim::REST });
        }
        out.glyphs[gi] = a;
    }
    if let Some((l, def)) = lp.filter(|(_, def)| def.block) {
        out.block = def.look(phase, 0, l.amount as f32, doc.style.size, doc.seed);
    }
    let fade_in = if tin > 0.0 { progress(since, tin * k) } else { 1.0 };
    let fade_out = if tout > 0.0 { 1.0 - progress(tout * k - until, tout * k) } else { 1.0 };
    out.plaque_opacity = fade_in * fade_out;
    // The typewriter's cursor: after the last letter shown, while typing.
    if let Some((r, def, ..)) = pin
        && def.id == "typewriter"
    {
        let shown: Vec<usize> = laid.glyphs.iter().enumerate().filter(|(i, g)| g.visible && out.glyphs[*i].opacity > 0.0).map(|(i, _)| i).collect();
        let _ = r;
        if !shown.is_empty() && shown.len() < laid.letters {
            out.cursor = shown.last().copied();
        }
    }
    out
}
```

(`let _ = r;` keeps the pattern readable; drop the binding instead if clippy objects.)

- [ ] **Step 3: Blur and the cursor in the raster** — in `crates/text/src/raster.rs`:
  - `bounds`: the margin also covers blurred glyphs: `let blur = finals.iter().map(|f| f.blur).fold(0.0f32, f32::max); let margin = outline_reach(doc) + 0.15 * laid.max_size + 3.0 * blur + 2.0;`
  - the fill pass draws a blurred glyph through a small mask:
    ```rust
            if let Some((img, x0, y0)) = at.image(laid, g, f, 0.0) {
                let paint = match f.colour {
                    Some(c) => crate::effects::Paint::Solid(premul(c, 1.0)),
                    None => crate::effects::Paint::of(fill_of(doc, g.run), block),
                };
                let r = f.blur * s;
                if r > 0.5 {
                    // Blur-in: the glyph's coverage blurred in a mask of its own, then painted.
                    let pad = (3.0 * r).ceil() as i32;
                    let (mw, mh) = (img.placement.width as i32 + 2 * pad, img.placement.height as i32 + 2 * pad);
                    let mut m = Mask::new(mw as usize, mh as usize);
                    m.union(&img, pad, pad, 1.0);
                    crate::effects::blur(&mut m, r);
                    for (i, &cov) in m.a.iter().enumerate() {
                        let (x, y) = (x0 - pad + i as i32 % mw, y0 - pad + i as i32 / mw);
                        if cov > 0.0 && x >= 0 && y >= 0 && (x as usize) < canvas.w && (y as usize) < canvas.h {
                            canvas.over(y as usize * canvas.w + x as usize, paint.at(x as usize, y as usize), cov * f.opacity);
                        }
                    }
                } else {
                    paint_glyph(&mut canvas, &img, x0, y0, |x, y| paint.at(x, y), f.opacity);
                }
            }
    ```
  - after the fill pass, the typewriter cursor — a bar after the glyph, 0.08 em wide, from 0.8 em above the baseline to 0.15 em below, in the base colour:
    ```rust
        if let Some(gi) = anim.cursor
            && let (Some(g), Some(f)) = (laid.glyphs.get(gi), finals.get(gi))
        {
            let (px, py) = ((f.x + g.advance * f.scale - at.ox) * s, (f.y - at.oy) * s);
            let em = g.size * f.scale * s;
            let mut m = Mask::new(w, h);
            m.round_rect(px + 0.04 * em, py - 0.8 * em, px + 0.12 * em, py + 0.15 * em, 0.0);
            canvas.over_mask(&m, premul(doc.style.fill.colour, 1.0));
        }
    ```

Run: `cargo test -p kadr-text`
Expected: PASS (the new `anim` tests and everything before).

- [ ] **Step 4: Write the failing style tests** — `crates/text/src/styles.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_style_loads_with_a_known_category_font_and_presets() {
        let all = builtin();
        assert_eq!(all.len(), 22);
        let mut ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 22, "unique ids");
        for s in all {
            assert!(CATEGORIES.contains(&s.category.as_str()), "{}: {}", s.id, s.category);
            assert!(crate::BUNDLED_FAMILIES.contains(&s.style.font.family.as_str()), "{}: a bundled font", s.id);
            for r in [&s.anim.reveal_in, &s.anim.reveal_out].into_iter().flatten() {
                assert!(crate::anim::reveal(&r.preset).is_some(), "{}: {}", s.id, r.preset);
            }
            if let Some(l) = &s.anim.looping {
                assert!(crate::anim::looping(&l.preset).is_some(), "{}: {}", s.id, l.preset);
            }
            assert!(s.sample.starts_with("text.sample."));
        }
        assert!(by_id("caption").is_some_and(|c| c.category == "captions" && c.at.1 > 0.3), "add_caption's style sits low");
    }

    #[test]
    fn applying_a_style_keeps_the_text_and_scales_to_the_frame() {
        let mut d = TextDoc::new("Победа");
        d.style_range(0..4, &kadr_project::RunStyle { size: Some(30.0), ..Default::default() });
        let neon = by_id("neon_pink").unwrap();
        neon.apply(&mut d, 2160);
        assert_eq!((d.plain(), d.runs[0].style.size), ("Победа".to_string(), Some(30.0)), "the text and its own styles stay");
        assert_eq!(d.style.size, neon.style.size * 2.0, "1080p sizes doubled for 2160 lines");
        assert_eq!(d.style.glow.unwrap().radius, neon.style.glow.unwrap().radius * 2.0);
        assert_eq!(d.anim, neon.anim);
        let back = StyleDef::from_doc("mine-1", &d, 2160);
        assert_eq!((back.style.size, back.category.as_str()), (neon.style.size, "mine"), "a saved style is stored at 1080p");
        assert_eq!(neon.position(1920, 1080), (neon.at.0 as f64 * 1920.0, neon.at.1 as f64 * 1080.0));
    }
}
```

Run: `cargo test -p kadr-text styles`
Expected: FAIL to compile — `cannot find function builtin`.

- [ ] **Step 5: Implement the styles** — top of `crates/text/src/styles.rs` (and `pub mod styles;` in `lib.rs`):

```rust
//! The built-in text styles (titles spec §1, §6.5): data in
//! `crates/text/styles.json`, sizes for a 1080-line frame (refinement 17).
//! Applying a style replaces a document's look — base style, paragraph,
//! shape, perspective, animation — and keeps its text and its runs' own
//! styles. A user's saved style is the same data (category "mine").

use kadr_project::{ParagraphStyle, Perspective, TextAnim, TextDoc, TextShape, TextStyle};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub const CATEGORIES: &[&str] = &["titles", "lower_thirds", "plaques", "neon", "captions", "other"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StyleDef {
    pub id: String,
    pub category: String,
    /// The i18n key of the gallery's sample text.
    #[serde(default = "default_sample")]
    pub sample: String,
    #[serde(default)]
    pub style: TextStyle,
    #[serde(default)]
    pub paragraph: ParagraphStyle,
    #[serde(default)]
    pub shape: TextShape,
    #[serde(default)]
    pub perspective: Option<Perspective>,
    #[serde(default)]
    pub anim: TextAnim,
    /// A new clip's block centre: fractions of the frame from its centre.
    #[serde(default)]
    pub at: (f32, f32),
}

fn default_sample() -> String {
    "text.sample.other".into()
}

static BUILTIN: LazyLock<Vec<StyleDef>> = LazyLock::new(|| serde_json::from_str(include_str!("../styles.json")).expect("crates/text/styles.json"));

pub fn builtin() -> &'static [StyleDef] {
    &BUILTIN
}

pub fn by_id(id: &str) -> Option<&'static StyleDef> {
    BUILTIN.iter().find(|s| s.id == id)
}

/// Every length of a style times `k`.
fn scale_style(s: &TextStyle, k: f32) -> TextStyle {
    let mut s = s.clone();
    s.size *= k;
    for o in &mut s.outlines {
        o.width *= k;
    }
    if let Some(x) = s.shadow.as_mut() {
        (x.offset_x, x.offset_y, x.blur) = (x.offset_x * k, x.offset_y * k, x.blur * k);
    }
    if let Some(x) = s.glow.as_mut() {
        x.radius *= k;
    }
    if let Some(x) = s.plaque.as_mut() {
        (x.radius, x.padding_x, x.padding_y) = (x.radius * k, x.padding_x * k, x.padding_y * k);
    }
    s
}

fn scale_paragraph(p: &ParagraphStyle, k: f32) -> ParagraphStyle {
    let mut p = p.clone();
    p.letter_spacing *= k;
    p.wrap = p.wrap.map(|w| w * k);
    p
}

fn scale_shape(s: TextShape, k: f32) -> TextShape {
    match s {
        TextShape::Circle { radius, start_deg } => TextShape::Circle { radius: radius * k, start_deg },
        other => other,
    }
}

impl StyleDef {
    pub fn name_key(&self) -> String {
        format!("text.style.{}", self.id)
    }

    /// This style's look on `doc`, sized for a `canvas_h`-line frame.
    pub fn apply(&self, doc: &mut TextDoc, canvas_h: u32) {
        let k = canvas_h.max(1) as f32 / 1080.0;
        doc.style = scale_style(&self.style, k);
        doc.paragraph = scale_paragraph(&self.paragraph, k);
        doc.shape = scale_shape(self.shape, k);
        doc.perspective = self.perspective;
        doc.anim = self.anim.clone();
    }

    pub fn new_doc(&self, text: &str, canvas_h: u32) -> TextDoc {
        let mut d = TextDoc::new(text);
        self.apply(&mut d, canvas_h);
        d
    }

    /// Where a new clip of this style goes: `Transform.x / y` in canvas px.
    pub fn position(&self, w: u32, h: u32) -> (f64, f64) {
        (self.at.0 as f64 * w as f64, self.at.1 as f64 * h as f64)
    }

    /// A user's style from a document of a `canvas_h`-line frame, stored at 1080p.
    pub fn from_doc(id: &str, doc: &TextDoc, canvas_h: u32) -> StyleDef {
        let k = 1080.0 / canvas_h.max(1) as f32;
        StyleDef {
            id: id.into(),
            category: "mine".into(),
            sample: "text.sample.other".into(),
            style: scale_style(&doc.style, k),
            paragraph: scale_paragraph(&doc.paragraph, k),
            shape: scale_shape(doc.shape, k),
            perspective: doc.perspective,
            anim: doc.anim.clone(),
            at: (0.0, 0.0),
        }
    }
}
```

`crates/text/styles.json` (times in flicks: 0.5 s = 352800000, 0.08 s = 56448000):

```json
[
  {"id": "title", "category": "titles", "sample": "text.sample.titles",
   "style": {"font": {"family": "Montserrat", "weight": 800}, "size": 120, "fill": {"colour": "#ffffffff"}, "shadow": {"colour": "#000000a0", "offset_x": 0, "offset_y": 4, "blur": 10}},
   "anim": {"in": {"preset": "rise", "unit": "word", "duration": 352800000, "stagger": 56448000, "ease": "snap"}, "out": {"preset": "fade", "unit": "word", "duration": 211680000, "stagger": 35280000, "ease": "smooth"}}},
  {"id": "headline_outline", "category": "titles", "sample": "text.sample.titles",
   "style": {"font": {"family": "Oswald", "weight": 700}, "size": 130, "fill": {"colour": "#ffffffff"}, "outlines": [{"width": 6, "colour": "#000000ff"}]},
   "paragraph": {"caps": true},
   "anim": {"in": {"preset": "pop", "unit": "letter", "duration": 246960000, "stagger": 21168000, "ease": "linear"}}},
  {"id": "subtitle", "category": "titles", "sample": "text.sample.titles", "at": [0, 0.1],
   "style": {"font": {"family": "Inter", "weight": 500}, "size": 64, "fill": {"colour": "#ffffffe0"}, "shadow": {"colour": "#00000099", "offset_x": 0, "offset_y": 2, "blur": 6}},
   "anim": {"in": {"preset": "fade", "unit": "word", "duration": 282240000, "stagger": 56448000, "ease": "smooth"}}},
  {"id": "outline_only", "category": "titles", "sample": "text.sample.titles",
   "style": {"font": {"family": "Montserrat", "weight": 900}, "size": 130, "fill": {"colour": "#ffffff00"}, "outlines": [{"width": 3, "colour": "#ffffffff"}]},
   "anim": {"in": {"preset": "zoom", "unit": "word", "duration": 317520000, "stagger": 56448000, "ease": "cinematic"}}},
  {"id": "gradient", "category": "titles", "sample": "text.sample.titles",
   "style": {"font": {"family": "Unbounded", "weight": 800}, "size": 120, "fill": {"colour": "#ffffffff", "gradient": {"stops": [{"pos": 0, "colour": "#ff8c00ff"}, {"pos": 1, "colour": "#ff0080ff"}], "angle": 0}}, "shadow": {"colour": "#00000080", "offset_x": 0, "offset_y": 4, "blur": 8}},
   "anim": {"in": {"preset": "rise", "unit": "letter", "duration": 352800000, "stagger": 28224000, "ease": "snap"}}},
  {"id": "shadow_heavy", "category": "titles", "sample": "text.sample.titles",
   "style": {"font": {"family": "Oswald", "weight": 700}, "size": 140, "fill": {"colour": "#ffffffff"}, "outlines": [{"width": 2, "colour": "#000000ff"}], "shadow": {"colour": "#000000ff", "offset_x": 8, "offset_y": 10, "blur": 0}},
   "paragraph": {"caps": true},
   "anim": {"in": {"preset": "drop", "unit": "word", "duration": 352800000, "stagger": 56448000, "ease": "snap"}}},
  {"id": "minimal", "category": "titles", "sample": "text.sample.titles",
   "style": {"font": {"family": "Inter", "weight": 300}, "size": 72, "fill": {"colour": "#ffffffff"}},
   "paragraph": {"caps": true, "letter_spacing": 18},
   "anim": {"in": {"preset": "fade", "unit": "letter", "duration": 423360000, "stagger": 28224000, "ease": "smooth"}}},
  {"id": "lower_third_bar", "category": "lower_thirds", "sample": "text.sample.lower_thirds", "at": [-0.28, 0.33],
   "style": {"font": {"family": "Montserrat", "weight": 700}, "size": 56, "fill": {"colour": "#ffffffff"}, "plaque": {"per_line": false, "colour": "#000000b3", "radius": 6, "padding_x": 28, "padding_y": 14}},
   "paragraph": {"align": "left", "line_height": 1.25},
   "anim": {"in": {"preset": "slide_right", "unit": "block", "duration": 352800000, "stagger": 0, "ease": "cinematic"}, "out": {"preset": "slide_left", "unit": "block", "duration": 282240000, "stagger": 0, "ease": "cinematic"}}},
  {"id": "lower_third_accent", "category": "lower_thirds", "sample": "text.sample.lower_thirds", "at": [-0.28, 0.33],
   "style": {"font": {"family": "Rubik", "weight": 600}, "size": 52, "fill": {"colour": "#ffffffff"}, "plaque": {"per_line": true, "colour": "#e53935ff", "radius": 4, "padding_x": 18, "padding_y": 8}},
   "paragraph": {"align": "left", "line_height": 1.35},
   "anim": {"in": {"preset": "slide_right", "unit": "line", "duration": 352800000, "stagger": 70560000, "ease": "cinematic"}}},
  {"id": "lower_third_minimal", "category": "lower_thirds", "sample": "text.sample.lower_thirds", "at": [-0.3, 0.34],
   "style": {"font": {"family": "Inter", "weight": 600}, "size": 50, "fill": {"colour": "#ffffffff"}, "shadow": {"colour": "#000000c0", "offset_x": 0, "offset_y": 2, "blur": 6}},
   "paragraph": {"align": "left"},
   "anim": {"in": {"preset": "rise", "unit": "line", "duration": 352800000, "stagger": 84672000, "ease": "snap"}}},
  {"id": "plaque_dark", "category": "plaques", "sample": "text.sample.plaques",
   "style": {"font": {"family": "Montserrat", "weight": 600}, "size": 64, "fill": {"colour": "#ffffffff"}, "plaque": {"per_line": true, "colour": "#000000b3", "radius": 10, "padding_x": 24, "padding_y": 10}},
   "anim": {"in": {"preset": "fade", "unit": "block", "duration": 282240000, "stagger": 0, "ease": "smooth"}}},
  {"id": "plaque_light", "category": "plaques", "sample": "text.sample.plaques",
   "style": {"font": {"family": "Montserrat", "weight": 700}, "size": 64, "fill": {"colour": "#111111ff"}, "plaque": {"per_line": true, "colour": "#ffffffff", "radius": 10, "padding_x": 24, "padding_y": 10}},
   "anim": {"in": {"preset": "fade", "unit": "block", "duration": 282240000, "stagger": 0, "ease": "smooth"}}},
  {"id": "plaque_yellow", "category": "plaques", "sample": "text.sample.plaques",
   "style": {"font": {"family": "Rubik", "weight": 700}, "size": 66, "fill": {"colour": "#000000ff"}, "plaque": {"per_line": true, "colour": "#ffd400ff", "radius": 6, "padding_x": 20, "padding_y": 8}},
   "anim": {"in": {"preset": "pop", "unit": "word", "duration": 246960000, "stagger": 56448000, "ease": "linear"}}},
  {"id": "neon_pink", "category": "neon", "sample": "text.sample.neon",
   "style": {"font": {"family": "Comfortaa", "weight": 700}, "size": 110, "fill": {"colour": "#ffe6ffff"}, "glow": {"colour": "#ff28c8ff", "radius": 14, "intensity": 1.5}},
   "anim": {"loop": {"preset": "flicker", "unit": "letter", "speed": 1.0, "amount": 0.5}}},
  {"id": "neon_blue", "category": "neon", "sample": "text.sample.neon",
   "style": {"font": {"family": "Exo 2", "weight": 700}, "size": 110, "fill": {"colour": "#e6fbffff"}, "glow": {"colour": "#00e5ffff", "radius": 14, "intensity": 1.4}},
   "anim": {"loop": {"preset": "pulse", "unit": "block", "speed": 0.8, "amount": 0.6}}},
  {"id": "sports_score", "category": "other", "sample": "text.sample.score", "at": [0, -0.38],
   "style": {"font": {"family": "Russo One", "weight": 400}, "size": 96, "fill": {"colour": "#ffffffff"}, "plaque": {"per_line": false, "colour": "#0d47a1e6", "radius": 8, "padding_x": 32, "padding_y": 12}},
   "paragraph": {"caps": true, "letter_spacing": 4},
   "anim": {"in": {"preset": "pop", "unit": "letter", "duration": 246960000, "stagger": 21168000, "ease": "linear"}}},
  {"id": "quote", "category": "other", "sample": "text.sample.quote",
   "style": {"font": {"family": "Playfair Display", "weight": 600, "italic": true}, "size": 72, "fill": {"colour": "#ffffffff"}, "shadow": {"colour": "#00000099", "offset_x": 0, "offset_y": 3, "blur": 8}},
   "paragraph": {"line_height": 1.3, "wrap": 1200},
   "anim": {"in": {"preset": "fade", "unit": "line", "duration": 423360000, "stagger": 176400000, "ease": "smooth"}}},
  {"id": "handwritten", "category": "other", "sample": "text.sample.other",
   "style": {"font": {"family": "Caveat", "weight": 700}, "size": 110, "fill": {"colour": "#fff3e0ff"}, "shadow": {"colour": "#00000080", "offset_x": 0, "offset_y": 3, "blur": 6}},
   "anim": {"in": {"preset": "typewriter", "unit": "letter", "duration": 705600, "stagger": 42336000, "ease": "linear"}}},
  {"id": "retro_pixel", "category": "other", "sample": "text.sample.other",
   "style": {"font": {"family": "Press Start 2P", "weight": 400}, "size": 56, "fill": {"colour": "#ffd400ff"}, "outlines": [{"width": 4, "colour": "#000000ff"}], "shadow": {"colour": "#e53935ff", "offset_x": 6, "offset_y": 6, "blur": 0}},
   "anim": {"in": {"preset": "typewriter", "unit": "letter", "duration": 705600, "stagger": 35280000, "ease": "linear"}}},
  {"id": "caption", "category": "captions", "sample": "text.sample.captions", "at": [0, 0.38],
   "style": {"font": {"family": "Inter", "weight": 600}, "size": 54, "fill": {"colour": "#ffffffff"}, "plaque": {"per_line": true, "colour": "#000000a6", "radius": 6, "padding_x": 16, "padding_y": 6}},
   "paragraph": {"wrap": 1500}},
  {"id": "typewriter_caption", "category": "captions", "sample": "text.sample.captions", "at": [0, 0.36],
   "style": {"font": {"family": "Roboto Condensed", "weight": 500}, "size": 54, "fill": {"colour": "#ffffffff"}, "plaque": {"per_line": true, "colour": "#000000a6", "radius": 6, "padding_x": 14, "padding_y": 6}},
   "paragraph": {"wrap": 1500},
   "anim": {"in": {"preset": "typewriter", "unit": "letter", "duration": 705600, "stagger": 28224000, "ease": "linear"}}},
  {"id": "highlight_caption", "category": "captions", "sample": "text.sample.captions", "at": [0, 0.36],
   "style": {"font": {"family": "Montserrat", "weight": 800}, "size": 60, "fill": {"colour": "#ffffffff"}, "outlines": [{"width": 4, "colour": "#000000ff"}]},
   "paragraph": {"caps": true, "wrap": 1500},
   "anim": {"highlight": {"colour": "#ffd400ff", "scale": 1.12}}}
]
```

Catalogs (RU / EN) — style names `text.style.title` «Заголовок» / "Title", `headline_outline` «Заголовок с обводкой» / "Outlined headline", `subtitle` «Подзаголовок» / "Subtitle", `outline_only` «Контур» / "Outline only", `gradient` «Градиент» / "Gradient", `shadow_heavy` «Глубокая тень» / "Heavy shadow", `minimal` «Минимал» / "Minimal", `lower_third_bar` «Нижняя треть: полоса» / "Lower third: bar", `lower_third_accent` «Нижняя треть: акцент» / "Lower third: accent", `lower_third_minimal` «Нижняя треть: минимал» / "Lower third: minimal", `plaque_dark` «Тёмная плашка» / "Dark plaque", `plaque_light` «Светлая плашка» / "Light plaque", `plaque_yellow` «Жёлтая плашка» / "Yellow plaque", `neon_pink` «Неон розовый» / "Pink neon", `neon_blue` «Неон голубой» / "Blue neon", `sports_score` «Табло» / "Scoreboard", `quote` «Цитата» / "Quote", `handwritten` «От руки» / "Handwritten", `retro_pixel` «Ретро-пиксель» / "Retro pixel", `caption` «Подпись» / "Caption", `typewriter_caption` «Печатная подпись» / "Typewriter caption", `highlight_caption` «Подпись с подсветкой» / "Highlighted caption"; categories `text.cat.all` «Все» / "All", `text.cat.titles` «Заголовки» / "Titles", `text.cat.lower_thirds` «Нижние трети» / "Lower thirds", `text.cat.plaques` «Плашки» / "Plaques", `text.cat.neon` «Неон» / "Neon", `text.cat.captions` «Подписи» / "Captions", `text.cat.other` «Другие» / "Other", `text.cat.mine` «Мои» / "Mine"; samples `text.sample.titles` «Заголовок» / "Headline", `text.sample.lower_thirds` «Анна Иванова\nрежиссёр» / "Anna Ivanova\ndirector", `text.sample.plaques` «Важно знать» / "Good to know", `text.sample.neon` «НЕОН» / "NEON", `text.sample.captions` «Подпись к видео» / "Video caption", `text.sample.other` «Текст» / "Text", `text.sample.score` «2 : 1» / "2 : 1", `text.sample.quote` «Лучше один раз увидеть» / "Seeing is believing"; presets `text.anim.none` «Нет» / "None", `text.anim.typewriter` «Печатная машинка» / "Typewriter", `fade` «Проявление» / "Fade", `rise` «Подъём» / "Rise", `drop` «Падение» / "Drop", `slide_left` «Слева» / "Slide left", `slide_right` «Справа» / "Slide right", `pop` «Хлопок» / "Pop", `wave_in` «Волна» / "Wave in", `scatter` «Рассыпание» / "Scatter", `zoom` «Наезд» / "Zoom", `blur_in` «Из размытия» / "Blur in", `pulse` «Пульс» / "Pulse", `sway` «Покачивание» / "Sway", `shake` «Дрожь» / "Shake", `flicker` «Мерцание» / "Flicker", `wave` «Бегущая волна» / "Wave", `float` «Парение» / "Float", `text.anim.highlight` «Подсветка слова» / "Word highlight"; curves `text.ease.smooth` «Плавно» / "Smooth", `linear` «Равномерно» / "Linear", `ease_in` «Разгон» / "Ease in", `ease_out` «Торможение» / "Ease out", `cinematic` «Кино» / "Cinematic", `snap` «Рывок» / "Snap", `gentle` «Мягко» / "Gentle"; units `text.unit.letter` «Буквы» / "Letters", `word` «Слова» / "Words", `line` «Строки» / "Lines", `block` «Целиком» / "Whole". (The JSON strings keep `\n` escaped.)

Run: `cargo test -p kadr-text`
Expected: PASS.

- [ ] **Step 6: Goldens of motion and the styles sheet** — append to `crates/text/tests/golden.rs`:

```rust
/// Frames stacked top to bottom, each over the background colour.
fn strip(doc: &TextDoc, at_ms: &[i64], clip_ms: i64) -> (u32, u32, Vec<u8>) {
    let frames: Vec<(u32, u32, Vec<u8>)> = at_ms
        .iter()
        .map(|&t| {
            let c = Clock { since_in: kadr_core::Time::from_millis(t), until_out: kadr_core::Time::from_millis(clip_ms - t), source: kadr_core::Time::from_millis(t), highlight: t as f64 / clip_ms as f64 };
            picture(&TextFrame::new(doc, c), 0.5)
        })
        .collect();
    let w = frames.iter().map(|f| f.0).max().unwrap();
    let h: u32 = frames.iter().map(|f| f.1).sum();
    let mut out = vec![0u8; (w * h * 4) as usize];
    for p in out.as_chunks_mut::<4>().0 {
        *p = [48, 48, 64, 255];
    }
    let mut y0 = 0;
    for (fw, fh, px) in frames {
        for y in 0..fh {
            let dst = (((y0 + y) * w) * 4) as usize;
            out[dst..dst + (fw * 4) as usize].copy_from_slice(&px[(y * fw * 4) as usize..((y + 1) * fw * 4) as usize]);
        }
        y0 += fh;
    }
    (w, h, out)
}

fn check_px(name: &str, (w, h, px): (u32, u32, Vec<u8>)) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.png"));
    if std::env::var("KADR_UPDATE_GOLDEN").as_deref() == Ok("1") {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        return;
    }
    let mut dec = png::Decoder::new(std::fs::File::open(&path).unwrap_or_else(|_| panic!("{} missing", path.display()))).read_info().unwrap();
    let mut want = vec![0u8; dec.output_buffer_size()];
    let info = dec.next_frame(&mut want).unwrap();
    assert_eq!((info.width, info.height), (w, h), "{name}");
    assert_eq!(px.iter().zip(&want).filter(|(a, b)| a.abs_diff(**b) > 1).count(), 0, "{name}");
}

#[test]
fn motion_goldens() {
    let mut rise = base("Раз два три");
    rise.anim.reveal_in = kadr_text::anim::reveal("rise").map(|r| r.fresh());
    check_px("anim_rise_strip", strip(&rise, &[0, 120, 240, 360, 480, 700], 4_000));
    let mut pop = base("ХЛОП");
    pop.anim.reveal_in = kadr_text::anim::reveal("pop").map(|r| r.fresh());
    check_px("anim_pop_strip", strip(&pop, &[0, 60, 120, 180, 240, 450], 4_000));
    let mut scatter = base("Рассыпание");
    scatter.anim.reveal_in = kadr_text::anim::reveal("scatter").map(|r| r.fresh());
    check_px("anim_scatter_strip", strip(&scatter, &[0, 150, 300, 450, 900], 4_000));
    let mut hl = base("каждое слово по очереди");
    hl.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.12, span: (0.0, 1.0), extra: Default::default() });
    check_px("anim_highlight_strip", strip(&hl, &[100, 1_100, 2_100, 3_100], 4_000));
}

#[test]
fn styles_sheet_golden() {
    let pics: Vec<(u32, u32, Vec<u8>)> = kadr_text::styles::builtin()
        .iter()
        .map(|s| {
            let sample = if s.sample == "text.sample.score" { "2 : 1" } else { "Кадр 2026" };
            let mut d = s.new_doc(sample, 540);
            d.seed = 1; // a fresh doc's seed is random: flicker would differ run to run
            picture(&TextFrame::new(&d, Clock { highlight: 0.3, ..Clock::REST }), 1.0)
        })
        .collect();
    let (cw, ch) = (pics.iter().map(|p| p.0).max().unwrap(), pics.iter().map(|p| p.1).max().unwrap());
    let cols = 4;
    let rows = pics.len().div_ceil(cols) as u32;
    let (w, h) = (cw * cols as u32, ch * rows);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for p in out.as_chunks_mut::<4>().0 {
        *p = [48, 48, 64, 255];
    }
    for (i, (pw, ph, px)) in pics.into_iter().enumerate() {
        let (x0, y0) = ((i % cols) as u32 * cw, (i / cols) as u32 * ch);
        for y in 0..ph {
            let dst = (((y0 + y) * w + x0) * 4) as usize;
            out[dst..dst + (pw * 4) as usize].copy_from_slice(&px[(y * pw * 4) as usize..((y + 1) * pw * 4) as usize]);
        }
    }
    check_px("styles_sheet", (w, h, out));
}
```

Run: `KADR_UPDATE_GOLDEN=1 cargo test -p kadr-text --test golden && cargo test -p kadr-text`
Expected: 5 new PNGs, then PASS. Look at each: «Раз два три» rising word by word, faint and low first, sharp at the last frame; «ХЛОП» letters growing past full size and settling; «Рассыпание» flying in from scattered, turned letters to a straight word; the yellow, slightly larger word walking from the first to the last; the sheet — 22 styles, each readable and recognisably its kind (plaques, lower thirds left-aligned, neon glowing, the gradient, the outline-only title, the pixel font with its red hard shadow, the caption with a black plaque, the highlighted caption's first word yellow). A style that looks broken is fixed in `styles.json` now (a numbers change only), then re-blessed and looked at again.

- [ ] **Step 7: Split and trim of animated titles in the evaluator** (Review Focus 5) — append to `crates/timeline/tests/text_eval.rs`:

```rust
/// The pixels and placement of the only layer at `t`.
fn picture_at(p: &Project, t: Time) -> (Placement, Vec<u8>) {
    let l = layer_at(p, t).remove(0);
    let raster = text_of(&l).picture.render(1.0, &kadr_core::FramePool::new(0));
    (l.placement, raster.data.to_vec())
}

fn edit(p: &Project, cmd: kadr_timeline::EditCommand) -> Project {
    let mut q = p.clone();
    kadr_timeline::EditEngine::new().execute(&mut q, cmd).unwrap();
    q
}

#[test]
fn a_split_keeps_the_loop_picture_at_the_joint() {
    let mut doc = TextDoc::starter("Пульс", 1080);
    doc.anim.looping = Some(LoopAnim { preset: "wave".into(), unit: Unit::Letter, speed: 1.0, amount: 1.0, extra: Default::default() });
    doc.anim.highlight = Some(Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.1, span: (0.0, 1.0), extra: Default::default() });
    let (p, id) = project(doc);
    let q = edit(&p, kadr_timeline::EditCommand::Split { at: s(2), clips: Some(vec![id]) });
    for t in [500, 1_960, 2_000, 2_040, 3_700, 4_960].map(Time::from_millis) {
        assert!(picture_at(&p, t) == picture_at(&q, t), "the picture at {t:?} is the same after the split");
    }
}

#[test]
fn split_hands_in_to_the_left_and_out_to_the_right() {
    let mut doc = TextDoc::starter("Раз два", 1080);
    doc.anim.reveal_in = kadr_text::anim::reveal("rise").map(|r| r.fresh());
    doc.anim.reveal_out = kadr_text::anim::reveal("fade").map(|r| r.fresh());
    let (p, id) = project(doc);
    let q = edit(&p, kadr_timeline::EditCommand::Split { at: Time::from_millis(2_500), clips: Some(vec![id]) });
    for t in [200, 2_400, 2_600, 4_800].map(Time::from_millis) {
        assert!(picture_at(&p, t) == picture_at(&q, t), "at {t:?}: the in stays left, the out right, the middle at rest");
    }
}

#[test]
fn trim_moves_the_reveals_with_the_edges() {
    let mut doc = TextDoc::starter("Раз два", 1080);
    doc.anim.reveal_in = kadr_text::anim::reveal("rise").map(|r| r.fresh());
    let (p, id) = project(doc);
    let q = edit(&p, kadr_timeline::EditCommand::TrimClip { clip: id, edge: kadr_timeline::TrimEdge::Start, to: s(1), ripple: false });
    let raster = |pr: &Project, t: i64| picture_at(pr, Time::from_millis(t)).1;
    assert!(raster(&q, 1_200) == raster(&p, 200), "the appearance starts at the new edge");
}
```

(Add `LoopAnim, Unit, Highlight, Rgba8` to the imports — `use kadr_project::*;` covers them.)

Run: `cargo test -p kadr-timeline --test text_eval`
Expected: PASS.

- [ ] **Step 8: Clippy and commit**

Run: `cargo clippy -p kadr-text -p kadr-timeline --all-targets && cargo test -p kadr-i18n`
Expected: no warnings; PASS (the i18n scan does not read `crates/text`: Task 12's editor test checks these keys — add them to the catalogs now anyway, both files, so that test passes when it lands).

```bash
git add crates/text crates/timeline/tests/text_eval.rs crates/i18n/locales
git commit -m "titles: reveals, loops and highlight per glyph; 22 built-in styles

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 11: The inspector, complete — styling a selection, gradient, shadow, glow, shape, perspective, animation, fonts drawn in themselves (`kadr-editor`, `kadr-text`)

**Files:**
- Create: `crates/text/src/sample.rs`, `apps/editor/src/font_previews.rs`
- Modify: `crates/text/src/lib.rs` (`pub mod sample;`), `apps/editor/src/text_ui.rs` (more props, `text_data` fields, selection scope, preset lists, animation preview), `apps/editor/ui/text_inspector.slint` (selection forwarding, scope row, `Dropdown`, looks / shape / perspective / animation blocks), `apps/editor/ui/inspector.slint`, `apps/editor/ui/app.slint` (new properties and callbacks), `apps/editor/src/app.rs` (callbacks), `apps/editor/src/main.rs` (`mod font_previews;`), `crates/i18n/locales/{ru,en}.json`

**Interfaces:**
- Consumes: Task 7 `apply_text_prop`, `text_data`, `TextSession`, `App.text_scope`; Task 9 shapes and perspective; Task 10 `anim::{REVEALS, LOOPS, EASES, reveal, looping}`, `RevealDef::fresh`, `LoopDef::fresh`; Task 5 `ColorSwatch`.
- Produces:
  - `kadr_text::sample::{Picture { width, height, rgba }, family_preview(family: &str, height: f32) -> Option<Picture>, fit(doc: &TextDoc, w: u32, h: u32, background: [u8; 4]) -> Picture}` (Task 12 uses `fit` for the gallery).
  - `apply_text_prop` props added: `gradient grad-a grad-b grad-angle shadow shadow-color shadow-x shadow-y shadow-blur glow glow-color glow-radius glow-intensity shape bend radius start-angle tilt-x tilt-y reset-selection anim-in anim-in-unit anim-in-dur anim-in-stagger anim-in-ease anim-out anim-out-unit anim-out-dur anim-out-stagger anim-out-ease anim-loop anim-loop-unit anim-loop-speed anim-loop-amount highlight highlight-color highlight-scale`; `text_ui::scope_of(text: &str, start: i32, end: i32) -> Option<Range<usize>>`; `App::{text_selection(i32, i32), text_scope_clear(), preview_text_animation()}`; `font_previews::FontPreviews`, `App::font_previews_ready`.

- [ ] **Step 1: Write the failing tests** — append to `text_ui.rs` tests:

```rust
    #[test]
    fn a_selection_is_clamped_to_characters_and_empty_is_none() {
        let t = "раз два";
        assert_eq!(scope_of(t, 9, 7), Some(7..9), "either order");
        assert_eq!(scope_of(t, 8, 11), Some(7..11), "inside a letter: widened to it");
        assert_eq!(scope_of(t, 4, 4), None);
        assert_eq!(scope_of(t, -3, 400), Some(0..t.len()));
    }

    #[test]
    fn looks_shape_and_tilt_switch_on_with_defaults_and_off_cleanly() {
        let mut d = TextDoc::starter("x", 1080);
        d.style.shadow = None;
        for (p, on) in [("gradient", true), ("shadow", true), ("glow", true)] {
            assert!(apply_text_prop(&mut d, None, p, &TextValue::Flag(on)), "{p}");
        }
        assert!(d.style.fill.gradient.as_ref().is_some_and(|g| g.stops.len() == 2));
        assert!(d.style.shadow.is_some_and(|s| s.blur > 0.0) && d.style.glow.is_some_and(|g| g.radius > 0.0));
        assert!(apply_text_prop(&mut d, None, "grad-angle", &TextValue::Num(90.0)));
        assert!(apply_text_prop(&mut d, None, "glow-intensity", &TextValue::Num(1.5)));
        assert!(apply_text_prop(&mut d, None, "shape", &TextValue::Num(1.0)));
        assert!(matches!(d.shape, TextShape::Arc { .. }));
        assert!(apply_text_prop(&mut d, None, "bend", &TextValue::Num(-50.0)));
        assert_eq!(d.shape, TextShape::Arc { bend: -0.5 });
        assert!(apply_text_prop(&mut d, None, "tilt-x", &TextValue::Num(20.0)));
        assert_eq!(d.perspective, Some(kadr_project::Perspective { tilt_x: 20.0, tilt_y: 0.0 }));
        assert!(apply_text_prop(&mut d, None, "tilt-x", &TextValue::Num(0.0)));
        assert_eq!(d.perspective, None, "no tilt: no perspective");
        assert!(apply_text_prop(&mut d, None, "gradient", &TextValue::Flag(false)));
        assert!(d.style.fill.gradient.is_none());
    }

    #[test]
    fn animation_props_pick_presets_with_their_defaults() {
        let mut d = TextDoc::new("x");
        assert!(apply_text_prop(&mut d, None, "anim-in", &TextValue::Name("pop".into())));
        let r = d.anim.reveal_in.clone().unwrap();
        assert_eq!((r.preset.as_str(), r.unit, r.duration), ("pop", Unit::Letter, kadr_core::Time::from_millis(350)));
        assert!(apply_text_prop(&mut d, None, "anim-in-dur", &TextValue::Num(0.8)));
        assert_eq!(d.anim.reveal_in.as_ref().unwrap().duration, kadr_core::Time::from_millis(800));
        assert!(apply_text_prop(&mut d, None, "anim-in-unit", &TextValue::Num(2.0)));
        assert_eq!(d.anim.reveal_in.as_ref().unwrap().unit, Unit::Line);
        assert!(apply_text_prop(&mut d, None, "anim-in-ease", &TextValue::Name("gentle".into())));
        assert!(apply_text_prop(&mut d, None, "anim-in", &TextValue::Name(String::new())));
        assert!(d.anim.reveal_in.is_none(), "«Нет» removes it");
        assert!(apply_text_prop(&mut d, None, "anim-loop", &TextValue::Name("sway".into())));
        assert!(apply_text_prop(&mut d, None, "anim-loop-amount", &TextValue::Num(0.25)));
        assert_eq!(d.anim.looping.as_ref().unwrap().amount, 0.25);
        assert!(apply_text_prop(&mut d, None, "highlight", &TextValue::Flag(true)));
        assert!(apply_text_prop(&mut d, None, "highlight-scale", &TextValue::Num(1.2)));
        assert!(!apply_text_prop(&mut d, None, "anim-out-dur", &TextValue::Num(1.0)), "no disappearance to time");
        assert!(!apply_text_prop(&mut d, None, "anim-in", &TextValue::Name("teleport".into())), "an unknown preset is refused");
    }

    #[test]
    fn a_selection_can_go_back_to_the_base_style() {
        let mut d = TextDoc::new("раз два");
        apply_text_prop(&mut d, Some(7..13), "fill", &TextValue::Colour(red()));
        assert!(apply_text_prop(&mut d, Some(7..13), "reset-selection", &TextValue::Flag(true)));
        assert_eq!(d.runs.len(), 1);
        assert!(!apply_text_prop(&mut d, None, "reset-selection", &TextValue::Flag(true)), "nothing selected: nothing to reset");
    }
```

and in `crates/text/src/sample.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_preview_is_its_name_in_white_and_none_when_missing() {
        let p = family_preview("Lobster", 22.0).unwrap();
        assert!(p.height >= 20 && p.height <= 40 && p.width > p.height, "{}×{}", p.width, p.height);
        assert!(p.rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 200 && px[0] > 200), "white letters");
        assert!(family_preview("No Such Family Here", 22.0).is_none());
    }

    #[test]
    fn fit_centres_a_style_in_its_box() {
        let doc = kadr_project::TextDoc::starter("Победа", 1080);
        let p = fit(&doc, 320, 180, [20, 20, 30, 255]);
        assert_eq!((p.width, p.height), (320, 180));
        assert_eq!(&p.rgba[..4], &[20, 20, 30, 255], "the corner is background");
        let lit = p.rgba.as_chunks::<4>().0.iter().filter(|px| px[0] > 200).count();
        assert!(lit > 200, "the text is there and big enough");
    }
}
```

Run: `cargo test -p kadr-editor text_ui && cargo test -p kadr-text sample`
Expected: FAIL to compile — `scope_of`, `sample::family_preview` missing.

- [ ] **Step 2: Sample renders** — top of `crates/text/src/sample.rs` (and `pub mod sample;` in `lib.rs`):

```rust
//! Small renders outside the timeline (titles spec §6.3, §6.5): a font's
//! name in that font (the font picker), a style on its sample text, fitted
//! into a thumbnail (the «Текст» tab).

use crate::{Clock, TextFrame};
use kadr_project::{Fill, Rgba8, TextDoc};

/// Premultiplied RGBA8, tightly packed.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `family`'s own name drawn in it, white, a line `height` px tall, on
/// transparency; `None` when this machine has no such family.
pub fn family_preview(family: &str, height: f32) -> Option<Picture> {
    if !crate::fonts().has(family) {
        return None;
    }
    let mut d = TextDoc::new(family);
    d.style.font = kadr_project::FontRef { family: family.into(), weight: 400, italic: false };
    d.style.size = height * 0.8;
    d.style.fill = Fill::solid(Rgba8::WHITE);
    d.paragraph.line_height = 1.0;
    let f = TextFrame::new(&d, Clock::REST);
    let r = f.raster(1.0);
    Some(Picture { width: r.width, height: r.height, rgba: (0..r.height).flat_map(|y| r.row(y).to_vec()).collect() })
}

/// `doc` at rest, scaled to fit `w × h` with an 8 % margin, centred over an
/// opaque `background`.
pub fn fit(doc: &TextDoc, w: u32, h: u32, background: [u8; 4]) -> Picture {
    let f = TextFrame::new(doc, Clock { highlight: 0.3, ..Clock::REST });
    let b = f.bounds();
    let s = ((w as f32 * 0.84) / b.width().max(1.0)).min((h as f32 * 0.84) / b.height().max(1.0)).min(4.0);
    let r = f.raster(s);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        rgba.extend_from_slice(&background);
    }
    let (ox, oy) = ((w as i64 - r.width as i64) / 2, (h as i64 - r.height as i64) / 2);
    for y in 0..r.height {
        let ty = oy + y as i64;
        if ty < 0 || ty >= h as i64 {
            continue;
        }
        for (x, px) in r.row(y).as_chunks::<4>().0.iter().enumerate() {
            let tx = ox + x as i64;
            if tx < 0 || tx >= w as i64 {
                continue;
            }
            let i = ((ty as u32 * w + tx as u32) * 4) as usize;
            let a = px[3] as f32 / 255.0;
            for c in 0..4 {
                rgba[i + c] = (px[c] as f32 + rgba[i + c] as f32 * (1.0 - a)).round().min(255.0) as u8;
            }
        }
    }
    Picture { width: w, height: h, rgba }
}
```

- [ ] **Step 3: The props, the scope, the presets** — in `text_ui.rs`: extend its `kadr_project` import with `Glow, GradientFill, GradientStop, Shadow, TextShape, Unit`, then:

```rust
/// The selection `start..end` (UTF-8 bytes from the text field, either
/// order, possibly out of range) as a scope: clamped, widened to whole
/// characters; empty → `None`.
pub fn scope_of(text: &str, start: i32, end: i32) -> Option<Range<usize>> {
    let clamp = |v: i32| (v.max(0) as usize).min(text.len());
    let (mut a, mut b) = (clamp(start.min(end)), clamp(start.max(end)));
    while !text.is_char_boundary(a) {
        a -= 1;
    }
    while !text.is_char_boundary(b) {
        b += 1;
    }
    (a < b).then_some(a..b)
}

fn unit_of(x: f64) -> Unit {
    match x.round() as i64 {
        0 => Unit::Letter,
        2 => Unit::Line,
        3 => Unit::Block,
        _ => Unit::Word,
    }
}

fn secs(x: f64) -> kadr_core::Time {
    kadr_core::Time::from_millis((x.clamp(0.0, 30.0) * 1000.0).round() as i64)
}
```

and in `apply_text_prop`, before the final `_ => false`:

```rust
        "reset-selection" => match &scoped {
            Some(r) => doc.reset_range(r.clone()),
            None => false,
        },
        "gradient" => {
            let Some(on) = v.flag() else { return false };
            match (on, doc.style.fill.gradient.is_some()) {
                (true, false) => {
                    let a = doc.style.fill.colour;
                    doc.style.fill.gradient = Some(GradientFill { stops: vec![GradientStop { pos: 0.0, colour: a }, GradientStop { pos: 1.0, colour: Rgba8::rgba(255, 0, 128, 255) }], angle: 0.0 });
                    true
                }
                (false, true) => take(&mut doc.style.fill.gradient),
                _ => false,
            }
        }
        "grad-a" | "grad-b" | "grad-angle" => {
            let Some(g) = doc.style.fill.gradient.as_mut() else { return false };
            match (prop, v) {
                ("grad-a", TextValue::Colour(c)) => g.stops.first_mut().is_some_and(|s| put(&mut s.colour, *c)),
                ("grad-b", TextValue::Colour(c)) => g.stops.last_mut().is_some_and(|s| put(&mut s.colour, *c)),
                ("grad-angle", TextValue::Num(x)) if x.is_finite() => put(&mut g.angle, x.rem_euclid(360.0) as f32),
                _ => false,
            }
        }
        "shadow" => {
            let Some(on) = v.flag() else { return false };
            let size = doc.style.size;
            match (on, doc.style.shadow.is_some()) {
                (true, false) => {
                    doc.style.shadow = Some(Shadow { colour: Rgba8::rgba(0, 0, 0, 153), offset_x: 0.0, offset_y: (size * 0.04).round(), blur: (size * 0.08).round() });
                    true
                }
                (false, true) => take(&mut doc.style.shadow),
                _ => false,
            }
        }
        "shadow-color" | "shadow-x" | "shadow-y" | "shadow-blur" => {
            let Some(sh) = doc.style.shadow.as_mut() else { return false };
            match (prop, v) {
                ("shadow-color", TextValue::Colour(c)) => put(&mut sh.colour, *c),
                ("shadow-x", TextValue::Num(x)) if x.is_finite() => put(&mut sh.offset_x, x.clamp(-500.0, 500.0) as f32),
                ("shadow-y", TextValue::Num(x)) if x.is_finite() => put(&mut sh.offset_y, x.clamp(-500.0, 500.0) as f32),
                ("shadow-blur", TextValue::Num(x)) if x.is_finite() => put(&mut sh.blur, x.clamp(0.0, 200.0) as f32),
                _ => false,
            }
        }
        "glow" => {
            let Some(on) = v.flag() else { return false };
            let size = doc.style.size;
            match (on, doc.style.glow.is_some()) {
                (true, false) => {
                    doc.style.glow = Some(Glow { colour: Rgba8::rgba(255, 40, 200, 255), radius: (size * 0.12).round().max(2.0), intensity: 1.2 });
                    true
                }
                (false, true) => take(&mut doc.style.glow),
                _ => false,
            }
        }
        "glow-color" | "glow-radius" | "glow-intensity" => {
            let Some(g) = doc.style.glow.as_mut() else { return false };
            match (prop, v) {
                ("glow-color", TextValue::Colour(c)) => put(&mut g.colour, *c),
                ("glow-radius", TextValue::Num(x)) if x.is_finite() => put(&mut g.radius, x.clamp(0.0, 300.0) as f32),
                ("glow-intensity", TextValue::Num(x)) if x.is_finite() => put(&mut g.intensity, x.clamp(0.0, 2.0) as f32),
                _ => false,
            }
        }
        "shape" => {
            let Some(x) = v.num() else { return false };
            let size = doc.style.size;
            let s = match x.round() as i64 {
                1 => match doc.shape {
                    TextShape::Arc { .. } => doc.shape,
                    _ => TextShape::Arc { bend: 0.4 },
                },
                2 => match doc.shape {
                    TextShape::Circle { .. } => doc.shape,
                    _ => TextShape::Circle { radius: (size * 2.5).round(), start_deg: 0.0 },
                },
                _ => TextShape::Straight,
            };
            put(&mut doc.shape, s)
        }
        "bend" | "radius" | "start-angle" => match (prop, doc.shape, v.num()) {
            ("bend", TextShape::Arc { .. }, Some(x)) => put(&mut doc.shape, TextShape::Arc { bend: (x / 100.0).clamp(-1.0, 1.0) as f32 }),
            ("radius", TextShape::Circle { start_deg, .. }, Some(x)) => put(&mut doc.shape, TextShape::Circle { radius: x.clamp(10.0, 5000.0) as f32, start_deg }),
            ("start-angle", TextShape::Circle { radius, .. }, Some(x)) => put(&mut doc.shape, TextShape::Circle { radius, start_deg: x.rem_euclid(360.0) as f32 }),
            _ => false,
        },
        "tilt-x" | "tilt-y" => {
            let Some(x) = v.num() else { return false };
            let mut p = doc.perspective.unwrap_or_default();
            let x = x.clamp(-60.0, 60.0) as f32;
            if prop == "tilt-x" { p.tilt_x = x } else { p.tilt_y = x }
            put(&mut doc.perspective, (p.tilt_x != 0.0 || p.tilt_y != 0.0).then_some(p))
        }
        "anim-in" | "anim-out" => {
            let TextValue::Name(id) = v else { return false };
            let slot = if prop == "anim-in" { &mut doc.anim.reveal_in } else { &mut doc.anim.reveal_out };
            if id.is_empty() {
                return take(slot);
            }
            let Some(def) = kadr_text::anim::reveal(id) else { return false };
            if slot.as_ref().is_some_and(|r| r.preset == *id) {
                return false;
            }
            *slot = Some(def.fresh());
            true
        }
        "anim-in-unit" | "anim-in-dur" | "anim-in-stagger" | "anim-in-ease" | "anim-out-unit" | "anim-out-dur" | "anim-out-stagger" | "anim-out-ease" => {
            let slot = if prop.starts_with("anim-in") { &mut doc.anim.reveal_in } else { &mut doc.anim.reveal_out };
            let Some(r) = slot.as_mut() else { return false };
            match (prop.rsplit('-').next().unwrap_or(""), v) {
                ("unit", TextValue::Num(x)) => put(&mut r.unit, unit_of(*x)),
                ("dur", TextValue::Num(x)) if x.is_finite() => put(&mut r.duration, secs(x.max(0.001))),
                ("stagger", TextValue::Num(x)) if x.is_finite() => put(&mut r.stagger, secs(*x)),
                ("ease", TextValue::Name(e)) if kadr_text::anim::curve(e).is_some() => put(&mut r.ease, e.clone()),
                _ => false,
            }
        }
        "anim-loop" => {
            let TextValue::Name(id) = v else { return false };
            if id.is_empty() {
                return take(&mut doc.anim.looping);
            }
            let Some(def) = kadr_text::anim::looping(id) else { return false };
            if doc.anim.looping.as_ref().is_some_and(|l| l.preset == *id) {
                return false;
            }
            doc.anim.looping = Some(def.fresh());
            true
        }
        "anim-loop-unit" | "anim-loop-speed" | "anim-loop-amount" => {
            let (Some(l), Some(x)) = (doc.anim.looping.as_mut(), v.num()) else { return false };
            match prop {
                "anim-loop-unit" => put(&mut l.unit, unit_of(x)),
                "anim-loop-speed" => put(&mut l.speed, x.clamp(0.05, 10.0)),
                _ => put(&mut l.amount, x.clamp(0.0, 1.0)),
            }
        }
        "highlight" => {
            let Some(on) = v.flag() else { return false };
            match (on, doc.anim.highlight.is_some()) {
                (true, false) => {
                    doc.anim.highlight = Some(kadr_project::Highlight { colour: Rgba8::rgba(255, 212, 0, 255), scale: 1.08, span: (0.0, 1.0), extra: Default::default() });
                    true
                }
                (false, true) => take(&mut doc.anim.highlight),
                _ => false,
            }
        }
        "highlight-color" | "highlight-scale" => {
            let Some(h) = doc.anim.highlight.as_mut() else { return false };
            match v {
                TextValue::Colour(c) if prop == "highlight-color" => put(&mut h.colour, *c),
                TextValue::Num(x) if prop == "highlight-scale" && x.is_finite() => put(&mut h.scale, x.clamp(0.5, 3.0) as f32),
                _ => false,
            }
        }
```

(`put(&mut doc.perspective, …)` needs `Perspective: PartialEq` — it is.) Extend `prop_label_key`: `gradient`/`grad-*` → `text.gradient`, `shadow*` → `text.shadow`, `glow*` → `text.glow`, `shape`/`bend`/`radius`/`start-angle` → `text.shape`, `tilt-*` → `text.perspective`, `anim-in*` → `text.anim.in`, `anim-out*` → `text.anim.out`, `anim-loop*` → `text.anim.loop`, `highlight*` → `text.anim.highlight`, `reset-selection` → `text.scope.reset`. `text_set`'s flag list gains `"gradient" | "shadow" | "glow" | "highlight" | "reset-selection"`, and names `"anim-in" | "anim-out" | "anim-loop" | "anim-in-ease" | "anim-out-ease"` are `TextValue::Name`.

`text_data` fills the new `TextInspectorData` fields: `scope` (the selected text, at most 24 characters then «…», "" for none), `gradient`, `grad_a`, `grad_b` (the first / last stop, default the fill and #ff0080), `grad_angle`, `shadow`, `shadow_color`, `shadow_x`, `shadow_y`, `shadow_blur`, `glow`, `glow_color`, `glow_radius`, `glow_intensity`, `shape` (0 / 1 / 2), `bend` (×100), `radius`, `start_angle`, `tilt_x`, `tilt_y`, and for the animation the indices into the lists of Step 4: `anim_in` / `anim_out` (0 = none, else 1 + the preset's index in `REVEALS`), `anim_in_unit` / `anim_out_unit` / `anim_loop_unit` (0 letter … 3 block), `anim_in_dur`, `anim_in_stagger` (seconds), `anim_in_ease` (index in `EASES`), the same for out, `anim_loop` (0 = none, else 1 + index in `LOOPS`), `anim_loop_speed`, `anim_loop_amount`, `highlight`, `highlight_color`, `highlight_scale`. For a scope, `text_data` also passes `scope: Option<&Range<usize>>` to show the selection's text.

App side (append to the `impl App` of `text_ui.rs`):

```rust
    /// The text field's selection (titles refinement 10): remembered, also
    /// after the field loses focus; an empty one while focused clears it.
    pub fn text_selection(&mut self, start: i32, end: i32) {
        let Some(id) = self.inspected() else { return };
        let Some(doc) = self.project.sequence().clip(id).and_then(|c| c.text.as_deref()) else { return };
        let scope = scope_of(&doc.plain(), start, end);
        if scope != self.text_scope {
            self.text_scope = scope;
            self.refresh_text_inspector();
        }
    }

    pub fn text_scope_clear(&mut self) {
        if self.text_scope.take().is_some() {
            self.refresh_text_inspector();
        }
    }

    /// «Показать»: plays the inspected clip from its start (at most 4 s),
    /// then puts the playhead back.
    pub fn preview_text_animation(&mut self) {
        let Some((start, len)) = self.inspected().and_then(|id| self.project.sequence().clip(id)).map(|c| (c.timeline_in, c.duration().min(kadr_core::Time::from_secs(4)))) else { return };
        let back = self.playhead;
        self.stop_playback();
        self.set_playhead(start);
        self.start_playback();
        let ms = len.as_millis().max(200) as u64;
        slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
            crate::app::with_app(|app| {
                if app.playing {
                    app.stop_playback();
                }
                app.set_playhead(back);
            });
        });
    }
```

`text_typed` clears `self.text_scope` (typing replaces any selection) before typing. The preset name lists for the dropdowns are set once per language (where the app applies a language): `ui.set_text_reveal_names(…)` = `[t("text.anim.none")] + REVEALS.map(|r| t(&format!("text.anim.{}", r.id)))`, `ui.set_text_loop_names` likewise with `LOOPS`, `ui.set_text_ease_names` = `EASES.map(|e| t(&format!("text.ease.{e}")))`, `ui.set_text_unit_names` = the four `text.unit.*` (models: `ModelRc::new(VecModel::from(...))`). The index → id mapping for `text_set("anim-in", "<index>")`: Slint sends the index; Rust maps `0 → ""`, `i → REVEALS[i - 1].id` (loops alike, eases `EASES[i]`); for `"anim-*-unit"` the index is the unit's number.

- [ ] **Step 4: The section's new Slint** — in `text_inspector.slint`:
  - `TextField` forwards its selection (bytes; deferred `changed` handlers run on the next event-loop turn):
    ```slint
        callback selection-changed(int, int);
        property <{a: int, c: int}> sel: { a: ti.anchor-position-byte-offset, c: ti.cursor-position-byte-offset };
        changed sel => {
            // A focus loss collapses the selection: ignored, Rust keeps the last one.
            if (ti.has-focus) { root.selection-changed(self.sel.a, self.sel.c); }
        }
    ```
  - a `Dropdown` component (a button showing `names[current]`, a `PopupWindow` list; the callback before `close()`):
    ```slint
    component Dropdown inherits Rectangle {
        in property <[string]> names;
        in property <int> current;
        callback selected(int);
        height: 24px;
        horizontal-stretch: 1;
        border-radius: 4px;
        background: da.has-hover ? Theme.bg3 : Theme.bg2;
        border-width: 1px;
        border-color: Theme.border;
        Text { x: 8px; width: parent.width - 28px; text: root.current >= 0 && root.current < root.names.length ? root.names[root.current] : ""; font-size: Theme.font-sm; color: Theme.text; vertical-alignment: center; overflow: elide; }
        Icon { x: parent.width - 20px; icon: Icons.chevron-down; width: 12px; height: 12px; }
        da := TouchArea { mouse-cursor: pointer; clicked => { list.show(); } }
        list := PopupWindow {
            x: 0;
            y: root.height + 2px;
            width: root.width;
            height: min(320px, root.names.length * 26px + 8px);
            close-policy: close-on-click-outside;
            Rectangle { background: Theme.bg2; border-radius: 5px; border-width: 1px; border-color: Theme.border-strong; }
            VerticalLayout {
                padding: 4px;
                for n[i] in root.names: Rectangle {
                    height: 26px;
                    border-radius: 3px;
                    background: ia.has-hover ? Theme.bg3 : transparent;
                    Text { x: 8px; text: n; font-size: Theme.font-sm; color: i == root.current ? Theme.accent : Theme.text; vertical-alignment: center; }
                    ia := TouchArea {
                        clicked => {
                            root.selected(i);
                            list.close();
                        }
                    }
                }
            }
        }
    }
    ```
  - `TextSection` gets `in property <[string]> reveal-names; in property <[string]> loop-names; in property <[string]> ease-names; in property <[string]> unit-names;` and callbacks `selection-changed(int, int)`, `scope-clear()`, `animation-preview()`; the field passes `selection-changed(a, c) => { root.selection-changed(a, c); }`; below the field the scope row:
    ```slint
        Row {
            label: Tr.t("text.scope");
            Text { text: root.d.scope == "" ? Tr.t("text.scope.all") : Tr.t1("text.scope.selection", root.d.scope); font-size: Theme.font-sm; color: root.d.scope == "" ? Theme.text-dim : Theme.accent; vertical-alignment: center; overflow: elide; horizontal-stretch: 1; }
            if root.d.scope != "": IconButton { icon: Icons.undo; tooltip: Tr.t("text.scope.reset"); clicked => { root.set("reset-selection", "1"); } }
            if root.d.scope != "": IconButton { icon: Icons.close; tooltip: Tr.t("text.scope.all"); clicked => { root.scope-clear(); } }
        }
    ```
  - after the fill row, the gradient (fill row gets `BoundSwitch { checked: root.d.gradient; toggled => { root.set("gradient", root.d.gradient ? "0" : "1"); } }` labelled by a `Text { text: Tr.t("text.gradient") … }`), and
    ```slint
        if root.d.gradient: Row {
            label: Tr.t("text.gradient");
            ColorSwatch { value: root.d.grad-a; tooltip: Tr.t("text.gradient"); live(c) => { root.color-live("grad-a", c); } committed(c) => { root.color-commit("grad-a", c); } }
            ColorSwatch { value: root.d.grad-b; tooltip: Tr.t("text.gradient"); live(c) => { root.color-live("grad-b", c); } committed(c) => { root.color-commit("grad-b", c); } }
            Rectangle { horizontal-stretch: 1; }
        }
        if root.d.gradient: PropSlider { label: Tr.t("text.angle"); value: root.d.grad-angle; minimum: 0; maximum: 360; default-value: 0; decimals: 0; unit: "°"; changed(v) => { root.live("grad-angle", v); } committed(v) => { root.commit("grad-angle", v); } }
    ```
  - after the outlines, the shadow and the glow, each a `Row` with a `BoundSwitch` (`set("shadow" | "glow", …)`) and a `ColorSwatch` (`shadow-color` / `glow-color`), then when on: `PropSlider`s `shadow-x` (−100..100 px), `shadow-y` (−100..100 px), `shadow-blur` (0..60 px), `glow-radius` (0..80 px), `glow-intensity` (0..2, shown ×100 %: `display-scale: 100; unit: "%"`);
  - after the paragraph: `SectionTitle { text: Tr.t("text.shape_title"); }`, `Row { label: Tr.t("text.shape"); Segmented { options: [Tr.t("text.shape.straight"), Tr.t("text.shape.arc"), Tr.t("text.shape.circle")]; current: root.d.shape; selected(i) => { root.set("shape", i); } } }` (Slint converts the int to the string argument: write `root.set("shape", "" + i)`), `if root.d.shape == 1: PropSlider bend (−100..100 %)`, `if root.d.shape == 2: PropSlider radius (20..1500 px)` and `start-angle` (0..360°), then `PropSlider tilt-x` and `tilt-y` (−60..60°, label `text.tilt_x` / `text.tilt_y`);
  - the animation block: `SectionTitle { text: Tr.t("text.anim_title"); }`, then
    ```slint
        Row { label: Tr.t("text.anim.in"); Dropdown { names: root.reveal-names; current: root.d.anim-in; selected(i) => { root.set("anim-in", "" + i); } } }
        if root.d.anim-in > 0: Row { label: Tr.t("text.unit"); Dropdown { names: root.unit-names; current: root.d.anim-in-unit; selected(i) => { root.set("anim-in-unit", "" + i); } } }
        if root.d.anim-in > 0: PropSlider { label: Tr.t("text.duration"); value: root.d.anim-in-dur; minimum: 0.05; maximum: 3; default-value: 0.5; decimals: 2; unit: Tr.t("unit.s"); changed(v) => { root.live("anim-in-dur", v); } committed(v) => { root.commit("anim-in-dur", v); } }
        if root.d.anim-in > 0: PropSlider { label: Tr.t("text.stagger"); value: root.d.anim-in-stagger; minimum: 0; maximum: 1; default-value: 0.08; decimals: 2; unit: Tr.t("unit.s"); changed(v) => { root.live("anim-in-stagger", v); } committed(v) => { root.commit("anim-in-stagger", v); } }
        if root.d.anim-in > 0: Row { label: Tr.t("text.ease"); Dropdown { names: root.ease-names; current: root.d.anim-in-ease; selected(i) => { root.set("anim-in-ease", "" + i); } } }
    ```
    the same for `anim-out` (`text.anim.out`), then `Row { label: Tr.t("text.anim.loop"); Dropdown { names: root.loop-names; current: root.d.anim-loop; selected(i) => { root.set("anim-loop", "" + i); } } }` with, when on, the unit `Dropdown`, `PropSlider anim-loop-speed` (0.1..4 Hz, `unit: " Hz"`) and `anim-loop-amount` (0..1 ×100 %); `Row { label: Tr.t("text.anim.highlight"); BoundSwitch { … "highlight" … } ColorSwatch { enabled: root.d.highlight; value: root.d.highlight-color; … "highlight-color" … } }` and when on `PropSlider highlight-scale` (1..1.5 ×100 %); last, `Button { text: Tr.t("text.anim.preview"); icon: Icons.play; clicked => { root.animation-preview(); } }`.
  - `TextInspectorData` gets the fields listed in Step 3 (Slint names with dashes: `grad-a`, `anim-in-dur`, …).
  - `inspector.slint` / `app.slint` pass the four name lists (`text-reveal-names`, `text-loop-names`, `text-ease-names`, `text-unit-names` on `AppWindow`) and the three callbacks through (`text-selection(int, int)`, `text-scope-clear()`, `text-anim-preview()`), wired in `app.rs` with `cb!` to `text_selection`, `text_scope_clear`, `preview_text_animation`. In `text_set` the Rust side maps the indices: `"anim-in" | "anim-out"` → `TextValue::Name(if i == 0 { "" } else { REVEALS[i - 1].id })`, `"anim-loop"` likewise with `LOOPS`, `"anim-in-ease" | "anim-out-ease"` → `TextValue::Name(EASES[i])`, `"anim-*-unit"` and `"shape"` → `TextValue::Num(i)`.

- [ ] **Step 5: Font names drawn in their fonts** — `apps/editor/src/font_previews.rs`:

```rust
//! The font picker's previews (titles spec §1): each family's name drawn in
//! that family by kadr-text, on a background thread, cached for the session.

use crate::app::App;
use std::collections::HashMap;

#[derive(Default)]
pub struct FontPreviews {
    pub images: HashMap<String, slint::Image>,
    running: bool,
}

impl App {
    /// Starts rendering the previews of `names` not yet drawn (one thread at a time).
    pub fn request_font_previews(&mut self, names: Vec<String>) {
        let todo: Vec<String> = names.into_iter().filter(|n| !self.font_previews.images.contains_key(n)).collect();
        if todo.is_empty() || self.font_previews.running {
            return;
        }
        self.font_previews.running = true;
        std::thread::Builder::new()
            .name("kadr-font-previews".into())
            .spawn(move || {
                let done: Vec<(String, u32, u32, Vec<u8>)> = todo.into_iter().filter_map(|n| kadr_text::sample::family_preview(&n, 44.0).map(|p| (n, p.width, p.height, p.rgba))).collect();
                crate::app::post(move |app| app.font_previews_ready(done));
            })
            .ok();
    }

    pub fn font_previews_ready(&mut self, done: Vec<(String, u32, u32, Vec<u8>)>) {
        self.font_previews.running = false;
        for (name, w, h, px) in done {
            let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&px, w, h);
            self.font_previews.images.insert(name, slint::Image::from_rgba8_premultiplied(buf));
        }
        self.font_rows_refresh();
    }
}
```

`App` gets `pub font_previews: crate::font_previews::FontPreviews`. `font_rows_refresh` fills `preview` / `has_preview` from `self.font_previews.images` and, at its end, calls `self.request_font_previews(<the non-header names of the rows>)`. In the Slint font list, the preview image row: `if !f.header && f.has-preview: Image { x: 8px; height: 22px; width: self.source.width / 2 * 1px; y: (parent.height - self.height) / 2; source: f.preview; image-fit: contain; }` (rendered at 44 px for 2× sharpness).

- [ ] **Step 6: Catalogs, tests, commit** — RU / EN: `text.scope` «Стиль для» / "Style applies to"; `text.scope.all` «всего текста» / "the whole text"; `text.scope.selection` «выделения «{0}»» / "the selection “{0}”"; `text.scope.reset` «Сбросить стиль выделения» / "Reset the selection's style"; `text.gradient` «Градиент» / "Gradient"; `text.angle` «Угол» / "Angle"; `text.shadow` «Тень» / "Shadow"; `text.shadow_x` «Смещение X» / "Offset X"; `text.shadow_y` «Смещение Y» / "Offset Y"; `text.blur` «Размытие» / "Blur"; `text.glow` «Свечение» / "Glow"; `text.glow_radius` «Радиус» / "Radius"; `text.glow_intensity` «Интенсивность» / "Intensity"; `text.shape_title` «ФОРМА» / "SHAPE"; `text.shape` «Линия» / "Path"; `text.shape.straight` «Прямо» / "Straight"; `text.shape.arc` «Дуга» / "Arc"; `text.shape.circle` «Круг» / "Circle"; `text.bend` «Изгиб» / "Bend"; `text.radius` «Радиус» / "Radius"; `text.start_angle` «Начало» / "Start"; `text.perspective` «Перспектива» / "Perspective"; `text.tilt_x` «Наклон X» / "Tilt X"; `text.tilt_y` «Наклон Y» / "Tilt Y"; `text.anim_title` «АНИМАЦИЯ» / "ANIMATION"; `text.anim.in` «Появление» / "In"; `text.anim.out` «Исчезновение» / "Out"; `text.anim.loop` «Цикл» / "Loop"; `text.anim.preview` «Показать» / "Preview"; `text.unit` «По» / "By"; `text.duration` «Длительность» / "Duration"; `text.stagger` «Шаг» / "Stagger"; `text.ease` «Кривая» / "Curve"; `text.speed` «Скорость» / "Speed"; `text.amount` «Сила» / "Amount"; `text.highlight_scale` «Масштаб» / "Scale" (`unit.s` exists? if not, add `"unit.s": " с"` / `" s"`). The `text.anim.*`, `text.ease.*`, `text.unit.*` keys came with Task 10.

Run: `cargo build -p kadr-editor && cargo test -p kadr-editor && cargo test -p kadr-text && cargo test -p kadr-i18n && cargo clippy -p kadr-editor -p kadr-text --all-targets`
Expected: PASS; no new warnings.

```bash
git add apps/editor crates/text crates/i18n/locales
git commit -m "titles: style a selection; gradient, shadow, glow, shape, tilt, animation in the inspector; fonts drawn in themselves

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 12: The «Текст» tab — styles with live thumbnails, hover try-on, click and drag, «Мои стили», «Сохранить стиль» (`kadr-editor`)

**Files:**
- Create: `apps/editor/ui/text_tab.slint`, `apps/editor/src/text_tab.rs`
- Modify: `apps/editor/ui/media.slint:149` and `apps/editor/ui/effects.slint:114` (a third tab name), `apps/editor/ui/app.slint` (`left-tab == 2` shows `TextPanel`; properties, callbacks, exports `TextCardView`), `apps/editor/ui/text_inspector.slint` (the «Сохранить стиль» button), `apps/editor/src/effects_ui.rs` (`fx_tab_changed`: tab 2), `apps/editor/src/preview.rs` (`preview_scenes`: the text try-on applied to the snapshot), `apps/editor/src/app.rs` (`Prompt::SaveTextStyle`, `App.text_tab`, callbacks), `apps/editor/src/persistence.rs` (`prompt_submit` arm), `apps/editor/src/main.rs` (`mod text_tab;`), `crates/i18n/locales/{ru,en}.json`

**Interfaces:**
- Consumes: Task 10 `styles::{builtin, by_id, StyleDef, CATEGORIES}`; Task 11 `sample::fit`; Task 6 `App::add_text`; `timeline_ui::drop_target(wx, wy) -> Option<(Time, usize)>`; the effects tab's drag pattern (`DRAG_PX`, ghost, `drag-active` properties); `App::prompt`.
- Produces: `text_tab::{TextTab, TextOverride { docs: Vec<(ClipId, TextDoc)> }, TextOverride::apply(&self, &mut Sequence), UserStyle { name: String, style: StyleDef }, load_user_styles(&Path) -> Vec<UserStyle>, save_user_styles(&Path, &[UserStyle]) -> std::io::Result<()>, cards(category: &str, user: &[UserStyle]) -> Vec<(String /*id*/, String /*name*/, String /*category*/)>}`; `App::{refresh_text_tab, text_card_hover, text_card_pressed, text_card_moved, text_card_released, text_category_selected, text_tab_add, save_text_style_prompt, save_text_style, delete_user_style, text_thumbs_ready}`; Slint `TextCardView { id, name, thumb, has-thumb, mine: bool }`, `TextPanel`.

- [ ] **Step 1: Write the failing tests** — `apps/editor/src/text_tab.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;
    use kadr_project::{Clip, Project};

    #[test]
    fn cards_filter_by_category_and_list_mine_last() {
        let all = cards("all", &[]);
        assert_eq!(all.len(), kadr_text::styles::builtin().len());
        let neon = cards("neon", &[]);
        assert!(neon.iter().all(|c| c.2 == "neon") && neon.len() == 2);
        let mine = vec![UserStyle { name: "Мой неон".into(), style: kadr_text::styles::StyleDef::from_doc("u1", &TextDoc::new("x"), 1080) }];
        let with = cards("all", &mine);
        assert_eq!(with.last().unwrap(), &("u1".to_string(), "Мой неон".to_string(), "mine".to_string()));
        assert_eq!(cards("mine", &mine).len(), 1);
    }

    #[test]
    fn user_styles_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text-styles.json");
        assert!(load_user_styles(&path).is_empty(), "no file: no styles");
        let mut d = TextDoc::starter("x", 2160);
        d.style.size = 200.0;
        let s = UserStyle { name: "Крупный".into(), style: kadr_text::styles::StyleDef::from_doc("u7", &d, 2160) };
        save_user_styles(&path, std::slice::from_ref(&s)).unwrap();
        let back = load_user_styles(&path);
        assert_eq!(back, [s]);
        assert_eq!(back[0].style.style.size, 100.0, "stored at 1080p");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load_user_styles(&path).is_empty(), "a broken file is no styles, not a crash");
    }

    #[test]
    fn a_try_on_changes_the_snapshot_not_the_project() {
        let mut p = Project::new("t");
        let c = Clip::new_text(TextDoc::new("Гол"), Time::ZERO, Time::from_secs(5));
        let id = c.id;
        p.sequence_mut().tracks[0].clips.push(c);
        let neon = kadr_text::styles::by_id("neon_pink").unwrap();
        let o = TextOverride::for_style(&p, &[id], neon);
        let mut snapshot = p.sequence().clone();
        o.apply(&mut snapshot);
        let tried = snapshot.clip(id).unwrap().text.as_deref().unwrap();
        assert!(tried.style.glow.is_some() && tried.plain() == "Гол");
        assert!(p.sequence().clip(id).unwrap().text.as_deref().unwrap().style.glow.is_none(), "the project is untouched");
    }
}
```

Run: `cargo test -p kadr-editor text_tab`
Expected: FAIL to compile — `cannot find function cards`.

- [ ] **Step 2: The model side** — top of `apps/editor/src/text_tab.rs`:

```rust
//! The «Текст» tab (titles spec §1, §6.5): «＋ Добавить текст», the
//! built-in styles and the user's own, as cards with thumbnails drawn by
//! kadr-text in the background. Hovering a card (≥ 150 ms) with text clips
//! selected tries the style on in the preview only (`TextOverride`); a
//! click applies it to the selected text clips (one undo entry) or adds a
//! new text of that style; a drag drops a new text at the drop point.

use crate::app::App;
use kadr_core::{ClipId, Time};
use kadr_i18n::{t, tf};
use kadr_project::{Project, Sequence, TextDoc};
use kadr_text::styles::{builtin, StyleDef};
use kadr_timeline::{ClipProperty, EditCommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A style the user saved: their name for it, its look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserStyle {
    pub name: String,
    pub style: StyleDef,
}

pub fn load_user_styles(path: &Path) -> Vec<UserStyle> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_user_styles(path: &Path, styles: &[UserStyle]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(styles).unwrap_or_default())
}

/// The cards of `category` ("all", a built-in category or "mine"): (id,
/// shown name, category), built-ins first (named through the catalogs),
/// the user's last.
pub fn cards(category: &str, user: &[UserStyle]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = builtin()
        .iter()
        .filter(|s| category == "all" || s.category == category)
        .map(|s| (s.id.clone(), t(&s.name_key()), s.category.clone()))
        .collect();
    if category == "all" || category == "mine" {
        out.extend(user.iter().map(|u| (u.style.id.clone(), u.name.clone(), "mine".to_string())));
    }
    out
}

/// A style tried on the selected text clips, applied to the preview's copy only.
#[derive(Clone, Debug, PartialEq)]
pub struct TextOverride {
    pub style_id: String,
    pub docs: Vec<(ClipId, TextDoc)>,
}

impl TextOverride {
    pub fn for_style(project: &Project, clips: &[ClipId], style: &StyleDef) -> TextOverride {
        let seq = project.sequence();
        let docs = clips
            .iter()
            .filter_map(|id| {
                let mut d = seq.clip(*id)?.text.as_deref()?.clone();
                style.apply(&mut d, seq.height);
                Some((*id, d))
            })
            .collect();
        TextOverride { style_id: style.id.clone(), docs }
    }

    pub fn apply(&self, seq: &mut Sequence) {
        for (id, d) in &self.docs {
            if let Some((ti, ci)) = seq.locate_clip(*id) {
                seq.tracks[ti].clips[ci].text = Some(Box::new(d.clone()));
            }
        }
    }
}

/// The tab's state.
#[derive(Default)]
pub struct TextTab {
    pub category: String,
    pub preview: Option<TextOverride>,
    hover: Option<(String, u64)>,
    drag: Option<(String, f32, f32, bool)>,
    pub user: Vec<UserStyle>,
    thumbs: HashMap<(String, String), slint::Image>,
    rendering: bool,
}
```

Run: `cargo test -p kadr-editor text_tab`
Expected: PASS.

- [ ] **Step 3: The App side** — append to `text_tab.rs`:

```rust
const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
const DRAG_PX: f32 = 5.0;

impl App {
    fn style_of(&self, id: &str) -> Option<StyleDef> {
        kadr_text::styles::by_id(id).cloned().or_else(|| self.text_tab.user.iter().find(|u| u.style.id == id).map(|u| u.style.clone()))
    }

    fn user_styles_path(&self) -> std::path::PathBuf {
        self.dirs.data.join("text-styles.json")
    }

    /// The selected text clips (on unlocked tracks).
    fn selected_texts(&self) -> Vec<ClipId> {
        let seq = self.project.sequence();
        self.tl.selection.iter().copied().filter(|id| seq.locate_clip(*id).is_some_and(|(ti, ci)| !seq.tracks[ti].locked && seq.tracks[ti].clips[ci].is_text())).collect()
    }

    /// Chips, cards, thumbnails — only while the tab shows.
    pub fn refresh_text_tab(&mut self) {
        let ui = self.ui();
        if ui.get_left_tab() != 2 {
            return;
        }
        if self.text_tab.category.is_empty() {
            self.text_tab.category = "all".into();
            self.text_tab.user = load_user_styles(&self.user_styles_path());
        }
        let lang = kadr_i18n::lang().code().to_string();
        let rows: Vec<crate::TextCardView> = cards(&self.text_tab.category, &self.text_tab.user)
            .into_iter()
            .map(|(id, name, cat)| {
                let thumb = self.text_tab.thumbs.get(&(id.clone(), lang.clone())).cloned();
                crate::TextCardView { id: id.into(), name: name.into(), has_thumb: thumb.is_some(), thumb: thumb.unwrap_or_default(), mine: cat == "mine" }
            })
            .collect();
        crate::util::sync_rows_by_key(ui.get_text_cards(), rows, |c| c.id.clone(), |m| ui.set_text_cards(m));
        let chips: Vec<crate::FxCategoryView> = std::iter::once("all").chain(kadr_text::styles::CATEGORIES.iter().copied()).chain(std::iter::once("mine")).map(|k| crate::FxCategoryView { key: k.into(), label: t(&format!("text.cat.{k}")).into() }).collect();
        ui.set_text_categories(crate::util::reuse_rows(Some(ui.get_text_categories()), chips));
        ui.set_text_category(self.text_tab.category.clone().into());
        ui.set_text_hint(if self.selected_texts().is_empty() { t("text.tab.hint_add") } else { t("text.tab.hint_apply") }.into());
        self.request_text_thumbs();
    }

    /// Renders the thumbnails the cards lack (one thread at a time; never while playing).
    fn request_text_thumbs(&mut self) {
        if self.text_tab.rendering || self.playing {
            return;
        }
        let lang = kadr_i18n::lang().code().to_string();
        let todo: Vec<(String, TextDoc)> = cards(&self.text_tab.category, &self.text_tab.user)
            .into_iter()
            .filter(|(id, ..)| !self.text_tab.thumbs.contains_key(&(id.clone(), lang.clone())))
            .filter_map(|(id, ..)| {
                let s = self.style_of(&id)?;
                Some((id, s.new_doc(&t(&s.sample), 1080)))
            })
            .collect();
        if todo.is_empty() {
            return;
        }
        self.text_tab.rendering = true;
        std::thread::Builder::new()
            .name("kadr-text-thumbs".into())
            .spawn(move || {
                let done: Vec<(String, String, Vec<u8>)> = todo.into_iter().map(|(id, doc)| (id, lang.clone(), kadr_text::sample::fit(&doc, 320, 180, [24, 25, 30, 255]).rgba)).collect();
                crate::app::post(move |app| app.text_thumbs_ready(done));
            })
            .ok();
    }

    pub fn text_thumbs_ready(&mut self, done: Vec<(String, String, Vec<u8>)>) {
        self.text_tab.rendering = false;
        for (id, lang, px) in done {
            let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&px, 320, 180);
            self.text_tab.thumbs.insert((id, lang), slint::Image::from_rgba8_premultiplied(buf));
        }
        self.refresh_text_tab();
    }

    pub fn text_category_selected(&mut self, key: &str) {
        self.text_tab.category = key.into();
        self.refresh_text_tab();
    }

    /// «＋ Добавить текст».
    pub fn text_tab_add(&mut self) {
        self.add_text_at_playhead();
    }

    /// Hover ≥ 150 ms over a card with text clips selected: try the style on.
    pub fn text_card_hover(&mut self, id: &str, on: bool) {
        if !on {
            self.text_tab.hover = None;
            if self.text_tab.preview.take().is_some() {
                self.preview.invalidate();
                self.request_frame();
            }
            return;
        }
        let generation = self.text_tab.hover.as_ref().map_or(0, |h| h.1) + 1;
        self.text_tab.hover = Some((id.to_string(), generation));
        slint::Timer::single_shot(HOVER_DELAY, move || {
            crate::app::with_app(|app| {
                let Some((id, g)) = app.text_tab.hover.clone() else { return };
                if g != generation || app.playing {
                    return;
                }
                let clips = app.selected_texts();
                let Some(style) = app.style_of(&id) else { return };
                if clips.is_empty() {
                    return;
                }
                app.text_tab.preview = Some(TextOverride::for_style(&app.project, &clips, &style));
                app.preview.invalidate();
                app.request_frame();
            });
        });
    }

    pub fn text_card_pressed(&mut self, id: &str, x: f32, y: f32) {
        self.text_tab.drag = Some((id.to_string(), x, y, false));
    }

    pub fn text_card_moved(&mut self, id: &str, x: f32, y: f32) {
        let ui = self.ui();
        let Some(d) = self.text_tab.drag.as_mut() else { return };
        if !d.3 && (x - d.1).hypot(y - d.2) < DRAG_PX {
            return;
        }
        d.3 = true;
        let name = cards("all", &self.text_tab.user).into_iter().find(|c| c.0 == id).map(|c| c.1).unwrap_or_default();
        ui.set_drag_active(true);
        ui.set_drag_x(x);
        ui.set_drag_y(y);
        ui.set_drag_label(name.into());
        ui.set_drag_refused(self.drop_target(x as f64, y as f64).is_none());
    }

    /// A click applies to the selected text clips (or adds a text); a drag drops a new text there.
    pub fn text_card_released(&mut self, id: &str, x: f32, y: f32) {
        let ui = self.ui();
        ui.set_drag_active(false);
        let Some((_, _, _, dragged)) = self.text_tab.drag.take() else { return };
        let Some(style) = self.style_of(id) else { return };
        let seq = self.project.sequence();
        let (w, h) = (seq.width, seq.height);
        let named = cards("all", &self.text_tab.user).into_iter().find(|c| c.0 == id).map(|c| c.1).unwrap_or_default();
        if dragged {
            let Some((at, row)) = self.drop_target(x as f64, y as f64) else {
                self.toast_warn(t("text.tab.drop_refused"));
                return;
            };
            let rows = crate::timeline_ui::rows(self.project.sequence(), self.tl.scroll_y);
            let Some(r) = rows.get(row).filter(|r| r.kind == kadr_project::TrackKind::Video) else {
                self.toast_warn(t("text.tab.drop_refused"));
                return;
            };
            let track = self.project.sequence().tracks[r.track].id;
            // A drop on a taken spot of that track falls back to the command's choice.
            let free = self.project.sequence().tracks[r.track].clips.iter().all(|c| !c.timeline_range().overlaps(&kadr_core::TimeRange::new(at, at + Time::from_secs(5))));
            self.add_styled_text(&style, at, free.then_some(track), (w, h));
            return;
        }
        let clips = self.selected_texts();
        if clips.is_empty() {
            let at = self.playhead;
            self.add_styled_text(&style, at, None, (w, h));
            return;
        }
        if self.text_tab.preview.take().is_some() {
            self.preview.invalidate();
            self.request_frame();
        }
        let o = TextOverride::for_style(&self.project, &clips, &style);
        let commands: Vec<EditCommand> = o
            .docs
            .into_iter()
            .filter(|(id, d)| self.project.sequence().clip(*id).and_then(|c| c.text.as_deref()) != Some(d))
            .map(|(id, d)| EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Text(Box::new(d)) })
            .collect();
        if commands.is_empty() {
            return; // the same style again: nothing
        }
        self.execute(EditCommand::Batch { label: tf("cmd.text_style_named", &[("name", &named)]), commands });
    }

    /// A new text of `style` at `at` on `track`, placed where the style puts it — one undo entry.
    fn add_styled_text(&mut self, style: &StyleDef, at: Time, track: Option<kadr_core::TrackId>, (w, h): (u32, u32)) {
        let doc = style.new_doc(&t("text.default"), h);
        self.add_text(at, track, Some(doc), Some(style.position(w, h)));
    }

    /// «Сохранить стиль»: asks for a name.
    pub fn save_text_style_prompt(&mut self) {
        let Some(id) = self.inspected().filter(|id| self.project.sequence().clip(*id).is_some_and(|c| c.is_text())) else { return };
        self.prompt(&t("text.save_style.title"), &t("text.save_style.label"), "", crate::app::Prompt::SaveTextStyle(id));
    }

    pub fn save_text_style(&mut self, clip: ClipId, name: &str) {
        let Some(doc) = self.project.sequence().clip(clip).and_then(|c| c.text.as_deref()).cloned() else { return };
        let id = format!("user-{}", uuid_like());
        let style = StyleDef::from_doc(&id, &doc, self.project.sequence().height);
        if self.text_tab.user.is_empty() {
            self.text_tab.user = load_user_styles(&self.user_styles_path());
        }
        self.text_tab.user.push(UserStyle { name: name.to_string(), style });
        match save_user_styles(&self.user_styles_path(), &self.text_tab.user) {
            Ok(()) => self.toast(tf("text.save_style.done", &[("name", name)])),
            Err(e) => self.toast_warn(tf("text.save_style.failed", &[("error", &e.to_string())])),
        }
        self.refresh_text_tab();
    }

    pub fn delete_user_style(&mut self, id: &str) {
        let before = self.text_tab.user.len();
        self.text_tab.user.retain(|u| u.style.id != id);
        if self.text_tab.user.len() != before {
            let _ = save_user_styles(&self.user_styles_path(), &self.text_tab.user);
            self.refresh_text_tab();
        }
    }
}

/// A unique enough id for a user style (time + counter).
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{:x}-{}", kadr_project::now_ms(), N.fetch_add(1, Ordering::Relaxed))
}
```

`self.drop_target(wx, wy)` is `timeline_ui`'s (window px → (time, row index into `timeline_ui::rows`)). `app.rs`: `Prompt::SaveTextStyle(ClipId)`; `persistence.rs::prompt_submit`: `Prompt::SaveTextStyle(id) => self.save_text_style(id, text),`; `App.text_tab: crate::text_tab::TextTab`; `effects_ui::fx_tab_changed`: when `tab == 2` call `self.refresh_text_tab()`, otherwise clear the text hover (`self.text_card_hover("", false)`); `preview.rs::preview_scenes`: the snapshot closure takes the text override too:

```rust
        let snapshot = |project: &Project, fx: &crate::effects_ui::FxUi, text: Option<&crate::text_tab::TextOverride>| match (&fx.preview, text) {
            (None, None) => ProjectScenes::new(project, bypass),
            (o, t) => ProjectScenes::new_with(project, bypass, |seq| {
                if let Some(o) = o {
                    o.apply(seq);
                }
                if let Some(t) = t {
                    t.apply(seq);
                }
            }),
        };
```

called as `snapshot(&self.project, &self.fx, self.text_tab.preview.as_ref())` in both places. A project open or new clears `text_tab.preview` / `hover` / `drag` (next to `fx_reset`).

- [ ] **Step 4: The tab's Slint** — `apps/editor/ui/text_tab.slint` (the card's pointer events in window coordinates, as `FxCard`):

```slint
import { Theme, Icons } from "theme.slint";
import { Tr } from "i18n.slint";
import { PanelTabs, Button, Icon, EmptyState } from "widgets.slint";
import { FxCategoryView } from "effects.slint";
import { ScrollView } from "std-widgets.slint";

export struct TextCardView {
    id: string,
    name: string,
    thumb: image,
    has-thumb: bool,
    mine: bool,
}

component TextCard inherits Rectangle {
    in property <TextCardView> c;
    in property <bool> refused;
    callback hover(bool);
    callback pressed(length, length);
    callback moved(length, length);
    callback released(length, length);
    callback delete();
    border-radius: 6px;
    background: ta.has-hover ? Theme.bg3 : Theme.bg2;
    border-width: 1px;
    border-color: ta.has-hover ? Theme.border-strong : Theme.border;
    VerticalLayout {
        padding: 4px;
        spacing: 4px;
        Rectangle {
            height: (root.width - 8px) * 9 / 16;
            vertical-stretch: 0;
            border-radius: 4px;
            clip: true;
            background: #18191e;
            if root.c.has-thumb: Image { width: parent.width; height: parent.height; source: root.c.thumb; image-fit: contain; }
            if !root.c.has-thumb: Icon { icon: Icons.text; width: 24px; height: 24px; color: Theme.text-faint; }
        }
        Text { text: root.c.name; font-size: Theme.font-sm; color: Theme.text; horizontal-alignment: center; overflow: elide; vertical-stretch: 0; }
    }
    ta := TouchArea {
        mouse-cursor: self.pressed ? (root.refused ? no-drop : grabbing) : pointer;
        changed has-hover => { root.hover(self.has-hover); }
        pointer-event(e) => {
            if (e.button == PointerEventButton.left && e.kind == PointerEventKind.down) { root.pressed(root.absolute-position.x + self.mouse-x, root.absolute-position.y + self.mouse-y); }
            if (e.button == PointerEventButton.left && e.kind == PointerEventKind.up) { root.released(root.absolute-position.x + self.mouse-x, root.absolute-position.y + self.mouse-y); }
        }
        moved => { if (self.pressed) { root.moved(root.absolute-position.x + self.mouse-x, root.absolute-position.y + self.mouse-y); } }
    }
    if root.c.mine && ta.has-hover: Rectangle {
        x: parent.width - 22px;
        y: 4px;
        width: 18px;
        height: 18px;
        border-radius: 9px;
        background: da.has-hover ? Theme.danger : #000000a0;
        Icon { icon: Icons.close; width: 10px; height: 10px; color: #ffffff; }
        da := TouchArea { clicked => { root.delete(); } }
    }
}

/// «Текст»: add a text, apply or drag a style (titles spec §6.5).
export component TextPanel inherits Rectangle {
    in property <[FxCategoryView]> categories;
    in property <string> category;
    in property <[TextCardView]> cards;
    in property <string> hint;
    in property <bool> drag-refused;
    callback tab-selected(int);
    callback category-selected(string);
    callback add();
    callback card-hover(string, bool);
    callback card-pressed(string, length, length);
    callback card-moved(string, length, length);
    callback card-released(string, length, length);
    callback card-delete(string);
    background: Theme.bg1;
    VerticalLayout {
        PanelTabs {
            tabs: [Tr.t("media.title"), Tr.t("fx.title"), Tr.t("text.title")];
            current: 2;
            selected(i) => { root.tab-selected(i); }
        }
        VerticalLayout {
            vertical-stretch: 0;
            padding: 8px;
            spacing: 8px;
            Button { vertical-stretch: 0; primary: true; icon: Icons.plus; text: Tr.t("text.tab.add"); clicked => { root.add(); } }
            Rectangle {
                height: 24px;
                vertical-stretch: 0;
                clip: true;
                chips := Flickable {
                    content-width: max(self.width, row.preferred-width);
                    content-height: self.height;
                    row := HorizontalLayout {
                        spacing: 4px;
                        alignment: start;
                        for cat in root.categories: Rectangle {
                            width: ct.preferred-width + 20px;
                            height: 24px;
                            border-radius: 12px;
                            background: root.category == cat.key ? Theme.bg4 : cta.has-hover ? Theme.bg3 : Theme.bg2;
                            border-width: 1px;
                            border-color: root.category == cat.key ? Theme.border-strong : Theme.border;
                            ct := Text { text: cat.label; font-size: Theme.font-sm; color: root.category == cat.key ? Theme.text : Theme.text-dim; }
                            cta := TouchArea { mouse-cursor: pointer; clicked => { root.category-selected(cat.key); } }
                        }
                    }
                }
            }
        }
        if root.cards.length == 0: EmptyState { icon: Icons.text; title: Tr.t("text.tab.empty"); subtitle: ""; }
        if root.cards.length > 0: sv := ScrollView {
            vertical-stretch: 1;
            property <length> gap: 8px;
            property <int> cols: max(2, floor((self.visible-width - 8px) / 168px));
            property <length> card-w: (self.visible-width - 16px - (self.cols - 1) * self.gap) / self.cols;
            property <length> card-h: (self.card-w - 8px) * 9 / 16 + 30px;
            content-width: self.visible-width;
            content-height: ceil(root.cards.length * 1.0 / self.cols) * (self.card-h + self.gap) + 4px;
            for card[i] in root.cards: TextCard {
                x: 8px + mod(i, sv.cols) * (sv.card-w + sv.gap);
                y: 4px + floor(i / sv.cols) * (sv.card-h + sv.gap);
                width: sv.card-w;
                height: sv.card-h;
                c: card;
                refused: root.drag-refused;
                hover(on) => { root.card-hover(card.id, on); }
                pressed(x, y) => { root.card-pressed(card.id, x, y); }
                moved(x, y) => { root.card-moved(card.id, x, y); }
                released(x, y) => { root.card-released(card.id, x, y); }
                delete => { root.card-delete(card.id); }
            }
        }
        Rectangle { height: 1px; background: Theme.border; vertical-stretch: 0; }
        VerticalLayout {
            vertical-stretch: 0;
            padding-left: 12px;
            padding-right: 12px;
            padding-top: 6px;
            padding-bottom: 7px;
            Text { text: root.hint; font-size: Theme.font-xs; color: Theme.text-dim; wrap: word-wrap; }
        }
    }
}
```

(`FxCategoryView` must be exported from `effects.slint` — it is: `export struct FxCategoryView`.) `media.slint:149` and `effects.slint:114`: `tabs: [Tr.t("media.title"), Tr.t("fx.title"), Tr.t("text.title")];`. `app.slint`: comment `// 0 media, 1 effects, 2 text`; a `TextPanel { visible: root.left-tab == 2; categories: root.text-categories; category: root.text-category; cards: root.text-cards; hint: root.text-hint; drag-refused: root.drag-refused; tab-selected(i) => { root.left-tab = i; root.left-tab-changed(i); root.focus-editor(); } category-selected(k) => { root.text-category-selected(k); } add => { root.text-tab-add(); root.focus-editor(); } card-hover(id, on) => { root.text-card-hover(id, on); } card-pressed(id, x, y) => { root.text-card-pressed(id, x, y); } card-moved(id, x, y) => { root.text-card-moved(id, x, y); } card-released(id, x, y) => { root.text-card-released(id, x, y); root.focus-editor(); } card-delete(id) => { root.text-card-delete(id); } }` next to the `EffectsPanel`; properties `in property <[FxCategoryView]> text-categories; in property <string> text-category; in property <[TextCardView]> text-cards; in property <string> text-hint;` and the callbacks; `TextCardView` exported. In `text_inspector.slint`, at the end of `TextSection`: `Button { text: Tr.t("text.save_style"); icon: Icons.plus; clicked => { root.save-style(); } }` with `callback save-style();`, passed through `Inspector` (`text-save-style()`) to `AppWindow` (`text-save-style()`) and wired to `save_text_style_prompt`. `app.rs`: `cb!` lines for the tab's callbacks (`text_category_selected`, `text_tab_add`, `text_card_hover`, `text_card_pressed`, `text_card_moved`, `text_card_released`, `delete_user_style`, `save_text_style_prompt`).

- [ ] **Step 5: The catalog test and the strings** — in `text_tab.rs` tests:

```rust
    /// Names built at run time (titles Global Constraints): every style,
    /// category, sample, preset, curve and unit in both catalogs.
    #[test]
    fn every_text_string_exists_in_both_catalogs() {
        let mut keys: Vec<String> = kadr_text::styles::builtin().iter().flat_map(|s| [s.name_key(), s.sample.clone()]).collect();
        keys.extend(["all", "mine"].iter().chain(kadr_text::styles::CATEGORIES).map(|c| format!("text.cat.{c}")));
        keys.extend(kadr_text::anim::REVEALS.iter().map(|r| format!("text.anim.{}", r.id)));
        keys.extend(kadr_text::anim::LOOPS.iter().map(|l| format!("text.anim.{}", l.id)));
        keys.extend(kadr_text::anim::EASES.iter().map(|e| format!("text.ease.{e}")));
        keys.extend(["letter", "word", "line", "block"].map(|u| format!("text.unit.{u}")));
        keys.extend(["text.anim.none", "text.anim.highlight", "text.default", "text.title"].map(String::from));
        for lang in [kadr_i18n::Lang::Ru, kadr_i18n::Lang::En] {
            let known = kadr_i18n::all_keys(lang);
            for k in &keys {
                assert!(known.contains(&k.as_str()), "{lang:?} lacks {k}");
            }
        }
    }
```

RU / EN: `text.title` «ТЕКСТ» / "TEXT"; `text.tab.add` «Добавить текст» / "Add text"; `text.tab.hint_add` «Щелчок — новый текст этого стиля у плейхеда; перетащите на таймлайн, чтобы положить в точку» / "Click to add a text of this style at the playhead; drag it onto the timeline to drop it there"; `text.tab.hint_apply` «Наведите — примерка на выделенном тексте; щелчок — применить» / "Hover to try it on the selected text; click to apply"; `text.tab.empty` «Здесь пока ничего» / "Nothing here yet"; `text.tab.drop_refused` «Текст кладётся на видеодорожку» / "A text goes on a video track"; `text.save_style` «Сохранить стиль» / "Save style"; `text.save_style.title` «Сохранить стиль» / "Save style"; `text.save_style.label` «Название» / "Name"; `text.save_style.done` «Стиль «{name}» сохранён в «Мои стили»» / "Style “{name}” saved to Mine"; `text.save_style.failed` «Не удалось сохранить стиль: {error}» / "Could not save the style: {error}"; `cmd.text_style_named` «Стиль текста: {name}» / "Text style: {name}".

- [ ] **Step 6: Run, clippy, commit**

Run: `cargo build -p kadr-editor && cargo test -p kadr-editor && cargo test -p kadr-i18n && cargo clippy -p kadr-editor --all-targets`
Expected: PASS; no new warnings.

```bash
git add apps/editor crates/i18n/locales
git commit -m "titles: the Текст tab — styles with thumbnails, try-on, apply, drag, my styles

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 13: Text for MCP and the AI assistant — `add_text`, `set_text`, `add_caption`, `get_state`, `list_text_styles` (`kadr-ai`, editor MCP, `kadr-mcp`)

**Files:**
- Modify: `crates/ai/Cargo.toml` (`kadr-text = { path = "../text" }`), `crates/ai/src/command.rs` (`AiCommand::{AddText, SetText}`, `AddCaption` supported, `reason()`, `validate_each`, `schema_for_prompt`, tests), `crates/ai/src/context.rs` (a text clip's `"text"`), `crates/ai/tests/assistant_flow.rs:157-159` (`add_caption` is no longer refused)
- Create: `apps/editor/src/mcp_text.rs`
- Modify: `apps/editor/src/mcp_api.rs` (`EDIT_OPS`, `REFUSED_OPS`, `handle` → `list_text_styles`, test `an_unknown_op_type_lists_every_op`), `apps/editor/src/mcp_state.rs` (`timeline_json`: `asset` null and `text` for text clips), `apps/editor/src/main.rs` (`mod mcp_text;`), `apps/kadr-mcp/src/tools.rs` (`EDIT_DESCRIPTION`, `list_text_styles` tool), `apps/kadr-mcp/tests/tools.rs`, `docs/mcp.md`

**Interfaces:**
- Consumes: Task 1 `Clip::new_text`, `TextDoc::{starter, set_plain}`, `Rgba8::parse`, `Align`, `EditCommand::AddText`, `ClipProperty::Text`; Task 3 `kadr_text::fonts`; Task 10 `kadr_text::styles::{by_id, builtin, StyleDef::{new_doc, apply, position}}`.
- Produces: `AiCommand::AddText { at_ms, duration_ms: Option<i64>, text, style: Option<String>, x: Option<f64>, y: Option<f64>, size: Option<f64>, track_id: Option<String>, reason }`, `AiCommand::SetText { clip_id, text: Option<String>, style: Option<String>, size: Option<f64>, color: Option<String>, font: Option<String>, align: Option<String>, reason }`, `AiCommand::AddCaption { start_ms, end_ms, text, reason }` (valid now); MCP edit ops `add_text`, `set_text`, `add_caption`; tool `list_text_styles` → `{"styles": [{"id", "category", "name": {"ru", "en"}}], "fonts": [names]}`; `get_state` clips carry `"text"` (the document's JSON) and `"asset": null` for text clips.

- [ ] **Step 1: Write the failing AI tests** — append to `crates/ai/src/command.rs` tests:

```rust
    fn text_project() -> (Project, kadr_core::ClipId) {
        let mut p = Project::new("t");
        let c = Clip::new_text(kadr_project::TextDoc::new("Гол"), Time::ZERO, Time::from_secs(10));
        let id = c.id;
        p.sequence_mut().tracks[0].clips.push(c);
        (p, id)
    }

    // `cmd(json)` (JSON → AiCommand) is the tests' existing helper.

    #[test]
    fn add_text_places_a_title_with_its_style() {
        let (p, _) = text_project();
        let out = validate_strict(&[cmd(json!({"type": "add_text", "at_ms": 2_000, "text": "Счёт 2:1", "style": "sports_score", "size": 80}))], &p, &Permissions::default()).unwrap();
        let EditCommand::AddText { clip, track: None } = &out[0] else { panic!("{out:?}") };
        let doc = clip.text.as_deref().unwrap();
        assert_eq!((doc.plain().as_str(), doc.style.size, clip.timeline_in, clip.duration()), ("Счёт 2:1", 80.0, Time::from_secs(2), Time::from_secs(5)));
        let style = kadr_text::styles::by_id("sports_score").unwrap();
        assert_eq!((clip.transform.x, clip.transform.y), style.position(1920, 1080), "the style's place unless x / y are given");
        let out = validate_strict(&[cmd(json!({"type": "add_text", "at_ms": 0, "text": "x", "duration_ms": 1_500, "x": -300, "y": 200}))], &p, &Permissions::default()).unwrap();
        let EditCommand::AddText { clip, .. } = &out[0] else { panic!() };
        assert_eq!((clip.duration(), clip.transform.x, clip.transform.y), (Time::from_millis(1_500), -300.0, 200.0));
    }

    #[test]
    fn set_text_changes_what_is_given_and_no_change_is_rejected() {
        let (p, id) = text_project();
        let id_s = id.to_string();
        let out = validate_strict(&[cmd(json!({"type": "set_text", "clip_id": id_s, "text": "Гол!!", "color": "#ffd400", "align": "left", "font": "Oswald"}))], &p, &Permissions::default()).unwrap();
        let EditCommand::SetClipProperty { prop: kadr_timeline::ClipProperty::Text(doc), .. } = &out[0] else { panic!() };
        assert_eq!((doc.plain().as_str(), doc.style.fill.colour, doc.paragraph.align, doc.style.font.family.as_str()), ("Гол!!", kadr_project::Rgba8::rgba(255, 212, 0, 255), kadr_project::Align::Left, "Oswald"));
        let same = validate_strict(&[cmd(json!({"type": "set_text", "clip_id": id_s, "text": "Гол"}))], &p, &Permissions::default());
        assert!(same.unwrap_err().to_string().contains("no change"));
        assert_eq!(validate(&[cmd(json!({"type": "set_text", "clip_id": id_s, "text": "Гол"}))], &p, &Permissions::default()).unwrap(), vec![], "dropped inside a plan (R10)");
        let styled = validate_strict(&[cmd(json!({"type": "set_text", "clip_id": id_s, "style": "neon_pink"}))], &p, &Permissions::default()).unwrap();
        let EditCommand::SetClipProperty { prop: kadr_timeline::ClipProperty::Text(doc), .. } = &styled[0] else { panic!() };
        assert!(doc.style.glow.is_some() && doc.plain() == "Гол", "a style changes the look and keeps the text");
    }

    #[test]
    fn add_caption_is_a_bottom_caption_text() {
        let (p, _) = text_project();
        let out = validate_strict(&[cmd(json!({"type": "add_caption", "start_ms": 1_000, "end_ms": 3_500, "text": "Подпись"}))], &p, &Permissions::default()).unwrap();
        let EditCommand::AddText { clip, track: None } = &out[0] else { panic!() };
        assert_eq!((clip.timeline_in, clip.duration()), (Time::from_secs(1), Time::from_millis(2_500)));
        assert!(clip.transform.y > 300.0, "low in the frame");
        assert!(clip.text.as_deref().unwrap().style.plaque.is_some_and(|p| p.per_line));
    }

    #[test]
    fn text_ops_say_what_is_wrong() {
        let (mut p, id) = text_project();
        let media = MediaAsset::new("m.mp4", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: None, audio: None, timecode: None });
        let m = Clip::new(media.id, "m", TimeRange::new(Time::ZERO, Time::from_secs(5)), Time::from_secs(20));
        let mid = m.id.to_string();
        p.sequence_mut().tracks[0].clips.push(m);
        p.assets.push(media);
        let a1 = p.sequence().tracks[1].id.to_string();
        let err = |j: serde_json::Value| validate_strict(&[cmd(j)], &p, &Permissions::default()).unwrap_err().to_string();
        assert!(err(json!({"type": "add_text", "at_ms": 0, "text": "  "})).contains("empty"));
        assert!(err(json!({"type": "add_text", "at_ms": 0, "text": "x", "style": "comic"})).contains("unknown text style"));
        assert!(err(json!({"type": "add_text", "at_ms": 0, "text": "x", "track_id": a1})).contains("video track"));
        assert!(err(json!({"type": "add_text", "at_ms": 0, "text": "x", "size": 0.5})).contains("size"));
        assert!(err(json!({"type": "set_text", "clip_id": mid, "text": "x"})).contains("not a text clip"));
        assert!(err(json!({"type": "set_text", "clip_id": id.to_string(), "color": "red"})).contains("#rrggbb"));
        assert!(err(json!({"type": "set_text", "clip_id": id.to_string(), "font": "No Such Font"})).contains("not installed"));
        assert!(err(json!({"type": "set_text", "clip_id": id.to_string()})).contains("nothing to set"));
        assert!(err(json!({"type": "add_caption", "start_ms": 3_000, "end_ms": 3_000, "text": "x"})).contains("after start"));
    }
```

Run: `cargo test -p kadr-ai`
Expected: FAIL to compile — `no variant AddText / SetText` (and `kadr_text` unresolved until the dependency is added).

- [ ] **Step 2: Implement the AI commands** — `crates/ai/Cargo.toml`: `kadr-text = { path = "../text" }`. In `command.rs`, the enum (replacing the reserved `AddCaption`):

```rust
    /// A text clip (titles spec §7): `text` from `at_ms` for `duration_ms`
    /// (default 5000) on the topmost video track (or `track_id`), in a
    /// built-in `style` (`list_text_styles`), its block centred `x` / `y`
    /// px from the frame's centre, font `size` px.
    AddText {
        at_ms: i64,
        #[serde(default)]
        duration_ms: Option<i64>,
        text: String,
        #[serde(default)]
        style: Option<String>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        #[serde(default)]
        size: Option<f64>,
        #[serde(default)]
        track_id: Option<String>,
        #[serde(default)]
        reason: String,
    },
    /// Changes a text clip: its text (styles of unchanged words kept), a
    /// built-in style (the look replaced, the text kept), the size, the
    /// colour, the font, the alignment.
    SetText {
        clip_id: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        style: Option<String>,
        #[serde(default)]
        size: Option<f64>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        font: Option<String>,
        #[serde(default)]
        align: Option<String>,
        #[serde(default)]
        reason: String,
    },
    /// A caption: a text clip of the «Подпись» style at the bottom of the frame.
    AddCaption {
        start_ms: i64,
        end_ms: i64,
        text: String,
        #[serde(default)]
        reason: String,
    },
```

`reason()`: add `AiCommand::AddText { reason, .. } | AiCommand::SetText { reason, .. } | AiCommand::AddCaption { reason, .. }` to the big arm (remove `AiCommand::AddCaption { .. } => ""`). In `validate_each`, replace the `AddCaption` refusal with:

```rust
            AiCommand::AddText { at_ms, duration_ms, text, style, x, y, size, track_id, .. } => {
                let range = |msg: String| ValidationError::Range { index: i, msg };
                let at = in_seq(i, *at_ms)?;
                let dur = duration_ms.unwrap_or(5_000);
                if !(1..=3_600_000).contains(&dur) {
                    return Err(range(format!("duration_ms={dur} outside 1..3600000")));
                }
                if text.trim().is_empty() {
                    return Err(range("empty text".into()));
                }
                let style = match style.as_deref() {
                    Some(id) => Some(kadr_text::styles::by_id(id).ok_or_else(|| range(format!("unknown text style {id:?} (one of {})", text_style_ids())))?),
                    None => None,
                };
                let mut doc = style.map_or_else(|| kadr_project::TextDoc::starter(text, seq.height), |s| s.new_doc(text, seq.height));
                if let Some(sz) = size {
                    check(i, "size", *sz, 4.0, 2000.0)?;
                    doc.style.size = *sz as f32;
                }
                let mut clip = Clip::new_text(doc, at, ms(dur));
                let (sx, sy) = style.map_or((0.0, 0.0), |s| s.position(seq.width, seq.height));
                (clip.transform.x, clip.transform.y) = (x.unwrap_or(sx), y.unwrap_or(sy));
                check(i, "x", clip.transform.x, -20_000.0, 20_000.0)?;
                check(i, "y", clip.transform.y, -20_000.0, 20_000.0)?;
                let track = match track_id.as_deref() {
                    Some(t) => {
                        let tr = kadr_core::TrackId::parse(t).and_then(|t| seq.track(t)).ok_or_else(|| range(format!("unknown track {t}")))?;
                        if tr.kind != TrackKind::Video {
                            return Err(range(format!("a text goes on a video track, not {}", tr.name)));
                        }
                        Some(tr.id)
                    }
                    None => None,
                };
                EditCommand::AddText { clip, track }
            }
            AiCommand::SetText { clip_id, text, style, size, color, font, align, .. } => {
                let range = |msg: String| ValidationError::Range { index: i, msg };
                let id = clip(i, clip_id)?;
                let Some(before) = seq.clip(id).unwrap().text.as_deref() else { return Err(range(format!("clip {clip_id} is not a text clip"))) };
                if text.is_none() && style.is_none() && size.is_none() && color.is_none() && font.is_none() && align.is_none() {
                    return Err(range("nothing to set (text, style, size, color, font, align)".into()));
                }
                let mut doc = before.clone();
                if let Some(id) = style.as_deref() {
                    kadr_text::styles::by_id(id).ok_or_else(|| range(format!("unknown text style {id:?} (one of {})", text_style_ids())))?.apply(&mut doc, seq.height);
                }
                if let Some(t) = text {
                    if t.trim().is_empty() {
                        return Err(range("empty text".into()));
                    }
                    doc.set_plain(t);
                }
                if let Some(sz) = size {
                    check(i, "size", *sz, 4.0, 2000.0)?;
                    doc.style.size = *sz as f32;
                    doc.runs.iter_mut().for_each(|r| r.style.size = None);
                }
                if let Some(c) = color {
                    let c = kadr_project::Rgba8::parse(c).ok_or_else(|| range(format!("color {c:?}: use \"#rrggbb\" or \"#rrggbbaa\"")))?;
                    doc.style.fill = kadr_project::Fill::solid(c);
                    doc.runs.iter_mut().for_each(|r| r.style.fill = None);
                }
                if let Some(f) = font {
                    if !kadr_text::fonts().has(f) {
                        return Err(range(format!("font {f:?} is not installed (list_text_styles lists the fonts)")));
                    }
                    doc.style.font.family = f.clone();
                    doc.runs.iter_mut().for_each(|r| r.style.family = None);
                }
                if let Some(a) = align {
                    doc.paragraph.align = match a.as_str() {
                        "left" => kadr_project::Align::Left,
                        "center" => kadr_project::Align::Center,
                        "right" => kadr_project::Align::Right,
                        other => return Err(range(format!("align {other:?}: left, center or right"))),
                    };
                }
                doc.normalise();
                if doc == *before {
                    if strict {
                        return Err(range(format!("no change: clip {clip_id} already looks like that")));
                    }
                    out.push(None);
                    continue;
                }
                EditCommand::SetClipProperty { clip: id, prop: ClipProperty::Text(Box::new(doc)) }
            }
            AiCommand::AddCaption { start_ms, end_ms, text, .. } => {
                let range = |msg: String| ValidationError::Range { index: i, msg };
                let start = in_seq(i, *start_ms)?;
                if *end_ms <= *start_ms {
                    return Err(range("end must be after start".into()));
                }
                if text.trim().is_empty() {
                    return Err(range("empty text".into()));
                }
                let style = kadr_text::styles::by_id("caption").expect("the built-in caption style");
                let mut clip = Clip::new_text(style.new_doc(text, seq.height), start, ms(*end_ms) - start);
                (clip.transform.x, clip.transform.y) = style.position(seq.width, seq.height);
                EditCommand::AddText { clip, track: None }
            }
```

with, near `check`:

```rust
fn text_style_ids() -> String {
    kadr_text::styles::builtin().iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ")
}
```

(`Clip` and `TrackKind` come into the imports.) The `ValidationError::Unsupported` variant stays (its message is generic). `schema_for_prompt`: after the `remove_effect` line of `head` add

```
 {"type":"add_text","at_ms":int,"text":string,"duration_ms"?:int,"style"?:string,"x"?:num,"y"?:num,"size"?:num,"reason":string} (a title on the top video track; x / y = its centre in px from the frame's centre)
 {"type":"set_text","clip_id":string,"text"?:string,"style"?:string,"size"?:num,"color"?:"#rrggbb","font"?:string,"align"?:"left"|"center"|"right","reason":string} (text clips only: their "text" in the context)
 {"type":"add_caption","start_ms":int,"end_ms":int,"text":string,"reason":string} (a caption at the bottom of the frame)
```

and append to the returned string, before `tail`: `format!("\nText styles (style ids): {}", text_style_ids())`. `context.rs`: in each clip's JSON add `"text"` (the plain text) for a text clip — `if let Some(d) = c.text.as_deref() { obj["text"] = d.plain().into(); }` in the form the file builds clips. In `crates/ai/tests/assistant_flow.rs::llm_commands_are_strictly_validated`, the last three lines ("Reserved commands are rejected until implemented.") become:

```rust
    // add_caption makes a caption text clip (titles spec §7).
    let c = command::parse_commands(r#"[{"type":"add_caption","start_ms":0,"end_ms":1000,"text":"hi"}]"#).unwrap();
    assert!(matches!(command::validate(&c, &p, &perms).as_deref(), Ok([kadr_timeline::EditCommand::AddText { .. }])));
```


Run: `cargo test -p kadr-ai`
Expected: PASS (and the earlier `every_ai_command_is_an_edit_op_or_refused` in the editor fails until Step 3).

- [ ] **Step 3: MCP** — `mcp_api.rs`: `EDIT_OPS` gains `"add_text", "set_text", "add_caption"` (one per line, after `"set_video_fade"`), `REFUSED_OPS` becomes `&[]` (keep the constant: `every_ai_command_is_an_edit_op_or_refused` still reads it); `handle`: `"list_text_styles" => Reply::Now(Ok(crate::mcp_text::list_text_styles())),` after `list_effects`; in `an_unknown_op_type_lists_every_op` replace the two "A refused type keeps its own reason" lines with

```rust
        // add_caption is an op now: a bad one names its problem.
        let e = edit_commands(&p, &[], &json!([{"type": "add_caption", "start_ms": 1_000, "end_ms": 1_000, "text": "hi"}])).unwrap_err().message;
        assert!(e.contains("end must be after start") && !e.contains("unknown op type"), "{e}");
```

`apps/editor/src/mcp_text.rs`:

```rust
//! `list_text_styles` (titles spec §7): the built-in text styles and the
//! installed font families, for `add_text` / `set_text`.

use serde_json::{json, Value};

pub fn list_text_styles() -> Value {
    let styles: Vec<Value> = kadr_text::styles::builtin()
        .iter()
        .map(|s| {
            let key = s.name_key();
            json!({
                "id": s.id,
                "category": s.category,
                "name": {"ru": kadr_i18n::t_in(kadr_i18n::Lang::Ru, &key), "en": kadr_i18n::t_in(kadr_i18n::Lang::En, &key)},
            })
        })
        .collect();
    let fonts: Vec<String> = kadr_text::fonts().families().into_iter().map(|f| f.name).collect();
    json!({"styles": styles, "fonts": fonts})
}

#[cfg(test)]
mod tests {
    #[test]
    fn styles_and_fonts_are_listed_with_both_names() {
        let v = super::list_text_styles();
        let styles = v["styles"].as_array().unwrap();
        assert_eq!(styles.len(), kadr_text::styles::builtin().len());
        let caption = styles.iter().find(|s| s["id"] == "caption").unwrap();
        assert_eq!((caption["name"]["ru"].as_str(), caption["name"]["en"].as_str()), (Some("Подпись"), Some("Caption")));
        assert!(v["fonts"].as_array().unwrap().iter().any(|f| f == "Montserrat"));
    }
}
```

`mcp_state.rs::timeline_json`, the clip object: `"asset": if c.is_text() { Value::Null } else { json!(c.asset.to_string()) },` and `"text": c.text.as_deref().map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),`. A test there (or in `mcp_api.rs` tests where `timeline_json` is reachable):

```rust
    #[test]
    fn get_state_shows_a_text_clip_with_its_document_and_no_asset() {
        let mut p = kadr_project::Project::new("t");
        let c = kadr_project::Clip::new_text(kadr_project::TextDoc::new("Гол"), kadr_core::Time::ZERO, kadr_core::Time::from_secs(2));
        p.sequence_mut().tracks[0].clips.push(c);
        let v = crate::mcp_state::timeline_json(p.sequence(), &[], kadr_core::Time::ZERO);
        let clip = &v["tracks"][0]["clips"][0];
        assert_eq!(clip["asset"], serde_json::Value::Null);
        assert_eq!(clip["text"]["runs"][0]["text"], "Гол");
    }
```

`apps/kadr-mcp/src/tools.rs`: in `EDIT_DESCRIPTION` replace the `add_caption` line with

```
- \"add_text\": at_ms, text, duration_ms? (default 5000), style? (an id from `list_text_styles`), x?, y? (the text block's centre, px from the frame's centre; default: the style's place, else the centre), size? (font px, 4..2000), track_id? (a video track; default: the topmost video track when free there, else a new track above)
- \"set_text\": clip_id (a text clip), text?, style? (replaces the look, keeps the text), size?, color? (\"#rrggbb\" or \"#rrggbbaa\"), font? (an installed family from `list_text_styles`), align? (\"left\"|\"center\"|\"right\"); at least one; no change is rejected
- \"add_caption\": start_ms, end_ms, text (a caption: a text clip of the «Подпись» style at the bottom of the frame)
```

(each line in the file's escaped form, `- \"op\": …` inside the string) and the sentence `get_state` part: "… `effects` …; a text clip has `asset: null` and `text` (its document: runs, style, paragraph, shape, anim)". In `kadr_tools()` after `list_effects`: `tool("list_text_styles", "The built-in text styles (id, category, name {ru, en}) for add_text / set_text `style`, and the installed font families for set_text `font`. Returns {\"styles\": [...], \"fonts\": [...]}.", json!({}), &[]),`. `apps/kadr-mcp/tests/tools.rs::edit_description_documents_every_op`: the op list gains `"add_text", "set_text"`; the field list gains `"track_id"`, `"list_text_styles"`, `"#rrggbb"`; add

```rust
#[test]
fn list_text_styles_is_a_tool() {
    let d = tool("list_text_styles")["description"].as_str().unwrap().to_string();
    for word in ["add_text", "set_text", "fonts", "styles"] {
        assert!(d.contains(word), "list_text_styles description lacks {word}: {d}");
    }
}
```

`docs/mcp.md`: the tool table gets `list_text_styles`; the edit ops table `add_text`, `set_text`, and `add_caption` (no longer "rejected").

- [ ] **Step 4: Run, clippy, commit**

Run: `cargo test -p kadr-ai && cargo test -p kadr-editor && cargo test -p kadr-mcp && cargo clippy -p kadr-ai -p kadr-editor -p kadr-mcp --all-targets`
Expected: PASS (`edit_ops_match_the_kadr_mcp_description`, `every_ai_command_is_an_edit_op_or_refused`, `edit_description_documents_every_op` included); no new warnings.

```bash
git add crates/ai apps/editor/src apps/kadr-mcp docs/mcp.md
git commit -m "titles: add_text, set_text and add_caption for MCP and the assistant; text in get_state

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---


### Task 14: Bench rows, the perf record, live acceptance, review (controller)

**Files:**
- Create: `apps/kadr-bench/src/text_bench.rs`, `docs/perf/2026-10-12-text.md`
- Modify: `apps/kadr-bench/src/main.rs` (`mod text_bench;`, `Some("text") => text_bench::run()`, usage string), `apps/kadr-bench/Cargo.toml` (`kadr-text = { path = "../../crates/text" }`), `apps/kadr-bench/src/scene_bench.rs` (the row «3 animated titles over video»), `ARCHITECTURE.md` (§18 roadmap: titles done; subtitles next in V0.3)
- If the owner tunes a look: `crates/text/styles.json`, the constants of `crates/text/src/anim.rs` (`REVEALS`, `LOOPS`), the goldens of `crates/text/tests/golden/` and `crates/project-scenes/tests/golden/text_*.png` and the tests pinning a moved number

**Interfaces:**
- Consumes: everything above; `kadr_text::{TextFrame, Clock, styles}`, `kadr_scene::TextPicture`, `kadr_core::FramePool`, `report::Report`, `kadr_core::perf::Stats`.
- Produces: `kadr-bench text` rows «static title first raster», «animated title per frame» (1080p, rise by letter, mid-reveal), «neon + shadow per frame», «3 animated titles per frame»; the scene row «3 animated titles over video» (`evaluate`, µs); the perf record with the T-D13 verdict (≤ 4 ms per animated title per 1080p frame; playback with three animated titles drops no more frames than without); the live acceptance record.

- [ ] **Step 1: The text bench** — `apps/kadr-bench/src/text_bench.rs`:

```rust
//! `kadr-bench text`: the cost of drawing titles (titles spec §8): a static
//! title's first raster at 1080p, an animated title per frame (rise by
//! letter, mid-reveal), a neon title with a shadow per frame, three
//! animated titles per frame. Per row: p50 / p90 / max of 200 frames, ms.

use crate::report::Report;
use kadr_core::perf::Stats;
use kadr_core::{FramePool, Time};
use kadr_project::TextDoc;
use kadr_scene::TextPicture;
use kadr_text::{Clock, TextFrame};
use std::time::{Duration, Instant};

fn title(style: &str, text: &str) -> TextDoc {
    kadr_text::styles::by_id(style).expect("a built-in style").new_doc(text, 1080)
}

/// Frames `i` of 200, 1/30 s apart from `t0`, each a new picture (no cache).
fn frames(docs: &[TextDoc], t0_ms: i64, pool: &FramePool) -> Stats {
    let mut samples: Vec<Duration> = vec![];
    for i in 0..200 {
        let t = Time::from_millis(t0_ms + i * 1_000 / 30 % 600);
        let started = Instant::now();
        for d in docs {
            let f = TextFrame::new(d, Clock { since_in: t, until_out: Time::from_secs(4), source: t, highlight: 0.4 });
            std::hint::black_box(f.render(1.0, pool));
        }
        samples.push(started.elapsed());
    }
    Stats::of(samples)
}

pub fn run() -> Result<Report, String> {
    let mut r = Report::new("text");
    let pool = FramePool::new(256 << 20);
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    // A static title: its first raster (what a cache miss costs), 50 distinct texts.
    let mut first = vec![];
    for i in 0..50 {
        let d = title("title", &format!("Заголовок номер {i}"));
        let started = Instant::now();
        std::hint::black_box(TextFrame::new(&d, Clock::REST).render(1.0, &pool));
        first.push(started.elapsed());
    }
    let s = Stats::of(first);
    for (m, v) in [("p50", s.p50), ("p90", s.p90), ("max", s.max)] {
        r.push("text", "static title first raster 1080p", m, ms(v), "ms");
    }
    let mut rise = title("gradient", "Победа нашей команды");
    rise.anim.reveal_in = kadr_text::anim::reveal("rise").map(|x| x.fresh());
    let neon = title("neon_pink", "НЕОН ВЕЧЕРИНКА");
    let mut shadowed = neon.clone();
    shadowed.style.shadow = Some(kadr_project::Shadow { colour: kadr_project::Rgba8::rgba(0, 0, 0, 200), offset_x: 6.0, offset_y: 8.0, blur: 12.0 });
    let lower = title("lower_third_bar", "Анна Иванова\nрежиссёр");
    for (name, docs) in [
        ("animated title per frame 1080p", vec![rise.clone()]),
        ("neon + shadow per frame 1080p", vec![shadowed]),
        ("3 animated titles per frame 1080p", vec![rise, neon, lower]),
    ] {
        let s = frames(&docs, 0, &pool);
        for (m, v) in [("p50", s.p50), ("p90", s.p90), ("max", s.max)] {
            r.push("text", name, m, ms(v), "ms");
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_small_run_draws_every_row() {
        let r = super::run().unwrap();
        for row in ["static title first raster 1080p", "animated title per frame 1080p", "neon + shadow per frame 1080p", "3 animated titles per frame 1080p"] {
            assert!(r.rows.iter().any(|x| x.case == row), "{row}");
        }
    }
}
```

`scene_bench.rs`: `fn titles_project() -> Project` = `project()` (its three video layers) plus a V4 with three 10 s text clips (`title`, `lower_third_bar`, `neon_pink`, each animated: rise, slide, pulse) staggered over the 60 s, and the row `r.push("scene", "3 animated titles over video", metric, us(v), "µs")` with a test that `evaluate` at 5 s returns the video layers plus `LayerContent::Text` ones.

Run: `cargo test -p kadr-bench && cargo run --release -p kadr-bench -- text && cargo run --release -p kadr-bench -- scene` (each bench three times).
Expected: tests PASS; the rows print.

- [ ] **Step 2: Full suite and clippy**

Run: `cargo test --workspace --exclude kadr-mcp && cargo test -p kadr-mcp && cargo clippy --workspace --all-targets`
Expected: exit 0; no new warnings.

- [ ] **Step 3: The perf record** — `docs/perf/2026-10-12-text.md` (Russian, as the E-records): **1. Машина**; **2. Растр текста** — the four rows, three runs (p50 / p90 / max ms); **3. Вычислитель** — «3 animated titles over video» against the 100 µs bar; **4. Пиксели** — the tests that hold the picture (Task 3 / 9 / 10 goldens, Task 4 `text_pixels`, Task 10 split / trim); **5. Вердикт** — T-D13: an animated title ≤ 4 ms per 1080p frame (p90 of «animated title per frame»), three titles within the frame budget (33.3 ms at 30 fps) with the video's own render time (`kadr-bench render` «1 layer» p90 + the three titles' p90), and the live playback numbers of Step 4 item 13. A missed budget: say which, profile (`glyph_image` cache hit rate, blur passes), and stop for the owner; **6. Живая приёмка** — Step 4's items with results and the owner's verdicts.

- [ ] **Step 4: Live acceptance through `kadr-mcp`** (controller; release build started headless per the testing lessons; the owner's clips in `C:\Users\Boris\Desktop\000-Brazil` — landscape `Tribal_Samba_…mp4` 1080p30, vertical `Brazil.mp4`, VFR `Neymar(3-10).mp4`, the alpha logo — never modified; scratch projects and exports in `%TEMP%\kadr-text\`; RU and EN; the window at 1024 × 640 and maximized). Record each item in §6, with `get_frame` frames where visual:
  1. **Add**: «Т» on the toolbar, Ctrl+T, Правка → «Добавить текст», «＋ Добавить текст» in the tab — each one undo entry «Добавление текста», a purple clip with «Т» and its text on the topmost free video track (a new track when the top one is taken at the playhead), 5 s, selected, typing open on the frame with «Ваш текст» selected; typing replaces it live; Esc finishes; one more undo entry «Правка текста».
  2. **Keys while typing** (Review Focus 1): in the preview field and in the inspector field, Space types a space (no playback), Delete deletes a letter (not the clip), Ctrl+Z undoes typing inside the field (the project history unchanged), arrows move the caret; Esc finishes first (the selection stays); Ctrl+S during typing saves the typed text (reopen the file to check); Ctrl+T during typing finishes it and adds a new text; clicking the timeline finishes it.
  3. **On the preview**: a click on a clip's picture selects it; the frame and handles at fit, 150 % and 250 %; a drag moves it with guides at the centre, the edges, the 90 % / 93 % lines and another text's edges (Shift: none); a corner scales about the centre; the round handle rotates in 15° steps; each drag one undo entry («Перемещение» / «Масштаб» / «Поворот»), a click none; with X keyframed, a drag sets a keyframe at the playhead; no handles while playing; a video clip gets the same frame.
  4. **Inspector**: font list with bundled / mine / system, names drawn in their fonts, search; «Добавить шрифт…» with a .ttf from `C:\Windows\Fonts` copied to a temp folder first (never the owner's files) — it appears under «Мои» and survives a restart; size, weight, italic, colour (picker: square, hue, alpha, hex, presets, recent), two outlines, shadow, glow, plaque (block / lines), paragraph, ALL CAPS, wrap; each slider drag one entry, a release in place none.
  5. **Style a selection**: select «мир» in «Привет, мир!» → the scope says «выделения «мир»»; colour, font, size, weight, italic change only it; the field loses focus and the scope stays; «весь текст» clears it; «Сбросить стиль выделения» restores it; typing keeps the styles of untouched words.
  6. **Cyrillic, emoji, fallback** (Review Focus 3): «Гол! ⚽🔥 日本 ✓» in every bundled font — the emoji in colour, no boxes; a hand-edited project naming «Nope Sans» opens, renders in Montserrat, the inspector warns «Шрифт «Nope Sans» не найден — показан Montserrat», saving keeps the name.
  7. **Looks on real material**: on `Tribal_Samba` the gradient, neon glow, hard and soft shadows, plaque per line, outline-only — readable and «дорого»; an arc (both ways), a circle and both tilts; shown to the owner, verdicts recorded; tuning happens in `styles.json` / the constants only, then re-bless and look.
  8. **Animations**: every reveal (in and out) by letters, words, lines and whole; every loop; the highlight walking the words; «Показать» plays the clip and returns the playhead; each looked at on real material; an animation longer than the clip shrinks.
  9. **Split and trim** (Review Focus 5): split an animated title during its loop — the frames around the cut are identical (`get_frame` before / after the split at the same times); the in stays on the left piece, the out on the right; trimming the start moves the in with the edge; trimming the start before the text's 0 works.
  10. **«Текст» tab**: 22 styles with thumbnails in RU and EN (the thumbnails follow the language), chips filter, hover with a text selected tries the style on (the project, history and «изменён» untouched; leaving restores), a click applies («Стиль текста: Неон розовый», the text kept), without a selection adds a new text of that style at its place; a drag onto V2 at 10 s drops it there; onto A1 refused with the reason; «Сохранить стиль» → «Мои» → survives a restart → delete it from its card.
  11. **Export**: a 10 s 1080p export and a 4K export of a sequence with three titles (static, animated, neon) over video: frames extracted with ffmpeg match `get_frame` at those times; the 4K title is sharp (Review Focus 2); the colours are the chosen ones.
  12. **MCP / AI**: `list_text_styles`; `edit add_text {at_ms, text, style: "lower_third_bar"}` places it left-low; `set_text {clip_id, color: "#ffd400", font: "Oswald"}`; a repeated `set_text` is rejected as no change; `add_caption {start_ms, end_ms, text}` makes a bottom caption; `get_state` shows `asset: null` and `text`; the assistant's «добавь заголовок „Финал“ на 5 секунде» proposes `add_text`, applying it is one entry.
  13. **Performance**: `get_perf` during playback of 1080p `Tribal_Samba` with three animated titles against the same without — frame time p90 and dropped frames (the verdict of Step 3); the tab's thumbnails and font previews never render during playback.
  14. **Small and large window**: at 1024 × 640 and maximized, the inspector's text section, the font list, the colour popup and the tab are unclipped in RU and EN.

- [ ] **Step 5: Commit and review**

```bash
git add apps/kadr-bench docs/perf/2026-10-12-text.md ARCHITECTURE.md
git commit -m "titles: text bench rows, perf record and live acceptance

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Then a whole-branch review with an Opus reviewer: the spec (§1–§9), T-D1…T-D14 with this plan's refinements, the five Review Focus items, format 4 and forward compatibility (`extra` round trips), the scene contract («Text»: sampled like media, nothing without a raster, never occluding), CPU / reference parity, i18n completeness (both catalogs; the editor's run-time key test), Slint model stability (no rebuilt rows while typing; the field's caret never resets), undo granularity (one entry per gesture, none for no change), fonts' licences; fix what it confirms before E3 Tasks 5–10 resume.

