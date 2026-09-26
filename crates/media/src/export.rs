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

/// One flat video segment; segments are laid end to end.
#[derive(Clone, Debug)]
pub struct ExportVideo {
    pub duration: Time,
    /// `None` renders black.
    pub source: Option<ExportVideoSource>,
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

/// Builds the FFmpeg input arguments and filter graph script.
pub fn build_graph(plan: &ExportPlan) -> (Vec<String>, String) {
    let st = &plan.settings;
    let (w, h) = (st.width & !1, st.height & !1);
    let rate = st.rate.to_ffmpeg_arg();
    let mut inputs: Vec<String> = vec![];
    let mut graph = String::new();
    let mut vlabels = vec![];
    let mut n_in = 0usize;

    for (i, seg) in plan.video.iter().enumerate() {
        let frames = st.rate.time_to_frame_round(seg.duration).max(1);
        match &seg.source {
            Some(src) => {
                let speed = if src.speed > 0.0 { src.speed } else { 1.0 };
                let src_dur = Time::from_secs_f64(seg.duration.as_secs_f64() * speed) + Time::from_secs(1);
                inputs.extend(["-ss".into(), src.source_start.max(Time::ZERO).to_ffmpeg_arg(), "-t".into(), src_dur.to_ffmpeg_arg(), "-i".into()]);
                inputs.push(src.path.to_string_lossy().into_owned());
                let _ = writeln!(
                    graph,
                    "[{n_in}:v:0]setpts=(PTS-STARTPTS)/{speed:.6},fps={rate},{look},format=yuv420p,\
                     tpad=stop_mode=clone:stop_duration=2,trim=end_frame={frames},setpts=PTS-STARTPTS[v{i}];",
                    look = look_filter(&src.look, w, h, 1.0)
                );
                n_in += 1;
            }
            None => {
                let _ = writeln!(graph, "color=c=black:s={w}x{h}:r={rate},format=yuv420p,trim=end_frame={frames},setpts=PTS-STARTPTS[v{i}];");
            }
        }
        vlabels.push(format!("[v{i}]"));
    }
    if vlabels.is_empty() {
        let frames = st.rate.time_to_frame_round(plan.total).max(1);
        let _ = writeln!(graph, "color=c=black:s={w}x{h}:r={rate},format=yuv420p,trim=end_frame={frames}[v0];");
        vlabels.push("[v0]".into());
    }
    let _ = writeln!(graph, "{}concat=n={}:v=1:a=0[vout];", vlabels.concat(), vlabels.len());

    let sr = st.sample_rate;
    let total_s = plan.total.as_secs_f64();
    let mut alabels = vec![];
    for (i, a) in plan.audio.iter().enumerate() {
        let speed = if a.speed > 0.0 { a.speed } else { 1.0 };
        let src_dur = Time::from_secs_f64(a.duration.as_secs_f64() * speed) + Time::from_millis(500);
        inputs.extend(["-ss".into(), a.source_start.max(Time::ZERO).to_ffmpeg_arg(), "-t".into(), src_dur.to_ffmpeg_arg(), "-i".into()]);
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
        n_in += 1;
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

fn container_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mov") => "mov",
        Some("mkv") => "matroska",
        _ => "mp4",
    }
}

pub(crate) fn run(ff: &FfmpegCli, plan: &ExportPlan, progress: Progress, cancel: &CancelToken) -> Result<()> {
    if plan.total <= Time::ZERO {
        return Err(MediaError::Unsupported("sequence is empty".into()));
    }
    let (inputs, graph) = build_graph(plan);
    let cmd_len: usize = inputs.iter().map(|s| s.len() + 3).sum();
    if cmd_len > 30_000 {
        // Windows limits a command line to 32 767 chars.
        return Err(MediaError::Unsupported(format!(
            "timeline has too many segments for a single export pass ({} inputs)",
            inputs.len() / 6
        )));
    }
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
