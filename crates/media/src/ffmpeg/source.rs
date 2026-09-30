//! Source decoding (render spec §4.2, §6, §9; M3 plan "Decisions"): a
//! source's own frames at its constant rate, exact size and explicit YUV
//! matrix and range — no composition, no letterbox.
//!
//! Frame numbering. Source frame `k` is the last frame whose timestamp is
//! before `frame_to_time(k) + frame/4` (a quarter frame of tolerance for
//! container timestamp rounding). A stream opened at `start_frame = s` puts
//! the origin of its timeline at `origin = frame_to_time(s) − frame/4`, so
//! frame `s` arrives at `+¼` and `fps` (round to nearest, `start_time=0`)
//! emits it on output tick 0, then `s + 1` on tick 1, and so on.
//!
//! * The seek is *not* accurate (`-noaccurate_seek`): FFmpeg's accurate seek
//!   drops every frame before the seek point, including a variable-rate
//!   frame that is still on screen at the origin, and the stream would then
//!   start with the next frame. Instead decoding starts at the keyframe
//!   before the origin and `fps` itself keeps the last frame at or before
//!   each tick — the same choice a read from frame 0 makes.
//! * `start_time=0`: when the first decodable frame comes after the origin
//!   (a video stream that starts after the container, a seek that lands
//!   late) the first frame is repeated back to tick 0, so later frames keep
//!   their numbers.
//! * The seek point is `max(0, origin − preroll)`, and `setpts` moves the
//!   timeline so its zero is `origin` whatever the seek point was (a
//!   positive shift at `s = 0`, a negative one with preroll).
//! * Containers without an index (MPEG-TS/M2TS, MPEG-PS, elementary
//!   streams) seek to any packet near the target, not to the keyframe before
//!   it; the decoder then either waits for the next keyframe or conceals the
//!   missing references, both wrong. They get a preroll of
//!   [`INDEXLESS_PREROLL`] — correct for GOPs up to that long, at the cost
//!   of decoding it on every seek (a keyframe index would remove both
//!   limits).

use super::{read_frame, truncate, FfmpegCli};
use crate::{MediaError, Result, SourceRequest, SourceStream};
use kadr_core::color::{Matrix, Range};
use kadr_core::Time;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Stdio};
use std::thread::JoinHandle;

/// Decoding starts this much before the origin in containers without an
/// index (see module docs); covers GOPs up to its length (HLS/YouTube
/// transport streams use 5 s, broadcast and cameras 0.5–2 s).
pub(crate) const INDEXLESS_PREROLL: Time = Time::from_secs(6);

/// Whether FFmpeg seeks in `path` without an index. Sniffed from the first
/// bytes, not the extension: a `.part` download or a renamed file keeps its
/// real format.
pub(crate) fn is_indexless(path: &Path) -> bool {
    let mut head = Vec::with_capacity(400);
    match std::fs::File::open(path).and_then(|f| f.take(400).read_to_end(&mut head)) {
        Ok(_) => is_indexless_head(&head),
        Err(_) => false,
    }
}

/// MPEG-TS (188-byte packets) or M2TS (192, with a 4-byte timestamp): sync
/// byte 0x47 at three packet starts. MPEG-PS and elementary streams (H.264,
/// HEVC, MPEG video): a start code, unless it is the size of an ISO BMFF box.
fn is_indexless_head(b: &[u8]) -> bool {
    let sync = |first: usize, stride: usize| (0..3).all(|i| b.get(first + i * stride) == Some(&0x47));
    let start_code = b.starts_with(&[0, 0, 1]) || b.starts_with(&[0, 0, 0, 1]);
    let iso_box = b.len() >= 8 && matches!(&b[4..8], b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot" | b"uuid");
    sync(0, 188) || sync(4, 192) || (start_code && !iso_box)
}

/// Where the decoder seeks, and the shift that puts the timeline origin at
/// `frame_to_time(start_frame) − frame/4` (see module docs).
pub(crate) fn seek_plan(req: &SourceRequest, preroll: Time) -> (Time, Time) {
    let quarter = Time::from_flicks(req.rate.frame_duration().flicks() / 4);
    let origin = req.rate.frame_to_time(req.start_frame) - quarter;
    let seek = (origin - preroll).max(Time::ZERO);
    (seek, seek - origin)
}

/// The `-vf` chain: the timeline shift from [`seek_plan`], constant rate,
/// exact size (SAR absorbed; autorotation has already made the frame
/// upright), YUV → full-range R'G'B' with the source's own matrix and
/// range, RGBA (alpha kept, 255 when opaque).
pub(crate) fn source_filter(req: &SourceRequest, shift: Time) -> String {
    let color = match req.color.matrix {
        // R'G'B' sources need no matrix, and FFmpeg treats RGB as full range.
        Matrix::Rgb => String::new(),
        m => {
            let matrix = match m {
                Matrix::Bt709 => "bt709",
                Matrix::Bt601 => "bt601",
                Matrix::Bt2020Ncl | Matrix::Rgb => "bt2020",
            };
            let range = match req.color.range {
                Range::Limited => "tv",
                Range::Full => "pc",
            };
            format!(":in_color_matrix={matrix}:in_range={range}")
        }
    };
    let setpts = match shift {
        Time::ZERO => String::new(),
        t if t.is_negative() => format!("setpts=PTS-{}/TB,", (-t).to_ffmpeg_arg()),
        t => format!("setpts=PTS+{}/TB,", t.to_ffmpeg_arg()),
    };
    format!(
        "{setpts}fps={rate}:start_time=0,scale={w}:{h}:flags={SOURCE_SWS_FLAGS}{color}:out_range=pc,format=rgba",
        rate = req.rate.to_ffmpeg_arg(),
        w = req.width,
        h = req.height,
    )
}

/// Bilinear: cheap, and the renderer resamples again anyway. It already
/// meets the ±2 LSB colour tests (spec §6); `accurate_rnd` changed nothing
/// there and `full_chroma_int` cost ~20 % of 4K → 1080p decode speed.
const SOURCE_SWS_FLAGS: &str = "bilinear";
/// Stills decode once: a better filter costs nothing noticeable.
const STILL_SWS_FLAGS: &str = "bicubic+accurate_rnd+full_chroma_int";

fn invalid(msg: String) -> MediaError {
    MediaError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

fn frame_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

fn check_exists(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(MediaError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} not found", path.display()))))
    }
}

/// Keeps the last [`STDERR_TAIL`] bytes of the decoder's stderr. Draining it
/// on a thread also means a chatty decoder never blocks on a full pipe.
const STDERR_TAIL: usize = 4096;

fn drain_stderr(mut r: ChildStderr) -> std::io::Result<JoinHandle<Vec<u8>>> {
    std::thread::Builder::new().name("ffmpeg-stderr".into()).spawn(move || {
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

/// Long-running decoder process writing raw RGBA frames to a pipe; the pipe
/// is backpressure (FFmpeg blocks while nobody reads).
pub(super) struct FfmpegSource {
    child: Child,
    stdout: ChildStdout,
    stderr: Option<JoinHandle<Vec<u8>>>,
    path: PathBuf,
    frame_len: usize,
    /// Frames delivered so far.
    frames: u64,
    done: bool,
}

impl FfmpegSource {
    pub(super) fn open(ff: &FfmpegCli, req: &SourceRequest) -> Result<Self> {
        if req.width == 0 || req.height == 0 {
            return Err(invalid(format!("source size {}×{}", req.width, req.height)));
        }
        if req.rate.num == 0 || req.rate.den == 0 {
            return Err(invalid(format!("source rate {}/{}", req.rate.num, req.rate.den)));
        }
        if req.start_frame < 0 {
            return Err(invalid(format!("negative start frame {}", req.start_frame)));
        }
        check_exists(&req.path)?;

        let mut cmd = ff.ffmpeg_cmd();
        if req.hwaccel {
            cmd.args(["-hwaccel", "auto"]);
        }
        let preroll = if is_indexless(&req.path) { INDEXLESS_PREROLL } else { Time::ZERO };
        let (seek, shift) = seek_plan(req, preroll);
        cmd.args(["-noaccurate_seek", "-ss", &seek.to_ffmpeg_arg()]).arg("-i").arg(&req.path);
        cmd.args(["-an", "-sn", "-dn", "-vf", &source_filter(req, shift), "-f", "rawvideo", "-"]);
        tracing::debug!(?cmd, "open source");
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| MediaError::Spawn { tool: "ffmpeg".into(), source })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = match drain_stderr(child.stderr.take().expect("piped stderr")) {
            Ok(h) => h,
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MediaError::Spawn { tool: "ffmpeg stderr reader".into(), source });
            }
        };
        Ok(FfmpegSource {
            child,
            stdout,
            stderr: Some(stderr),
            path: req.path.clone(),
            frame_len: frame_len(req.width, req.height),
            frames: 0,
            done: false,
        })
    }

    /// The process has closed its output: success is the end of the stream,
    /// anything else an error carrying FFmpeg's own message.
    fn finish(&mut self, io: Option<std::io::Error>) -> Result<bool> {
        self.done = true;
        let status = self.child.wait()?;
        let tail = self.stderr.take().and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = truncate(String::from_utf8_lossy(&tail).trim().to_string());
        if !status.success() {
            tracing::warn!(path = %self.path.display(), %status, %stderr, frames = self.frames, "source decode failed");
            let stderr = if stderr.is_empty() { format!("{}: no output", self.path.display()) } else { stderr };
            return Err(MediaError::ToolFailed { tool: "ffmpeg".into(), status: status.to_string(), stderr });
        }
        if let Some(e) = io {
            return Err(e.into());
        }
        if !stderr.is_empty() {
            tracing::debug!(path = %self.path.display(), %stderr, frames = self.frames, "source decode ended with messages");
        }
        Ok(false)
    }
}

impl SourceStream for FfmpegSource {
    fn read_into(&mut self, buf: &mut [u8]) -> Result<bool> {
        if buf.len() != self.frame_len {
            return Err(invalid(format!("frame buffer is {} bytes, expected {}", buf.len(), self.frame_len)));
        }
        if self.done {
            return Ok(false);
        }
        match read_frame(&mut self.stdout, buf) {
            Ok(true) => {
                self.frames += 1;
                Ok(true)
            }
            Ok(false) => self.finish(None),
            Err(e) => self.finish(Some(e)),
        }
    }
}

impl Drop for FfmpegSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The stderr thread ends by itself once the pipe closes.
    }
}

/// First frame of `path` scaled to exactly `width × height`, RGBA straight
/// alpha, into `out` (one PAM through a pipe; a still decodes once).
pub(super) fn decode_still(ff: &FfmpegCli, path: &Path, width: u32, height: u32, out: &mut [u8]) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(invalid(format!("still size {width}×{height}")));
    }
    if out.len() != frame_len(width, height) {
        return Err(invalid(format!("still buffer is {} bytes, expected {}", out.len(), frame_len(width, height))));
    }
    check_exists(path)?;
    let mut cmd = ff.ffmpeg_cmd();
    cmd.arg("-i").arg(path);
    cmd.args([
        "-frames:v",
        "1",
        "-an",
        "-sn",
        "-dn",
        "-vf",
        &format!("scale={width}:{height}:flags={STILL_SWS_FLAGS},format=rgba"),
        "-c:v",
        "pam",
        "-f",
        "image2pipe",
        "-",
    ]);
    let pam = super::run_capture(cmd, "ffmpeg")?;
    let (w, h, px) = super::frames::pam_payload(&pam)
        .ok_or_else(|| MediaError::Unsupported(format!("no image in {}", path.display())))?;
    if (w, h) != (width, height) {
        return Err(MediaError::Unsupported(format!("{}: decoded {w}×{h}, asked for {width}×{height}", path.display())));
    }
    out.copy_from_slice(px);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{ColorInfo, FrameRate};

    fn req(rate: FrameRate, start_frame: i64, color: ColorInfo) -> SourceRequest {
        SourceRequest { path: "x.mp4".into(), rate, start_frame, width: 85, height: 48, color, hwaccel: false }
    }

    #[test]
    fn origin_is_a_quarter_frame_early_whatever_the_seek_point() {
        let c = ColorInfo::guess_video(1920, 1080);
        let plan = |rate, start, preroll| {
            let (seek, shift) = seek_plan(&req(rate, start, c), preroll);
            (seek.to_ffmpeg_arg(), shift.to_ffmpeg_arg())
        };
        // Frame 0: the seek cannot go before 0, the timeline shifts instead.
        assert_eq!(plan(FrameRate::FPS_25, 0, Time::ZERO), ("0.000000".into(), "0.010000".into()));
        assert_eq!(plan(FrameRate::FPS_25, 10, Time::ZERO), ("0.390000".into(), "0.000000".into()));
        // 30000/1001: frame 30 at 1.001 s, a quarter frame is 8.341(6) ms.
        assert_eq!(plan(FrameRate::FPS_29_97, 30, Time::ZERO), ("0.992658".into(), "0.000000".into()));
        // Preroll seeks earlier and shifts back.
        assert_eq!(plan(FrameRate::FPS_25, 200, Time::from_secs(6)), ("1.990000".into(), "-6.000000".into()));
        assert_eq!(plan(FrameRate::FPS_25, 10, Time::from_secs(6)), ("0.000000".into(), "-0.390000".into()));
        let f = source_filter(&req(FrameRate::FPS_25, 200, c), Time::from_secs(-6));
        assert!(f.starts_with("setpts=PTS-6.000000/TB,fps=25/1:start_time=0,"), "{f}");
        let f = source_filter(&req(FrameRate::FPS_25, 0, c), Time::from_millis(10));
        assert!(f.starts_with("setpts=PTS+0.010000/TB,fps=25/1:start_time=0,"), "{f}");
    }

    #[test]
    fn index_less_containers_are_sniffed_not_guessed_from_the_name() {
        let ts: Vec<u8> = (0..3).flat_map(|_| std::iter::once(0x47).chain(std::iter::repeat_n(0xff, 187))).collect();
        let m2ts: Vec<u8> = (0..3).flat_map(|_| [0u8, 0, 0, 0, 0x47].into_iter().chain(std::iter::repeat_n(0xff, 187))).collect();
        assert!(is_indexless_head(&ts));
        assert!(is_indexless_head(&m2ts));
        assert!(is_indexless_head(&[0, 0, 1, 0xba, 0x44, 0, 4, 0]), "MPEG-PS pack header");
        assert!(is_indexless_head(&[0, 0, 0, 1, 0x67, 0x64, 0, 0x1f]), "H.264 Annex B");
        assert!(!is_indexless_head(b"\0\0\0\x20ftypisom\0\0\x02\0"), "MP4");
        assert!(!is_indexless_head(b"\0\0\x01\x08moov"), "an ISO box whose size looks like a start code");
        assert!(!is_indexless_head(&[0x1a, 0x45, 0xdf, 0xa3]), "Matroska");
        assert!(!is_indexless_head(&ts[..300]), "one sync byte is not a stream");

        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("clip.mp4.part");
        std::fs::write(&part, &ts).unwrap();
        assert!(is_indexless(&part));
        assert!(!is_indexless(&dir.path().join("missing.ts")));
    }

    #[test]
    fn filter_names_the_matrix_and_range_explicitly() {
        let mut c = ColorInfo::guess_video(1920, 1080);
        assert_eq!(
            source_filter(&req(FrameRate::FPS_29_97, 1, c), Time::ZERO),
            "fps=30000/1001:start_time=0,scale=85:48:flags=bilinear:in_color_matrix=bt709:in_range=tv:out_range=pc,format=rgba"
        );
        c.matrix = Matrix::Bt601;
        c.range = Range::Full;
        assert!(source_filter(&req(FrameRate::FPS_25, 1, c), Time::ZERO).contains(":in_color_matrix=bt601:in_range=pc:out_range=pc,"));
        c.matrix = Matrix::Bt2020Ncl;
        assert!(source_filter(&req(FrameRate::FPS_25, 1, c), Time::ZERO).contains(":in_color_matrix=bt2020:"));
        let f = source_filter(&req(FrameRate::FPS_25, 1, ColorInfo::IMAGE_SRGB), Time::ZERO);
        assert!(!f.contains("in_color_matrix") && !f.contains("in_range"), "{f}");
    }
}
