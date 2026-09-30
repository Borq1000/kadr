//! Final export. The editor turns the timeline composition into an
//! [`ExportPlan`] (plain data); the backend renders it. Plans know nothing
//! about project types, so the media crate stays independent of the model.

use crate::ffmpeg::progress::run_with_progress;
use crate::ffmpeg::FfmpegCli;
use crate::{MediaError, Progress, Result};
use kadr_core::{CancelToken, FrameRate, Time};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Per-clip picture adjustments shared by preview and export filters.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoLook {
    pub scale: f64,
    /// Offset from centre, in sequence pixels.
    pub x: f64,
    pub y: f64,
    pub rotation_deg: f64,
    pub opacity: f64,
    pub crop: [f64; 4], // left, right, top, bottom (fractions)
    pub exposure: f64,
    pub contrast: f64,
    pub saturation: f64,
}

impl Default for VideoLook {
    fn default() -> Self {
        VideoLook { scale: 1.0, x: 0.0, y: 0.0, rotation_deg: 0.0, opacity: 1.0, crop: [0.0; 4], exposure: 0.0, contrast: 1.0, saturation: 1.0 }
    }
}

impl VideoLook {
    fn is_geometry_identity(&self) -> bool {
        self.scale == 1.0 && self.x == 0.0 && self.y == 0.0 && self.rotation_deg == 0.0 && self.opacity >= 1.0 && self.crop == [0.0; 4]
    }
    fn is_color_identity(&self) -> bool {
        self.exposure == 0.0 && self.contrast == 1.0 && self.saturation == 1.0
    }
}

#[derive(Clone, Debug)]
pub struct ExportVideoSource {
    pub path: PathBuf,
    pub source_start: Time,
    pub speed: f64,
    pub look: VideoLook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportTransitionKind {
    Dissolve,
    DipToBlack,
    Wipe,
}

impl ExportTransitionKind {
    fn xfade_name(self) -> &'static str {
        match self {
            ExportTransitionKind::Dissolve => "fade",
            ExportTransitionKind::DipToBlack => "fadeblack",
            ExportTransitionKind::Wipe => "wipeleft",
        }
    }
}

/// A transition centred on the cut *into* a segment: half of it plays over
/// the end of the previous segment, half over the start of this one.
#[derive(Clone, Debug)]
pub struct ExportTransition {
    pub kind: ExportTransitionKind,
    pub duration: Time,
}

/// One flat video segment; segments are laid end to end.
#[derive(Clone, Debug)]
pub struct ExportVideo {
    pub duration: Time,
    /// `None` renders black.
    pub source: Option<ExportVideoSource>,
    /// Transition from the previous segment (ignored on the first one).
    pub transition_in: Option<ExportTransition>,
}

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

#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    pub sample_rate: u32,
    pub video_codec: String,
    pub crf: u8,
    pub preset: String,
    pub audio_bitrate_k: u32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            width: 1920,
            height: 1080,
            rate: FrameRate::FPS_30,
            sample_rate: 48_000,
            video_codec: "libx264".into(),
            crf: 18,
            preset: "medium".into(),
            audio_bitrate_k: 192,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportPlan {
    pub output: PathBuf,
    pub total: Time,
    pub video: Vec<ExportVideo>,
    pub audio: Vec<ExportAudio>,
    pub settings: ExportSettings,
}

/// Filter chain turning a decoded source into a `w×h` frame, applying the
/// clip look. `px_scale` converts sequence pixels to output pixels (1.0 for
/// export, <1 for a reduced-size preview). Used by preview *and* export.
pub fn look_filter(look: &VideoLook, w: u32, h: u32, px_scale: f64) -> String {
    let mut f = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,setsar=1");
    if !look.is_color_identity() {
        let _ = write!(
            f,
            ",eq=brightness={:.4}:contrast={:.4}:saturation={:.4}",
            (look.exposure * 0.25).clamp(-1.0, 1.0),
            look.contrast.clamp(0.0, 4.0),
            look.saturation.clamp(0.0, 3.0)
        );
    }
    if look.is_geometry_identity() {
        let _ = write!(f, ",pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black");
        return f;
    }
    let [l, r, t, b] = look.crop.map(|v| v.clamp(0.0, 0.95));
    if l + r > 0.0 || t + b > 0.0 {
        let _ = write!(f, ",crop=iw*{:.4}:ih*{:.4}:iw*{l:.4}:ih*{t:.4}", (1.0 - l - r).max(0.05), (1.0 - t - b).max(0.05));
    }
    let s = look.scale.clamp(0.01, 16.0);
    if s != 1.0 {
        let _ = write!(f, ",scale=trunc(iw*{s:.4}/2)*2:trunc(ih*{s:.4}/2)*2");
    }
    let _ = write!(f, ",format=rgba");
    if look.rotation_deg != 0.0 {
        let a = look.rotation_deg.to_radians();
        let _ = write!(f, ",rotate={a:.5}:ow=rotw({a:.5}):oh=roth({a:.5}):c=none");
    }
    if look.opacity < 1.0 {
        let _ = write!(f, ",colorchannelmixer=aa={:.4}", look.opacity.clamp(0.0, 1.0));
    }
    // Place on a transparent canvas big enough for any offset, cut the
    // w×h window, then premultiply: RGB×alpha is exactly "over black".
    let (dx, dy) = ((look.x * px_scale).round(), (look.y * px_scale).round());
    let (mx, my) = (dx.abs() as u32 * 2 + w, dy.abs() as u32 * 2 + h);
    let _ = write!(
        f,
        ",pad=w=iw+{mx}:h=ih+{my}:x=(ow-iw)/2+{dx}:y=(oh-ih)/2+{dy}:color=black@0,\
         crop={w}:{h}:(iw-{w})/2:(ih-{h})/2,premultiply=inplace=1"
    );
    f
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
/// trailing `;`. Shared by the legacy export and the frame encoder.
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

/// Builds the FFmpeg input arguments and filter graph script.
pub fn build_graph(plan: &ExportPlan) -> (Vec<String>, String) {
    let st = &plan.settings;
    let (w, h) = (st.width & !1, st.height & !1);
    let rate = st.rate.to_ffmpeg_arg();
    let mut inputs: Vec<String> = vec![];
    let mut graph = String::new();
    let mut n_in = 0usize;

    // Everything is counted in output frames so cuts stay frame-exact.
    let n = plan.video.len();
    let seg_frames: Vec<i64> = plan.video.iter().map(|s| st.rate.time_to_frame_round(s.duration).max(1)).collect();
    // Frames of the transition into segment i, clamped to both neighbours so
    // the transitions on either side of a short segment never overlap.
    let xf: Vec<i64> = (0..n)
        .map(|i| match &plan.video[i].transition_in {
            Some(t) if i > 0 => st.rate.time_to_frame_round(t.duration).min(seg_frames[i - 1]).min(seg_frames[i]),
            _ => 0,
        })
        .map(|f| if f >= 2 { f } else { 0 })
        .collect();
    // Each segment is rendered with handles: the second half of the incoming
    // transition before its start, the first half of the outgoing one after.
    let head = |i: usize| xf[i] / 2;
    let tail = |i: usize| if i + 1 < n { xf[i + 1] - xf[i + 1] / 2 } else { 0 };
    let fps = st.rate.as_f64();
    let secs = |frames: i64| frames as f64 / fps;

    let mut seg_len = vec![];
    for (i, seg) in plan.video.iter().enumerate() {
        let frames = seg_frames[i] + head(i) + tail(i);
        seg_len.push(frames);
        match &seg.source {
            Some(src) => {
                let speed = if src.speed > 0.0 { src.speed } else { 1.0 };
                // Media before the clip's in-point may not exist: take what is
                // there and freeze the first frame for the rest of the handle.
                let start = src.source_start.max(Time::ZERO);
                let have = head(i).min((start.as_secs_f64() / speed * fps).floor() as i64);
                let seek = start - Time::from_secs_f64(secs(have) * speed);
                let src_dur = Time::from_secs_f64(secs(frames) * speed) + Time::from_secs(1);
                inputs.extend(["-ss".into(), seek.max(Time::ZERO).to_ffmpeg_arg(), "-t".into(), src_dur.to_ffmpeg_arg(), "-i".into()]);
                inputs.push(src.path.to_string_lossy().into_owned());
                let freeze = match head(i) - have {
                    0 => String::new(),
                    f => format!("start_mode=clone:start={f}:"),
                };
                let _ = writeln!(
                    graph,
                    "[{n_in}:v:0]setpts=(PTS-STARTPTS)/{speed:.6},fps={rate},{look},format=yuv420p,                     tpad={freeze}stop_mode=clone:stop_duration=2,trim=end_frame={frames},setpts=PTS-STARTPTS[v{i}];",
                    look = look_filter(&src.look, w, h, 1.0)
                );
                n_in += 1;
            }
            None => {
                let _ = writeln!(graph, "color=c=black:s={w}x{h}:r={rate},format=yuv420p,trim=end_frame={frames},setpts=PTS-STARTPTS[v{i}];");
            }
        }
    }
    if n == 0 {
        let frames = st.rate.time_to_frame_round(plan.total).max(1);
        let _ = writeln!(graph, "color=c=black:s={w}x{h}:r={rate},format=yuv420p,trim=end_frame={frames}[vout];");
    } else {
        // Plain cuts are concatenated in runs; each transition cross-fades the
        // stream built so far with the next segment.
        let mut run = vec!["[v0]".to_string()];
        let mut acc = seg_len[0];
        let flush = |run: &mut Vec<String>, graph: &mut String, out: &str| {
            if run.len() == 1 {
                let _ = writeln!(graph, "{}null{out};", run[0]);
            } else {
                // concat always outputs a 1/1000000 time base and xfade requires both inputs on the same one, so put it back on 1/fps.
                let _ = writeln!(graph, "{}concat=n={}:v=1:a=0,fps={rate}{out};", run.concat(), run.len());
            }
            run.clear();
            run.push(out.to_string());
        };
        for i in 1..n {
            if xf[i] > 0 {
                flush(&mut run, &mut graph, &format!("[c{i}]"));
                let kind = plan.video[i].transition_in.as_ref().map_or(ExportTransitionKind::Dissolve, |t| t.kind);
                let _ = writeln!(
                    graph,
                    "[c{i}][v{i}]xfade=transition={}:duration={:.6}:offset={:.6}[x{i}];",
                    kind.xfade_name(),
                    secs(xf[i]),
                    secs(acc - xf[i])
                );
                run = vec![format!("[x{i}]")];
                acc += seg_len[i] - xf[i];
            } else {
                run.push(format!("[v{i}]"));
                acc += seg_len[i];
            }
        }
        flush(&mut run, &mut graph, "[vout]");
    }

    let (audio_inputs, audio) = audio_graph(&plan.audio, n_in, plan.total, st.sample_rate);
    inputs.extend(audio_inputs);
    graph.push_str(&audio);
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

pub(crate) fn run(ff: &FfmpegCli, plan: &ExportPlan, progress: Progress, cancel: &CancelToken) -> Result<()> {
    if plan.total <= Time::ZERO {
        return Err(MediaError::Unsupported("sequence is empty".into()));
    }
    let (inputs, graph) = build_graph(plan);
    check_command_len(&inputs)?;
    // Unique per export: several exports may run concurrently.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!("kadr-export-{}-{n}.txt", std::process::id()));
    std::fs::write(&script, &graph)?;
    tracing::debug!(%graph, "export filter graph");

    let part = {
        let mut s = plan.output.as_os_str().to_owned();
        s.push(".part");
        PathBuf::from(s)
    };
    let st = &plan.settings;
    let mut cmd = ff.ffmpeg_cmd();
    cmd.arg("-y").args(&inputs).arg("-/filter_complex").arg(&script);
    cmd.args(["-map", "[vout]", "-map", "[aout]", "-c:v", &st.video_codec]);
    if st.video_codec.starts_with("libx26") {
        cmd.args(["-preset", &st.preset, "-crf", &st.crf.to_string()]);
    }
    cmd.args(["-pix_fmt", "yuv420p", "-r", &st.rate.to_ffmpeg_arg(), "-c:a", "aac", "-b:a", &format!("{}k", st.audio_bitrate_k)]);
    cmd.args(["-movflags", "+faststart", "-f", container_for(&plan.output)]).arg(&part);

    let r = run_with_progress(ff, cmd, plan.total, progress, cancel);
    let _ = std::fs::remove_file(&script);
    match r {
        Ok(()) => {
            std::fs::rename(&part, &plan.output)?;
            tracing::info!(output = %plan.output.display(), "export finished");
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan(with_audio: bool) -> ExportPlan {
        let look = VideoLook { scale: 0.5, x: 40.0, rotation_deg: 15.0, opacity: 0.8, ..VideoLook::default() };
        let seg = |secs: i64, path: &str, look: VideoLook, t: Option<ExportTransition>| ExportVideo {
            duration: Time::from_secs(secs),
            source: Some(ExportVideoSource { path: path.into(), source_start: Time::from_secs(1), speed: 1.0, look }),
            transition_in: t,
        };
        let audio = if with_audio {
            vec![
                ExportAudio {
                    path: "a.wav".into(),
                    source_start: Time::from_secs(2),
                    timeline_start: Time::from_millis(500),
                    duration: Time::from_secs(3),
                    speed: 0.25,
                    gain_db: -6.0,
                    pan: 0.5,
                    fade_in: Time::from_millis(200),
                    fade_out: Time::from_millis(300),
                },
                ExportAudio {
                    path: "b.wav".into(),
                    source_start: Time::ZERO,
                    timeline_start: Time::from_secs(3),
                    duration: Time::from_secs(2),
                    speed: 1.0,
                    gain_db: 0.0,
                    pan: 0.0,
                    fade_in: Time::ZERO,
                    fade_out: Time::ZERO,
                },
            ]
        } else {
            vec![]
        };
        ExportPlan {
            output: "out.mp4".into(),
            total: Time::from_secs(6),
            video: vec![
                seg(2, "v1.mp4", VideoLook::default(), None),
                seg(2, "v2.mp4", look, Some(ExportTransition { kind: ExportTransitionKind::Dissolve, duration: Time::from_millis(500) })),
                ExportVideo { duration: Time::from_secs(2), source: None, transition_in: None },
            ],
            audio,
            settings: ExportSettings { width: 640, height: 360, ..ExportSettings::default() },
        }
    }

    /// Frozen output of the pre-refactor `build_graph` (audio half factored
    /// out into `audio_graph`): the legacy export must stay byte-identical.
    /// The one deliberate change since: an audio clip whose `source_start` is
    /// zero no longer carries an input `-ss 0` (the AAC priming-sample fix;
    /// see `audio_graph`), so `b.wav`'s input has no `-ss`.
    #[test]
    fn build_graph_is_unchanged_with_audio() {
        let (inputs, graph) = build_graph(&sample_plan(true));
        let expected_inputs: Vec<&str> = vec!["-ss", "1.000000", "-t", "3.266667", "-i", "v1.mp4", "-ss", "0.766667", "-t", "3.233333", "-i", "v2.mp4", "-ss", "2.000000", "-t", "1.250000", "-i", "a.wav", "-t", "2.500000", "-i", "b.wav"];
        assert_eq!(inputs, expected_inputs);
        assert_eq!(graph, r#"[0:v:0]setpts=(PTS-STARTPTS)/1.000000,fps=30/1,scale=640:360:force_original_aspect_ratio=decrease,setsar=1,pad=640:360:(ow-iw)/2:(oh-ih)/2:black,format=yuv420p,                     tpad=stop_mode=clone:stop_duration=2,trim=end_frame=68,setpts=PTS-STARTPTS[v0];
[1:v:0]setpts=(PTS-STARTPTS)/1.000000,fps=30/1,scale=640:360:force_original_aspect_ratio=decrease,setsar=1,scale=trunc(iw*0.5000/2)*2:trunc(ih*0.5000/2)*2,format=rgba,rotate=0.26180:ow=rotw(0.26180):oh=roth(0.26180):c=none,colorchannelmixer=aa=0.8000,pad=w=iw+720:h=ih+360:x=(ow-iw)/2+40:y=(oh-ih)/2+0:color=black@0,crop=640:360:(iw-640)/2:(ih-360)/2,premultiply=inplace=1,format=yuv420p,                     tpad=stop_mode=clone:stop_duration=2,trim=end_frame=67,setpts=PTS-STARTPTS[v1];
color=c=black:s=640x360:r=30/1,format=yuv420p,trim=end_frame=60,setpts=PTS-STARTPTS[v2];
[v0]null[c1];
[c1][v1]xfade=transition=fade:duration=0.500000:offset=1.766667[x1];
[x1][v2]concat=n=2:v=1:a=0,fps=30/1[vout];
[2:a:0]asetpts=PTS-STARTPTS,atempo=0.5,atempo=0.500000,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,apad=whole_dur=3.000000,atrim=duration=3.000000,volume=-6.000dB,pan=stereo|c0=0.5000*c0|c1=1.0000*c1,afade=t=in:st=0:d=0.2000,afade=t=out:st=2.7000:d=0.3000,adelay=delays=24000S:all=1[a0];
[3:a:0]asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,apad=whole_dur=2.000000,atrim=duration=2.000000,adelay=delays=144000S:all=1[a1];
[a0][a1]amix=inputs=2:normalize=0:dropout_transition=0,apad=whole_dur=6.000000,atrim=duration=6.000000[aout]
"#);
    }

    /// Frozen output of the pre-refactor `build_graph` (audio half factored
    /// out into `audio_graph`): the legacy export must stay byte-identical.
    #[test]
    fn build_graph_is_unchanged_without_audio() {
        let (inputs, graph) = build_graph(&sample_plan(false));
        let expected_inputs: Vec<&str> = vec!["-ss", "1.000000", "-t", "3.266667", "-i", "v1.mp4", "-ss", "0.766667", "-t", "3.233333", "-i", "v2.mp4"];
        assert_eq!(inputs, expected_inputs);
        assert_eq!(graph, r#"[0:v:0]setpts=(PTS-STARTPTS)/1.000000,fps=30/1,scale=640:360:force_original_aspect_ratio=decrease,setsar=1,pad=640:360:(ow-iw)/2:(oh-ih)/2:black,format=yuv420p,                     tpad=stop_mode=clone:stop_duration=2,trim=end_frame=68,setpts=PTS-STARTPTS[v0];
[1:v:0]setpts=(PTS-STARTPTS)/1.000000,fps=30/1,scale=640:360:force_original_aspect_ratio=decrease,setsar=1,scale=trunc(iw*0.5000/2)*2:trunc(ih*0.5000/2)*2,format=rgba,rotate=0.26180:ow=rotw(0.26180):oh=roth(0.26180):c=none,colorchannelmixer=aa=0.8000,pad=w=iw+720:h=ih+360:x=(ow-iw)/2+40:y=(oh-ih)/2+0:color=black@0,crop=640:360:(iw-640)/2:(ih-360)/2,premultiply=inplace=1,format=yuv420p,                     tpad=stop_mode=clone:stop_duration=2,trim=end_frame=67,setpts=PTS-STARTPTS[v1];
color=c=black:s=640x360:r=30/1,format=yuv420p,trim=end_frame=60,setpts=PTS-STARTPTS[v2];
[v0]null[c1];
[c1][v1]xfade=transition=fade:duration=0.500000:offset=1.766667[x1];
[x1][v2]concat=n=2:v=1:a=0,fps=30/1[vout];
anullsrc=r=48000:cl=stereo,atrim=duration=6.000000[aout]
"#);
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
