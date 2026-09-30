//! FFmpeg CLI backend. Each operation is an isolated child process: a
//! decoder crash can never take the editor down, and GPL FFmpeg builds are
//! used without being linked into Kadr.

mod frames;
mod pcm;
mod probe;
pub(crate) mod progress;
mod source;

use crate::{ExportPlan, MediaBackend, MediaError, Progress, RgbaFrame, Result, SourceRequest, SourceStream, StreamRequest, VideoStream};
use kadr_core::{CancelToken, MediaInfo, Time};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct FfmpegCli {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    /// Use hardware decoding for playback streams.
    pub hwaccel: bool,
}

impl FfmpegCli {
    /// Locates binaries: `KADR_FFMPEG_DIR`, next to the executable, then PATH.
    pub fn locate() -> Result<Self> {
        let exe = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let mut dirs: Vec<PathBuf> = vec![];
        if let Some(d) = std::env::var_os("KADR_FFMPEG_DIR") {
            dirs.push(d.into());
        }
        if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            dirs.push(d.join("ffmpeg"));
            dirs.push(d);
        }
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        let find = |name: &str| dirs.iter().map(|d| d.join(exe(name))).find(|p| p.is_file());
        match (find("ffmpeg"), find("ffprobe")) {
            (Some(ffmpeg), Some(ffprobe)) => {
                tracing::info!(ffmpeg = %ffmpeg.display(), "media backend: FFmpeg CLI");
                Ok(FfmpegCli { ffmpeg, ffprobe, hwaccel: true })
            }
            _ => Err(MediaError::BackendMissing("ffmpeg, ffprobe".into())),
        }
    }

    pub(crate) fn ffmpeg_cmd(&self) -> Command {
        let mut c = command(&self.ffmpeg);
        c.args(["-hide_banner", "-nostdin", "-v", "error"]);
        c
    }
    pub(crate) fn ffprobe_cmd(&self) -> Command {
        let mut c = command(&self.ffprobe);
        c.args(["-hide_banner", "-v", "error"]);
        c
    }
}

/// Creates a command that never pops a console window in the GUI app.
fn command(program: &Path) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Runs to completion, returning stdout; maps failures to `ToolFailed`.
pub(crate) fn run_capture(mut cmd: Command, tool: &str) -> Result<Vec<u8>> {
    tracing::trace!(?cmd, "run");
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| MediaError::Spawn { tool: tool.into(), source })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        tracing::warn!(tool, status = %out.status, %stderr, "media tool failed");
        return Err(MediaError::ToolFailed { tool: tool.into(), status: out.status.to_string(), stderr: truncate(stderr) });
    }
    Ok(out.stdout)
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

/// Reads exactly `buf.len()` bytes; `Ok(false)` on clean EOF at a boundary.
pub(crate) fn read_frame(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 if filled == 0 => return Ok(false),
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => filled += n,
        }
    }
    Ok(true)
}

impl MediaBackend for FfmpegCli {
    fn name(&self) -> &str {
        "FFmpeg CLI"
    }
    fn probe(&self, path: &Path) -> Result<MediaInfo> {
        probe::probe(self, path)
    }
    fn decode_frame(&self, path: &Path, at: Time, max_w: u32, max_h: u32) -> Result<RgbaFrame> {
        frames::decode_frame(self, path, at, max_w, max_h, false)
    }
    fn open_stream(&self, req: &StreamRequest) -> Result<Box<dyn VideoStream>> {
        Ok(Box::new(frames::FfmpegStream::open(self, req)?))
    }
    fn open_source(&self, req: &SourceRequest) -> Result<Box<dyn SourceStream>> {
        Ok(Box::new(source::FfmpegSource::open(self, req)?))
    }
    fn decode_still(&self, path: &Path, width: u32, height: u32, out: &mut [u8]) -> Result<()> {
        source::decode_still(self, path, width, height, out)
    }
    fn thumbnails(&self, path: &Path, times: &[Time], height: u32, cancel: &CancelToken) -> Result<Vec<RgbaFrame>> {
        let mut out = Vec::with_capacity(times.len());
        for &t in times {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            // Keyframe-only decode is fast but finds nothing when the nearest
            // keyframe lies before a seek point near the end; fall back to exact.
            let f = frames::decode_frame(self, path, t, height * 4, height, true)
                .or_else(|_| frames::decode_frame(self, path, t, height * 4, height, false))?;
            out.push(f);
        }
        Ok(out)
    }
    fn extract_pcm(
        &self,
        path: &Path,
        out: &Path,
        rate: u32,
        channels: u32,
        duration: Time,
        progress: Progress,
        cancel: &CancelToken,
    ) -> Result<()> {
        pcm::extract_pcm(self, path, out, rate, channels, duration, progress, cancel)
    }
    fn export(&self, plan: &ExportPlan, progress: Progress, cancel: &CancelToken) -> Result<()> {
        crate::export::run(self, plan, progress, cancel)
    }
}
