# Jev Decision Service + Multicam — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Kadr grades shots (KEEP / REVIEW / DISCARD), cuts multicam edits between angles, and predicts the editor's own choices from their past corrections — all through Jev typed decisions on locally extracted features.

**Architecture:** Four layers, each testable alone.
1. *Local analysis* (`kadr-analysis`): frame statistics → shots; audio-envelope sync for multicam. No network.
2. *Multicam editing* (`kadr-project`, `kadr-timeline`, editor): a multicam edit is a chain of ordinary clips carrying `Clip.multicam`; switching angle = `EditCommand::SwitchAngle` that swaps `asset`/source range by the group's sync offsets. Preview, export and transitions work unchanged.
3. *Jev decision service* (`kadr-ai::jev`): templates → batched `/v1/systemone` calls → `Decided` records with gates, cached in the project for reproducibility; precedent selection + frequency prior for personalisation.
4. *Editor UI*: cost/consent card (same rules as cloud chat), shot badges and overrides, angle viewer with live switching, "auto-cut cameras" plan, corrections logged as preference events.

**Tech Stack:** Rust 2024, Slint 1.18, FFmpeg CLI, tokio (AI runtime only), serde_json, blake3.

**Spec:** `mds/jev-research.md` (§4.3, §5a–c, §6, §7) and `mds/tesk.md` (multicam, AI sections).

## Global Constraints

- Nothing leaves the machine without `privacy::gate` → `Allow`, or `Ask` + explicit user confirmation; every Jev call passes `cost::check_budget` first.
- Only text features go to Jev: bucketed words ("sharp / soft / very blurry"), never media, file paths or raw numbers (research §4, §6.2).
- `instructions` / `criteria` in English; every Choice has an escape option (`REVIEW` / `UNSURE_NEW_SITUATION`) (research §1.4).
- Thresholds are on `p_max` and `margin = p1 − p2`, never on API `confidence` (research §0).
- Default Jev model pinned to `jev-1.13.0` (research §9.9).
- All UI strings in `crates/i18n/locales/{en,ru}.json`; RU plural keys `.one/.few/.many`, EN `.one/.other`; `crates/i18n/tests/sources.rs` must stay green.
- Non-destructive: DISCARD only marks; AI camera cuts arrive as a reviewable Plan and are one undo step.
- Run cargo as `PATH="$HOME/.cargo/bin:$PATH" cargo …` (Git Bash). The user may have `target/debug/kadr.exe` running: before `cargo build -p kadr-editor` rename it (`mv target/debug/kadr.exe target/debug/kadr-old-$RANDOM.exe`); never kill their process.

## Review Focus

1. Angles with different lengths / late starts: switching to an angle that has no media at that time must fail with `EditError::InvalidRange`, not produce a clip with negative `source_in`. → Task 5 test `switch_to_angle_without_media_fails`.
2. Jev answers missing a question id or with an unknown option (API drift): the item becomes `Gate::Review`, never panics or auto-applies. → Task 3 test `missing_or_unknown_answer_becomes_review`.
3. Repeated runs must not "blink": identical features → cached decision, no network call; probability jitter < 0.05 keeps the old gate. → Task 3 test `hysteresis_keeps_gate`.
4. Projects saved before this change (no `jev_decisions`, no angle `description`, no `decision` on events) must still open. → Task 1 test `old_project_json_still_loads`.
5. Silent / music-only clips for audio sync: envelopes with no correlation peak must report low confidence and fall back to timecode or manual (offset 0), not a random offset. → Task 6 test `flat_envelopes_report_low_confidence`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/project/src/decisions.rs` (new) | `DecisionId`, `StoredDecision`, `DecisionKind`, `Gate` — persisted Jev results |
| `crates/project/src/model.rs` | `Project.jev_decisions`, `MulticamAngle.description` |
| `crates/project/src/ai_log.rs` | `EditorPreferenceEvent.decision: Option<DecisionId>` |
| `crates/analysis/src/video.rs` (new) | `FrameStats`, `VideoOverview`, `analyze_gray_frame`, `detect_shots`, buckets |
| `crates/analysis/src/sync.rs` (new) | `align_envelopes` (cross-correlation of 10 ms dB levels), `timecode_offset` |
| `crates/timeline/src/multicam.rs` (new) | `angle_source()` mapping, used by `SwitchAngle` |
| `crates/timeline/src/commands.rs` | `EditCommand::SwitchAngle`, `EditContext.multicam` |
| `crates/ai/src/providers/jev.rs` | `serde_json::Value` state/criteria, local limits validation, error mapping |
| `crates/ai/src/jev/mod.rs` (was `jev.rs`) | re-exports; `ShotFeatures` etc. move to templates |
| `crates/ai/src/jev/decided.rs` | `Decided`, `decide_gate()` thresholds, `from_answer()` |
| `crates/ai/src/jev/service.rs` | `JevDecisionService`: estimate, batch split, transport, cache, hysteresis |
| `crates/ai/src/jev/templates.rs` | usability / camera-fit / editor-pick state + questions (prompt versions) |
| `crates/ai/src/jev/prefs.rs` | precedent selection, frequency prior, `blend()` |
| `crates/ai/src/command.rs` | `SelectCamera` validated → `EditCommand::SwitchAngle` batch |
| `apps/editor/src/video_analysis.rs` (new) | analysis job per asset (stream 4 fps 160×90 gray) |
| `apps/editor/src/multicam_ui.rs` (new) | create group, insert, angle viewer, live switching, corrections |
| `apps/editor/src/jev_ui.rs` (new) | consent/cost card, run service, apply results, overrides → outcomes |
| `apps/editor/ui/multicam.slint` (new) | angle strip under preview, group dialog |
| `apps/editor/ui/media.slint`, `inspector.slint`, `timeline.slint`, `ai.slint` | badges, shot list, clip quality strip, Jev card |

---

### Task 1: Persisted decision model + multicam fields

**Files:** Create `crates/project/src/decisions.rs`; modify `crates/project/src/{lib.rs,model.rs,ai_log.rs}`, `crates/core/src/id.rs`; test `crates/project/tests/serialization.rs`.

**Interfaces — Produces:**
```rust
// kadr_core::id — add DecisionId to the id macro list
pub struct DecisionId(Uuid);

// kadr_project::decisions
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind { ShotUsability, CameraPick }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate { AutoApply, Suggest, Review }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredDecision {
    pub id: DecisionId,
    pub kind: DecisionKind,
    /// blake3 of canonical {model, prompt_version, item state, question}.
    pub key: String,
    /// What it is about: "asset:<uuid>:shot:3" / "clip:<uuid>@12000".
    pub subject: String,
    pub value: String,
    pub probs: BTreeMap<String, f32>,
    pub p_max: f32,
    pub margin: f32,
    pub gate: Gate,
    pub model: String,
    pub prompt_version: String,
    pub at_ms: i64,
    /// Bucketed features that were sent (for precedents and audits).
    pub features: serde_json::Value,
    /// Set when the human overrode it.
    #[serde(default)]
    pub human: Option<String>,
}
// Project: #[serde(default)] pub jev_decisions: Vec<StoredDecision>
// MulticamAngle: #[serde(default)] pub description: String   // "wide stage", "singer close-up"
// EditorPreferenceEvent: #[serde(default)] pub decision: Option<DecisionId>
```

- [ ] **Step 1: failing test** — append to `crates/project/tests/serialization.rs`:
```rust
#[test]
fn old_project_json_still_loads() {
    // A project written before decisions/angle descriptions existed.
    let mut v = serde_json::to_value(kadr_project::Project::new("old")).unwrap();
    v.as_object_mut().unwrap().remove("jev_decisions");
    v["multicam_groups"] = serde_json::json!([{"id": kadr_core::MulticamId::new(), "name": "g",
        "angles": [{"asset": kadr_core::AssetId::new(), "label": "CAM1", "sync_offset": 0}]}]);
    let p: kadr_project::Project = serde_json::from_value(v).unwrap();
    assert!(p.jev_decisions.is_empty());
    assert_eq!(p.multicam_groups[0].angles[0].description, "");
}
```
(check `Project::new` name and `Time` serialisation in `crates/project/src/model.rs` / `kadr_core::Time` before running; adapt the literal for `sync_offset` to however `Time` serialises.)
- [ ] **Step 2:** `cargo test -p kadr-project old_project_json_still_loads` → FAIL (no field).
- [ ] **Step 3:** add `DecisionId` to the id list in `crates/core/src/id.rs`, create `decisions.rs` with the types above, `pub mod decisions; pub use decisions::*;` in `lib.rs`, add the three `#[serde(default)]` fields, and initialise `jev_decisions: vec![]` in `Project::new`; `description: String::new()` wherever `MulticamAngle` is constructed; `decision: None` in the two `EditorPreferenceEvent` literals in `apps/editor/src/ai_ui.rs`.
- [ ] **Step 4:** `cargo test -p kadr-project && cargo check --workspace` → PASS.
- [ ] **Step 5:** commit `project: persisted Jev decisions, angle descriptions`.

---

### Task 2: Jev transport hardening

**Files:** modify `crates/ai/src/providers/jev.rs`; move `crates/ai/src/jev.rs` → `crates/ai/src/jev/mod.rs`.

**Interfaces — Produces:**
```rust
pub const DEFAULT_MODEL: &str = "jev-1.13.0";
pub enum JevQuestion {
    Noul { instructions: String },
    Choice { instructions: String, criteria: BTreeMap<String, serde_json::Value> },
    Score { instructions: String, criteria: Vec<serde_json::Value> },
}
impl JevQuestion { pub fn validate(&self) -> Result<(), AiError>; } // Choice 1..=255, Score 2..=10
pub trait JevTransport: Send + Sync {
    fn decide<'a>(&'a self, model: &'a str, state: &'a serde_json::Value, q: &'a BTreeMap<String, JevQuestion>)
        -> BoxFuture<'a, Result<JevResponse, AiError>>;
}
impl JevTransport for JevProvider { … }
// AiError gains: TooLarge (400 max_tokens_exceeded), UnknownModel(String)
```
`JevProvider::decide` keeps its token pre-check, sends `state` as a JSON value, and maps 400 bodies containing `max_tokens_exceeded` → `AiError::TooLarge`, `Unknown model` → `AiError::UnknownModel`. `settings_ui.rs` connectivity test passes `&json!("Connectivity test.")`. `settings.rs` default price list already has `jev-1.13.0`.

- [ ] **Step 1: failing tests** in `providers/jev.rs` `mod tests`:
```rust
#[test]
fn local_limits_are_enforced() {
    let many: BTreeMap<String, serde_json::Value> = (0..256).map(|i| (format!("O{i}"), json!(null))).collect();
    assert!(JevQuestion::Choice { instructions: "i".into(), criteria: many }.validate().is_err());
    assert!(JevQuestion::Score { instructions: "i".into(), criteria: vec![json!("only")] }.validate().is_err());
    assert!(JevQuestion::Score { instructions: "i".into(), criteria: vec![json!("a"), json!({"what": "b"})] }.validate().is_ok());
}
#[test]
fn maps_documented_400s() {
    assert_eq!(map_400(r#"{"detail":{"error_type":"max_tokens_exceeded"}}"#), Some(AiError::TooLarge));
    assert!(matches!(map_400(r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: jev-9"}}"#), Some(AiError::UnknownModel(_))));
    assert_eq!(map_400("{}"), None);
}
```
- [ ] **Step 2:** run `cargo test -p kadr-ai providers::jev` → FAIL.
- [ ] **Step 3:** implement `validate`, `map_400(body: &str) -> Option<AiError>` (called from `decide` when `AiError::Http{status:400, body}` comes back from `send_json`), `JevTransport`, value-typed criteria/state; fix call sites (`examples/probe_jev.rs`, `tests/live_providers.rs`, `settings_ui.rs`).
- [ ] **Step 4:** `cargo test -p kadr-ai && cargo check --workspace --examples --tests` → PASS.
- [ ] **Step 5:** commit `ai: Jev transport — JSON state, local limits, error mapping, pinned model`.

---

### Task 3: `Decided`, gates, service core (batching, cache, hysteresis)

**Files:** create `crates/ai/src/jev/{decided.rs,service.rs}`; test inside modules.

**Interfaces — Produces:**
```rust
pub struct Decided { pub stored: StoredDecision, pub from_cache: bool }

/// Starting thresholds (research §7.4); `extra_ok` = local detector agrees (DISCARD) etc.
pub fn decide_gate(kind: DecisionKind, value: &str, p_max: f32, margin: f32, extra_ok: bool) -> Gate;

pub struct JevItem {            // one thing to decide
    pub kind: DecisionKind,
    pub subject: String,
    pub prompt_version: &'static str,
    pub state: serde_json::Value,          // item-local state, merged under "items[i]" by the batcher
    pub questions: BTreeMap<String, JevQuestion>,   // ids are local ("usability"), batcher prefixes "i3."
    pub primary: String,                   // which question yields `value`
    pub features: serde_json::Value,
    pub extra_ok: bool,
}
pub struct JevEstimate { pub calls: usize, pub input_tokens: u64, pub usd: f64, pub cached: usize }

pub struct JevDecisionService { transport: Arc<dyn JevTransport>, model: String, pricing: Pricing }
impl JevDecisionService {
    pub fn new(transport: Arc<dyn JevTransport>, model: String, pricing: Pricing) -> Self;
    pub fn cache_key(&self, item: &JevItem) -> String;              // blake3 hex
    pub fn estimate(&self, items: &[JevItem], cache: &[StoredDecision]) -> JevEstimate;
    /// Uncached items → batches (≤ 28k tokens shared state+longest q, ≤ 56k total, ≤ 40 items),
    /// halves a batch on AiError::TooLarge. Returns decisions in item order + actual usage.
    pub async fn run(&self, items: Vec<JevItem>, cache: &[StoredDecision], cancel: CancelToken)
        -> Result<(Vec<Decided>, u64 /*in*/, u64 /*out*/), AiError>;
}
/// Keeps the old gate unless p_max moved by > 0.05.
pub fn apply_hysteresis(old: Option<&StoredDecision>, new: &mut StoredDecision);
```
Batch `state` shape: `{"shared": <common context>, "items": [<item.state>, …]}`; each question's instructions reference `` `items[3]` ``. Answer parsing: `probs` from `probabilities` (Choice/Score) or `{"YES": noul, "NO": 1-noul}` (Noul); `p_max`, `margin` sorted; missing answer or value not among the question's options → `Gate::Review`, `value = "REVIEW"`.

- [ ] **Step 1: failing tests** (`service.rs`), with a fake transport:
```rust
struct Fake { calls: AtomicUsize, reply: Box<dyn Fn(&BTreeMap<String, JevQuestion>) -> BTreeMap<String, JevAnswer> + Send + Sync> }
impl JevTransport for Fake { /* counts calls, returns Ok(JevResponse{model:"jev-1.13.0", answers:(self.reply)(q), input_tokens:100, output_tokens:10}) */ }

fn usability_item(n: usize) -> JevItem { /* Choice KEEP/DISCARD/REVIEW, subject format!("s{n}") */ }

#[tokio::test] async fn batches_and_caches() {
    let fake = Arc::new(Fake::keep_all());   // every question → KEEP p=0.95
    let svc = JevDecisionService::new(fake.clone(), "jev-1.13.0".into(), Pricing { input_per_mtok: 0.042, output_per_mtok: 0.0 });
    let (d, _, _) = svc.run((0..50).map(usability_item).collect(), &[], CancelToken::new()).await.unwrap();
    assert_eq!(d.len(), 50);
    assert_eq!(fake.calls(), 2);                            // 40 + 10
    assert!(d.iter().all(|x| x.stored.value == "KEEP" && x.stored.gate == Gate::AutoApply));
    let cache: Vec<_> = d.into_iter().map(|x| x.stored).collect();
    let (d2, _, _) = svc.run((0..50).map(usability_item).collect(), &cache, CancelToken::new()).await.unwrap();
    assert_eq!(fake.calls(), 2);                            // all from cache
    assert!(d2.iter().all(|x| x.from_cache));
}
#[tokio::test] async fn missing_or_unknown_answer_becomes_review() { /* Fake returns {} for odd items, "MAYBE" for even → all Review */ }
#[tokio::test] async fn too_large_halves_the_batch() { /* Fake returns Err(TooLarge) when q.len() > 10 → still 40 decisions, ≥ 4 calls */ }
#[test] fn hysteresis_keeps_gate() { /* old AutoApply p=0.91, new Suggest p=0.89 → stays AutoApply; new p=0.80 → Suggest */ }
#[test] fn gate_thresholds() {
    assert_eq!(decide_gate(DecisionKind::ShotUsability, "KEEP", 0.93, 0.6, false), Gate::AutoApply);
    assert_eq!(decide_gate(DecisionKind::ShotUsability, "DISCARD", 0.97, 0.9, false), Gate::Suggest); // detector disagrees
    assert_eq!(decide_gate(DecisionKind::ShotUsability, "DISCARD", 0.97, 0.9, true), Gate::AutoApply);
    assert_eq!(decide_gate(DecisionKind::CameraPick, "CAM2", 0.6, 0.25, false), Gate::Suggest);
    assert_eq!(decide_gate(DecisionKind::CameraPick, "CAM2", 0.4, 0.1, false), Gate::Review);
}
```
(add `tokio` `rt` + `macros` to kadr-ai dev-deps if not already available through the normal dep.)
- [ ] **Step 2:** `cargo test -p kadr-ai jev::` → FAIL.
- [ ] **Step 3:** implement per the interfaces. Thresholds (research §7.4): KEEP auto p≥0.90∧margin≥0.5, suggest ≥0.70; DISCARD auto p≥0.95∧extra_ok, suggest ≥0.80; REVIEW value → Review; CameraPick auto p≥0.75∧margin≥0.3, suggest ≥0.5. Cancellation: check `cancel` between batches and race each call like `Assistant::run_cloud`.
- [ ] **Step 4:** `cargo test -p kadr-ai` → PASS.
- [ ] **Step 5:** commit `ai: JevDecisionService — batching, cache, gates, hysteresis`.

---

### Task 4: Local video analysis → shots

**Files:** create `crates/analysis/src/video.rs`; modify `crates/analysis/src/lib.rs`, `crates/project/src/analysis.rs` (`AnalysisData::Shots`).

**Interfaces — Produces:**
```rust
pub const VIDEO_VERSION: u32 = 1;
pub const ANALYSIS_W: u32 = 160; pub const ANALYSIS_H: u32 = 90; pub const ANALYSIS_FPS: u32 = 4;
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameStats { pub luma: f32 /*0..1*/, pub sharpness: f32 /*Laplacian variance, normalised*/, pub motion: f32 /*mean |Δ| vs prev, 0..1*/, pub black: bool }
pub struct VideoOverview { pub fps: u32, pub frames: Vec<FrameStats> }   // to_bytes / from_bytes like AudioOverview
pub fn analyze_gray_frame(gray: &[u8], w: u32, h: u32, prev: Option<&[u8]>) -> FrameStats;
pub fn rgba_to_gray(rgba: &[u8]) -> Vec<u8>;
// kadr_project::analysis
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotSummary { pub range: TimeRange /*source time*/, pub sharpness: String, pub exposure: String, pub shake: String, pub black: bool }
// AnalysisData::Shots { shots: Vec<ShotSummary> }
pub fn detect_shots(ov: &VideoOverview, min_len: Time) -> Vec<ShotSummary>;  // cut where motion spikes > 0.35 and > 3× local median
pub fn bucket_sharpness(s: f32) -> &'static str;  // "sharp" | "soft" | "very blurry"
pub fn bucket_exposure(l: f32) -> &'static str;   // "black" | "dark" | "normal" | "blown out"
pub fn bucket_shake(m: f32) -> &'static str;      // "none" | "slight" | "heavy"   (median motion inside the shot)
```
Bucket words are chosen to match Task 7's Score levels literally (research §5b risk).

- [ ] **Step 1: failing tests** in `video.rs`:
```rust
fn checker(w: u32, h: u32, cell: u32, shift: u32) -> Vec<u8> { (0..w*h).map(|i| { let (x, y) = (i % w + shift, i / w); if (x / cell + y / cell) % 2 == 0 { 230 } else { 20 } }).collect() }
fn flat(w: u32, h: u32, v: u8) -> Vec<u8> { vec![v; (w*h) as usize] }
#[test] fn sharp_beats_flat_and_black_is_black() {
    let s = analyze_gray_frame(&checker(160, 90, 4, 0), 160, 90, None);
    let f = analyze_gray_frame(&flat(160, 90, 120), 160, 90, None);
    let b = analyze_gray_frame(&flat(160, 90, 3), 160, 90, None);
    assert!(s.sharpness > 10.0 * f.sharpness.max(1e-6));
    assert!(b.black && !s.black);
    assert_eq!(bucket_sharpness(s.sharpness), "sharp");
    assert_eq!(bucket_sharpness(f.sharpness), "very blurry");
}
#[test] fn motion_measures_change() {
    let a = checker(160, 90, 8, 0);
    assert!(analyze_gray_frame(&checker(160, 90, 8, 4), 160, 90, Some(&a)).motion > 0.2);
    assert!(analyze_gray_frame(&a, 160, 90, Some(&a)).motion < 0.01);
}
#[test] fn cuts_split_shots() {
    let calm = FrameStats { luma: 0.5, sharpness: 0.5, motion: 0.02, black: false };
    let mut frames = vec![calm; 40];
    frames[20].motion = 0.8;                                 // hard cut at 5 s (4 fps)
    let shots = detect_shots(&VideoOverview { fps: 4, frames }, Time::from_secs(1));
    assert_eq!(shots.len(), 2);
    assert_eq!(shots[1].range.start, Time::from_secs(5));
    assert_eq!(shots[0].shake, "none");
}
#[test] fn overview_roundtrip() { /* to_bytes → from_bytes equality */ }
```
- [ ] **Step 2:** `cargo test -p kadr-analysis video` → FAIL.
- [ ] **Step 3:** implement. Sharpness = variance of 4-neighbour Laplacian / 255², luma mean /255, motion = mean |a−b| /255, black = luma < 0.04 ∧ sharpness tiny. Thresholds for buckets calibrated on the synthetic tests + one real clip in Task 8.
- [ ] **Step 4:** `cargo test -p kadr-analysis -p kadr-project` → PASS.
- [ ] **Step 5:** commit `analysis: frame statistics and shot detection`.

---

### Task 5: Multicam model operations — `SwitchAngle`

**Files:** create `crates/timeline/src/multicam.rs`; modify `crates/timeline/src/{commands.rs,engine.rs,lib.rs}`; tests `crates/timeline/tests/engine.rs`.

**Interfaces — Produces:**
```rust
// EditContext gains: pub multicam: &'a [MulticamGroup]
// EditCommand gains: SwitchAngle { clip: ClipId, angle: u32 }   label "cmd.switch_angle"
/// Source range for `clip`'s timeline span on `angle`: group time g ↔ angle source time g + sync_offset.
pub fn angle_source(group: &MulticamGroup, angle: u32, clip: &Clip, assets: &[MediaAsset]) -> Result<(AssetId, TimeRange), EditError>;
/// Group time shown at the clip start (inverse of the current angle mapping).
pub fn group_time_at_clip_start(group: &MulticamGroup, clip: &Clip) -> Option<Time>;
/// Clip for a whole group placed at `at` on the timeline using `angle`.
pub fn group_clip(group: &MulticamGroup, angle: u32, assets: &[MediaAsset], at: Time) -> Option<Clip>;
/// Common group-time span where the group has any angle with media.
pub fn group_span(group: &MulticamGroup, assets: &[MediaAsset]) -> TimeRange;
```
`SwitchAngle` keeps `timeline_in/out`, sets `asset`, `source_in/out`, `name = angle.label`, `multicam.angle`; errors `InvalidRange` if the angle's media doesn't cover the span; `NoOp` if already on that angle. Engine passes `&project.multicam_groups`.

- [ ] **Step 1: failing tests** (`crates/timeline/tests/engine.rs`):
```rust
fn multicam_project() -> (Project, ClipId) {
    // Two 60 s angles; CAM2 started recording 2 s later (sync_offset −2 s means group t = src t + 2).
    // Build: assets A (60 s), B (60 s); group angles [A offset 0, B offset -2 s];
    // clip = group_clip(group, 0, assets, 0) trimmed to group time 10..20 s at timeline 0.
}
#[test] fn switch_angle_keeps_timing_and_maps_source() {
    let (mut p, clip) = multicam_project();
    let mut e = EditEngine::new();
    e.execute(&mut p, EditCommand::SwitchAngle { clip, angle: 1 }).unwrap();
    let c = p.sequence().clip(clip).unwrap();
    assert_eq!((c.timeline_in, c.timeline_out), (Time::ZERO, Time::from_secs(10)));
    assert_eq!(c.source_in, Time::from_secs(8));                 // group 10 s on CAM2
    assert_eq!(c.multicam.as_ref().unwrap().angle, 1);
    e.undo(&mut p).unwrap();
    assert_eq!(p.sequence().clip(clip).unwrap().source_in, Time::from_secs(10));
}
#[test] fn switch_to_angle_without_media_fails() {
    // clip spans group 0..10 s; CAM2 has media only from group 2 s → InvalidRange, sequence unchanged
}
#[test] fn split_then_switch_is_a_live_cut() {
    // Batch[Split at 5 s, SwitchAngle(right half, 1)] → two clips, left CAM1 right CAM2, one undo step
}
```
(Use the real helper names in `crates/timeline/tests/engine.rs`; `Split` returns new clip ids via the sequence — find the right half with `track.clip_at(5 s)`. The live-cut command in the UI is built the same way, see Task 9.)
- [ ] **Step 2:** `cargo test -p kadr-timeline` → FAIL.
- [ ] **Step 3:** implement `multicam.rs` + command arm. Convention (already implied by the `MulticamAngle.sync_offset` doc comment "group time 0 corresponds to this source time"): **`source_time = group_time + sync_offset`**; an angle that started recording 2 s late has `sync_offset = −2 s`. Task 6 converts `SyncResult.offset` into this form: `angle_b.sync_offset = angle_a.sync_offset − result.offset`.
- [ ] **Step 4:** `cargo test -p kadr-timeline -p kadr-ai` → PASS.
- [ ] **Step 5:** commit `timeline: multicam angle switching`.

---

### Task 6: Multicam sync (audio envelopes, timecode)

**Files:** create `crates/analysis/src/sync.rs`; modify `lib.rs`.

**Interfaces — Produces:**
```rust
pub struct SyncResult { pub offset: Time /* b relative to a: b_src = a_src - offset */, pub confidence: f32 /*0..1*/ }
/// Normalised cross-correlation of 10 ms dB envelopes (silence floor −60 dB), search ±max_offset.
pub fn align_envelopes(a_db: &[f32], b_db: &[f32], window: Time, max_offset: Time) -> SyncResult;
/// From embedded start timecodes ("01:00:05:12") at `rate`.
pub fn timecode_offset(a_tc: &str, b_tc: &str, rate: FrameRate) -> Option<Time>;
```
Confidence = (peak − second-best outside ±100 ms) / peak, clamped; < 0.15 means "unreliable".

- [ ] **Step 1: failing tests**:
```rust
fn envelope(n: usize, seed: u64) -> Vec<f32> { /* deterministic pseudo-random loud/quiet pattern, −60..−6 dB */ }
#[test] fn finds_known_shift() {
    let a = envelope(6000, 7);                     // 60 s
    let b: Vec<f32> = a[137..].to_vec();          // b starts 1.37 s later
    let r = align_envelopes(&a, &b, Time::from_millis(10), Time::from_secs(10));
    assert_eq!(r.offset, Time::from_millis(1370));
    assert!(r.confidence > 0.5);
}
#[test] fn flat_envelopes_report_low_confidence() {
    let r = align_envelopes(&vec![-60.0; 3000], &vec![-60.0; 3000], Time::from_millis(10), Time::from_secs(10));
    assert!(r.confidence < 0.15);
}
#[test] fn timecode_difference() {
    assert_eq!(timecode_offset("01:00:00:00", "01:00:02:12", FrameRate::FPS_25), Some(Time::from_millis(2480)));
}
```
- [ ] **Step 2–4:** FAIL → implement (O(n·k) direct correlation on ≤ 10 min is fine: 60 000 × 2 000 lags; if slower than 1 s in release, decimate to 50 ms first then refine ±60 ms at 10 ms) → PASS.
- [ ] **Step 5:** commit `analysis: multicam sync by audio envelope and timecode`.

---

### Task 7: Jev templates + preferences (personalisation)

**Files:** create `crates/ai/src/jev/{templates.rs,prefs.rs}`.

**Interfaces — Produces:**
```rust
pub const USABILITY_V: &str = "shot_usability/v1";
pub const CAMERA_V: &str = "camera_pick/v1";
pub fn shot_item(subject: String, shot: &ShotSummary, duration: Time, speech: &str /*"speech"|"music"|"silence"*/) -> JevItem;
pub struct AngleFeatures { pub label: String, pub description: String, pub sharpness: String, pub exposure: String, pub shake: String, pub audio_level: String /*"loudest"|"quiet"|"silent"*/ }
pub struct CameraInterval { pub subject: String, pub time_label: String, pub angles: Vec<AngleFeatures>, pub previous: Option<(String, Time)>, pub context: serde_json::Value }
/// Camera pick with optional editor history → questions: best_cam (Choice over labels + UNSURE),
/// fit_<label> (Score, 4 levels), and, when history is non-empty, editor_pick + has_precedent (Noul).
pub fn camera_item(iv: &CameraInterval, history: &[Precedent]) -> JevItem;

// prefs.rs
pub struct Precedent { pub context: serde_json::Value, pub ai_suggested: String, pub editor_chose: String, pub at_ms: i64 }
pub fn precedents(events: &[EditorPreferenceEvent], decisions: &[StoredDecision], kind: DecisionKind, current: &serde_json::Value, k: usize) -> Vec<Precedent>;
pub fn frequency_prior(precedents: &[Precedent], options: &[String]) -> BTreeMap<String, f32>;  // Dirichlet α = 1
pub fn blend_weight(n_precedents: usize) -> f32;      // weight of Jev vs frequency prior
pub fn blend(p_jev: &BTreeMap<String, f32>, p_freq: &BTreeMap<String, f32>, w: f32) -> BTreeMap<String, f32>;
```
Similarity for `precedents`: count of equal leaf values between `context` objects, times `0.5^(age_days/30)`; top-k (k = 16) returned in chronological order; ~⅔ overrides (`ai_suggested != editor_chose`) when available.
When `has_precedent < 0.5`, the camera decision uses `best_cam` only; otherwise `editor_pick` blended with the frequency prior via `blend(…, blend_weight(n))`.

- [ ] **Step 1: failing tests**:
```rust
#[test] fn prior_is_smoothed_frequency() {
    let p = |c: &str| Precedent { context: json!({}), ai_suggested: "CAM3".into(), editor_chose: c.into(), at_ms: 0 };
    let f = frequency_prior(&[p("CAM1"), p("CAM1"), p("CAM3")], &["CAM1".into(), "CAM2".into(), "CAM3".into()]);
    assert!((f["CAM1"] - 0.5).abs() < 1e-6 && (f["CAM2"] - 1.0 / 6.0).abs() < 1e-6);
}
#[test] fn precedents_prefer_similar_context() { /* 3 "guitar solo" + 20 "verse" events; current solo → first 3 of top-3 are solos */ }
#[test] fn camera_item_has_escape_option_and_history() {
    let it = camera_item(&interval_with(["CAM1", "CAM2"]), &[precedent()]);
    let JevQuestion::Choice { criteria, .. } = &it.questions["best_cam"] else { panic!() };
    assert!(criteria.contains_key("UNSURE"));
    assert!(it.questions.contains_key("editor_pick") && it.state["editor_history"].as_array().unwrap().len() == 1);
    assert!(!serde_json::to_string(&it.state).unwrap().contains(":\\"));   // no file paths
}
#[test] fn blend_endpoints() { /* w = 1 → p_jev; w = 0 → p_freq; sums to 1 */ }
```
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement. `blend_weight` is the one real policy knob — **user contribution** (see below); ship with a placeholder body only if the user declines, using `(4.0 / (4.0 + n as f32)).clamp(0.3, 1.0)`.
- [ ] **Step 4:** PASS; then a live check `cargo test -p kadr-ai --test live_providers -- --ignored jev_templates` (new ignored test: one shot item + one camera item against real Jev, asserts parse + probabilities sum ≈ 1; cost < $0.001).
- [ ] **Step 5:** commit `ai: Jev templates for shot usability and camera picks, editor precedents`.

---

### Task 8: Editor — video analysis job

**Files:** create `apps/editor/src/video_analysis.rs`; modify `import.rs` (`process_asset` submits it after thumbnails for video assets), `main.rs`, `app.rs` (`AssetRt.video: Option<Arc<VideoOverview>>`).

Job: `media.open_stream(StreamRequest { path, start: 0, width: 160, height: 90, rate: FrameRate{num:4,den:1}, speed: 1.0, look: VideoLook::default(), px_scale: 1.0 })`, loop `next_frame()` → `rgba_to_gray` → `analyze_gray_frame(prev)`, progress = i / (dur·4), cancel-aware; cache `video.bin` (`VIDEO_VERSION`); `post(app.on_video_ready(id, ov))` stores `AnalysisData::Shots { shots: detect_shots(&ov, 1 s) }` replacing older ones. Priority `Low`, category `"video"`, title key `jobs.title.video`.

- [ ] **Step 1:** add an integration test in `crates/media/tests/ffmpeg_integration.rs` that streams `make_clip` at 4 fps 160×90 and runs `analyze_gray_frame` over it: expects `frames ≈ 24`, all `!black`, sharpness bucket `"sharp"` (testsrc2 is crisp). This pins the real-media calibration of bucket thresholds.
- [ ] **Step 2–4:** FAIL → implement job + calibration → PASS; `cargo build -p kadr-editor`; launch with a real clip, check the job appears in the jobs popup and finishes.
- [ ] **Step 5:** commit `editor: background video analysis into shots`.

---

### Task 9: Editor — multicam groups, angle viewer, live switching

**Files:** create `apps/editor/src/multicam_ui.rs`, `apps/editor/ui/multicam.slint`; modify `library.rs` (multi-select with Ctrl/Shift+click → `selected_assets: Vec<AssetId>`, context menu `"multicam"`), `media.slint` (group cards), `preview.slint` (angle strip slot), `keys.rs` (`1`–`9` = cut to angle at playhead when the clip under playhead is multicam; otherwise unchanged), `persistence.rs` if needed; locales.

Flows:
1. *Create*: select ≥ 2 video assets → «Создать мультикам-группу». Sync order: timecode if all have it; else audio `align_envelopes` against angle 1 (needs audio overviews ready; if pending, toast `multicam.sync_pending` and retry when ready); confidence < 0.15 → offset 0 and a warning toast naming the angle. Dialog lists angles with label, description (editable text: «общий план сцены»), detected offset and method; «Создать».
2. *Insert*: group card → Insert/Overwrite at playhead places `group_clip` (angle 1) on V1 plus a linked audio clip from `master_audio` or angle 1.
3. *Angle viewer*: when the playhead clip has `multicam`, a strip under the preview shows every angle (thumbnail via `decode_frame` 192×108, latest-wins on seek/pause, refreshed ≤ 4 Hz during playback), label + description, active angle outlined in `Theme.accent`, keys 1–9 hinted.
4. *Live cut*: click/key during pause or playback → `Batch["cmd.cut_to_angle", [Split{at: playhead, clips: Some(vec![clip])}, SwitchAngle{right half, angle}]]`; at the clip start (no split needed) just `SwitchAngle`.
5. *Correction logging*: when the switched clip carries `decision: Some(id)` in `StoredDecision.subject` (set by Task 11), push `EditorPreferenceEvent { kind: CameraChanged, decision: Some(id), ai_choice, human_choice, context: stored.features, … }` and set `stored.human`.

- [ ] **Step 1:** unit test in `multicam_ui.rs` for the command builder `cut_to_angle_command(seq, clip, at, angle) -> Option<EditCommand>` (split in middle, no split at start, None when already on that angle).
- [ ] **Step 2–4:** FAIL → implement → PASS; build; run the app on 2–3 real clips (create group, insert, switch with keys, undo, export 10 s and check the cut appears) with PrintWindow screenshots of the angle strip and group dialog in RU and EN.
- [ ] **Step 5:** commit `editor: multicam groups, angle viewer and live switching`.

---

### Task 10: Editor — Jev consent card and shot grading

**Files:** create `apps/editor/src/jev_ui.rs`; modify `ai_ui.rs` (offer enum `OfferKind::{Chat(CloudOffer), Jev(JevRequest)}` sharing `OfferView`), `crates/ai/src/intent.rs` (`Intent::GradeShots`: «оцени кадры», «grade shots», «найди брак»), `library.rs` menu `"grade"`, `media.slint` badge, `inspector.slint` shot list, `timeline.slint` clip quality strip, locales.

`JevRequest { title, items: Vec<JevItem>, estimate: JevEstimate, gate: GateDecision, budget: BudgetDecision }`. Flow: build items from `AnalysisData::Shots` of the chosen assets (or all used on the timeline) → estimate against `project.jev_decisions` → card «Jev · 312 кадров · ~$0.004 · 12 из кэша» → Run (or auto-run when gate Allow and budget Allowed and `estimate.usd < per_request_usd`) → service on `assistant.runtime()` → `post` results: upsert `project.jev_decisions` with hysteresis, record ledger + `project.ai_cost`, `meta_dirty = true`.

UI: asset card badge (`N брак`, `M проверить`) coloured success/warning/danger; timeline clips get a 3 px strip coloured by the gate/value of shots under them; inspector for a selected asset/clip lists shots (time, buckets, verdict chip). Clicking the chip cycles KEEP → REVIEW → DISCARD → sets `stored.human`, pushes `EditorPreferenceEvent { kind: SegmentRestored | SegmentDeleted, decision: Some(id), ai_choice, human_choice, … }`.

- [ ] **Step 1:** tests: `intent::parse("оцени кадры") == Intent::GradeShots`; `jev_ui::shot_items(project, assets)` skips assets without shots and never includes paths.
- [ ] **Step 2–4:** FAIL → implement → PASS; build; live run with the stored key on real footage (privacy mode AskBeforeCloud): card shows cost, run, badges appear, override a verdict, save, reopen: decisions and override persist, second run is 100 % cache (0 calls in log).
- [ ] **Step 5:** commit `editor: Jev shot grading with consent card and overrides`.

---

### Task 11: Editor — Jev auto-cut cameras (personalised)

**Files:** modify `jev_ui.rs`, `multicam_ui.rs`, `crates/ai/src/{command.rs,intent.rs}`, `ai.slint` if needed, locales.

Flow: intent «смонтируй камеры» / multicam clip context menu «Авто-монтаж камер (Jev)» → intervals of 4 s over the selected multicam clip(s) (split at shot boundaries of the active angle when they fall within ±1 s) → `CameraInterval` per interval (angle buckets from each angle's `Shots` at the synced time, audio level from each angle's overview) → `precedents(...)` (k = 16) → `camera_item` → service → per interval pick: `editor_pick` blended with the frequency prior when `has_precedent ≥ 0.5`, else `best_cam`; `UNSURE`/Review → keep the current angle. Code rules after Jev: merge consecutive equal picks; minimum shot 2 s (merge shorter into the neighbour with higher p); never hold one angle > 20 s when another angle scores ≥ «good».
Result = standard AI `Plan` (tier Economy, provider Jev, cost) with `AiCommand::SelectCamera { start_ms, end_ms, angle: label, reason: "p=0.82 · like your picks in similar moments" }`; `command::validate` now converts SelectCamera → `Split` + `SwitchAngle` (was "Unsupported"). Apply = one undo step; each resulting clip's `StoredDecision.subject = "clip:<id>@<start_ms>"` so Task 9's correction logging finds it.

- [ ] **Step 1:** tests: `command::validate` accepts SelectCamera on a multicam clip and rejects unknown angle labels; `merge_picks` (pure fn in `jev_ui.rs`) enforces the 2 s minimum and merges equal neighbours.
- [ ] **Step 2–4:** FAIL → implement → PASS; live run on real multicam footage: plan card, Apply, override two cuts by hand, run again on another clip → state contains those precedents (check the debug log of the request body length / `editor_history` count), decisions shift accordingly.
- [ ] **Step 5:** commit `editor: personalised multicam auto-cut via Jev`.

---

### Task 12: Docs, settings, final verification

- [ ] `ARCHITECTURE.md` §11: JevDecisionService, decision cache, personalisation; §3 crate table (video analysis, multicam).
- [ ] Settings dialog: Jev model field shows `jev-1.13.0` default; «Учиться на моих правках» toggle (`AiSettings.jev_personalize: bool`, default true) gating precedents.
- [ ] `cargo test --workspace` and `cargo test -p kadr-ai --test live_providers -- --ignored` — paste outputs.
- [ ] Full manual pass in RU and EN at 1366×768 and maximised: screenshots of group dialog, angle strip, Jev card, badges, inspector shot list.
- [ ] Commit `docs: Jev decisions and multicam`.

---

## User contribution point (learning mode)

`crates/ai/src/jev/prefs.rs::blend_weight(n_precedents) -> f32` — how much to trust Jev's in-context guess vs. the plain frequency of your past picks. Research §4.3: Jev *sharpens* frequencies (3:2 history → 0.76/0.24) and becomes 1.00-certain on 5 uniform examples, while raw frequencies are noisy with few samples. It is 5–10 lines and decides how quickly the editor "gets your taste".
