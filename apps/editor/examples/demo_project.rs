//! Builds a demo project from media files: linked A/V clips laid end to
//! end, a few markers and a split, saved as a `.kadr` file.
//! Usage: cargo run -p kadr-editor --example demo_project -- out.kadr a.mp4 b.mp4

use kadr_core::{LinkId, Time, TimeRange};
use kadr_media::MediaBackend;
use kadr_project::{io, Clip, MediaAsset, Marker, Project, Track, TrackKind};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("output .kadr"));
    let files: Vec<PathBuf> = args.map(|a| std::fs::canonicalize(a).expect("media path")).collect();
    let ff = kadr_media::ffmpeg::FfmpegCli::locate().expect("ffmpeg");
    let mut p = Project::new("Demo");
    p.sequence_mut().name = "Concert cut".into();
    let mut at = Time::ZERO;
    for (i, f) in files.iter().enumerate() {
        let info = ff.probe(f).expect("probe");
        let asset = MediaAsset::new(f, info);
        if i == 0 {
            if let Some(v) = &asset.info.video {
                let seq = p.sequence_mut();
                seq.width = v.width;
                seq.height = v.height;
                seq.frame_rate = v.frame_rate.unwrap_or_default();
            }
        }
        let dur = asset.duration();
        let link = LinkId::new();
        let mut v = Clip::new(asset.id, &asset.name, TimeRange::new(Time::ZERO, dur), at);
        v.link = Some(link);
        let mut a = v.clone();
        a.id = kadr_core::ClipId::new();
        let seq = p.sequence_mut();
        seq.tracks[0].clips.push(v);
        seq.tracks[1].clips.push(a);
        at += dur;
        p.assets.push(asset);
    }
    let seq = p.sequence_mut();
    seq.tracks.push(Track::new(TrackKind::Audio, "A2"));
    seq.markers.push(Marker::new(Time::from_secs(4), "Intro"));
    let mut m = Marker::new(Time::from_secs(13), "Song 2");
    m.color = "#4fd1c5".into();
    seq.markers.push(m);
    io::save(&mut p, &out).expect("save");
    println!("wrote {}", out.display());
}
