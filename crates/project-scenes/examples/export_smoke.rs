//! Export smoke test on real media: `export_smoke <video> <logo.png> <out.mp4>`.
//! ~10 s of the video on V1 (with its audio) and the logo, scaled small, on V2.
use kadr_core::{Time, TimeRange};
use kadr_media::{EncodeJob, ExportAudio, ExportSettings, MediaBackend};
use kadr_playback::{ExportRequest, FfmpegDecoders, MissingPolicy};
use kadr_project::{Clip, MediaAsset, Project, Track, TrackKind};
use kadr_project_scenes::ProjectScenes;
use std::sync::Arc;

fn main() {
    tracing_subscriber_init();
    let a: Vec<String> = std::env::args().collect();
    let media: Arc<dyn MediaBackend> = Arc::new(kadr_media::ffmpeg::FfmpegCli::locate().expect("ffmpeg"));
    let video = MediaAsset::new(&a[1], media.probe(a[1].as_ref()).expect("probe video"));
    let logo = MediaAsset::new(&a[2], media.probe(a[2].as_ref()).expect("probe logo"));
    let mut p = Project::new("smoke");
    let ten = TimeRange::new(Time::ZERO, Time::from_secs(10));
    let seq = p.sequence_mut();
    seq.tracks[0].clips.push(Clip::new(video.id, "v", ten, Time::ZERO));
    let at = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
    seq.tracks[at].clips.push(Clip::new(video.id, "a", ten, Time::ZERO));
    let mut l = Clip::new(logo.id, "logo", ten, Time::ZERO);
    l.transform.scale = 0.25;
    l.transform.x = 600.0;
    l.transform.y = -350.0;
    seq.tracks.insert(1, Track::new(TrackKind::Video, "V2"));
    seq.tracks[1].clips.push(l);
    let (fps, total) = (seq.frame_rate, seq.duration());
    let (w, h) = (seq.width, seq.height);
    let audio = vec![ExportAudio { path: video.path.clone(), source_start: Time::ZERO, timeline_start: Time::ZERO, duration: total, speed: 1.0, gain_db: 0.0, pan: 0.0, fade_in: Time::ZERO, fade_out: Time::ZERO }];
    p.assets.extend([video, logo]);
    let job = EncodeJob { output: a[3].clone().into(), width: w & !1, height: h & !1, rate: fps, frames: fps.time_to_frame_round(total), total, audio, settings: ExportSettings { crf: 21, preset: "fast".into(), ..Default::default() } };
    let req = ExportRequest::new(Arc::new(ProjectScenes::new(&p, false)), job).with_missing(MissingPolicy::Fail);
    let s = kadr_playback::export::export(req, Arc::new(FfmpegDecoders::new(media.clone())), &*media, &|_| {}, &Default::default()).expect("export");
    println!("frames {} fps {:.1} elapsed {:?} render p50 {:?} decode wait {:?}", s.frames, s.fps, s.elapsed, s.render.p50, s.decode_wait);
}

fn tracing_subscriber_init() {}
