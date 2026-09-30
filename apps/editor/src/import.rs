//! Media import pipeline: probe → thumbnail → audio proxy (PCM) → overview
//! (peaks + levels) → silence analysis. Every step is a background job and
//! is cached with a versioned key, so reopening a project recomputes nothing.

use crate::app::{post, with_app, App, AssetStatus};
use kadr_analysis::{analyze_pcm, detect_silence, AudioOverview, SilenceParams, OVERVIEW_VERSION, SILENCE_VERSION};
use kadr_audio::{PCM_CHANNELS, PCM_RATE};
use kadr_core::{AssetId, MediaInfo, MediaKind, Time, TimeRange};
use kadr_jobs::{JobError, JobSpec, Priority};
use kadr_i18n::{t, tf, tn};
use kadr_project::{AnalysisData, AnalysisResult, MediaAsset};
use std::path::PathBuf;
use std::sync::Arc;

const THUMB_VERSION: u32 = 1;
const PCM_VERSION: u32 = 1;
const THUMB_H: u32 = 108;

pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "mxf", "m4v", "webm", "mts", "m2ts", "wmv", "mpg", "mpeg", "3gp", "wav", "mp3", "aac", "m4a",
    "flac", "ogg", "opus", "aif", "aiff", "wma", "png", "jpg", "jpeg", "bmp", "tif", "tiff", "webp",
];

/// File > Import. The native dialog runs outside the app borrow.
pub fn import_dialog() {
    let files = rfd::FileDialog::new()
        .set_title(t("dlg.import"))
        .add_filter(t("dlg.filter.media"), MEDIA_EXTENSIONS)
        .add_filter(t("dlg.filter.all"), &["*"])
        .pick_files();
    if let Some(files) = files {
        with_app(|app| app.import_paths(files));
    }
}

impl App {
    pub fn import_paths(&mut self, paths: Vec<PathBuf>) {
        let Some(media) = self.media.clone() else {
            return self.toast_error(t("err.ffmpeg_missing"));
        };
        let mut n = 0;
        for path in paths {
            if self.project.assets.iter().any(|a| a.path == path) {
                continue;
            }
            n += 1;
            let m = media.clone();
            let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let bin = self.bin_filter;
            self.jobs.submit(JobSpec::new(tf("jobs.title.probe", &[("name", &name)]), "import").priority(Priority::High), move |_ctx| {
                let info = match m.probe(&path) {
                    Ok(i) => i,
                    Err(e) => {
                        let (n, err) = (name.clone(), e.to_string());
                        post(move |app| app.toast_error(tf("err.import.unsupported", &[("name", &n), ("error", &err)])));
                        return Err(JobError::Fatal(e.to_string()));
                    }
                };
                let p = path.clone();
                post(move |app| app.on_probed(p, info, bin));
                Ok(())
            });
        }
        if n > 0 {
            self.toast(tn("toast.importing", n as i64, &[]));
        }
    }

    fn on_probed(&mut self, path: PathBuf, info: MediaInfo, bin: Option<kadr_core::BinId>) {
        let mut asset = MediaAsset::new(&path, info);
        asset.bin = bin;
        asset.source_mtime_ms = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        tracing::info!(name = %asset.name, kind = ?asset.kind(), duration = ?asset.duration(), "imported");
        let id = asset.id;
        self.project.assets.push(asset);
        self.meta_dirty = true;
        self.selected_asset = Some(id);
        self.process_asset(id);
        self.refresh_library();
        self.refresh_status();
    }

    /// Starts (or restarts) thumbnail + audio processing for an asset.
    pub fn process_asset(&mut self, id: AssetId) {
        let Some(asset) = self.project.asset(id).cloned() else { return };
        let Some(media) = self.media.clone() else { return };
        let rt = self.assets_rt.entry(id).or_default();
        if !asset.exists() {
            rt.status = AssetStatus::Missing;
            return;
        }
        rt.status = AssetStatus::Working;
        let cache = match self.cache.for_source(&asset.path) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                rt.status = AssetStatus::Error(e.to_string());
                return;
            }
        };

        if asset.kind() == MediaKind::Video {
            self.submit_video_analysis(id, media.clone(), cache.clone(), asset.path.clone(), asset.duration(), &asset.name);
        }
        if asset.kind() != MediaKind::Audio {
            let still = asset.kind() == MediaKind::Image;
            let (m, c, path, dur) = (media.clone(), cache.clone(), asset.path.clone(), asset.duration());
            self.jobs.submit(JobSpec::new(tf("jobs.title.thumb", &[("name", &asset.name)]), "thumbnails").priority(Priority::High).retries(1), move |ctx| {
                let frame = match c.read("thumb.rgba", THUMB_VERSION).and_then(|b| decode_thumb(&b)) {
                    Some(f) => f,
                    None => {
                        let at = thumb_time(still, dur);
                        let f = m
                            .thumbnails(&path, &[at], THUMB_H, &ctx.cancel)
                            .map_err(|e| JobError::Retryable(e.to_string()))?
                            .pop()
                            .ok_or(JobError::Fatal("no frame".into()))?;
                        let _ = c.write("thumb.rgba", THUMB_VERSION, &encode_thumb(&f));
                        f
                    }
                };
                post(move |app| app.on_thumb(id, frame));
                Ok(())
            });
        }

        if asset.has_audio() {
            let (m, c, path, dur, name) = (media, cache, asset.path.clone(), asset.duration(), asset.name.clone());
            let job = self.jobs.submit(JobSpec::new(tf("jobs.title.audio", &[("name", &name)]), "audio").priority(Priority::Normal).retries(1), move |ctx| {
                // 1. Audio proxy: raw PCM for playback, waveform and analysis.
                let pcm = c.path("audio.pcm");
                if !c.is_valid("audio.pcm", PCM_VERSION) {
                    let prog = |p: f32| ctx.progress(p * 0.8);
                    m.extract_pcm(&path, &pcm, PCM_RATE, PCM_CHANNELS, dur, &prog, &ctx.cancel).map_err(|e| match e {
                        kadr_media::MediaError::Cancelled => JobError::Cancelled,
                        e => JobError::Retryable(e.to_string()),
                    })?;
                    c.commit("audio.pcm", PCM_VERSION)?;
                }
                // 2. Peaks + level envelope.
                let ov = match c.read("overview.bin", OVERVIEW_VERSION).and_then(|b| AudioOverview::from_bytes(&b)) {
                    Some(ov) => ov,
                    None => {
                        let f = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(&pcm)?);
                        let total = dur.as_secs_f64().max(0.001);
                        let ov = analyze_pcm(f, PCM_RATE, PCM_CHANNELS, |s| {
                            ctx.progress(0.8 + 0.18 * (s / total) as f32);
                            !ctx.cancel.is_cancelled()
                        })?;
                        c.write("overview.bin", OVERVIEW_VERSION, &ov.to_bytes())?;
                        ov
                    }
                };
                // 3. Silence (raw, fine-grained; the planner applies user thresholds).
                let params = SilenceParams { threshold_db: None, min_duration: Time::from_millis(300), padding: Time::ZERO, max_blip: Time::from_millis(80) };
                let (threshold, ranges) = detect_silence(&ov.levels_db, Time::from_millis(10), &params);
                let ov = Arc::new(ov);
                post(move |app| app.on_audio_ready(id, pcm, ov, threshold, ranges));
                Ok(())
            });
            if let Some(rt) = self.assets_rt.get_mut(&id) {
                rt.job = Some(job);
            }
        } else if let Some(rt) = self.assets_rt.get_mut(&id) {
            rt.status = AssetStatus::Ready;
        }
    }

    fn on_thumb(&mut self, id: AssetId, f: kadr_media::RgbaFrame) {
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&f.data, f.width, f.height);
        self.assets_rt.entry(id).or_default().thumb = Some(slint::Image::from_rgba8(buf));
        if self.project.asset(id).is_some_and(|a| !a.has_audio()) {
            self.assets_rt.entry(id).or_default().status = AssetStatus::Ready;
        }
        self.refresh_library();
        self.refresh_timeline();
    }

    fn on_audio_ready(&mut self, id: AssetId, pcm: PathBuf, ov: Arc<AudioOverview>, threshold: f32, ranges: Vec<TimeRange>) {
        let rt = self.assets_rt.entry(id).or_default();
        rt.pcm = Some(pcm);
        rt.overview = Some(ov);
        rt.status = AssetStatus::Ready;
        rt.progress = 1.0;
        rt.job = None;
        tracing::info!(?id, silences = ranges.len(), threshold, "audio analysis ready");
        self.project.analysis.retain(|a| !(a.asset == id && matches!(a.data, AnalysisData::Silence { .. })));
        self.project.analysis.push(AnalysisResult {
            asset: id,
            algo_version: SILENCE_VERSION,
            data: AnalysisData::Silence { threshold_db: threshold, min_duration: Time::from_millis(300), ranges },
        });
        self.waves.clear();
        self.refresh_library();
        self.refresh_timeline();
    }

    /// Mirrors job state (progress/errors) into the per-asset status.
    pub fn sync_asset_jobs(&mut self) -> bool {
        let mut changed = false;
        for rt in self.assets_rt.values_mut() {
            let Some(job) = rt.job else { continue };
            let Some(info) = self.jobs.info(job) else { continue };
            match info.state {
                kadr_jobs::JobState::Failed(e) => {
                    rt.status = AssetStatus::Error(e);
                    rt.job = None;
                    changed = true;
                }
                kadr_jobs::JobState::Cancelled => {
                    rt.status = AssetStatus::Error(t("media.status.cancelled"));
                    rt.job = None;
                    changed = true;
                }
                kadr_jobs::JobState::Running if (rt.progress - info.progress).abs() > 0.01 => {
                    rt.progress = info.progress;
                    rt.status = AssetStatus::Working;
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    pub fn cancel_all_asset_jobs(&mut self) {
        for rt in self.assets_rt.values() {
            if let Some(j) = rt.job {
                self.jobs.cancel(j);
            }
        }
    }
}

fn encode_thumb(f: &kadr_media::RgbaFrame) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + f.data.len());
    v.extend_from_slice(&f.width.to_le_bytes());
    v.extend_from_slice(&f.height.to_le_bytes());
    v.extend_from_slice(&f.data);
    v
}

fn decode_thumb(b: &[u8]) -> Option<kadr_media::RgbaFrame> {
    let w = u32::from_le_bytes(b.get(0..4)?.try_into().ok()?);
    let h = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?);
    let data = b.get(8..8 + (w * h * 4) as usize)?.to_vec();
    Some(kadr_media::RgbaFrame { width: w, height: h, data })
}

/// Where the library thumbnail is taken: a tenth into a longer video (at
/// most 3 s), else the first frame. A still has only that one frame:
/// seeking into its nominal duration finds nothing (the job failed).
fn thumb_time(still: bool, dur: Time) -> Time {
    if !still && dur > Time::from_secs(4) { Time::from_secs_f64((dur.as_secs_f64() * 0.1).min(3.0)) } else { Time::ZERO }
}

#[cfg(test)]
mod thumb_tests {
    use super::*;

    #[test]
    fn stills_are_thumbnailed_at_their_only_frame() {
        assert_eq!(thumb_time(true, Time::from_secs(5)), Time::ZERO);
        assert_eq!(thumb_time(false, Time::from_secs(5)), Time::from_millis(500));
        assert_eq!(thumb_time(false, Time::from_secs(60)), Time::from_secs(3));
        assert_eq!(thumb_time(false, Time::from_secs(2)), Time::ZERO);
    }
}
