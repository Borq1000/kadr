//! M5: export on the render pipeline (`kadr-playback::export`) against the legacy FFmpeg-graph
//! exporter: fps on the M0 scenario, at 4K and with three layers; PSNR of the two on a
//! cuts-only single track; and the 20-minute A/V sync export (spec §8).

use crate::avsync;
use crate::media::{self, TestClip};
use crate::report::Report;
use kadr_core::{AssetId, CancelToken, FrameRate, LinkId, Time, TimeRange, TransitionId};
use kadr_media::export::{ExportVideoSource, VideoLook};
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::{EncodeJob, ExportAudio, ExportPlan, ExportSettings, ExportTransition, ExportTransitionKind, ExportVideo, MediaBackend};
use kadr_media::{FrameEncoder, MediaError};
use kadr_playback::{EncoderFactory, ExportRequest, ExportStats, FfmpegDecoders, MissingPolicy};
use kadr_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind, Transition, TransitionKind};
use kadr_project_scenes::ProjectScenes;
use kadr_timeline::composition::{audio_segments, transitions_into, video_segments};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

/// Runs of every fps row; the run with the median fps is reported.
const RUNS: usize = 3;

// ---------------------------------------------------------------- project building

fn new_project(name: &str, w: u32, h: u32, fps: FrameRate) -> Project {
    let mut p = Project::new(name);
    let seq = p.sequence_mut();
    seq.width = w;
    seq.height = h;
    seq.frame_rate = fps;
    p
}

fn probe(media: &FfmpegCli, path: &Path) -> Result<MediaAsset, String> {
    Ok(MediaAsset::new(path, media.probe(path).map_err(|e| format!("probe {}: {e}", path.display()))?))
}

fn video_track(seq: &Sequence) -> usize {
    seq.tracks.iter().position(|t| t.kind == TrackKind::Video).expect("V1")
}

fn dissolve(seq: &mut Sequence, track: usize, at: Time, duration: Time) {
    let track = seq.tracks[track].id;
    seq.transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, track, at, duration });
}

/// `segments` × (source start, duration) laid end to end on V1.
fn cuts_on_v1(seq: &mut Sequence, asset: AssetId, segments: &[(Time, Time)]) {
    let v1 = video_track(seq);
    let mut at = Time::ZERO;
    for (i, (start, dur)) in segments.iter().enumerate() {
        seq.tracks[v1].clips.push(Clip::new(asset, format!("seg{i}"), TimeRange::new(*start, *start + *dur), at));
        at += *dur;
    }
}

/// The M0 export scenario as a project: 10 × 2 s segments of `clip` from source offsets `i·5 s`,
/// 0.5 s dissolves into segments 3 and 7 (M0 §6).
fn m0_project(clip: &MediaAsset) -> Project {
    let mut p = new_project("m0", 1920, 1080, FrameRate::FPS_30);
    let segs: Vec<_> = (0..10).map(|i| (Time::from_secs(i * 5), Time::from_secs(2))).collect();
    let seq = p.sequence_mut();
    cuts_on_v1(seq, clip.id, &segs);
    let v1 = video_track(seq);
    for i in [3, 7] {
        dissolve(seq, v1, Time::from_secs(i * 2), Time::from_millis(500));
    }
    p.assets.push(clip.clone());
    p
}

/// 4K: four 2.5 s segments (three cuts) of the 2160p clip, 10 s.
fn uhd_project(clip: &MediaAsset) -> Project {
    let mut p = new_project("uhd", 3840, 2160, FrameRate::FPS_30);
    let segs: Vec<_> = (0..4).map(|i| (Time::from_secs(i * 5), Time::from_millis(2500))).collect();
    cuts_on_v1(p.sequence_mut(), clip.id, &segs);
    p.assets.push(clip.clone());
    p
}

/// The M0 timeline plus, over all of it, a 4K clip as a picture-in-picture (scale 0.3, rotated 5°)
/// on V2 and a straight-alpha logo on V3.
fn three_layer_project(clip: &MediaAsset, uhd: &MediaAsset, logo: &MediaAsset) -> Project {
    let mut p = m0_project(clip);
    let total = Time::from_secs(20);
    let seq = p.sequence_mut();
    let mut pip = Clip::new(uhd.id, "pip", TimeRange::new(Time::ZERO, total), Time::ZERO);
    pip.transform.scale = 0.3;
    pip.transform.rotation_deg = 5.0;
    pip.transform.x = 560.0;
    pip.transform.y = -290.0;
    let mut mark = Clip::new(logo.id, "logo", TimeRange::new(Time::ZERO, total), Time::ZERO);
    mark.transform.scale = 0.5;
    mark.transform.x = -700.0;
    mark.transform.y = 380.0;
    seq.tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    seq.tracks[1].clips.push(pip);
    seq.tracks.insert(2, Track::new(TrackKind::Video, "V3"));
    seq.tracks[2].clips.push(mark);
    p.assets.extend([uhd.clone(), logo.clone()]);
    p
}

/// A 512×512 straight-alpha PNG: a colour gradient disc with a 56 px soft edge.
fn ensure_logo(dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join("logo512.png");
    if path.exists() {
        return Ok(path);
    }
    let graph = "color=c=black:s=512x512:d=1,format=gbrap,geq=r='255*X/W':g='140':b='255*Y/H':a='255*clip(1-(hypot(X-W/2,Y-H/2)-200)/56,0,1)',format=rgba";
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", graph, "-frames:v", "1", "-pix_fmt", "rgba"])
        .arg(&path)
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if out.status.success() { Ok(path) } else { Err(format!("logo: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

// ---------------------------------------------------------------- exports

fn settings(w: u32, h: u32, rate: FrameRate) -> ExportSettings {
    ExportSettings { width: w, height: h, rate, crf: 21, preset: "fast".into(), ..Default::default() }
}

/// The encode job the app builds (`encode_job`): even size, frames as the legacy exporter rounds them.
fn encode_job(p: &Project, output: PathBuf, audio: Vec<ExportAudio>) -> EncodeJob {
    let seq = p.sequence();
    let total = seq.duration();
    let (w, h) = (seq.width & !1, seq.height & !1);
    EncodeJob { output, width: w, height: h, rate: seq.frame_rate, frames: seq.frame_rate.time_to_frame_round(total), total, audio, settings: settings(w, h, seq.frame_rate) }
}

fn export_audio(p: &Project) -> Vec<ExportAudio> {
    let seq = p.sequence();
    audio_segments(seq, TimeRange::new(Time::ZERO, seq.duration()))
        .into_iter()
        .filter_map(|a| {
            let asset = p.asset(a.asset)?;
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
        .collect()
}

fn export_new(media: &Arc<dyn MediaBackend>, p: &Project, job: EncodeJob, progress: &(dyn Fn(f32) + Sync)) -> Result<ExportStats, String> {
    let req = ExportRequest::new(Arc::new(ProjectScenes::new(p, false)), job).with_missing(MissingPolicy::Fail);
    kadr_playback::export::export(req, Arc::new(FfmpegDecoders::new(media.clone())), &**media, progress, &CancelToken::new()).map_err(|e| e.to_string())
}

/// The legacy plan of a project with no look (a cuts/dissolves-only timeline), as `build_export_plan`.
fn legacy_plan(p: &Project, output: PathBuf, audio: Vec<ExportAudio>) -> ExportPlan {
    let seq = p.sequence();
    let total = seq.duration();
    let segs = video_segments(seq, TimeRange::new(Time::ZERO, total));
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
            source: s.source.and_then(|v| Some(ExportVideoSource { path: p.asset(v.asset)?.path.clone(), source_start: v.source_start, speed: v.speed, look: VideoLook::default() })),
        })
        .collect();
    ExportPlan { output, total, video, audio, settings: settings(seq.width & !1, seq.height & !1, seq.frame_rate) }
}

fn export_legacy(ff: &FfmpegCli, plan: &ExportPlan) -> Result<f64, String> {
    let started = Instant::now();
    ff.export(plan, &|_| {}, &CancelToken::new()).map_err(|e| e.to_string())?;
    Ok(started.elapsed().as_secs_f64())
}

/// An encoder that drops the frames: the export without the cost (and the CPU contention) of x264.
struct NullFactory;
struct NullEncoder(i64);

impl EncoderFactory for NullFactory {
    fn start(&self, _: &EncodeJob) -> Result<Box<dyn FrameEncoder>, MediaError> {
        Ok(Box::new(NullEncoder(0)))
    }
}

impl FrameEncoder for NullEncoder {
    fn write_frame(&mut self, rgba: &[u8]) -> Result<(), MediaError> {
        std::hint::black_box(rgba);
        self.0 += 1;
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<(), MediaError> {
        Ok(())
    }
    fn abort(self: Box<Self>) {}
    fn frames_written(&self) -> i64 {
        self.0
    }
}

fn export_null(media: &Arc<dyn MediaBackend>, p: &Project, job: EncodeJob) -> Result<ExportStats, String> {
    let req = ExportRequest::new(Arc::new(ProjectScenes::new(p, false)), job).with_missing(MissingPolicy::Fail);
    kadr_playback::export::export(req, Arc::new(FfmpegDecoders::new(media.clone())), &NullFactory, &|_| {}, &CancelToken::new()).map_err(|e| e.to_string())
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// [`RUNS`] exports; the stats of the run with the median fps, and every run's fps.
fn measure(media: &Arc<dyn MediaBackend>, p: &Project, out: &Path) -> Result<(ExportStats, Vec<f64>), String> {
    let mut runs = vec![];
    for _ in 0..RUNS {
        let s = export_new(media, p, encode_job(p, out.to_path_buf(), vec![]), &|_| {})?;
        runs.push(s);
    }
    let fps: Vec<f64> = runs.iter().map(|s| s.fps).collect();
    runs.sort_by(|a, b| a.fps.total_cmp(&b.fps));
    let _ = std::fs::remove_file(out);
    Ok((runs.swap_remove(RUNS / 2), fps))
}

fn push_stats(r: &mut Report, case: &str, s: &ExportStats, fps_runs: &[f64]) {
    r.push("export_new", case, "fps", s.fps, "fps");
    let (lo, hi) = fps_runs.iter().fold((f64::MAX, 0.0f64), |(l, h), f| (l.min(*f), h.max(*f)));
    r.push("export_new", case, "fps min of runs", lo, "fps");
    r.push("export_new", case, "fps max of runs", hi, "fps");
    r.push("export_new", case, "frames", s.frames as f64, "n");
    r.push("export_new", case, "elapsed", s.elapsed.as_secs_f64(), "s");
    r.push("export_new", case, "render p50", ms(s.render.p50), "ms");
    r.push("export_new", case, "render p90", ms(s.render.p90), "ms");
    r.push("export_new", case, "decode wait", s.decode_wait.as_secs_f64(), "s");
    r.push("export_new", case, "encoder wait", s.encoder_wait.as_secs_f64(), "s");
    r.push("export_new", case, "writer write_frame", s.write_total.as_secs_f64(), "s");
    r.push("export_new", case, "encoder finish", s.finish.as_secs_f64(), "s");
    r.push("export_new", case, "layers drawn", s.layers_drawn as f64, "n");
    r.push("export_new", case, "fast-path layers", s.fast_paths as f64, "n");
    r.push("export_new", case, "streams opened", s.resolver.streams_opened as f64, "n");
    r.push("export_new", case, "late allocations", s.late_allocations as f64, "n");
}

// ---------------------------------------------------------------- kadr-bench export

pub fn run(diag_only: bool) -> Result<Report, String> {
    let ff = FfmpegCli::locate().map_err(|e| e.to_string())?;
    let media: Arc<dyn MediaBackend> = Arc::new(FfmpegCli::locate().map_err(|e| e.to_string())?);
    let dir = media::bench_dir();
    let clips = media::ensure(&dir).map_err(|e| e.to_string())?;
    let find = |name: &str| -> Result<&TestClip, String> { clips.iter().find(|c| c.name == name).ok_or(format!("no {name} clip")) };
    let hd = probe(&ff, &find("h264_1080p30")?.path)?;
    let uhd = probe(&ff, &find("h264_2160p30")?.path)?;
    let logo = probe(&ff, &ensure_logo(&dir)?)?;
    let out = dir.join("export-new.mp4");
    let mut r = Report::new("m5-export");

    // Same-run legacy figure for the M0 scenario (M0 §6: 133.30 fps), repeated like the new one.
    let m0 = m0_project(&hd);
    let plan = legacy_plan(&m0, dir.join("export-legacy.mp4"), vec![]);
    let mut legacy: Vec<f64> = vec![];
    for _ in 0..if diag_only { 0 } else { RUNS } {
        legacy.push(600.0 / export_legacy(&ff, &plan)?);
    }
    let _ = std::fs::remove_file(&plan.output);
    legacy.sort_by(f64::total_cmp);
    let case = "1080p30 20 s, 10 cuts, 2 dissolves, crf 21 fast";
    if !diag_only {
        r.push("export_legacy", case, "fps", legacy[RUNS / 2], "fps");
        r.push("export_legacy", case, "fps min of runs", legacy[0], "fps");
        r.push("export_legacy", case, "fps max of runs", legacy[RUNS - 1], "fps");
    }

    let cases: [(&str, Project); 3] = [
        (case, m0),
        ("4K 10 s, 3 cuts, 3840x2160, crf 21 fast", uhd_project(&uhd)),
        ("1080p30 20 s, 3 layers (M0 timeline + 4K PiP 0.3 rot 5 + alpha logo)", three_layer_project(&hd, &uhd, &logo)),
    ];
    for (name, p) in cases.iter().filter(|_| !diag_only) {
        eprintln!("kadr-bench: exporting {name} …");
        let (stats, fps_runs) = measure(&media, p, &out)?;
        push_stats(&mut r, name, &stats, &fps_runs);
    }

    // Fast-path diagnosis: the same clip, as little as possible around it.
    let mut single = new_project("single", 1920, 1080, FrameRate::FPS_30);
    single.sequence_mut().tracks[0].clips.push(Clip::new(hd.id, "full", TimeRange::new(Time::ZERO, Time::from_secs(20)), Time::ZERO));
    single.assets.push(hd.clone());
    let mut with_logo = single.clone();
    let mut mark = Clip::new(logo.id, "logo", TimeRange::new(Time::ZERO, Time::from_secs(20)), Time::ZERO);
    mark.transform.scale = 0.25;
    mark.transform.x = 700.0;
    mark.transform.y = -380.0;
    with_logo.sequence_mut().tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    with_logo.sequence_mut().tracks[1].clips.push(mark);
    with_logo.assets.push(logo.clone());
    for (name, p) in [("diag: one full-frame 1080p clip, 20 s, no cuts", &single), ("diag: the same + small alpha logo", &with_logo)] {
        eprintln!("kadr-bench: exporting {name} …");
        let (stats, fps_runs) = measure(&media, p, &out)?;
        push_stats(&mut r, name, &stats, &fps_runs);
    }

    // The same two projects into an encoder that drops the frames: what the pipeline costs without x264.
    for (name, p) in [("diag, frames dropped: one full-frame 1080p clip, 20 s", &single), ("diag, frames dropped: M0 timeline", &cases[0].1)] {
        eprintln!("kadr-bench: exporting {name} …");
        let mut runs = vec![];
        for _ in 0..RUNS {
            runs.push(export_null(&media, p, encode_job(p, out.clone(), vec![]))?);
        }
        let fps: Vec<f64> = runs.iter().map(|s| s.fps).collect();
        runs.sort_by(|a, b| a.fps.total_cmp(&b.fps));
        push_stats(&mut r, name, &runs[RUNS / 2], &fps);
    }

    if !diag_only {
        psnr_rows(&mut r, &ff, &media, &hd, &dir)?;
    }
    Ok(r)
}

// ---------------------------------------------------------------- PSNR

/// `[psnr]` line of `ffmpeg -i a -i b -lavfi psnr`: (y, u, v, average) in dB (`inf` when identical).
pub fn parse_psnr(stderr: &str) -> Option<[f64; 4]> {
    let line = stderr.lines().rev().find(|l| l.contains("PSNR y:"))?;
    let field = |key: &str| -> Option<f64> {
        let rest = line.split(key).nth(1)?;
        let tok = rest.split_whitespace().next()?;
        if tok == "inf" { Some(f64::INFINITY) } else { tok.parse().ok() }
    };
    Some([field("y:")?, field("u:")?, field("v:")?, field("average:")?])
}

fn frame_count(path: &Path) -> Result<u64, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    String::from_utf8_lossy(&out.stdout).trim().parse().map_err(|_| format!("ffprobe frame count of {}", path.display()))
}

fn psnr_of(a: &Path, b: &Path) -> Result<[f64; 4], String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-i"])
        .arg(a)
        .arg("-i")
        .arg(b)
        .args(["-lavfi", "[0:v][1:v]psnr", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let text = String::from_utf8_lossy(&out.stderr);
    parse_psnr(&text).ok_or_else(|| format!("no PSNR in ffmpeg output: {}", text.lines().last().unwrap_or("")))
}

/// Single track, cuts only (legacy rounds transitions differently by design), a BT.709-tagged 1080p
/// source, no look: legacy and new export with the same settings, PSNR of the two.
fn psnr_rows(r: &mut Report, ff: &FfmpegCli, media: &Arc<dyn MediaBackend>, hd: &MediaAsset, dir: &Path) -> Result<(), String> {
    let mut p = new_project("psnr", 1920, 1080, FrameRate::FPS_30);
    let segs: Vec<_> = (0..5).map(|i| (Time::from_secs(i * 10 + 1), Time::from_secs(2))).collect();
    cuts_on_v1(p.sequence_mut(), hd.id, &segs);
    p.assets.push(hd.clone());
    let (legacy, new) = (dir.join("psnr-legacy.mp4"), dir.join("psnr-new.mp4"));
    export_legacy(ff, &legacy_plan(&p, legacy.clone(), vec![]))?;
    export_new(media, &p, encode_job(&p, new.clone(), vec![]), &|_| {})?;
    let (nl, nn) = (frame_count(&legacy)?, frame_count(&new)?);
    let case = "1080p30 10 s, 5 cuts, no transitions, no look, BT.709 tagged";
    r.push("psnr", case, "frames legacy", nl as f64, "n");
    r.push("psnr", case, "frames new", nn as f64, "n");
    if nl != nn {
        return Err(format!("frame counts differ: legacy {nl}, new {nn}\n{}", crate::report::markdown(r)));
    }
    let [y, u, v, avg] = psnr_of(&new, &legacy)?;
    r.push("psnr", case, "y", y, "dB");
    r.push("psnr", case, "u", u, "dB");
    r.push("psnr", case, "v", v, "dB");
    r.push("psnr", case, "average", avg, "dB");
    r.push("psnr", case, "exit criterion >= 40 dB", if avg >= 40.0 { 1.0 } else { 0.0 }, "pass");
    for f in [legacy, new] {
        let _ = std::fs::remove_file(f);
    }
    Ok(())
}

// ---------------------------------------------------------------- 20-minute A/V sync export

const AV_SECS: i64 = 1200;
const AV_FPS: FrameRate = FrameRate::FPS_29_97;
/// Cut `i` (1-based) is at `12.5 s + 23 s · (i − 1)`: always half way between two flashes.
const AV_FIRST_CUT_MS: i64 = 12_500;
const AV_SEGMENT_MS: i64 = 23_000;
/// A dissolve on every 4th cut: 8 frames centred on it.
const AV_DISSOLVE_EVERY: usize = 4;
const AV_DISSOLVE_FRAMES: i64 = 8;
/// Source left before an incoming clip and after an outgoing one, for the dissolve's overlap.
const AV_HANDLE_SECS: f64 = 0.2;

/// One clip of the sync timeline: `[timeline_in, timeline_out)` shows the source from `source_in`.
#[derive(Debug, Clone, PartialEq)]
pub struct AvSeg {
    pub timeline_in: Time,
    pub timeline_out: Time,
    pub source_in: Time,
    /// The source's offset from the timeline in whole frames (source second `s` shows `frames` frames
    /// away from where it would without the cut); a whole number of frames, so frames map 1:1.
    pub shift_frames: i64,
}

/// The cut list of the sync timeline of `total` seconds at `fps` and the cuts (1-based) with a
/// dissolve. Every clip's source offset from its timeline position is a whole number of frames
/// closest to a whole number of seconds `K` (chosen anywhere the source allows): the frames map
/// one to one and the clips' flashes stay near whole timeline seconds, far from the (mid-second)
/// cuts; the audio takes the same source range, so click and flash stay together by construction.
pub fn av_plan(total_secs: i64, fps: FrameRate) -> (Vec<AvSeg>, Vec<usize>) {
    let total = Time::from_secs(total_secs);
    let mut cuts = vec![];
    let mut ms = AV_FIRST_CUT_MS;
    while ms < total_secs * 1000 - 2000 {
        cuts.push(Time::from_millis(ms));
        ms += AV_SEGMENT_MS;
    }
    let fd = fps.frame_duration();
    let mut segs = vec![];
    for i in 0..=cuts.len() {
        let tin = if i == 0 { Time::ZERO } else { cuts[i - 1] };
        let tout = if i == cuts.len() { total } else { cuts[i] };
        let (a, b) = (tin.as_secs_f64(), tout.as_secs_f64());
        let lo = if i == 0 { 0.0 } else { (AV_HANDLE_SECS - a).ceil() };
        let hi = (total_secs as f64 - b - if i == cuts.len() { 0.0 } else { AV_HANDLE_SECS }).floor();
        let target = ((i as i64 * 173 + 41) % 1201) - 600;
        let k = (target as f64).clamp(lo, hi);
        let frames = (k * fps.num as f64 / fps.den as f64).round() as i64;
        segs.push(AvSeg { timeline_in: tin, timeline_out: tout, source_in: tin + Time::from_flicks(frames * fd.flicks()), shift_frames: frames });
    }
    let dissolves = (1..=cuts.len()).filter(|c| c % AV_DISSOLVE_EVERY == 0).collect();
    (segs, dissolves)
}

fn av_project(source: &MediaAsset, total_secs: i64) -> Project {
    let (segs, dissolves) = av_plan(total_secs, AV_FPS);
    let mut p = new_project("avsync-export", 1280, 720, AV_FPS);
    let seq = p.sequence_mut();
    let a1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).expect("A1");
    for (i, s) in segs.iter().enumerate() {
        let range = TimeRange::new(s.source_in, s.source_in + (s.timeline_out - s.timeline_in));
        let link = LinkId::new();
        let mut v = Clip::new(source.id, format!("v{i}"), range, s.timeline_in);
        let mut a = Clip::new(source.id, format!("a{i}"), range, s.timeline_in);
        v.link = Some(link);
        a.link = Some(link);
        seq.tracks[0].clips.push(v);
        seq.tracks[a1].clips.push(a);
    }
    let v1 = seq.tracks[0].id;
    let len = AV_FPS.frame_to_time(AV_DISSOLVE_FRAMES);
    for c in dissolves {
        seq.transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, track: v1, at: segs[c].timeline_in, duration: len });
    }
    p.assets.push(source.clone());
    p
}

/// `kadr-bench avsync-export`: 20 minutes of a flash-and-click source, cut up and dissolved, exported
/// on the new pipeline and analyzed.
pub fn run_avsync() -> Result<Report, String> {
    let ff = FfmpegCli::locate().map_err(|e| e.to_string())?;
    let media: Arc<dyn MediaBackend> = Arc::new(FfmpegCli::locate().map_err(|e| e.to_string())?);
    let dir = media::bench_dir();
    let src = dir.join("avsync_1200s_2997_720p.mp4");
    if !src.exists() {
        eprintln!("kadr-bench: generating {} (20 min) …", src.display());
        avsync::generate_source(&src, AV_SECS as u32, 1280, 720, AV_FPS).map_err(|e| e.to_string())?;
    }
    let p = av_project(&probe(&ff, &src)?, AV_SECS);
    let (cuts, transitions) = (p.sequence().tracks[0].clips.len() - 1, p.sequence().transitions.len());
    eprintln!("kadr-bench: exporting 20 min, {cuts} cuts, {transitions} dissolves …");
    let out = dir.join("avsync-export.mp4");
    let job = encode_job(&p, out.clone(), export_audio(&p));
    let last = std::sync::atomic::AtomicU32::new(0);
    let stats = export_new(&media, &p, job, &|f| {
        let pct = (f * 10.0) as u32;
        if last.swap(pct, std::sync::atomic::Ordering::Relaxed) != pct {
            eprintln!("kadr-bench: export {}%", pct * 10);
        }
    })?;
    eprintln!("kadr-bench: analyzing …");
    let m = avsync::analyze(&out, AV_FPS)?;
    if !m.within_one_frame() {
        eprintln!("kadr-bench: flashes without a click at {:?} s; clicks without a flash at {:?} s", m.unpaired_flashes(), m.unpaired_clicks());
    }
    let case = "20 min 1280x720 29.97, V1+A1 linked";
    let mut r = m.report("avsync-export", case);
    r.push("avsync_export", case, "cuts", cuts as f64, "n");
    r.push("avsync_export", case, "dissolves", transitions as f64, "n");
    r.push("avsync_export", case, "export fps", stats.fps, "fps");
    r.push("avsync_export", case, "export elapsed", stats.elapsed.as_secs_f64(), "s");
    r.push("avsync_export", case, "render p50", ms(stats.render.p50), "ms");
    r.push("avsync_export", case, "decode wait", stats.decode_wait.as_secs_f64(), "s");
    r.push("avsync_export", case, "encoder wait", stats.encoder_wait.as_secs_f64(), "s");
    r.push("avsync_export", case, "within +-1 frame (all flashes paired)", if m.within_one_frame() { 1.0 } else { 0.0 }, "pass");
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psnr_line_is_parsed() {
        let text = "[Parsed_psnr_0 @ 000] PSNR y:44.58 u:47.05 v:inf average:45.12 min:40.00 max:60.10\nframe=  300 fps=0.0";
        let [y, u, v, avg] = parse_psnr(text).unwrap();
        assert_eq!((y, u, avg), (44.58, 47.05, 45.12));
        assert!(v.is_infinite());
        assert!(parse_psnr("nothing").is_none());
    }

    #[test]
    fn sync_plan_has_the_cuts_dissolves_and_handles_the_exit_criterion_asks_for() {
        let (segs, dissolves) = av_plan(AV_SECS, AV_FPS);
        assert!(segs.len() > 50, "{} cuts", segs.len() - 1);
        assert!(dissolves.len() >= 10, "{} dissolves", dissolves.len());
        assert_eq!(segs[0].timeline_in, Time::ZERO);
        assert_eq!(segs.last().unwrap().timeline_out, Time::from_secs(AV_SECS));
        let fd = AV_FPS.frame_duration();
        let mut jumps = 0;
        for (i, s) in segs.iter().enumerate() {
            assert_eq!(s.source_in - s.timeline_in, Time::from_flicks(s.shift_frames * fd.flicks()), "a whole number of frames");
            let dur = s.timeline_out - s.timeline_in;
            let (a, b) = (s.source_in.as_secs_f64(), (s.source_in + dur).as_secs_f64());
            let (head, tail) = (if i == 0 { 0.0 } else { AV_HANDLE_SECS }, if i == segs.len() - 1 { 0.0 } else { AV_HANDLE_SECS });
            assert!(a >= head - 0.02 && b <= AV_SECS as f64 - tail + 0.02, "segment {i} source {a}..{b}");
            if i > 0 {
                assert_eq!(s.timeline_in, segs[i - 1].timeline_out);
                // Cuts sit half way between whole seconds; flashes are at whole seconds, up to a frame later,
                // shifted by ~whole seconds: a dissolve (±4 frames) never covers one.
                let frac = s.timeline_in.as_secs_f64().fract();
                assert!((frac - 0.5).abs() < 1e-9, "cut at {}", s.timeline_in.as_secs_f64());
                jumps += (s.shift_frames != segs[i - 1].shift_frames) as usize;
            }
            // Shift ≈ K seconds: the clip's flashes are within a frame of whole timeline seconds.
            let shift_secs = s.shift_frames as f64 * fd.as_secs_f64();
            assert!((shift_secs - shift_secs.round()).abs() <= fd.as_secs_f64(), "shift {shift_secs}");
        }
        assert!(jumps >= 40, "only {jumps} cuts jump in the source");
        assert!(dissolves.iter().all(|c| *c >= 1 && *c < segs.len()));
        // Cuts are 23 s apart, not on a whole second.
        let dur = AV_FPS.frame_to_time(AV_DISSOLVE_FRAMES).as_secs_f64();
        assert!(dur > 0.26 && dur < 0.27);
    }

    #[test]
    fn project_has_linked_video_and_audio_clips_and_centred_dissolves() {
        let source = MediaAsset::new(
            "src.mp4",
            kadr_core::MediaInfo {
                kind: kadr_core::MediaKind::Video,
                duration: Time::from_secs(AV_SECS),
                container: "mp4".into(),
                size_bytes: 0,
                video: None,
                audio: None,
                timecode: None,
            },
        );
        let p = av_project(&source, AV_SECS);
        let seq = p.sequence();
        assert_eq!(seq.duration(), Time::from_secs(AV_SECS));
        let (v, a) = (&seq.tracks[0].clips, &seq.tracks[seq.tracks.len() - 1].clips);
        assert_eq!((v.len(), a.len()), (a.len(), v.len()));
        assert!(v.iter().zip(a).all(|(v, a)| v.link.is_some() && v.link == a.link && v.source_range() == a.source_range() && v.timeline_range() == a.timeline_range()));
        assert!(seq.transitions.len() >= 10);
        assert!(seq.transitions.iter().all(|t| v.iter().any(|c| c.timeline_in == t.at) && t.duration == AV_FPS.frame_to_time(8)));
    }

    #[test]
    fn m0_project_matches_the_m0_scenario() {
        let clip = MediaAsset::new(
            "clip.mp4",
            kadr_core::MediaInfo { kind: kadr_core::MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: None, audio: None, timecode: None },
        );
        let p = m0_project(&clip);
        let seq = p.sequence();
        assert_eq!(seq.duration(), Time::from_secs(20));
        assert_eq!(seq.tracks[0].clips.len(), 10);
        assert_eq!(seq.tracks[0].clips[4].source_in, Time::from_secs(20));
        assert_eq!(seq.transitions.iter().map(|t| t.at).collect::<Vec<_>>(), vec![Time::from_secs(6), Time::from_secs(14)]);
        let job = encode_job(&p, "o.mp4".into(), vec![]);
        assert_eq!((job.width, job.height, job.frames), (1920, 1080, 600));
        assert_eq!((job.settings.crf, job.settings.preset.as_str()), (21, "fast"));
    }
}
