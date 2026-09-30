//! Export building blocks shared by the frame encoder: the audio filter
//! graph, the settings and the container choice. Video is composited by
//! the renderer and arrives as frames; FFmpeg only encodes it.

use crate::{MediaError, Result};
use kadr_core::Time;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ExportAudio {
    pub path: PathBuf,
    pub source_start: Time,
    pub timeline_start: Time,
    pub duration: Time,
    pub speed: f64,
    pub gain_db: f64,
    pub pan: f64,
    pub fade_in: Time,
    pub fade_out: Time,
}

/// Encoder quality and audio settings. Frame size and rate belong to the
/// [`EncodeJob`](crate::EncodeJob).
#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub sample_rate: u32,
    pub video_codec: String,
    pub crf: u8,
    pub preset: String,
    pub audio_bitrate_k: u32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            sample_rate: 48_000,
            video_codec: "libx264".into(),
            crf: 18,
            preset: "medium".into(),
            audio_bitrate_k: 192,
        }
    }
}

fn atempo_chain(speed: f64) -> String {
    // atempo accepts 0.5..=100 per instance; chain for slower speeds.
    let mut s = speed.clamp(0.05, 20.0);
    let mut parts = vec![];
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s /= 0.5;
    }
    parts.push(format!("atempo={s:.6}"));
    parts.join(",")
}

/// The audio half of the export graph: one chain per clip (`-ss`/`-t`/`-i`
/// argument groups returned as inputs, numbered from `first_input`), mixed
/// into `[aout]` and padded/trimmed to `total`. The last line has no
/// trailing `;`. Used by the frame encoder.
pub(crate) fn audio_graph(audio: &[ExportAudio], first_input: usize, total: Time, sample_rate: u32) -> (Vec<String>, String) {
    let mut inputs: Vec<String> = vec![];
    let mut graph = String::new();
    let sr = sample_rate;
    let total_s = total.as_secs_f64();
    let mut alabels = vec![];
    for (i, a) in audio.iter().enumerate() {
        let n_in = first_input + i;
        let speed = if a.speed > 0.0 { a.speed } else { 1.0 };
        let src_dur = Time::from_secs_f64(a.duration.as_secs_f64() * speed) + Time::from_millis(500);
        let start = a.source_start.max(Time::ZERO);
        // No input seek at zero: FFmpeg's seek on an AAC/MP4 source drops the
        // priming samples of the first granule, silencing audio that starts at
        // sample 0 (the missing click at t = 0 of the M5 A/V sync report).
        // Reading from the beginning needs no seek.
        if start > Time::ZERO {
            inputs.extend(["-ss".into(), start.to_ffmpeg_arg()]);
        }
        inputs.extend(["-t".into(), src_dur.to_ffmpeg_arg(), "-i".into()]);
        inputs.push(a.path.to_string_lossy().into_owned());
        let d = a.duration.as_secs_f64();
        let mut chain = format!("[{n_in}:a:0]asetpts=PTS-STARTPTS");
        if (speed - 1.0).abs() > 1e-6 {
            let _ = write!(chain, ",{}", atempo_chain(speed));
        }
        let _ = write!(chain, ",aresample={sr},aformat=sample_fmts=fltp:channel_layouts=stereo,apad=whole_dur={d:.6},atrim=duration={d:.6}");
        if a.gain_db != 0.0 {
            let _ = write!(chain, ",volume={:.3}dB", a.gain_db);
        }
        if a.pan != 0.0 {
            let p = a.pan.clamp(-1.0, 1.0);
            let _ = write!(chain, ",pan=stereo|c0={:.4}*c0|c1={:.4}*c1", (1.0 - p).min(1.0), (1.0 + p).min(1.0));
        }
        if a.fade_in > Time::ZERO {
            let _ = write!(chain, ",afade=t=in:st=0:d={:.4}", a.fade_in.as_secs_f64());
        }
        if a.fade_out > Time::ZERO {
            let fo = a.fade_out.as_secs_f64().min(d);
            let _ = write!(chain, ",afade=t=out:st={:.4}:d={fo:.4}", d - fo);
        }
        let delay = a.timeline_start.to_samples(sr).max(0);
        let _ = writeln!(graph, "{chain},adelay=delays={delay}S:all=1[a{i}];");
        alabels.push(format!("[a{i}]"));
    }
    if alabels.is_empty() {
        let _ = writeln!(graph, "anullsrc=r={sr}:cl=stereo,atrim=duration={total_s:.6}[aout]");
    } else {
        let _ = writeln!(
            graph,
            "{}amix=inputs={}:normalize=0:dropout_transition=0,apad=whole_dur={total_s:.6},atrim=duration={total_s:.6}[aout]",
            alabels.concat(),
            alabels.len()
        );
    }
    (inputs, graph)
}

pub(crate) fn container_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mov") => "mov",
        Some("mkv") => "matroska",
        _ => "mp4",
    }
}

/// Windows limits a command line to 32 767 chars; the inputs are what grows.
pub(crate) fn check_command_len(inputs: &[String]) -> Result<()> {
    let cmd_len: usize = inputs.iter().map(|s| s.len() + 3).sum();
    if cmd_len > 30_000 {
        return Err(MediaError::Unsupported(format!(
            "timeline has too many segments for a single export pass ({} inputs)",
            inputs.len() / 6
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(path: &str, source_start: Time, timeline_start: Time, duration: Time, speed: f64) -> ExportAudio {
        ExportAudio { path: path.into(), source_start, timeline_start, duration, speed, gain_db: 0.0, pan: 0.0, fade_in: Time::ZERO, fade_out: Time::ZERO }
    }

    /// Frozen output of `audio_graph` (shaped, sped-up and plain clip, then
    /// none): the encoder's audio must not change by accident.
    #[test]
    fn audio_graph_output_is_pinned() {
        let a = ExportAudio { gain_db: -6.0, pan: 0.5, fade_in: Time::from_millis(200), fade_out: Time::from_millis(300), ..clip("a.wav", Time::from_secs(2), Time::from_millis(500), Time::from_secs(3), 0.25) };
        let b = clip("b.wav", Time::ZERO, Time::from_secs(3), Time::from_secs(2), 1.0);
        let (inputs, graph) = audio_graph(&[a, b], 2, Time::from_secs(6), 48_000);
        assert_eq!(inputs, vec!["-ss", "2.000000", "-t", "1.250000", "-i", "a.wav", "-t", "2.500000", "-i", "b.wav"]);
        assert_eq!(graph, r#"[2:a:0]asetpts=PTS-STARTPTS,atempo=0.5,atempo=0.500000,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,apad=whole_dur=3.000000,atrim=duration=3.000000,volume=-6.000dB,pan=stereo|c0=0.5000*c0|c1=1.0000*c1,afade=t=in:st=0:d=0.2000,afade=t=out:st=2.7000:d=0.3000,adelay=delays=24000S:all=1[a0];
[3:a:0]asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,apad=whole_dur=2.000000,atrim=duration=2.000000,adelay=delays=144000S:all=1[a1];
[a0][a1]amix=inputs=2:normalize=0:dropout_transition=0,apad=whole_dur=6.000000,atrim=duration=6.000000[aout]
"#);
        let (inputs, graph) = audio_graph(&[], 0, Time::from_secs(6), 48_000);
        assert!(inputs.is_empty());
        assert_eq!(graph, "anullsrc=r=48000:cl=stereo,atrim=duration=6.000000[aout]
");
    }

    /// An audio input whose `source_start` is zero (or negative) reads from
    /// the beginning: no input seek, so the source's AAC priming samples are
    /// handled by its edit list instead of being dropped. Any nonzero start
    /// still seeks.
    #[test]
    fn audio_input_at_source_zero_has_no_seek_and_a_nonzero_one_does() {
        let clip = |source_start: Time| ExportAudio {
            path: "a.wav".into(),
            source_start,
            timeline_start: Time::ZERO,
            duration: Time::from_secs(1),
            speed: 1.0,
            gain_db: 0.0,
            pan: 0.0,
            fade_in: Time::ZERO,
            fade_out: Time::ZERO,
        };
        for start in [Time::ZERO, Time::from_millis(-500)] {
            let (inputs, graph) = audio_graph(&[clip(start)], 0, Time::from_secs(2), 48_000);
            assert_eq!(inputs, vec!["-t", "1.500000", "-i", "a.wav"], "source_start {start:?}");
            assert!(graph.starts_with("[0:a:0]asetpts"), "{graph}");
        }
        let (inputs, _) = audio_graph(&[clip(Time::from_millis(250))], 0, Time::from_secs(2), 48_000);
        assert_eq!(inputs, vec!["-ss", "0.250000", "-t", "1.500000", "-i", "a.wav"]);
    }
}
