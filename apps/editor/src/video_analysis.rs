//! Background picture analysis: a 4 fps 160×90 proxy stream → per-frame
//! statistics (cached) → shots with bucketed quality words stored in the
//! project. Feeds shot grading and multicam camera picks.

use crate::app::{post, App};
use kadr_analysis::video::*;
use kadr_cache::AssetCache;
use kadr_core::{AssetId, FrameRate, Time};
use kadr_i18n::tf;
use kadr_jobs::{JobError, JobSpec, Priority};
use kadr_media::export::VideoLook;
use kadr_media::{MediaBackend, StreamRequest};
use kadr_project::{AnalysisData, AnalysisResult, ShotSummary};
use std::path::PathBuf;
use std::sync::Arc;

pub fn shot_summaries(shots: &[Shot]) -> Vec<ShotSummary> {
    shots
        .iter()
        .map(|s| ShotSummary {
            range: s.range,
            sharpness: bucket_sharpness(s.sharpness).into(),
            exposure: bucket_exposure(s.luma).into(),
            shake: bucket_shake(s.motion).into(),
            black: s.black,
        })
        .collect()
}

/// Streams the whole asset once; cancel-aware, progress in 0..1.
fn analyze(media: &dyn MediaBackend, path: PathBuf, duration: Time, ctx: &kadr_jobs::JobCtx) -> Result<VideoOverview, JobError> {
    let req = StreamRequest {
        path,
        start: Time::ZERO,
        width: ANALYSIS_W,
        height: ANALYSIS_H,
        rate: FrameRate::new(ANALYSIS_FPS, 1),
        speed: 1.0,
        look: VideoLook::default(),
        px_scale: 1.0,
    };
    let mut stream = media.open_stream(&req).map_err(|e| JobError::Retryable(e.to_string()))?;
    let expected = (duration.as_secs_f64() * ANALYSIS_FPS as f64).max(1.0);
    let mut frames = vec![];
    let mut prev: Option<Vec<u8>> = None;
    while let Some(f) = stream.next_frame().map_err(|e| JobError::Retryable(e.to_string()))? {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let g = rgba_to_gray(&f.data);
        frames.push(analyze_gray_frame(&g, f.width, f.height, prev.as_deref()));
        prev = Some(g);
        ctx.progress((frames.len() as f64 / expected).min(1.0) as f32);
    }
    Ok(VideoOverview { fps: ANALYSIS_FPS, frames })
}

impl App {
    /// Low-priority job: the editor is fully usable without it.
    pub(crate) fn submit_video_analysis(&mut self, id: AssetId, media: Arc<dyn MediaBackend>, cache: Arc<AssetCache>, path: PathBuf, duration: Time, name: &str) {
        let spec = JobSpec::new(tf("jobs.title.video", &[("name", name)]), "video").priority(Priority::Background).retries(1);
        self.jobs.submit(spec, move |ctx| {
            let ov = match cache.read("video.bin", VIDEO_VERSION).and_then(|b| VideoOverview::from_bytes(&b)) {
                Some(ov) => ov,
                None => {
                    let ov = analyze(media.as_ref(), path.clone(), duration, &ctx)?;
                    cache.write("video.bin", VIDEO_VERSION, &ov.to_bytes())?;
                    ov
                }
            };
            let shots = shot_summaries(&detect_shots(&ov, Time::from_secs(1)));
            post(move |app| app.on_shots_ready(id, shots));
            Ok(())
        });
    }

    fn on_shots_ready(&mut self, id: AssetId, shots: Vec<ShotSummary>) {
        let data = AnalysisData::Shots { shots };
        let old = self.project.analysis.iter().position(|a| a.asset == id && matches!(a.data, AnalysisData::Shots { .. }));
        if old.is_some_and(|i| self.project.analysis[i].data == data) {
            return;
        }
        tracing::info!(?id, shots = match &data { AnalysisData::Shots { shots } => shots.len(), _ => 0 }, "video analysis ready");
        if let Some(i) = old {
            self.project.analysis.remove(i);
        }
        self.project.analysis.push(AnalysisResult { asset: id, algo_version: VIDEO_VERSION, data });
        self.meta_dirty = true;
        self.refresh_library();
        self.refresh_timeline();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_analysis::video::Shot;
    use kadr_core::{Time, TimeRange};

    #[test]
    fn shots_become_word_summaries() {
        let s = |a, b, sharp, luma, motion, black| Shot {
            range: TimeRange::new(Time::from_secs(a), Time::from_secs(b)),
            sharpness: sharp,
            luma,
            motion,
            black,
        };
        let got = shot_summaries(&[s(0, 4, 0.02, 0.5, 0.01, false), s(4, 6, 0.0, 0.01, 0.2, true)]);
        assert_eq!((got[0].sharpness.as_str(), got[0].exposure.as_str(), got[0].shake.as_str()), ("sharp", "normal", "none"));
        assert_eq!((got[1].sharpness.as_str(), got[1].exposure.as_str(), got[1].shake.as_str()), ("very blurry", "black", "heavy"));
        assert!(got[1].black && got[1].range.start == Time::from_secs(4));
    }
}
