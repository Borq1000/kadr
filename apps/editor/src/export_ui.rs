//! Export: builds a backend-agnostic plan from the same composition the
//! preview uses, runs it as a high-priority background job.

use crate::app::{with_app, App};
use crate::preview::look_of;
use crate::ExportState;
use kadr_core::{Time, TimeRange};
use kadr_i18n::{duration, t, tf};
use kadr_jobs::{JobError, JobSpec, JobState, Priority};
use kadr_media::export::ExportVideoSource;
use kadr_project::TransitionKind;
use kadr_media::{ExportAudio, ExportPlan, ExportSettings, ExportTransition, ExportTransitionKind, ExportVideo};
use kadr_timeline::composition::{audio_segments, transitions_into, video_segments};
use std::path::PathBuf;

#[derive(Default)]
pub struct ExportUi {
    pub path: Option<PathBuf>,
    pub job: Option<kadr_jobs::JobId>,
    pub preset: i32,
    pub resolution: i32,
    pub status: String,
    pub done: bool,
    pub failed: bool,
    pub started: Option<std::time::Instant>,
}

/// Native save dialog outside the app borrow.
pub fn choose_path() {
    let default = with_app(|app| format!("{}.mp4", app.display_name())).unwrap_or_else(|| "export.mp4".into());
    if let Some(p) = rfd::FileDialog::new()
        .set_title(t("dlg.export_to"))
        .set_file_name(default)
        .add_filter(t("dlg.filter.mp4"), &["mp4"])
        .add_filter(t("dlg.filter.mov"), &["mov"])
        .save_file()
    {
        with_app(|app| {
            app.export.path = Some(p);
            app.export.done = false;
            app.export.failed = false;
            app.export.status.clear();
            app.refresh_export();
        });
    }
}

impl App {
    pub fn open_export(&mut self) {
        if self.project.sequence().duration() == Time::ZERO {
            return self.toast_warn(t("toast.nothing_to_export"));
        }
        if self.export.job.is_none() {
            self.export.done = false;
            self.export.failed = false;
            self.export.status.clear();
        }
        if self.export.path.is_none() {
            let dir = self
                .path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Videos")));
            self.export.path = dir.map(|d| d.join(format!("{}.mp4", self.display_name())));
        }
        self.refresh_export();
        self.ui().set_export_open(true);
    }

    fn export_size(&self, resolution: i32) -> (u32, u32) {
        let seq = self.project.sequence();
        match resolution {
            1 => fit(seq.width, seq.height, 1920, 1080),
            2 => fit(seq.width, seq.height, 1280, 720),
            3 => fit(seq.width, seq.height, 3840, 2160),
            _ => (seq.width, seq.height),
        }
    }

    pub fn build_export_plan(&self, output: PathBuf, preset: i32, resolution: i32) -> ExportPlan {
        let seq = self.project.sequence();
        let dur = seq.duration();
        let range = TimeRange::new(Time::ZERO, dur);
        let (w, h) = self.export_size(resolution);
        let px_scale = w as f64 / seq.width.max(1) as f64;
        let segs = video_segments(seq, range);
        let transitions = transitions_into(seq, &segs);
        let video = segs
            .into_iter()
            .zip(transitions)
            .map(|(s, tr)| ExportVideo {
                transition_in: tr.map(|t| ExportTransition {
                    kind: match t.kind {
                        TransitionKind::CrossDissolve => ExportTransitionKind::Dissolve,
                        TransitionKind::DipToBlack => ExportTransitionKind::DipToBlack,
                        TransitionKind::Wipe => ExportTransitionKind::Wipe,
                    },
                    duration: t.duration,
                }),
                duration: s.range.duration(),
                source: s.source.and_then(|v| {
                    let a = self.project.asset(v.asset)?;
                    let mut look = look_of(&v.transform, &v.color, false);
                    look.x *= px_scale;
                    look.y *= px_scale;
                    Some(ExportVideoSource { path: a.path.clone(), source_start: v.source_start, speed: v.speed, look })
                }),
            })
            .collect();
        let audio = audio_segments(seq, range)
            .into_iter()
            .filter_map(|a| {
                let asset = self.project.asset(a.asset)?;
                Some(ExportAudio {
                    path: asset.path.clone(),
                    source_start: a.source_start,
                    timeline_start: a.range.start,
                    duration: a.range.duration(),
                    speed: a.speed,
                    gain_db: a.props.gain_db,
                    pan: a.props.pan,
                    fade_in: a.props.fade_in,
                    fade_out: a.props.fade_out,
                })
            })
            .collect();
        let (crf, preset_name) = match preset {
            0 => (18, "medium"),
            1 => (21, "fast"),
            _ => (26, "veryfast"),
        };
        ExportPlan {
            output,
            total: dur,
            video,
            audio,
            settings: ExportSettings { width: w, height: h, rate: seq.frame_rate, sample_rate: seq.sample_rate, crf, preset: preset_name.into(), ..Default::default() },
        }
    }

    /// Rough output size so the user isn't surprised by a 20 GB file.
    fn estimate_label(&self, preset: i32, resolution: i32) -> String {
        let seq = self.project.sequence();
        let (w, h) = self.export_size(resolution);
        let base_kbps = match preset {
            0 => 12_000.0,
            1 => 8_000.0,
            _ => 4_000.0,
        };
        let scale = (w as f64 * h as f64 * seq.frame_rate.as_f64()) / (1920.0 * 1080.0 * 30.0);
        let kbps = base_kbps * scale.powf(0.8) + 192.0;
        let bytes = kbps * 1000.0 / 8.0 * seq.duration().as_secs_f64();
        tf("export.estimate", &[("size", &crate::util::fmt_bytes(bytes as u64)), ("w", &w.to_string()), ("h", &h.to_string()), ("fps", &seq.frame_rate.to_string())])
    }

    pub fn export_changed(&mut self, preset: i32, resolution: i32) {
        self.export.preset = preset;
        self.export.resolution = resolution;
        self.refresh_export();
    }

    pub fn export_start(&mut self, preset: i32, resolution: i32) {
        let Some(media) = self.media.clone() else { return self.toast_error(t("err.ffmpeg_missing")) };
        let Some(path) = self.export.path.clone() else { return };
        let offline = self.project.assets.iter().filter(|a| !a.exists()).count();
        if offline > 0 {
            return self.toast_error(kadr_i18n::tn("err.export.offline", offline as i64, &[]));
        }
        if let Some(dir) = path.parent() {
            if !dir.exists() {
                return self.toast_error(tf("err.export.no_folder", &[("path", &dir.display().to_string())]));
            }
        }
        let plan = self.build_export_plan(path.clone(), preset, resolution);
        tracing::info!(output = %path.display(), segments = plan.video.len(), audio = plan.audio.len(), "export requested");
        self.export.preset = preset;
        self.export.resolution = resolution;
        self.export.done = false;
        self.export.failed = false;
        self.export.status = t("export.starting");
        self.export.started = Some(std::time::Instant::now());
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.export.job = Some(self.jobs.submit(JobSpec::new(tf("jobs.title.export", &[("name", &name)]), "export").priority(Priority::High), move |ctx| {
            let prog = |p: f32| ctx.progress(p);
            media.export(&plan, &prog, &ctx.cancel).map_err(|e| match e {
                kadr_media::MediaError::Cancelled => JobError::Cancelled,
                e => JobError::Fatal(e.to_string()),
            })
        }));
        self.refresh_export();
    }

    pub fn export_cancel(&mut self) {
        if let Some(j) = self.export.job {
            self.jobs.cancel(j);
        }
    }

    pub fn export_reveal(&mut self) {
        if let Some(p) = &self.export.path {
            crate::util::reveal(p);
        }
    }

    pub fn export_open_file(&mut self) {
        if let Some(p) = &self.export.path {
            crate::util::open_file(p);
        }
    }

    /// Called from the status tick.
    pub fn export_poll(&mut self) {
        let Some(j) = self.export.job else { return };
        let Some(info) = self.jobs.info(j) else { return };
        match info.state {
            JobState::Done => {
                let secs = self.export.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                let size = self.export.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| crate::util::fmt_bytes(m.len())).unwrap_or_default();
                self.export.status = tf("export.done", &[("time", &duration(secs)), ("size", &size)]);
                self.export.done = true;
                self.export.job = None;
                self.toast_ok(t("toast.export_done"));
            }
            JobState::Failed(e) => {
                self.export.status = tf("export.failed", &[("error", &e)]);
                self.export.failed = true;
                self.export.job = None;
                self.toast_error(t("toast.export_failed"));
            }
            JobState::Cancelled => {
                self.export.status = t("export.cancelled");
                self.export.failed = true;
                self.export.job = None;
            }
            _ => {
                let eta = match (self.export.started, info.progress) {
                    (Some(s), p) if p > 0.02 => {
                        let el = s.elapsed().as_secs_f64();
                        tf("export.eta", &[("time", &duration(el / p as f64 - el))])
                    }
                    _ => String::new(),
                };
                self.export.status = tf("export.rendering", &[("pct", &format!("{:.0}", info.progress * 100.0)), ("eta", &eta)]);
            }
        }
        self.refresh_export();
    }

    pub fn refresh_export(&mut self) {
        let ui = self.ui();
        let progress = self.export.job.and_then(|j| self.jobs.info(j)).map(|i| i.progress).unwrap_or(if self.export.done { 1.0 } else { 0.0 });
        let estimate = if self.project.sequence().duration() > Time::ZERO { self.estimate_label(self.export.preset, self.export.resolution) } else { String::new() };
        ui.set_export_state(ExportState {
            path: self.export.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default().into(),
            preset: self.export.preset,
            resolution: self.export.resolution,
            running: self.export.job.is_some(),
            progress,
            status: self.export.status.clone().into(),
            done: self.export.done,
            failed: self.export.failed,
            duration: duration(self.project.sequence().duration().as_secs_f64()).into(),
            estimate: estimate.into(),
        });
    }
}

fn fit(w: u32, h: u32, mw: u32, mh: u32) -> (u32, u32) {
    let s = (mw as f64 / w.max(1) as f64).min(mh as f64 / h.max(1) as f64);
    (((w as f64 * s) as u32).max(2) & !1, ((h as f64 * s) as u32).max(2) & !1)
}
