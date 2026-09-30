//! Frame encoding for the render pipeline export: the caller renders each
//! output frame to RGBA and pushes it into an encoder; audio still goes
//! through the FFmpeg audio graph of the legacy export (M5 plan, task 1).
//!
//! The FFmpeg command reads `-f rawvideo -pix_fmt rgba` from stdin, converts
//! full-range R'G'B' to limited-range BT.709 `yuv420p` with an explicit
//! `scale` (never FFmpeg's defaults) and tags the stream BT.709 / limited, so
//! players decode it with the matrix the encoder used. Output goes to
//! `<output>.part`, renamed on [`FrameEncoder::finish`] and deleted on
//! failure or abort.

use crate::export::{audio_graph, check_command_len, container_for, ExportAudio, ExportSettings};
use crate::ffmpeg::FfmpegCli;
use crate::{MediaError, Result};
use kadr_core::{FrameRate, Time};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Stdio};
use std::thread::JoinHandle;

/// One encode: `frames` RGBA frames of `width × height` at `rate`, plus the
/// audio graph of `audio`, into `output`.
#[derive(Clone, Debug)]
pub struct EncodeJob {
    pub output: PathBuf,
    /// Frame size; both even (yuv420p).
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    /// Frames the caller will write (`rate.time_to_frame_round(total)`).
    pub frames: i64,
    /// Length of the sequence; the audio is padded or cut to it.
    pub total: Time,
    pub audio: Vec<ExportAudio>,
    /// Codec, CRF, preset, audio bitrate and sample rate (its `width`,
    /// `height` and `rate` are ignored: the job's own are used).
    pub settings: ExportSettings,
}

/// Sink for the rendered frames of one [`EncodeJob`].
pub trait FrameEncoder: Send {
    /// Appends the next frame: `width × height × 4` bytes, RGBA8, full-range
    /// non-linear R'G'B', alpha ignored. Blocks while the encoder is busy.
    /// A wrong buffer size is an error and leaves the encoder usable; a
    /// failure of the encoder process is an error carrying FFmpeg's message
    /// and ends the encode (the next writes fail as well).
    fn write_frame(&mut self, rgba: &[u8]) -> Result<()>;
    /// Ends the stream, waits for the encoder and moves the result to its
    /// final name. On failure the partial output is deleted.
    fn finish(self: Box<Self>) -> Result<()>;
    /// Stops the encoder at once and deletes everything it wrote.
    fn abort(self: Box<Self>);
    /// Frames accepted by [`write_frame`](Self::write_frame) so far.
    fn frames_written(&self) -> i64;
}

fn invalid(msg: String) -> MediaError {
    MediaError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

fn truncate(mut s: String) -> String {
    const MAX: usize = 2000;
    if s.len() > MAX {
        let cut = s.char_indices().map(|(i, _)| i).take_while(|&i| i <= MAX).last().unwrap_or(0);
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// The last [`STDERR_TAIL`] bytes of FFmpeg's stderr, kept by a thread that
/// drains the pipe so a chatty process never blocks on it.
const STDERR_TAIL: usize = 4096;

fn drain_stderr(mut r: ChildStderr) -> std::io::Result<JoinHandle<Vec<u8>>> {
    std::thread::Builder::new().name("ffmpeg-encode-stderr".into()).spawn(move || {
        let mut tail = Vec::new();
        let mut chunk = [0u8; 1024];
        while let Ok(n @ 1..) = r.read(&mut chunk) {
            tail.extend_from_slice(&chunk[..n]);
            if tail.len() > 2 * STDERR_TAIL {
                tail.drain(..tail.len() - STDERR_TAIL);
            }
        }
        if tail.len() > STDERR_TAIL {
            tail.drain(..tail.len() - STDERR_TAIL);
        }
        tail
    })
}

/// The filter script: RGBA from stdin (input 0) to limited-range BT.709
/// yuv420p, then the audio graph over inputs `1..`.
fn encode_graph(audio: &[ExportAudio], total: Time, sample_rate: u32) -> (Vec<String>, String) {
    let (inputs, audio_script) = audio_graph(audio, 1, total, sample_rate);
    let graph = format!("[0:v]scale=in_range=pc:out_color_matrix=bt709:out_range=tv,format=yuv420p,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv[vout];\n{audio_script}");
    (inputs, graph)
}

fn part_path(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

struct FfmpegEncoder {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    script: PathBuf,
    part: PathBuf,
    output: PathBuf,
    frame_len: usize,
    expected_frames: i64,
    frames: i64,
    /// The process failed or was stopped: nothing more can be written.
    failed: Option<String>,
    /// `finish` or `abort` ran; `Drop` has nothing left to clean up.
    closed: bool,
}

pub(crate) fn start(ff: &FfmpegCli, job: &EncodeJob) -> Result<Box<dyn FrameEncoder>> {
    if job.width == 0 || job.height == 0 || !job.width.is_multiple_of(2) || !job.height.is_multiple_of(2) {
        return Err(invalid(format!("encode size {}×{} must be even and non-zero", job.width, job.height)));
    }
    if job.rate.num == 0 || job.rate.den == 0 {
        return Err(invalid(format!("encode rate {}/{}", job.rate.num, job.rate.den)));
    }
    if job.frames < 0 {
        return Err(invalid(format!("negative frame count {}", job.frames)));
    }
    if job.total <= Time::ZERO {
        return Err(MediaError::Unsupported("sequence is empty".into()));
    }
    let frame_len = (job.width as usize)
        .checked_mul(job.height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| invalid(format!("encode size {}×{} overflows", job.width, job.height)))?;

    let st = &job.settings;
    let (inputs, graph) = encode_graph(&job.audio, job.total, st.sample_rate);
    check_command_len(&inputs)?;

    // Unique per encode: several exports may run concurrently.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!("kadr-encode-{}-{n}.txt", std::process::id()));
    std::fs::write(&script, &graph)?;
    tracing::debug!(%graph, "encode filter graph");

    let part = part_path(&job.output);
    let rate = job.rate.to_ffmpeg_arg();
    let mut cmd = ff.ffmpeg_cmd();
    cmd.arg("-y").args(["-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{}x{}", job.width, job.height), "-framerate", &rate, "-i", "-"]);
    cmd.args(&inputs).arg("-/filter_complex").arg(&script);
    cmd.args(["-map", "[vout]", "-map", "[aout]", "-c:v", &st.video_codec]);
    if st.video_codec.starts_with("libx26") {
        cmd.args(["-preset", &st.preset, "-crf", &st.crf.to_string()]);
    }
    cmd.args(["-pix_fmt", "yuv420p", "-r", &rate, "-c:a", "aac", "-b:a", &format!("{}k", st.audio_bitrate_k)]);
    cmd.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]);
    cmd.args(["-movflags", "+faststart", "-f", container_for(&job.output)]).arg(&part);
    tracing::debug!(?cmd, "start encode");

    let mut child = match cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(source) => {
            let _ = std::fs::remove_file(&script);
            return Err(MediaError::Spawn { tool: "ffmpeg".into(), source });
        }
    };
    let stdin = child.stdin.take().expect("piped stdin");
    let stderr = match drain_stderr(child.stderr.take().expect("piped stderr")) {
        Ok(h) => h,
        Err(source) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&script);
            return Err(MediaError::Spawn { tool: "ffmpeg stderr reader".into(), source });
        }
    };
    Ok(Box::new(FfmpegEncoder {
        child,
        stdin: Some(stdin),
        stderr: Some(stderr),
        script,
        part,
        output: job.output.clone(),
        frame_len,
        expected_frames: job.frames,
        frames: 0,
        failed: None,
        closed: false,
    }))
}

impl FfmpegEncoder {
    fn stderr_tail(&mut self) -> String {
        let tail = self.stderr.take().and_then(|h| h.join().ok()).unwrap_or_default();
        truncate(String::from_utf8_lossy(&tail).trim().to_string())
    }

    /// Kills the process and deletes its files; safe to call repeatedly.
    fn kill_and_clean(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.stderr.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.part);
        let _ = std::fs::remove_file(&self.script);
        self.closed = true;
    }

    /// The process is gone or going: reap it and build the error from its
    /// exit status and stderr.
    fn process_failure(&mut self, io: Option<&std::io::Error>) -> MediaError {
        self.stdin = None;
        let status = match self.child.try_wait() {
            Ok(Some(s)) => s.to_string(),
            // Still running (the write failed for another reason): stop it.
            _ => {
                let _ = self.child.kill();
                self.child.wait().map(|s| s.to_string()).unwrap_or_else(|_| "unknown".into())
            }
        };
        let mut stderr = self.stderr_tail();
        if stderr.is_empty() {
            stderr = io.map_or_else(|| "no output".into(), |e| e.to_string());
        }
        tracing::warn!(output = %self.output.display(), %status, %stderr, frames = self.frames, "encode failed");
        self.failed = Some(stderr.clone());
        MediaError::ToolFailed { tool: "ffmpeg".into(), status, stderr }
    }
}

impl FrameEncoder for FfmpegEncoder {
    fn write_frame(&mut self, rgba: &[u8]) -> Result<()> {
        if rgba.len() != self.frame_len {
            return Err(invalid(format!("frame buffer is {} bytes, expected {}", rgba.len(), self.frame_len)));
        }
        if let Some(stderr) = &self.failed {
            return Err(MediaError::ToolFailed { tool: "ffmpeg".into(), status: "already failed".into(), stderr: stderr.clone() });
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(invalid("encoder is closed".into()));
        };
        match stdin.write_all(rgba) {
            Ok(()) => {
                self.frames += 1;
                Ok(())
            }
            Err(e) => Err(self.process_failure(Some(&e))),
        }
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if let Some(stderr) = self.failed.clone() {
            self.kill_and_clean();
            return Err(MediaError::ToolFailed { tool: "ffmpeg".into(), status: "already failed".into(), stderr });
        }
        if self.frames != self.expected_frames {
            tracing::warn!(written = self.frames, expected = self.expected_frames, "encode finished with an unexpected frame count");
        }
        // Closing stdin is the end of the video stream.
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.flush();
        }
        let status = match self.child.wait() {
            Ok(s) => s,
            Err(e) => {
                self.kill_and_clean();
                return Err(e.into());
            }
        };
        let stderr = self.stderr_tail();
        if !status.success() {
            tracing::warn!(output = %self.output.display(), %status, %stderr, "encode failed");
            self.kill_and_clean();
            let stderr = if stderr.is_empty() { "no output".into() } else { stderr };
            return Err(MediaError::ToolFailed { tool: "ffmpeg".into(), status: status.to_string(), stderr });
        }
        let _ = std::fs::remove_file(&self.script);
        if let Err(e) = std::fs::rename(&self.part, &self.output) {
            self.kill_and_clean();
            return Err(e.into());
        }
        self.closed = true;
        tracing::info!(output = %self.output.display(), frames = self.frames, "encode finished");
        Ok(())
    }

    fn abort(mut self: Box<Self>) {
        self.kill_and_clean();
    }

    fn frames_written(&self) -> i64 {
        self.frames
    }
}

impl Drop for FfmpegEncoder {
    fn drop(&mut self) {
        if !self.closed {
            self.kill_and_clean();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_puts_the_audio_after_the_raw_video_input() {
        let (inputs, graph) = encode_graph(&[], Time::from_secs(2), 48_000);
        assert!(inputs.is_empty());
        assert_eq!(
            graph,
            "[0:v]scale=in_range=pc:out_color_matrix=bt709:out_range=tv,format=yuv420p,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv[vout];\n\
             anullsrc=r=48000:cl=stereo,atrim=duration=2.000000[aout]\n"
        );
        let audio = ExportAudio {
            path: "a.wav".into(),
            source_start: Time::ZERO,
            timeline_start: Time::ZERO,
            duration: Time::from_secs(1),
            speed: 1.0,
            gain_db: 0.0,
            pan: 0.0,
            fade_in: Time::ZERO,
            fade_out: Time::ZERO,
        };
        let (inputs, graph) = encode_graph(&[audio], Time::from_secs(2), 48_000);
        assert_eq!(inputs.last().map(String::as_str), Some("a.wav"));
        assert!(graph.contains("[1:a:0]asetpts"), "{graph}");
    }
}
