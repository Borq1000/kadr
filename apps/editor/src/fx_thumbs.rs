//! Card thumbnails of the «Эффекты» tab (effects spec §6.1): the inspected
//! clip's frame at the playhead with the card's effect at «Обычно»,
//! rendered in the background at 160 × 90 and cached by (clip, source frame,
//! the clip's look, recipe, version, phase); a new batch cancels the
//! previous one, leaving the tab cancels it; a hovered motion card plays a
//! short loop of its motion. Without a clip a built-in sample picture
//! stands in (no decoding).
//!
//! One thread renders, one thumbnail at a time, from small one-clip copies
//! (the project is never touched) — the latest batch, after a short quiet
//! spell (a batch it has not started is replaced). It decodes through the
//! export-mode resolver MCP `get_frame` uses, never the preview's, so the
//! preview and playback are not slowed or superseded by it.

use crate::app::App;
use crate::effects_ui::visible_recipes;
use crate::scene_source::ProjectScenes;
use kadr_core::color::AlphaMode;
use kadr_core::{CancelToken, ClipId, ColorInfo, CpuFrame, EffectId, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_effects::{ParamKind, RecipeDef};
use kadr_playback::{Mode, Resolver, SceneSource};
use kadr_project::{AnimParam, AppliedEffect, Clip, EdgeFades, MediaAsset, ParamValue, Project, SourceTime, Track, TrackKey, TrackKind, Transform};
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{LayerContent, OutputSpec, RenderQuality, SizeU};
use std::collections::{HashMap, HashSet};
use std::hash::Hasher;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const THUMB_W: u32 = 160;
pub const THUMB_H: u32 = 90;
/// Frames of a hovered card's loop (~1 s at 12 fps).
pub const LOOP_FRAMES: u8 = 12;
pub const LOOP_TICK: Duration = Duration::from_millis(83);
/// The real time a loop spans, first frame to last (0.913 s).
pub const LOOP_SECS: f64 = (LOOP_FRAMES - 1) as f64 * LOOP_TICK.as_secs_f64();
/// Recipes whose motion runs on the effect's own clock (`progress ×
/// clip_secs`) rather than through the clip: their loop gets real time.
const ON_ITS_CLOCK: &[&str] = &["handheld"];
/// Quiet time before a batch renders: a newer one arriving meanwhile takes
/// its place, so paused playhead steps in a row render their last frame only.
const DEBOUNCE: Duration = Duration::from_millis(100);
/// The phase byte of a still thumbnail.
pub const STILL: u8 = u8::MAX;
/// A still shows the motion well under way.
const STILL_PHASE: f64 = 0.85;
/// Cached pictures (160 × 90 RGBA: ~17 MB at most).
const MAX_IMAGES: usize = 300;
const SAMPLE_W: u32 = 320;
const SAMPLE_H: u32 = 180;
/// The sample clip's frame the thumbnails show.
pub const SAMPLE_AT: Time = Time::from_secs(5);
/// The id every demo effect has: Handheld's shake is seeded by the id, so a
/// card's still is the same each time it renders and its loop is one motion.
const DEMO_EFFECT_ID: &str = "6b616472-0000-4000-8000-0000000000fe";

/// What the thumbnails show now: the inspected video clip at the playhead
/// (its middle when the playhead is off it), or the sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbBase {
    pub clip: Option<ClipId>,
    /// Timeline time rendered.
    pub at: Time,
    /// The source frame there (the cache key's "кадр").
    pub frame: Time,
    /// The clip's look ([`clip_state`]); 0 for the sample.
    pub state: u64,
}

pub fn thumb_base(project: &Project, inspected: Option<ClipId>, playhead: Time) -> ThumbBase {
    let seq = project.sequence();
    let video = inspected.and_then(|id| seq.locate_clip(id)).filter(|(ti, _)| seq.tracks[*ti].kind == TrackKind::Video);
    match video {
        Some((ti, ci)) => {
            let c = &seq.tracks[ti].clips[ci];
            let at = if c.timeline_in <= playhead && playhead < c.timeline_out { playhead } else { c.timeline_in + Time(c.duration().flicks() / 2) };
            ThumbBase { clip: Some(c.id), at, frame: c.source_time_at(at), state: clip_state(project, c) }
        }
        None => ThumbBase { clip: None, at: SAMPLE_AT, frame: SAMPLE_AT, state: 0 },
    }
}

/// Feeds what serde writes straight into a hasher (no string built).
struct HashWriter<'a, H: Hasher>(&'a mut H);

impl<H: Hasher> std::io::Write for HashWriter<'_, H> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A hash of the clip's look beyond its id and the source frame (ruling
/// R3: the spec's key alone would keep a card's old picture after an edit
/// of the clip) — of exactly what [`thumb_project`] keeps of it: its file,
/// source range and speed, colour and colour keyframes, multicam angle,
/// and the sequence's format. What the cards never show is not part of it
/// (M3): its place on the timeline, name, sound, fades, on/off, transform
/// and crop, motion keyframes and its own effects.
pub fn clip_state(project: &Project, clip: &Clip) -> u64 {
    let seq = project.sequence();
    let c = card_clip(clip);
    let file = project.asset(c.asset).map(|a| (&a.path, a.source_mtime_ms));
    let look = (c.asset, (c.source_in, c.source_out, c.duration()), &c.color, &c.animation, &c.multicam, file, (seq.width, seq.height, seq.frame_rate));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // These types always serialise; a failure would only make the key coarser.
    let _ = serde_json::to_writer(HashWriter(&mut h), &look);
    h.finish()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub clip: Option<ClipId>,
    pub frame: Time,
    /// [`ThumbBase::state`]: an edit of the clip renders its cards again.
    pub state: u64,
    pub recipe: &'static str,
    pub version: u16,
    /// `STILL`, or a loop frame `0..LOOP_FRAMES`.
    pub phase: u8,
}

impl ThumbKey {
    pub fn new(base: &ThumbBase, d: &RecipeDef, phase: u8) -> ThumbKey {
        ThumbKey { clip: base.clip, frame: base.frame, state: base.state, recipe: d.id, version: d.version, phase }
    }

    /// The motion phase 0..1 this thumbnail shows.
    pub fn phase_f(&self) -> f64 {
        if self.phase == STILL { STILL_PHASE } else { self.phase as f64 / (LOOP_FRAMES - 1) as f64 }
    }

    /// Whether `other` is a picture of the same frame of the same clip as it looks now.
    fn same_frame(&self, other: &ThumbKey) -> bool {
        (self.clip, self.frame, self.state) == (other.clip, other.frame, other.state)
    }
}

/// The effect as a thumbnail shows it at `phase`: «Обычно», the progress
/// held at `phase`, and every «Момент» placed so its motion is `phase` of
/// the way through. The frame is `0.3·phase − 0.22` s of screen time past
/// the moment: Punch In's 0.12 s snap, which ends at its moment (R17), runs
/// from phase ≈ 0.33 to ≈ 0.73 — just ahead at 0, done well before 1.
///
/// An effect on its own clock (Handheld's noise runs at `progress ×
/// clip_secs`) gets a real-time demo clock instead: loop frame to loop
/// frame is one [`LOOP_TICK`] of it, so the card sways as the clip will —
/// not a jitter of samples ~1/11 of the clip apart (I6).
pub fn demo_effect(def: &RecipeDef, clip: &Clip, at: Time, phase: f64) -> AppliedEffect {
    let mut e = kadr_effects::fresh(def, clip, at);
    e.id = EffectId::parse(DEMO_EFFECT_ID).unwrap_or(e.id);
    let secs = clip.duration().as_secs_f64();
    let p = if ON_ITS_CLOCK.contains(&def.id) && secs > 0.0 { phase * LOOP_SECS / secs } else { phase };
    e.progress = (p, p);
    let s = clip.source_time_at(at);
    for d in def.params.iter().filter(|d| matches!(d.kind, ParamKind::Moment)) {
        let lead = Time::from_secs_f64((phase * 0.3 - 0.22) * clip.speed());
        e.params.insert(d.key.to_string(), ParamValue::Moment(SourceTime(s - lead)));
    }
    e
}

/// `clip` as the cards show it: its own picture at full frame — transform
/// and crop, motion keys, fades and on/off dropped; its colour, animated or
/// not, kept — without effects. [`clip_state`] hashes what is left.
fn card_clip(clip: &Clip) -> Clip {
    let mut c = Clip { effects: vec![], ..clip.clone() };
    c.transform = Transform::default();
    for k in c.animation.keys() {
        if !matches!(k, TrackKey::Clip(AnimParam::Exposure | AnimParam::Contrast | AnimParam::Saturation | AnimParam::Temperature)) {
            c.animation.clear(k);
        }
    }
    c.video_fade = EdgeFades::default();
    c.enabled = true;
    c
}

/// A one-clip project of `clip`'s own picture at full frame ([`card_clip`])
/// with only `effect`.
pub fn thumb_project(project: &Project, clip: &Clip, effect: AppliedEffect) -> Project {
    let mut c = card_clip(clip);
    c.effects = vec![effect];
    let src = project.sequence();
    let mut p = Project::new("thumb");
    p.assets = project.asset(c.asset).cloned().into_iter().collect();
    let seq = p.sequence_mut();
    (seq.width, seq.height, seq.frame_rate) = (src.width, src.height, src.frame_rate);
    seq.tracks = vec![Track::new(TrackKind::Video, "V1")];
    seq.tracks[0].clips.push(c);
    p
}

/// The picture cards show without a clip: a dusk sky, a sun and two hills.
pub fn sample_frame() -> Arc<CpuFrame> {
    let mut px = vec![0u8; (SAMPLE_W * SAMPLE_H * 4) as usize];
    for y in 0..SAMPLE_H {
        for x in 0..SAMPLE_W {
            let (u, v) = (x as f32 / SAMPLE_W as f32, y as f32 / SAMPLE_H as f32);
            let mut c = [0.16 + 0.55 * v, 0.20 + 0.30 * v, 0.42 - 0.12 * v];
            if (u - 0.68).powi(2) + ((v - 0.42) * 0.5625).powi(2) < 0.006 {
                c = [1.0, 0.86, 0.55];
            }
            if v > 0.80 - 0.06 * (u * 5.0 + 1.3).cos() {
                c = [0.10, 0.16, 0.12];
            } else if v > 0.70 - 0.08 * (u * 9.0).sin() {
                c = [0.22, 0.30, 0.26];
            }
            let i = ((y * SAMPLE_W + x) * 4) as usize;
            for k in 0..3 {
                px[i + k] = (c[k].clamp(0.0, 1.0) * 255.0) as u8;
            }
            px[i + 3] = 255;
        }
    }
    Arc::new(CpuFrame::from_rgba8(SAMPLE_W, SAMPLE_H, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, px))
}

/// The sample as a 10 s clip on a 16:9 sequence, with `def` at `phase`.
pub fn sample_project(def: &RecipeDef, phase: f64) -> Project {
    let video = VideoInfo { width: SAMPLE_W, height: SAMPLE_H, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "sample".into(), pixel_format: "rgba".into(), rotation: 0, sar: (1, 1), color: None };
    let a = MediaAsset::new("kadr-sample", MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(10), container: "sample".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None });
    let mut c = Clip::new(a.id, "sample", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::ZERO);
    c.effects = vec![demo_effect(def, &c, SAMPLE_AT, phase)];
    let mut p = Project::new("sample");
    p.sequence_mut().tracks[0].clips.push(c);
    p.assets.push(a);
    p
}

/// Whether an input is missing (a file offline, a decode failed), also
/// inside a transition.
fn missing(i: &LayerInput) -> bool {
    match i {
        LayerInput::Missing(_) => true,
        LayerInput::Transition { from, to } => from.iter().chain(to).any(missing),
        _ => false,
    }
}

/// One thumbnail: `p` at `at` into 160 × 90 premultiplied RGBA. With
/// `sample` every media layer shows it; otherwise `resolver` decodes and
/// waits (export mode). `None` without a way to get the picture, or when
/// the clip's file is missing or does not decode (the card keeps its icon
/// rather than showing black).
pub fn render(p: &Project, at: Time, resolver: Option<&Resolver>, sample: Option<&Arc<CpuFrame>>) -> Option<Vec<u8>> {
    let out = OutputSpec::new(SizeU::new(THUMB_W, THUMB_H), RenderQuality::PreviewHigh);
    let scenes = ProjectScenes::new(p, false);
    let scene = scenes.scene_at(at, &out);
    let inputs = match sample {
        Some(f) => RenderInputs { layers: scene.layers.iter().map(|l| match l.content { LayerContent::Media { .. } => LayerInput::Cpu(f.clone()), _ => LayerInput::None }).collect() },
        None => {
            let mut perf = kadr_core::perf::FramePerf::default();
            let inputs = resolver?.prepare(&scene, &scenes, Mode::Export, &mut perf);
            if inputs.layers.iter().any(missing) {
                return None;
            }
            inputs
        }
    };
    let mut px = vec![0u8; (THUMB_W * THUMB_H * 4) as usize];
    let mut target = RenderTarget::Cpu(CpuTarget::packed(THUMB_W, THUMB_H, &mut px));
    CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &inputs }, &mut target).ok()?;
    Some(px)
}

/// One thumbnail to render: a one-clip project at a timeline time.
struct Job {
    key: ThumbKey,
    project: Project,
    at: Time,
}

/// The thumbnails a request needs, rendered in order until `token` is cancelled.
struct Batch {
    token: CancelToken,
    jobs: Vec<Job>,
    resolver: Option<Arc<Resolver>>,
    sample: Arc<CpuFrame>,
}

/// The thumbnail thread's inbox: one batch not started yet — a newer one
/// takes its place (latest wins: never a queue of stale batches holding
/// their project copies) — and whether the app closed it.
struct Inbox<T> {
    slot: Mutex<Slot<T>>,
    wake: Condvar,
}

struct Slot<T> {
    next: Option<T>,
    closed: bool,
}

impl<T> Inbox<T> {
    fn new() -> Self {
        Inbox { slot: Mutex::new(Slot { next: None, closed: false }), wake: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, Slot<T>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `item` in, dropping one not started yet; gives it back once closed.
    fn put(&self, item: T) -> Result<(), T> {
        let mut s = self.lock();
        if s.closed {
            return Err(item);
        }
        s.next = Some(item);
        self.wake.notify_all();
        Ok(())
    }

    /// Takes nothing more: the worker ends.
    fn close(&self) {
        let mut s = self.lock();
        s.closed = true;
        s.next = None;
        self.wake.notify_all();
    }

    /// The next item once `quiet` has passed without a newer one (a newer
    /// one replaces it and the wait starts again); `None` once closed.
    fn take(&self, quiet: Duration) -> Option<T> {
        let mut s = self.lock();
        while s.next.is_none() && !s.closed {
            s = self.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
        let mut item = s.next.take()?;
        let mut until = Instant::now() + quiet;
        loop {
            if s.closed {
                return None;
            }
            if let Some(newer) = s.next.take() {
                item = newer;
                until = Instant::now() + quiet;
            }
            let now = Instant::now();
            if now >= until {
                return Some(item);
            }
            s = self.wake.wait_timeout(s, until - now).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// The thumbnail thread: the latest batch after a quiet [`DEBOUNCE`], one
/// render at a time; a cancelled batch stops at its next job. Holds nothing
/// between batches (the resolver goes with its batch); ends when the inbox
/// is closed (app shutdown, or `Thumbs` dropped).
fn work(inbox: Arc<Inbox<Batch>>) {
    while let Some(Batch { token, jobs, resolver, sample }) = inbox.take(DEBOUNCE) {
        for Job { key, project, at } in jobs {
            if token.is_cancelled() {
                break;
            }
            match render(&project, at, resolver.as_deref(), key.clip.is_none().then_some(&sample)) {
                Some(px) => crate::app::post(move |app| app.fx_thumb_ready(key, px)),
                None => crate::app::post(move |app| app.fx_thumb_failed(key)),
            }
        }
    }
}

/// Rendered thumbnails, the batch in flight and the hovered card's loop.
#[derive(Default)]
pub struct Thumbs {
    images: HashMap<ThumbKey, slint::Image>,
    pending: HashSet<ThumbKey>,
    cancel: Option<CancelToken>,
    sample: Option<Arc<CpuFrame>>,
    /// The picture each card showed last: it stays while the new one renders.
    last: HashMap<&'static str, slint::Image>,
    /// The thumbnail thread's inbox and the thread (started on first use).
    worker: Option<(Arc<Inbox<Batch>>, JoinHandle<()>)>,
    /// The app is closing: nothing starts the thread again.
    closed: bool,
    /// The card whose loop plays (the hovered motion card, once its hover
    /// delay is over) and its frame.
    looping: Option<&'static str>,
    pub loop_frame: u8,
    /// Advances `loop_frame` while the loop plays (dropping it stops it).
    timer: Option<slint::Timer>,
}

impl Drop for Thumbs {
    /// The thread ends with its owner, as it did with a dropped channel.
    fn drop(&mut self) {
        if let Some((inbox, _)) = &self.worker {
            inbox.close();
        }
    }
}

impl Thumbs {
    pub fn get(&self, k: &ThumbKey) -> Option<slint::Image> {
        self.images.get(k).cloned()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.images.len()
    }

    /// What `d`'s card shows of `base`: while its loop plays, its loop frame
    /// once the whole loop is rendered (no flicker between the still and the
    /// frames as they arrive); else its still.
    pub fn card(&self, d: &RecipeDef, base: &ThumbBase) -> Option<slint::Image> {
        let looping = self.looping == Some(d.id) && (0..LOOP_FRAMES).all(|i| self.images.contains_key(&ThumbKey::new(base, d, i)));
        looping.then(|| self.get(&ThumbKey::new(base, d, self.loop_frame))).flatten().or_else(|| self.get(&ThumbKey::new(base, d, STILL)))
    }

    /// The card whose loop plays.
    pub fn looping(&self) -> Option<&'static str> {
        self.looping
    }

    /// `recipe`'s card starts its loop, from its first frame; `tick`
    /// advances it every [`LOOP_TICK`].
    pub fn start_loop(&mut self, recipe: &'static str, tick: impl FnMut() + 'static) {
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, LOOP_TICK, tick);
        self.timer = Some(timer);
        self.looping = Some(recipe);
        self.loop_frame = 0;
    }

    /// The loop stops (its card shows its still); `true` when one played.
    pub fn stop_loop(&mut self) -> bool {
        self.timer = None;
        self.loop_frame = 0;
        self.looping.take().is_some()
    }

    /// The keys a new batch must render — every wanted key not cached — or
    /// `None` when the batch in flight already covers them.
    pub fn plan(&self, keys: impl IntoIterator<Item = ThumbKey>) -> Option<HashSet<ThumbKey>> {
        let need: HashSet<ThumbKey> = keys.into_iter().filter(|k| !self.images.contains_key(k)).collect();
        (!need.is_empty() && !need.is_subset(&self.pending)).then_some(need)
    }

    /// A new batch for `keys`; the previous one is cancelled.
    pub fn start(&mut self, keys: HashSet<ThumbKey>) -> CancelToken {
        self.cancel();
        let token = CancelToken::new();
        self.cancel = Some(token.clone());
        self.pending = keys;
        token
    }

    /// Whether the batch in flight asked for `k` (a late result of a
    /// cancelled batch is not kept).
    pub fn wants(&self, k: &ThumbKey) -> bool {
        self.pending.contains(k)
    }

    /// Caches `img`. When the cache is full only the pictures of `k`'s
    /// frame stay (the one shown now).
    pub fn insert(&mut self, k: ThumbKey, img: slint::Image) {
        self.pending.remove(&k);
        if self.images.len() >= MAX_IMAGES {
            self.images.retain(|o, _| o.same_frame(&k));
        }
        self.images.insert(k, img);
    }

    /// What `recipe`'s card shows: `now` (remembered), or while it is not
    /// rendered yet the picture the card showed last.
    pub fn shown(&mut self, recipe: &'static str, now: Option<slint::Image>) -> Option<slint::Image> {
        match now {
            Some(img) => {
                self.last.insert(recipe, img.clone());
                Some(img)
            }
            None => self.last.get(recipe).cloned(),
        }
    }

    /// `recipe`'s card has no picture now (its clip's file is missing).
    pub fn forget_shown(&mut self, recipe: &str) {
        self.last.remove(recipe);
    }

    /// Stops the batch in flight (the tab was left, the frame changed).
    pub fn cancel(&mut self) {
        if let Some(t) = self.cancel.take() {
            t.cancel();
        }
        self.pending.clear();
    }

    /// Another project: the work stops, every picture is freed, the loop stops.
    pub fn clear(&mut self) {
        self.cancel();
        self.images.clear();
        self.last.clear();
        self.stop_loop();
    }

    /// The app is closing: the work stops and the thread ends — for good: a
    /// late result's refresh asking for pictures starts nothing (M4).
    pub fn shutdown(&mut self) {
        self.clear();
        self.closed = true;
        if let Some((inbox, _)) = self.worker.take() {
            inbox.close();
        }
    }

    pub fn sample(&mut self) -> Arc<CpuFrame> {
        self.sample.get_or_insert_with(sample_frame).clone()
    }

    /// Hands `batch` to the thumbnail thread — in place of one it has not
    /// started — starting the thread on first use (or again if it died);
    /// after `shutdown` the batch is dropped.
    fn submit(&mut self, batch: Batch) {
        if self.closed {
            return self.cancel();
        }
        let batch = match &self.worker {
            Some((inbox, thread)) if !thread.is_finished() => match inbox.put(batch) {
                Ok(()) => return,
                Err(batch) => batch,
            },
            _ => batch,
        };
        let inbox = Arc::new(Inbox::new());
        let theirs = inbox.clone();
        match std::thread::Builder::new().name("kadr-fx-thumbs".into()).spawn(move || work(theirs)) {
            Ok(thread) => {
                let _ = inbox.put(batch);
                if let Some((old, _)) = self.worker.replace((inbox, thread)) {
                    old.close();
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "thumbnail thread not started");
                self.cancel();
            }
        }
    }
}

impl App {
    /// What a card shows for `d`: its loop frame while its loop plays, else its still.
    pub fn fx_thumb(&self, d: &RecipeDef, base: &ThumbBase) -> Option<slint::Image> {
        self.fx.thumbs.card(d, base)
    }

    /// Starts rendering what the tab needs and lacks: stills of the listed
    /// cards and the loop that plays (a hovered motion card's, once its
    /// hover delay is over — not every card the pointer crosses). Not while
    /// playing.
    pub fn fx_request_thumbs(&mut self, base: ThumbBase) {
        if self.playing {
            return;
        }
        let mut wanted: Vec<(ThumbKey, &'static RecipeDef)> = visible_recipes(&self.fx.category, &self.fx.search).into_iter().map(|d| (ThumbKey::new(&base, d, STILL), d)).collect();
        if let Some(d) = self.fx.thumbs.looping().and_then(kadr_effects::recipe) {
            wanted.extend((0..LOOP_FRAMES).map(|i| (ThumbKey::new(&base, d, i), d)));
        }
        let Some(need) = self.fx.thumbs.plan(wanted.iter().map(|(k, _)| *k)) else { return };
        let jobs: Vec<Job> = wanted
            .into_iter()
            .filter(|(k, _)| need.contains(k))
            .filter_map(|(key, d)| match base.clip {
                Some(id) => {
                    let c = self.project.sequence().clip(id)?;
                    Some(Job { key, project: thumb_project(&self.project, c, demo_effect(d, c, base.at, key.phase_f())), at: base.at })
                }
                None => Some(Job { key, project: sample_project(d, key.phase_f()), at: SAMPLE_AT }),
            })
            .collect();
        let resolver = if base.clip.is_some() { self.preview.mcp_resolver() } else { None };
        let sample = self.fx.thumbs.sample();
        let token = self.fx.thumbs.start(need);
        self.fx.thumbs.submit(Batch { token, jobs, resolver, sample });
    }

    /// A thumbnail came back from the background (UI thread).
    pub fn fx_thumb_ready(&mut self, key: ThumbKey, px: Vec<u8>) {
        if !self.fx.thumbs.wants(&key) {
            return;
        }
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&px, THUMB_W, THUMB_H);
        self.fx.thumbs.insert(key, slint::Image::from_rgba8_premultiplied(buf));
        self.refresh_effects();
    }

    /// A thumbnail could not be made (the clip's file is missing or does
    /// not decode): the card shows its icon instead of an older picture.
    /// The key stays pending, so it is not tried again until the next batch.
    pub fn fx_thumb_failed(&mut self, key: ThumbKey) {
        if self.fx.thumbs.wants(&key) && key.phase == STILL {
            self.fx.thumbs.forget_shown(key.recipe);
            self.refresh_effects();
        }
    }

    /// The hovered card's loop advances (~12 fps) — not while playing:
    /// playback has the machine, the cards wait for the stop (M4).
    pub fn fx_loop_tick(&mut self) {
        if self.playing {
            return;
        }
        self.fx.thumbs.loop_frame = (self.fx.thumbs.loop_frame + 1) % LOOP_FRAMES;
        self.refresh_effects();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::AssetId;

    /// Timeline 10..20 s showing source 30..40 s.
    fn clip() -> Clip {
        Clip::new(AssetId::new(), "c", TimeRange::new(Time::from_secs(30), Time::from_secs(40)), Time::from_secs(10))
    }

    fn video_asset(path: &std::path::Path) -> MediaAsset {
        let video = VideoInfo { width: 1920, height: 1080, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
        MediaAsset::new(path, MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
    }

    fn key(phase: u8) -> ThumbKey {
        ThumbKey { clip: None, frame: SAMPLE_AT, state: 0, recipe: "smooth_zoom", version: 1, phase }
    }

    #[test]
    fn demo_effects_hold_a_phase_and_place_the_moment_around_it() {
        let def = kadr_effects::recipe("punch_in").unwrap();
        let e = demo_effect(def, &clip(), Time::from_secs(12), 0.0);
        assert_eq!(e.progress, (0.0, 0.0), "a constant progress: the frame shows the phase");
        // The snap ends at the moment (R17): it runs from 0.12 s before it.
        assert_eq!(e.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(32) + Time::from_millis(220))), "phase 0: the punch is 0.1 s ahead");
        let mid = demo_effect(def, &clip(), Time::from_secs(12), 0.5);
        assert_eq!(mid.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(32) + Time::from_millis(70))), "mid-loop: mid-punch");
        let done = demo_effect(def, &clip(), Time::from_secs(12), 1.0);
        assert_eq!(done.params["moment"], ParamValue::Moment(SourceTime(Time::from_secs(32) - Time::from_millis(80))), "phase 1: past it");
        assert_eq!(done.params["strength"], ParamValue::Number(0.6), "«Обычно»");
        assert_eq!(done.id, e.id, "one id: Handheld's shake (seeded by it) is the same on every render and loop frame");
        assert_eq!(ThumbKey { phase: STILL, ..key(0) }.phase_f(), 0.85);
        assert_eq!(ThumbKey { phase: LOOP_FRAMES - 1, ..key(0) }.phase_f(), 1.0);
    }

    /// I6: Handheld runs on its own clock (`progress × clip_secs`): its loop
    /// gets real time — one loop tick of noise time from frame to frame,
    /// whatever the clip's length — still at «Обычно»; a zoom, driven by its
    /// progress, still goes through its whole motion.
    #[test]
    fn a_clock_driven_card_loops_in_real_time() {
        let hand = kadr_effects::recipe("handheld").unwrap();
        let long = Clip::new(AssetId::new(), "l", TimeRange::new(Time::ZERO, Time::from_secs(120)), Time::ZERO);
        for c in [clip(), long] {
            let clock = |i: u8| {
                let e = demo_effect(hand, &c, c.timeline_in, ThumbKey { phase: i, ..key(0) }.phase_f());
                assert_eq!(e.progress.0, e.progress.1, "held");
                assert_eq!(e.params["strength"], ParamValue::Number(0.6), "«Обычно»");
                e.progress.0 * c.duration().as_secs_f64()
            };
            for i in 1..LOOP_FRAMES {
                let step = clock(i) - clock(i - 1);
                assert!((step - LOOP_TICK.as_secs_f64()).abs() < 1e-9, "{}: {step}", c.name);
            }
        }
        let zoom = kadr_effects::recipe("smooth_zoom").unwrap();
        assert_eq!(demo_effect(zoom, &clip(), Time::from_secs(12), 1.0).progress, (1.0, 1.0));
    }

    #[test]
    fn a_thumbnail_project_shows_the_clip_at_full_frame_with_only_that_effect() {
        let mut p = Project::new("t");
        let mut c = clip();
        c.transform.scale = 0.3;
        c.video_fade.fade_in = Time::from_secs(1);
        c.color.saturation = 0.5;
        c.effects.push(AppliedEffect::new("handheld", 1, Default::default()));
        let src = SourceTime(Time::from_secs(31));
        c.animation.set_key(AnimParam::Scale, src, 0.5, None);
        c.animation.set_key(AnimParam::Exposure, src, 1.0, None);
        p.sequence_mut().tracks[0].clips.push(c.clone());
        let e = kadr_effects::fresh(kadr_effects::recipe("smooth_zoom").unwrap(), &c, Time::from_secs(12));
        let t = thumb_project(&p, &c, e);
        let tc = &t.sequence().tracks[0].clips[0];
        assert_eq!(t.sequence().tracks.len(), 1);
        assert_eq!((tc.transform.scale, tc.video_fade.fade_in, tc.color.saturation), (1.0, Time::ZERO, 0.5), "full frame, no fade, its own colour");
        assert_eq!(tc.animation.keys(), vec![TrackKey::Clip(AnimParam::Exposure)], "its colour keys stay, its motion goes");
        assert_eq!(tc.effects.iter().map(|e| e.recipe.as_str()).collect::<Vec<_>>(), ["smooth_zoom"]);
        assert_eq!((t.sequence().width, t.sequence().height), (p.sequence().width, p.sequence().height));
    }

    /// No decoder needed: the sample picture renders, and a zoom changes it.
    #[test]
    fn the_sample_renders_and_a_zoom_changes_it() {
        let sample = sample_frame();
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let start = render(&sample_project(def, 0.0), SAMPLE_AT, None, Some(&sample)).unwrap();
        let end = render(&sample_project(def, 1.0), SAMPLE_AT, None, Some(&sample)).unwrap();
        assert_eq!(start.len(), (THUMB_W * THUMB_H * 4) as usize);
        assert!(start.chunks(4).any(|p| p[0] > 40), "not a black frame");
        assert_ne!(start, end, "phase 1 is zoomed in");
        assert!(render(&sample_project(def, 0.0), SAMPLE_AT, None, None).is_none(), "no decoder and no sample: nothing");
    }

    /// A clip's frame comes through the resolver (export mode: it waits);
    /// a missing file makes no thumbnail rather than a black one.
    #[test]
    fn a_clip_renders_through_the_resolver_and_a_missing_file_renders_nothing() {
        use kadr_playback::testing::{FakeDecoders, FakeMedia};
        let path = std::env::temp_dir().join(format!("kadr-fx-thumb-{}.mp4", std::process::id()));
        std::fs::write(&path, b"fake").unwrap();
        let decoders = Arc::new(FakeDecoders::new(Duration::ZERO, Duration::ZERO));
        decoders.add(&path, FakeMedia { tag: 220, frames: None, fail: false });
        let resolver = Resolver::new(decoders, kadr_playback::ResolverConfig::default());
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let thumb = |a: &MediaAsset| {
            let mut p = Project::new("t");
            let c = Clip::new(a.id, "c", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::ZERO);
            p.assets.push(a.clone());
            p.sequence_mut().tracks[0].clips.push(c.clone());
            let at = Time::from_secs(2);
            render(&thumb_project(&p, &c, demo_effect(def, &c, at, 0.85)), at, Some(&resolver), None)
        };
        let px = thumb(&video_asset(&path)).expect("a thumbnail of the clip");
        let mid = ((THUMB_H / 2 * THUMB_W + THUMB_W / 2) * 4) as usize;
        assert!(px[mid] > 150 && px[mid + 3] == 255, "the clip's own picture: {:?}", &px[mid..mid + 4]);
        let _ = std::fs::remove_file(&path);
        assert!(thumb(&video_asset(&std::env::temp_dir().join("kadr-fx-thumb-missing.mp4"))).is_none(), "offline: no thumbnail");
    }

    #[test]
    fn thumbnails_follow_the_inspected_video_clip_or_fall_back_to_the_sample() {
        let mut p = Project::new("t");
        let c = clip();
        let id = c.id;
        p.sequence_mut().tracks[0].clips.push(c);
        let a = Clip::new(AssetId::new(), "a", TimeRange::new(Time::ZERO, Time::from_secs(5)), Time::ZERO);
        let aid = a.id;
        p.sequence_mut().tracks[1].clips.push(a);
        let b = thumb_base(&p, Some(id), Time::from_secs(12));
        assert_eq!((b.clip, b.at, b.frame), (Some(id), Time::from_secs(12), Time::from_secs(32)));
        assert_ne!(b.state, 0, "the clip's look is part of the key");
        assert_eq!(thumb_base(&p, Some(id), Time::from_secs(3)).at, Time::from_secs(15), "playhead off the clip: its middle");
        assert_eq!(thumb_base(&p, Some(aid), Time::ZERO).clip, None, "a sound clip has no picture: the sample");
        assert_eq!(thumb_base(&p, None, Time::ZERO), ThumbBase { clip: None, at: SAMPLE_AT, frame: SAMPLE_AT, state: 0 });
    }

    /// Ruling R3: an edit of what the clip looks like *in the cards* gives
    /// them new keys (they render again); a change elsewhere — or of what
    /// `thumb_project` drops (M3) — keeps them (the cache holds).
    #[test]
    fn an_edit_of_the_clip_renders_again_and_an_unrelated_change_does_not() {
        let mut p = Project::new("t");
        let a = video_asset(std::path::Path::new("C:/media/a.mp4"));
        let mut c = clip();
        c.asset = a.id;
        let id = c.id;
        p.assets.push(a);
        p.sequence_mut().tracks[0].clips.push(c);
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let key_at = |p: &Project, playhead: i64| ThumbKey::new(&thumb_base(p, Some(id), Time::from_secs(playhead)), def, STILL);
        let edited = |p: &Project, f: &dyn Fn(&mut Clip)| {
            let mut q = p.clone();
            let (ti, ci) = q.sequence().locate_clip(id).unwrap();
            f(&mut q.sequence_mut().tracks[ti].clips[ci]);
            q
        };
        let k0 = key_at(&p, 12);
        assert_eq!(key_at(&p, 12), k0, "stable");

        let src = SourceTime(Time::from_secs(31));
        let mut unrelated = edited(&p, &|c| {
            c.name = "renamed".into();
            c.audio.gain_db = -6.0;
            c.video_fade.fade_in = Time::from_secs(1);
            c.enabled = false;
            // The cards show the clip at full frame with only their own effect.
            c.transform.scale = 0.5;
            c.transform.crop_left = 0.1;
            c.animation.set_key(AnimParam::Scale, src, 0.8, None);
            c.effects.push(AppliedEffect::new("handheld", 1, Default::default()));
        });
        unrelated.sequence_mut().tracks[1].clips.push(Clip::new(AssetId::new(), "o", TimeRange::new(Time::ZERO, Time::from_secs(2)), Time::ZERO));
        unrelated.sequence_mut().name = "other".into();
        assert_eq!(
            key_at(&unrelated, 12),
            k0,
            "its name, sound, fades and on/off, transform, crop, motion keys and effects (cards show none), another clip, the sequence's name: same pictures"
        );
        assert_eq!(key_at(&edited(&p, &|c| c.move_to(Time::from_secs(20))), 22), k0, "moved with the playhead: the same frame, the same picture");

        type Edit<'a> = (&'a str, &'a dyn Fn(&mut Clip));
        let edits: [Edit; 5] = [
            ("colour", &|c| c.color.saturation = 0.5),
            ("colour keyframes", &|c| c.animation.set_key(AnimParam::Exposure, src, 1.0, None)),
            ("speed", &|c| c.timeline_out = Time::from_secs(15)),
            ("source range", &|c| (c.source_in, c.source_out) = (Time::from_secs(29), Time::from_secs(39))),
            ("asset", &|c| c.asset = AssetId::new()),
        ];
        for (what, f) in edits {
            assert_ne!(key_at(&edited(&p, f), 12), k0, "{what}");
        }
        let mut relinked = p.clone();
        relinked.assets[0].path = "D:/moved/a.mp4".into();
        assert_ne!(key_at(&relinked, 12), k0, "relinked to another file");
        let mut resized = p.clone();
        resized.sequence_mut().width = 1080;
        assert_ne!(key_at(&resized, 12), k0, "another sequence format");
    }

    #[test]
    fn a_batch_restarts_only_for_keys_the_cache_lacks() {
        let mut th = Thumbs::default();
        let need = th.plan([key(STILL), key(0)]).expect("nothing cached");
        assert_eq!(need.len(), 2);
        let token = th.start(need);
        assert!(th.plan([key(STILL), key(0)]).is_none(), "the running batch covers them");
        th.insert(key(STILL), slint::Image::default());
        assert!(th.get(&key(STILL)).is_some());
        assert_eq!(th.plan([key(STILL), key(0), key(1)]).map(|n| n.len()), Some(2), "a new key restarts the batch with every missing one");
        th.cancel();
        assert!(token.is_cancelled(), "leaving the tab stops the work");
        assert_eq!(th.plan([key(0)]).map(|n| n.len()), Some(1), "cancelled keys are asked for again");
    }

    /// A result of a cancelled batch arriving late is not kept (another
    /// project's pictures would sit in memory); clearing frees every picture.
    #[test]
    fn late_results_are_dropped_and_clearing_frees_the_pictures() {
        let mut th = Thumbs::default();
        th.start([key(STILL), key(0)].into());
        assert!(th.wants(&key(STILL)));
        assert!(!th.wants(&key(1)), "never asked for");
        th.cancel();
        assert!(!th.wants(&key(STILL)), "cancelled: its late result is dropped");
        th.insert(key(0), slint::Image::default());
        assert_eq!(th.shown("smooth_zoom", th.get(&key(0))).map(|_| ()), Some(()));
        th.clear();
        assert!(th.get(&key(0)).is_none() && th.shown("smooth_zoom", None).is_none(), "a new project: nothing kept");
    }

    /// While a card's new picture renders it keeps the one it showed (no
    /// flash of icons on every playhead step); a failed one shows the icon.
    #[test]
    fn a_card_keeps_its_last_picture_until_the_new_one_arrives() {
        let mut th = Thumbs::default();
        assert!(th.shown("smooth_zoom", None).is_none(), "nothing yet: the icon");
        let img = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(2, 2));
        assert!(th.shown("smooth_zoom", Some(img.clone())).is_some());
        assert_eq!(th.shown("smooth_zoom", None), Some(img), "the next frame is not there yet");
        assert!(th.shown("punch_in", None).is_none(), "per card");
        th.forget_shown("smooth_zoom");
        assert!(th.shown("smooth_zoom", None).is_none());
    }

    /// A card loops only while its loop plays (a hovered motion card, once
    /// its hover delay is over — M4) and once every loop frame is there;
    /// until then, and otherwise, it shows its still.
    #[test]
    fn a_hovered_card_loops_once_its_whole_loop_is_rendered() {
        let def = kadr_effects::recipe("smooth_zoom").unwrap();
        let base = ThumbBase { clip: None, at: SAMPLE_AT, frame: SAMPLE_AT, state: 0 };
        // Slint compares pictures by their buffer: each is its own.
        let img = || slint::Image::from_rgba8(slint::SharedPixelBuffer::new(1, 1));
        let (still, frames): (slint::Image, Vec<slint::Image>) = (img(), (0..LOOP_FRAMES).map(|_| img()).collect());
        let mut th = Thumbs::default();
        th.insert(ThumbKey::new(&base, def, STILL), still.clone());
        for i in 0..LOOP_FRAMES - 1 {
            th.insert(ThumbKey::new(&base, def, i), frames[i as usize].clone());
        }
        th.insert(ThumbKey::new(&base, def, LOOP_FRAMES - 1), frames[LOOP_FRAMES as usize - 1].clone());
        assert_eq!(th.card(def, &base), Some(still.clone()), "no loop plays: the still");
        th.looping = Some(def.id);
        th.loop_frame = 3;
        assert_eq!(th.card(def, &base), Some(frames[3].clone()), "the loop plays");
        th.images.remove(&ThumbKey::new(&base, def, 5));
        assert_eq!(th.card(def, &base), Some(still.clone()), "one frame missing: the still");
        th.insert(ThumbKey::new(&base, def, 5), frames[5].clone());
        th.looping = Some("handheld");
        assert_eq!(th.card(def, &base), Some(still.clone()), "another card's loop");
        th.looping = Some(def.id);
        assert!(th.stop_loop(), "it played");
        assert_eq!((th.card(def, &base), th.looping(), th.loop_frame), (Some(still), None, 0), "stopped: the still, from the first frame next time");
        assert!(!th.stop_loop(), "nothing plays");
    }

    /// The thumbnail thread's inbox holds one batch, the latest (M4): steps
    /// in a row never queue stale batches; a batch waits for a quiet spell,
    /// a newer one arriving meanwhile takes its place; closed, it ends.
    #[test]
    fn the_inbox_keeps_the_latest_and_waits_for_quiet() {
        let inbox = Arc::new(Inbox::new());
        inbox.put(1).unwrap();
        inbox.put(2).unwrap();
        let quiet = Duration::from_millis(40);
        let started = Instant::now();
        assert_eq!(inbox.take(quiet), Some(2), "latest wins");
        assert!(started.elapsed() >= quiet, "after the quiet spell");

        inbox.put(3).unwrap();
        let other = inbox.clone();
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            other.put(4).unwrap();
        });
        assert_eq!(inbox.take(Duration::from_millis(400)), Some(4), "a newer one during the wait takes its place");
        late.join().unwrap();

        let other = inbox.clone();
        let taker = std::thread::spawn(move || other.take(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        inbox.close();
        assert_eq!(taker.join().unwrap(), None, "closed: the worker ends");
        assert_eq!(inbox.put(5), Err(5), "and takes nothing more");
    }

    /// After shutdown nothing starts the thread again (M4): a late result's
    /// refresh asking for pictures drops its batch.
    #[test]
    fn after_shutdown_no_batch_restarts_the_thread() {
        let mut th = Thumbs::default();
        let batch = |th: &mut Thumbs| Batch { token: th.start([key(STILL)].into()), jobs: vec![], resolver: None, sample: th.sample() };
        let b = batch(&mut th);
        th.submit(b);
        assert!(th.worker.is_some(), "started on first use");
        th.shutdown();
        assert!(th.worker.is_none());
        let b = batch(&mut th);
        th.submit(b);
        assert!(th.worker.is_none(), "not started again");
        assert!(!th.wants(&key(STILL)), "its batch is dropped");
    }

    /// The cache is bounded: when full, only the pictures of the frame
    /// shown now stay.
    #[test]
    fn the_cache_is_bounded_and_keeps_the_frame_shown() {
        let mut th = Thumbs::default();
        let at = |ms: i64, phase: u8| ThumbKey { frame: Time::from_millis(ms), ..key(phase) };
        for i in 0..MAX_IMAGES as i64 - 1 {
            th.insert(at(i, STILL), slint::Image::default());
        }
        th.insert(at(7_000_000, 0), slint::Image::default());
        assert_eq!(th.len(), MAX_IMAGES);
        th.insert(at(7_000_000, STILL), slint::Image::default());
        assert_eq!(th.len(), 2, "full: the other frames go");
        assert!(th.get(&at(7_000_000, 0)).is_some() && th.get(&at(7_000_000, STILL)).is_some());
    }
}
