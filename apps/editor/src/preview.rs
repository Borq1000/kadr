//! Preview: a decode thread driven by commands.
//!
//! - Paused: single frames on demand, coalesced (latest request wins), so
//!   scrubbing never queues up stale decodes.
//! - Playing: streams the composition segment by segment and paces frames
//!   against the audio master clock, dropping late frames.
//!
//! Frames go through the same `look_filter` as export (WYSIWYG).

use crate::app::{post, App};
use crossbeam_channel::{Receiver, Sender};
use slint::ComponentHandle;
use kadr_audio::{AudioClock, MixSource};
use kadr_core::perf::{FramePerf, LayerTiming, PerfRing};
use kadr_core::{FrameRate, Time, TimeRange};
use kadr_media::export::VideoLook;
use kadr_media::{MediaBackend, RgbaFrame, StreamRequest};
use kadr_project::{ColorAdjust, Project, Transform};
use kadr_timeline::composition::{audio_segments, video_segments, VideoSource};
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

pub struct PreviewController {
    tx: Sender<Cmd>,
    generation: Arc<AtomicU64>,
    pub quality: i32,
    /// Last 600 preview frames (MCP `get_perf`, DEV overlay later).
    pub perf: Arc<PerfRing>,
    /// Generation and start time of the pending paused-frame request.
    pub seek_started: Option<(u64, Instant)>,
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

    fn next_gen(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn current_gen(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Cmd::Quit);
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
}

impl App {
    /// Requests the frame at the playhead (paused mode).
    pub fn request_frame(&mut self) {
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
        let segs: Vec<Seg> = video_segments(seq, range).iter().map(|s| to_seg(&self.project, s.range, s.source.as_ref(), bypass)).collect();
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
        let (w, h, px) = self.preview.size(&self.project);
        let rate = seq.frame_rate;
        self.audio.play(from, sources);
        let generation = self.preview.next_gen();
        let _ = self.preview.tx.send(Cmd::Play { generation, from, segs, w, h, rate, px_scale: px });
        self.playing = true;
        let ui = self.ui();
        ui.set_playing(true);
        ui.set_preview_empty(0);
        ui.set_preview_clip("".into());
    }

    pub fn stop_playback(&mut self) {
        if !self.playing {
            return;
        }
        self.playing = false;
        self.audio.stop();
        let _ = self.preview.tx.send(Cmd::Stop);
        self.preview.next_gen();
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
                if self.playing { self.restart_playback() } else { self.request_frame() }
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
