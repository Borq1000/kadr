//! `kadr-bench scene`: cost of one `evaluate` (the scene evaluator), four
//! rows — M1's realistic 3-layer scene with a transition, the keyframes row
//! (10 clips × 13 animated params, keyframes spec §8, target ≤ 100 µs), the
//! effects row (10 clips × 5 motion effects, effects spec §9) and the colour
//! row: the same with a Look (Винтаж) and a Vignette on every clip.

use crate::report::Report;
use kadr_core::perf::Stats;
use kadr_core::{FrameRate, MediaInfo, MediaKind, Time, TimeRange, TransitionId, VideoInfo};
use kadr_project::{AnimParam, Clip, MediaAsset, Project, SourceTime, Track, TrackKind, Transition, TransitionKind};
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

/// 10 full-length clips on 10 video tracks, each with all 13 parameters animated (3 keyframes each).
fn animated_project() -> Project {
    let mut p = Project::new("bench-anim");
    let a = asset(MediaKind::Video, 1920, 1080);
    p.assets.push(a.clone());
    for i in 1..10 {
        p.sequence_mut().tracks.insert(i, Track::new(TrackKind::Video, format!("V{}", i + 1)));
    }
    for i in 0..10 {
        let mut c = Clip::new(a.id, "c", TimeRange::new(Time::ZERO, Time::from_secs(60)), Time::ZERO);
        for param in AnimParam::ALL {
            // Scale starts at 0.3 (not its default 1.0): a full-frame opaque layer would cull the nine below it.
            let base = if param == AnimParam::Scale { 0.3 } else { param.default_value() };
            for (k, s) in [0i64, 30, 60].iter().enumerate() {
                c.animation.set_key(param, SourceTime(Time::from_secs(*s)), base + k as f64 * 0.1, None);
            }
        }
        p.sequence_mut().tracks[i].clips.push(c);
    }
    p
}

/// 10 full-length picture-in-picture clips on 10 video tracks, each with all
/// five motion effects at 100 %.
fn motion_project() -> Project {
    let mut p = Project::new("bench-motion");
    let a = asset(MediaKind::Video, 1920, 1080);
    p.assets.push(a.clone());
    for i in 1..10 {
        p.sequence_mut().tracks.insert(i, Track::new(TrackKind::Video, format!("V{}", i + 1)));
    }
    for i in 0..10 {
        let mut c = Clip::new(a.id, "c", TimeRange::new(Time::ZERO, Time::from_secs(60)), Time::ZERO);
        c.transform.scale = 0.3; // none of the ten covers the canvas: nothing is culled
        for def in kadr_effects::registry().iter().filter(|d| d.category == kadr_effects::Category::Motion) {
            let mut e = kadr_effects::fresh(def, &c, Time::from_secs(30));
            e.params.insert("strength".into(), kadr_project::ParamValue::Number(1.0));
            c.effects.push(e);
        }
        p.sequence_mut().tracks[i].clips.push(c);
    }
    p
}

/// The effects row with colour too: every clip also gets a Look (Винтаж,
/// all five point operations) and a Vignette.
fn look_project() -> Project {
    let mut p = motion_project();
    for track in &mut p.sequence_mut().tracks {
        for c in &mut track.clips {
            let mut look = kadr_effects::fresh(kadr_effects::recipe("look").expect("E2 recipe"), c, Time::from_secs(30));
            look.params.insert("preset".into(), kadr_project::ParamValue::Choice("vintage".into()));
            let vignette = kadr_effects::fresh(kadr_effects::recipe("vignette").expect("E2 recipe"), c, Time::from_secs(30));
            c.effects.extend([look, vignette]);
        }
    }
    p
}

/// Mean cost of one `evaluate` over batches of 100 calls sweeping the scenes' 60 s of frames.
fn sample(p: &Project) -> Stats {
    let seq = p.sequence();
    let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
    let fr = FrameRate::FPS_30;
    let mut samples = vec![];
    // 200 batches of 100 evaluations cycle through frames 0..1800 (all three scenes are
    // 60 s long; the M1 one has its transition at 29..31 s); each sample is the mean per call.
    for batch in 0..200 {
        let started = Instant::now();
        for i in 0..100 {
            let t = fr.frame_to_time(((batch * 100 + i) % 1800) as i64);
            std::hint::black_box(evaluate(p, seq, t, &out));
        }
        samples.push(started.elapsed() / 100);
    }
    Stats::of(samples)
}

pub fn run() -> Result<Report, String> {
    let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
    let mut r = Report::new("m1-scene");
    let s = sample(&project());
    for (metric, v) in [("p50", s.p50), ("p90", s.p90), ("p99", s.p99), ("max", s.max)] {
        r.push("scene", "3 layers + transition", metric, us(v), "µs");
    }
    let s = sample(&animated_project());
    for (metric, v) in [("p50", s.p50), ("p90", s.p90), ("p99", s.p99), ("max", s.max)] {
        r.push("scene", "10 clips × 13 animated params", metric, us(v), "µs");
    }
    let s = sample(&motion_project());
    for (metric, v) in [("p50", s.p50), ("p90", s.p90), ("p99", s.p99), ("max", s.max)] {
        r.push("scene", "10 clips × 5 motion effects", metric, us(v), "µs");
    }
    let s = sample(&look_project());
    for (metric, v) in [("p50", s.p50), ("p90", s.p90), ("p99", s.p99), ("max", s.max)] {
        r.push("scene", "10 clips × motion + Look + Vignette", metric, us(v), "µs");
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

    /// The animated row must really evaluate 10 live layers with animation, not cull them away.
    #[test]
    fn animated_bench_scene_keeps_ten_layers_at_every_sample() {
        let p = animated_project();
        let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
        // Every frame the bench samples.
        for f in 0..1800 {
            let scene = evaluate(&p, p.sequence(), FrameRate::FPS_30.frame_to_time(f), &out);
            assert_eq!(scene.layers.len(), 10, "frame {f}");
        }
        let clips: Vec<_> = p.sequence().tracks.iter().flat_map(|t| &t.clips).collect();
        assert_eq!(clips.len(), 10);
        assert!(clips.iter().all(|c| c.animation.params().len() == 13));
    }

    /// The effects row really evaluates ten layers with five effects each.
    #[test]
    fn motion_bench_scene_keeps_ten_layers_with_their_effects() {
        let p = motion_project();
        let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
        for f in (0..1800).step_by(30) {
            assert_eq!(evaluate(&p, p.sequence(), FrameRate::FPS_30.frame_to_time(f), &out).layers.len(), 10, "frame {f}");
        }
        assert!(p.sequence().tracks.iter().flat_map(|t| &t.clips).all(|c| c.effects.len() == 5));
    }

    /// The colour row really evaluates ten layers, each with its Look's grain.
    #[test]
    fn look_bench_scene_keeps_ten_graded_layers() {
        let p = look_project();
        let out = OutputSpec::new(SizeU::new(1920, 1080), RenderQuality::PreviewHigh);
        for f in (0..1800).step_by(30) {
            let scene = evaluate(&p, p.sequence(), FrameRate::FPS_30.frame_to_time(f), &out);
            assert_eq!(scene.layers.len(), 10, "frame {f}");
            assert!(scene.layers.iter().all(|l| l.effects.iter().any(|e| matches!(e, kadr_scene::Effect::Grain(_)))), "frame {f}");
        }
        assert!(p.sequence().tracks.iter().flat_map(|t| &t.clips).all(|c| c.effects.len() == 7));
    }
}
