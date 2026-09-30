//! Preview controller with two backends, chosen by `KADR_RENDERER`
//! (`legacy` | `cpu`, default `cpu`):
//!
//! - `cpu` (render foundation M4): the timeline as it really is — every
//!   layer, transitions, crop, rotation, opacity, colour — through
//!   `ProjectScenes` → `PreviewPlayer` → `CpuRenderer`, rendered straight
//!   into the display buffer (see `preview_cpu`).
//! - `legacy`: a decode thread driven by commands, kept until M6 to compare.
//!   Paused: single frames on demand, coalesced (latest request wins).
//!   Playing: streams the composition segment by segment (the top clip
//!   only) and paces frames against the audio master clock. Frames go
//!   through the same `look_filter` as the legacy export.

use crate::app::{post, App};
use crate::preview_cpu::{CpuPreview, PendingPlay, PlayerFrame, PREROLL_FALLBACK};
use crate::scene_source::ProjectScenes;
use crossbeam_channel::{Receiver, Sender};
use slint::ComponentHandle;
use kadr_audio::{AudioClock, MixSource};
use kadr_core::perf::{FramePerf, LayerTiming, PerfRing, PERF_RING_FRAMES};
use kadr_core::{FrameRate, Time, TimeRange};
use kadr_media::export::VideoLook;
use kadr_media::{MediaBackend, RgbaFrame, StreamRequest};
use kadr_playback::{FfmpegDecoders, Resolver, ResolverConfig};
use kadr_project::{ColorAdjust, Project, Transform};
use kadr_scene::{OutputSpec, RenderQuality, SizeU};
use kadr_timeline::composition::{audio_segments, video_segments, VideoSource};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Seg {
    pub range: TimeRange,
    pub src: Option<(PathBuf, Time, f64, VideoLook)>,
}

enum Cmd {
    Show { generation: u64, t: Time, seg: Seg, w: u32, h: u32, rate: FrameRate, px_scale: f64 },
    Play { generation: u64, from: Time, segs: Vec<Seg>, w: u32, h: u32, rate: FrameRate, px_scale: f64 },
    Stop,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererKind {
    Legacy,
    Cpu,
}

impl RendererKind {
    /// `KADR_RENDERER`: `legacy` or `cpu`; unset or anything else → `cpu`.
    pub fn from_env(v: Option<&str>) -> Self {
        match v.map(|s| s.trim().to_ascii_lowercase()) {
            Some(s) if s == "legacy" => RendererKind::Legacy,
            Some(s) if s != "cpu" && !s.is_empty() => {
                tracing::warn!(value = %s, "unknown KADR_RENDERER; using cpu");
                RendererKind::Cpu
            }
            _ => RendererKind::Cpu,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RendererKind::Legacy => "legacy",
            RendererKind::Cpu => "cpu",
        }
    }
}

/// Frames shown on the UI thread in the last second, and how long they
/// took from the player to the screen (DEV overlay).
#[derive(Default)]
pub struct DisplayMeter {
    shown: VecDeque<Instant>,
    latency: VecDeque<Duration>,
}

impl DisplayMeter {
    pub fn note(&mut self, now: Instant, latency: Duration) {
        self.shown.push_back(now);
        while self.shown.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1)) {
            self.shown.pop_front();
        }
        if self.latency.len() == 120 {
            self.latency.pop_front();
        }
        self.latency.push_back(latency);
    }

    /// Frames shown during the second before `now`.
    pub fn fps(&self, now: Instant) -> usize {
        self.shown.iter().filter(|t| now.duration_since(**t) <= Duration::from_secs(1)).count()
    }

    pub fn latency_p50(&self) -> Option<Duration> {
        let mut v: Vec<Duration> = self.latency.iter().copied().collect();
        v.sort_unstable();
        v.get(v.len().saturating_sub(1) / 2).copied()
    }
}

pub struct PreviewController {
    tx: Sender<Cmd>,
    generation: Arc<AtomicU64>,
    pub quality: i32,
    /// Last 600 preview frames (MCP `get_perf`, DEV overlay), either backend.
    pub perf: Arc<PerfRing>,
    /// Legacy: generation and start time of the pending paused-frame request.
    pub seek_started: Option<(u64, Instant)>,
    pub kind: RendererKind,
    /// The `cpu` backend (`None` for legacy or without FFmpeg).
    pub cpu: Option<CpuPreview>,
    media: Option<Arc<dyn MediaBackend>>,
    /// MCP `get_frame`'s own resolver (export mode), created on first use.
    mcp_resolver: Option<Arc<Resolver>>,
    pub meter: DisplayMeter,
}

pub fn look_of(t: &Transform, c: &ColorAdjust, bypass: bool) -> VideoLook {
    if bypass {
        return VideoLook::default();
    }
    VideoLook {
        scale: t.scale,
        x: t.x,
        y: t.y,
        rotation_deg: t.rotation_deg,
        opacity: t.opacity,
        crop: [t.crop_left, t.crop_right, t.crop_top, t.crop_bottom],
        exposure: c.exposure,
        contrast: c.contrast,
        saturation: c.saturation,
    }
}

fn to_seg(project: &Project, range: TimeRange, src: Option<&VideoSource>, bypass: bool) -> Seg {
    Seg {
        range,
        src: src.and_then(|s| {
            let a = project.asset(s.asset)?;
            Some((a.path.clone(), s.source_start, s.speed, look_of(&s.transform, &s.color, bypass)))
        }),
    }
}

impl PreviewController {
    pub fn new(media: Option<Arc<dyn MediaBackend>>, clock: AudioClock, kind: RendererKind) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let perf = Arc::new(PerfRing::new(PERF_RING_FRAMES));
        tracing::info!(renderer = kind.name(), "preview renderer (KADR_RENDERER=legacy|cpu)");
        let mut cpu = None;
        if let Some(m) = media.clone() {
            match kind {
                RendererKind::Legacy => {
                    let g = generation.clone();
                    let p = perf.clone();
                    std::thread::Builder::new().name("kadr-preview".into()).spawn(move || worker(m, rx, clock, g, p)).expect("preview thread");
                }
                RendererKind::Cpu => cpu = Some(CpuPreview::new(m, clock, perf.clone())),
            }
        }
        PreviewController { tx, generation, quality: 1, perf, seek_started: None, kind, cpu, media, mcp_resolver: None, meter: DisplayMeter::default() }
    }

    fn next_gen(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn current_gen(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Stops the preview threads: the player (joined) and every decoder
    /// session of both resolvers (joined, their processes killed).
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(cpu) = self.cpu.take() {
            let t0 = Instant::now();
            drop(cpu);
            tracing::info!(ms = t0.elapsed().as_millis() as u64, "preview player stopped");
        }
        self.mcp_resolver = None;
    }

    /// The project changed: take a new snapshot before the next frame.
    pub fn invalidate(&mut self) {
        if let Some(c) = &mut self.cpu {
            c.dirty = true;
        }
    }

    /// The resolver MCP `get_frame` renders with (its own: export mode is never superseded).
    pub fn mcp_resolver(&mut self) -> Option<Arc<Resolver>> {
        if self.mcp_resolver.is_none() {
            let media = self.media.clone()?;
            let config = ResolverConfig { cache_bytes: 256 << 20, pool_free_bytes: 64 << 20, max_sessions: 4, ..Default::default() };
            self.mcp_resolver = Some(Arc::new(Resolver::new(Arc::new(FfmpegDecoders::new(media)), config)));
        }
        self.mcp_resolver.clone()
    }

    fn size(&self, project: &Project) -> (u32, u32, f64) {
        let seq = project.sequence();
        let div = match self.quality {
            0 => 1.0,
            1 => 2.0,
            _ => 4.0,
        };
        let max_w = (seq.width as f64 / div).min(1920.0);
        let s = max_w / seq.width.max(1) as f64;
        let w = ((seq.width as f64 * s) as u32).max(2) & !1;
        let h = ((seq.height as f64 * s) as u32).max(2) & !1;
        (w, h, s)
    }

    /// The output the `cpu` preview renders at: the legacy size for the
    /// quality; full quality renders `PreviewHigh`, ½ and ¼ `PreviewFast`.
    pub fn output(&self, project: &Project) -> OutputSpec {
        let (w, h, _) = self.size(project);
        OutputSpec::new(SizeU::new(w, h), if self.quality == 0 { RenderQuality::PreviewHigh } else { RenderQuality::PreviewFast })
    }
}

impl App {
    /// Requests the frame at the playhead (paused mode). With the `cpu`
    /// backend while playing it only refreshes the player's snapshot, so a
    /// live inspector drag shows up in the playing frames.
    pub fn request_frame(&mut self) {
        if self.preview.kind == RendererKind::Cpu {
            return self.request_frame_cpu();
        }
        if self.playing || self.media.is_none() {
            return;
        }
        let seq = self.project.sequence();
        let t = self.playhead.min((seq.duration() - seq.frame_rate.frame_duration()).max(Time::ZERO));
        let v = kadr_timeline::composition::video_at(seq, t);
        let bypass = self.ui().get_bypass();
        let seg = to_seg(&self.project, TimeRange::new(t, t + seq.frame_rate.frame_duration()), v.as_ref(), bypass);
        let ui = self.ui();
        let clip_name = v.as_ref().and_then(|v| seq.clip(v.clip)).map(|c| c.name.clone()).unwrap_or_default();
        ui.set_preview_clip(clip_name.into());
        ui.set_preview_offline(false);
        let offline = v.as_ref().and_then(|v| self.project.asset(v.asset)).is_some_and(|a| !a.exists());
        let empty = if seq.duration() == Time::ZERO {
            1
        } else if offline {
            3
        } else if seg.src.is_none() {
            2
        } else {
            0
        };
        ui.set_preview_empty(empty);
        if empty != 0 {
            ui.set_preview_has_frame(false);
            ui.set_preview_loading(false);
            return;
        }
        let (w, h, px) = self.preview.size(&self.project);
        let generation = self.preview.next_gen();
        self.preview.seek_started = Some((generation, Instant::now()));
        self.ui().set_preview_loading(true);
        let rate = seq.frame_rate;
        let _ = self.preview.tx.send(Cmd::Show { generation, t, seg, w, h, rate, px_scale: px });
    }

    /// The snapshot the preview shows (taken now if the project changed).
    pub fn preview_scenes(&mut self) -> Arc<ProjectScenes> {
        let bypass = self.ui().get_bypass();
        let out = self.preview.output(&self.project);
        let mcp = self.preview.mcp_resolver.clone();
        match &mut self.preview.cpu {
            Some(cpu) => {
                cpu.set_output(out);
                match &cpu.scenes {
                    Some(s) if !cpu.dirty && s.bypass() == bypass => s.clone(),
                    _ => {
                        let s = Arc::new(ProjectScenes::new(&self.project, bypass));
                        cpu.set_scenes(s.clone(), mcp.as_deref());
                        s
                    }
                }
            }
            None => Arc::new(ProjectScenes::new(&self.project, bypass)),
        }
    }

    fn request_frame_cpu(&mut self) {
        if self.preview.cpu.is_none() {
            return;
        }
        let scenes = self.preview_scenes();
        if self.playing {
            return;
        }
        let ui = self.ui();
        let seq = self.project.sequence();
        if seq.duration() == Time::ZERO {
            ui.set_preview_empty(1);
            ui.set_preview_offline(false);
            ui.set_preview_clip("".into());
            ui.set_preview_has_frame(false);
            ui.set_preview_loading(false);
            return;
        }
        let t = self.playhead.min((seq.duration() - seq.frame_rate.frame_duration()).max(Time::ZERO));
        // The same scene the player evaluates (cheap: no pixels), for the overlays.
        let scene = kadr_playback::SceneSource::scene_at(&*scenes, t, &self.preview.output(&self.project));
        let clip_name = scenes.top_clip(&scene).and_then(|id| seq.clip(id)).map(|c| c.name.clone()).unwrap_or_default();
        ui.set_preview_empty(if scene.layers.is_empty() { 2 } else { 0 });
        ui.set_preview_offline(scenes.any_offline(&scene));
        ui.set_preview_clip(clip_name.into());
        ui.set_preview_loading(true);
        if let Some(cpu) = &self.preview.cpu {
            cpu.player().show(t);
        }
    }

    /// A frame the `cpu` player presented (UI thread): shown without copying.
    pub fn on_player_frame(&mut self, f: PlayerFrame) {
        let Some(cpu) = self.preview.cpu.as_mut() else { return };
        // A play or stop came after it, or a newer frame is already on screen.
        if !cpu.player().accepts(f.generation) || f.seq <= cpu.shown_seq {
            return;
        }
        cpu.shown_seq = f.seq;
        let latest = f.generation == cpu.player().generation();
        let ui = self.ui();
        ui.set_preview_frame(slint::Image::from_rgba8_premultiplied(f.buf));
        ui.set_preview_has_frame(true);
        if latest {
            ui.set_preview_loading(false);
        }
        let now = Instant::now();
        self.preview.meter.note(now, now.duration_since(f.posted));
    }

    /// The `cpu` player has the first frame of play `generation` ready (or
    /// [`PREROLL_FALLBACK`] passed): start the audio, which starts the clock.
    pub fn start_pending_audio(&mut self, generation: u64) {
        let Some(cpu) = self.preview.cpu.as_mut() else { return };
        if cpu.pending.as_ref().is_none_or(|p| p.generation != generation) {
            return;
        }
        let p = cpu.pending.take().expect("checked");
        self.audio.play(p.from, p.sources);
    }

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
        self.preview.meter.note(Instant::now(), perf.present);
        if show_loading_done {
            ui.set_preview_loading(false);
            if let Some((_, asked)) = self.preview.seek_started.take_if(|(g, _)| *g == generation) {
                perf.seek_latency = Some(asked.elapsed());
            }
        }
        perf.total = perf.decode_total() + perf.present;
        self.preview.perf.push(perf);
    }

    pub fn start_playback(&mut self) {
        let seq = &self.project.sequence().clone();
        let dur = seq.duration();
        if dur == Time::ZERO {
            return;
        }
        if self.playhead >= dur - seq.frame_rate.frame_duration() {
            self.playhead = Time::ZERO;
        }
        let from = self.playhead;
        let range = TimeRange::new(from, dur);
        let bypass = self.ui().get_bypass();
        let sources: Vec<MixSource> = audio_segments(seq, range)
            .into_iter()
            .filter_map(|a| {
                let pcm = self.assets_rt.get(&a.asset)?.pcm.clone()?;
                // Fades are relative to the whole clip; the mixer sees only
                // the part from `from`, so trim the fade-in accordingly.
                let into = a.range.start - a.clip_range.start;
                let fade_in = (a.props.fade_in - into).max(Time::ZERO);
                Some(MixSource {
                    pcm,
                    timeline_start: a.range.start,
                    timeline_end: a.range.end,
                    source_start: a.source_start,
                    speed: a.speed,
                    gain_db: a.props.gain_db,
                    pan: a.props.pan,
                    fade_in,
                    fade_out: a.props.fade_out.min(a.range.duration()),
                })
            })
            .collect();
        let missing_audio = audio_segments(seq, range).len() - sources.len();
        if missing_audio > 0 {
            self.toast_warn(kadr_i18n::tn("toast.audio_not_ready", missing_audio as i64, &[]));
        }
        if self.preview.kind == RendererKind::Cpu {
            if self.preview.cpu.is_none() {
                return;
            }
            self.preview_scenes();
            self.audio.stop();
            let cpu = self.preview.cpu.as_mut().expect("checked");
            // Pre-roll: the player renders the first frame, then the audio starts.
            let generation = cpu.player().play(from);
            cpu.pending = Some(PendingPlay { generation, from, sources });
            slint::Timer::single_shot(PREROLL_FALLBACK, move || {
                crate::app::with_app(|app| app.start_pending_audio(generation));
            });
        } else {
            let segs: Vec<Seg> = video_segments(seq, range).iter().map(|s| to_seg(&self.project, s.range, s.source.as_ref(), bypass)).collect();
            let (w, h, px) = self.preview.size(&self.project);
            let rate = seq.frame_rate;
            self.audio.play(from, sources);
            let generation = self.preview.next_gen();
            let _ = self.preview.tx.send(Cmd::Play { generation, from, segs, w, h, rate, px_scale: px });
        }
        self.playing = true;
        let ui = self.ui();
        ui.set_playing(true);
        ui.set_preview_empty(0);
        ui.set_preview_offline(false);
        ui.set_preview_clip("".into());
        ui.set_preview_loading(false);
    }

    pub fn stop_playback(&mut self) {
        if !self.playing {
            return;
        }
        self.playing = false;
        self.audio.stop();
        if let Some(cpu) = self.preview.cpu.as_mut() {
            cpu.pending = None;
            cpu.player().stop();
        } else {
            let _ = self.preview.tx.send(Cmd::Stop);
            self.preview.next_gen();
        }
        let ui = self.ui();
        ui.set_playing(false);
        self.playhead = self.project.sequence().frame_rate.snap(self.playhead);
        self.refresh_playhead();
        self.request_frame();
    }

    pub fn restart_playback(&mut self) {
        self.audio.stop();
        self.playing = false;
        self.start_playback();
    }

    pub fn toggle_playback(&mut self) {
        if self.playing { self.stop_playback() } else { self.start_playback() }
    }

    /// 60 Hz: advance the playhead from the audio clock.
    pub fn tick_playback(&mut self) {
        if !self.playing {
            return;
        }
        let dur = self.project.sequence().duration();
        if let Some(t) = self.audio.position() {
            self.playhead = t.min(dur);
            self.refresh_playhead();
            self.follow_playhead();
            if t >= dur {
                self.stop_playback();
                self.playhead = dur;
                self.refresh_playhead();
            }
        }
    }

    pub fn step_frames(&mut self, n: i64) {
        self.stop_playback();
        let fr = self.project.sequence().frame_rate;
        let f = fr.time_to_frame_round(self.playhead) + n;
        self.set_playhead(fr.frame_to_time(f.max(0)));
    }

    pub fn transport(&mut self, action: &str) {
        match action {
            "toggle" => self.toggle_playback(),
            "prev" => self.step_frames(-1),
            "next" => self.step_frames(1),
            "start" => {
                self.stop_playback();
                self.set_playhead(Time::ZERO);
            }
            "end" => {
                self.stop_playback();
                let d = self.project.sequence().duration();
                self.set_playhead(d);
            }
            "safe" => {
                let ui = self.ui();
                ui.set_safe_areas(!ui.get_safe_areas());
            }
            "bypass" => {
                let ui = self.ui();
                ui.set_bypass(!ui.get_bypass());
                self.preview.invalidate();
                // The cpu player takes the new snapshot while playing; legacy restarts.
                if self.playing && self.preview.kind == RendererKind::Legacy { self.restart_playback() } else { self.request_frame() }
            }
            "fullscreen" => {
                self.fullscreen = !self.fullscreen;
                self.ui().window().set_fullscreen(self.fullscreen);
            }
            _ => {}
        }
    }

    pub fn set_quality(&mut self, q: i32) {
        self.preview.quality = q;
        self.ui().set_preview_quality(q);
        self.settings.preview_quality = q;
        self.settings.save(&self.dirs.settings_file());
        self.update_preview_info();
        self.refresh_settings();
        if self.playing { self.restart_playback() } else { self.request_frame() }
    }

    pub fn set_preview_zoom(&mut self, z: i32) {
        self.ui().set_preview_zoom(z);
    }

    pub fn scrub(&mut self, f: f32) {
        let d = self.project.sequence().duration();
        self.set_playhead(Time::from_secs_f64(d.as_secs_f64() * f as f64));
    }

    pub fn update_preview_info(&mut self) {
        let (w, h, _) = self.preview.size(&self.project);
        let seq = self.project.sequence();
        self.ui().set_preview_info(kadr_i18n::tf("preview.info", &[("w", &w.to_string()), ("h", &h.to_string()), ("sw", &seq.width.to_string()), ("sh", &seq.height.to_string())]).into());
        self.ui().set_preview_aspect(seq.width as f32 / seq.height.max(1) as f32);
    }

    pub fn toggle_dev_overlay(&mut self) {
        let ui = self.ui();
        ui.set_dev_overlay(!ui.get_dev_overlay());
        self.tick_dev_overlay();
    }

    /// 2 Hz while the DEV overlay is open: its numbers.
    pub fn tick_dev_overlay(&mut self) {
        let ui = self.ui();
        if !ui.get_dev_overlay() {
            return;
        }
        let snapshot = self.preview.perf.snapshot();
        let summary = self.preview.perf.summary();
        let now = Instant::now();
        let rows = crate::perf_view::dev_rows(&summary, &snapshot, self.preview.meter.fps(now), self.preview.meter.latency_p50(), self.preview.kind.name());
        let rows: Vec<crate::DevRow> = rows.into_iter().map(|(label, value)| crate::DevRow { label: label.into(), value: value.into() }).collect();
        ui.set_dev_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
}

fn worker(media: Arc<dyn MediaBackend>, rx: Receiver<Cmd>, clock: AudioClock, current: Arc<AtomicU64>, perf: Arc<PerfRing>) {
    let mut pending: Option<Cmd> = None;
    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        // Coalesce: keep only the newest queued command.
        let cmd = rx.try_iter().last().unwrap_or(cmd);
        match cmd {
            Cmd::Quit => return,
            Cmd::Stop => {}
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
        }
    }
}

fn decode_one(media: &dyn MediaBackend, t: Time, seg: &Seg, w: u32, h: u32, rate: FrameRate, px: f64) -> Option<RgbaFrame> {
    let (path, src_start, speed, look) = seg.src.clone()?;
    let src_t = src_start + Time::from_secs_f64((t - seg.range.start).as_secs_f64() * speed);
    let req = StreamRequest { path, start: src_t, width: w, height: h, rate, speed, look, px_scale: px };
    let mut s = media.open_stream(&req).map_err(|e| tracing::warn!(error = %e, "preview decode failed")).ok()?;
    s.next_frame().ok().flatten()
}

/// Streams segments from `from`. Returns a command that interrupted playback.
#[allow(clippy::too_many_arguments)]
fn play(
    media: &dyn MediaBackend,
    rx: &Receiver<Cmd>,
    clock: &AudioClock,
    current: &AtomicU64,
    perf: &PerfRing,
    generation: u64,
    from: Time,
    segs: &[Seg],
    w: u32,
    h: u32,
    rate: FrameRate,
    px: f64,
) -> Option<Cmd> {
    let fd = rate.frame_duration();
    let now = || clock.now();
    for seg in segs {
        let start = seg.range.start.max(from);
        if start >= seg.range.end {
            continue;
        }
        match &seg.src {
            None => {
                let allocs = kadr_media::stats::frame_allocs_on_this_thread();
                let black = RgbaFrame::black(w, h);
                let pf = FramePerf { frame_allocs: (kadr_media::stats::frame_allocs_on_this_thread() - allocs) as u32, ..Default::default() };
                post(move |app| app.on_preview_frame(generation, black, true, pf));
                // Wait out the gap.
                loop {
                    if let Ok(c) = rx.try_recv() {
                        return Some(c);
                    }
                    if current.load(Ordering::Acquire) != generation {
                        return None;
                    }
                    if now().is_some_and(|t| t >= seg.range.end) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Some((path, src_start, speed, look)) => {
                let src_t = *src_start + Time::from_secs_f64((start - seg.range.start).as_secs_f64() * speed);
                let req = StreamRequest { path: path.clone(), start: src_t, width: w, height: h, rate, speed: *speed, look: look.clone(), px_scale: px };
                let mut stream = match media.open_stream(&req) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "preview stream failed");
                        continue;
                    }
                };
                let mut i: i64 = 0;
                loop {
                    let ts = start + Time(fd.flicks() * i);
                    if ts >= seg.range.end {
                        break;
                    }
                    if let Ok(c) = rx.try_recv() {
                        return Some(c);
                    }
                    if current.load(Ordering::Acquire) != generation {
                        return None;
                    }
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
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_switch_defaults_to_cpu() {
        assert_eq!(RendererKind::from_env(None), RendererKind::Cpu);
        assert_eq!(RendererKind::from_env(Some("legacy")), RendererKind::Legacy);
        assert_eq!(RendererKind::from_env(Some(" LEGACY ")), RendererKind::Legacy);
        assert_eq!(RendererKind::from_env(Some("cpu")), RendererKind::Cpu);
        assert_eq!(RendererKind::from_env(Some("gpu")), RendererKind::Cpu, "unknown values fall back to cpu");
        assert_eq!(RendererKind::from_env(Some("")), RendererKind::Cpu);
    }

    #[test]
    fn display_meter_counts_the_last_second_and_takes_the_median_latency() {
        let mut m = DisplayMeter::default();
        let t0 = Instant::now();
        for i in 0..50 {
            m.note(t0 + Duration::from_millis(i * 40), Duration::from_millis(i % 5));
        }
        let end = t0 + Duration::from_millis(49 * 40);
        assert_eq!(m.fps(end), 26, "frames within the last second at 25 fps (both ends included)");
        assert_eq!(m.latency_p50(), Some(Duration::from_millis(2)));
        assert_eq!(DisplayMeter::default().latency_p50(), None);
    }
}
