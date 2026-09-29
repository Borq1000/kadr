use super::{read_frame, run_capture, FfmpegCli};
use crate::{MediaError, RgbaFrame, Result, StreamRequest, VideoStream};
use kadr_core::Time;
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};

/// Single frame via PAM output: its text header carries the final size, so
/// aspect-correct scaling needs no separate probe.
pub(super) fn decode_frame(ff: &FfmpegCli, path: &Path, at: Time, max_w: u32, max_h: u32, fast: bool) -> Result<RgbaFrame> {
    let mut cmd = ff.ffmpeg_cmd();
    if fast {
        // Nearest preceding keyframe only: ~10× faster, good for thumbnails.
        cmd.args(["-skip_frame", "nokey", "-noaccurate_seek"]);
    }
    cmd.args(["-ss", &at.max(Time::ZERO).to_ffmpeg_arg()]).arg("-i").arg(path);
    cmd.args([
        "-frames:v",
        "1",
        "-an",
        "-sn",
        "-vf",
        &format!("scale=w={max_w}:h={max_h}:force_original_aspect_ratio=decrease:force_divisible_by=2,format=rgba"),
        "-c:v",
        "pam",
        "-f",
        "image2pipe",
        "-",
    ]);
    let out = run_capture(cmd, "ffmpeg")?;
    parse_pam(&out).ok_or_else(|| MediaError::Unsupported(format!("no frame at {at:?} in {}", path.display())))
}

pub(crate) fn parse_pam(buf: &[u8]) -> Option<RgbaFrame> {
    const END: &[u8] = b"ENDHDR\n";
    let hdr_end = buf.windows(END.len()).position(|w| w == END)? + END.len();
    let header = std::str::from_utf8(&buf[..hdr_end]).ok()?;
    let (mut w, mut h, mut depth) = (0u32, 0u32, 0u32);
    for line in header.lines() {
        let mut it = line.split_whitespace();
        match (it.next(), it.next()) {
            (Some("WIDTH"), Some(v)) => w = v.parse().ok()?,
            (Some("HEIGHT"), Some(v)) => h = v.parse().ok()?,
            (Some("DEPTH"), Some(v)) => depth = v.parse().ok()?,
            _ => {}
        }
    }
    let len = (w * h * 4) as usize;
    if depth != 4 || buf.len() < hdr_end + len {
        return None;
    }
    crate::stats::note_frame_alloc();
    Some(RgbaFrame { width: w, height: h, data: buf[hdr_end..hdr_end + len].to_vec() })
}

/// Long-running decoder process streaming raw RGBA frames through a pipe.
/// The pipe provides natural backpressure: FFmpeg blocks when we stop reading.
pub(super) struct FfmpegStream {
    child: Child,
    stdout: ChildStdout,
    width: u32,
    height: u32,
}

impl FfmpegStream {
    pub(super) fn open(ff: &FfmpegCli, req: &StreamRequest) -> Result<Self> {
        let (w, h) = (req.width & !1, req.height & !1);
        let mut cmd = ff.ffmpeg_cmd();
        if ff.hwaccel {
            cmd.args(["-hwaccel", "auto"]);
        }
        cmd.args(["-ss", &req.start.max(Time::ZERO).to_ffmpeg_arg()]).arg("-i").arg(&req.path);
        let speed = if req.speed.is_finite() && req.speed > 0.0 { req.speed } else { 1.0 };
        let vf = format!(
            "setpts=(PTS-STARTPTS)/{speed},fps={rate},scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black,format=rgba",
            rate = req.rate.to_ffmpeg_arg()
        );
        cmd.args(["-an", "-sn", "-vf", &vf, "-f", "rawvideo", "-"]);
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| MediaError::Spawn { tool: "ffmpeg".into(), source })?;
        let stdout = child.stdout.take().expect("piped stdout");
        Ok(FfmpegStream { child, stdout, width: w, height: h })
    }
}

impl VideoStream for FfmpegStream {
    fn next_frame(&mut self) -> Result<Option<RgbaFrame>> {
        let mut data = vec![0u8; (self.width * self.height * 4) as usize];
        crate::stats::note_frame_alloc();
        if read_frame(&mut self.stdout, &mut data)? {
            Ok(Some(RgbaFrame { width: self.width, height: self.height, data }))
        } else {
            Ok(None)
        }
    }
}

impl Drop for FfmpegStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
