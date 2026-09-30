//! M1: cost of `evaluate` for a realistic 3-layer scene with a transition.

use crate::report::Report;
use kadr_core::perf::Stats;
use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, TransitionId, VideoInfo};
use kadr_project::{Clip, MediaAsset, Project, Track, TrackKind, Transition, TransitionKind};
use kadr_scene::{OutputSpec, RenderQuality, SizeU};
use kadr_timeline::scene::evaluate;
use std::time::Instant;

fn asset(kind: MediaKind, w: u32, h: u32) -> MediaAsset {
    let video = VideoInfo { width: w, height: h, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaAsset::new("m", MediaInfo { kind, duration: Time::from_secs(600), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
}

fn project() -> Project {
    let mut p = Project::new("bench");
    let seq = p.sequence_mut();
    seq.tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    seq.tracks.insert(2, Track::new(TrackKind::Video, "V3"));
    let (a, b, logo) = (asset(MediaKind::Video, 1920, 1080), asset(MediaKind::Video, 3840, 2160), asset(MediaKind::Image, 512, 512));
    for x in [&a, &b, &logo] {
        p.assets.push(x.clone());
    }
    let mut clip = |track: usize, asset: &MediaAsset, tl_s: i64, dur_s: i64| {
        let mut c = Clip::new(asset.id, "c", TimeRange::new(Time::from_secs(tl_s), Time::from_secs(tl_s + dur_s)), Time::from_secs(tl_s));
        if track == 1 {
            c.transform.scale = 0.3;
            c.transform.rotation_deg = 5.0;
            c.transform.x = 600.0;
        }
        if track == 2 {
            c.transform.scale = 0.2;
            c.transform.x = -800.0;
            c.transform.y = -400.0;
        }
        p.sequence_mut().tracks[track].clips.push(c);
    };
    clip(0, &a, 0, 30);
    clip(0, &a, 30, 30);
    clip(1, &b, 0, 60);
    clip(2, &logo, 0, 60);
    let track = p.sequence().tracks[0].id;
    p.sequence_mut().transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, track, at: Time::from_secs(30), duration: Time::from_secs(2) });
    p
}

pub fn run() -> Result<Report, String> {
    let p = project();
    let seq = p.sequence();
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
    let fr = FrameRate::FPS_30;
    let mut samples = vec![];
    // Batches of 100 evaluations sweep 60 s around the transition; each sample is the mean per call.
    for batch in 0..200 {
        let started = Instant::now();
        for i in 0..100 {
            let t = fr.frame_to_time(((batch * 100 + i) % 1800) as i64);
            std::hint::black_box(evaluate(&p, seq, t, &out));
        }
        samples.push(started.elapsed() / 100);
    }
    let s = Stats::of(samples);
    let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
    let mut r = Report::new("m1-scene");
    for (metric, v) in [("p50", s.p50), ("p90", s.p90), ("p99", s.p99), ("max", s.max)] {
        r.push("scene", "3 layers + transition", metric, us(v), "µs");
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_scene::LayerContent;

    /// The row is named "3 layers + transition": keep the bench scene honest.
    #[test]
    fn bench_scene_has_three_layers_and_a_transition_at_the_cut() {
        let p = project();
        let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
        let at = |s: i64| evaluate(&p, p.sequence(), Time::from_secs(s), &out);
        let plain = at(10);
        assert_eq!(plain.layers.len(), 3);
        assert!(plain.layers.iter().all(|l| matches!(l.content, LayerContent::Media { .. })));
        let cut = at(30);
        assert_eq!(cut.layers.len(), 3);
        assert!(matches!(cut.layers[0].content, LayerContent::Transition(_)));
    }
}
