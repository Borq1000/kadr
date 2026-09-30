//! A/V sync harness (render spec §8): a synthetic source with one flash per second on the video
//! and one click per second on the audio, and an analyzer that measures how far each click starts
//! from its flash in any file (the source itself, or a later export of a cut-up timeline).
//!
//! Source (see `generate_source`):
//! * video: the frame on screen encodes its source second `s` in luma. Ordinary frames are dim,
//!   `16 + (s mod 40)·2` (limited range); the ONE flash frame per second, the first frame whose
//!   start time is ≥ `s` seconds, is bright, `190 + (s mod 45)`.
//! * audio: silence, and a 5 ms 1 kHz burst (cosine, so full level from the very first sample)
//!   starting at sample `s·48000`.
//!
//! Analysis (see `analyze`): flash = run of frames whose mean luma is above the threshold (first
//! frame of the run), click = first sample above the threshold after ≥ 0.5 s since the previous
//! click. Each flash is paired with the nearest click; offset = click start − flash frame start.
//! A source made at 30000/1001 has offsets in (−1 frame, 0] by construction, because the flash
//! frame starts up to one frame after the whole second where the click sits.

use crate::report::Report;
use kadr_core::FrameRate;
use std::io;
use std::path::Path;
use std::process::Command;

const SAMPLE_RATE: usize = 48_000;
/// Mean luma (0..255, after decoding to gray) above which a frame is a flash. Flash frames are
/// ≥ 190 in limited range (≥ 203 if expanded), ordinary ones ≤ 94 (≤ 107 if expanded).
const FLASH_LUMA: f64 = 150.0;
/// Absolute sample level (of 32768) that starts a click; the source click is ~0.8 full scale.
const CLICK_LEVEL: i32 = 6_500;
/// Minimum distance between two clicks.
const CLICK_GAP_SECS: f64 = 0.5;
/// A flash and a click further apart than this are not a pair.
const PAIR_WINDOW_SECS: f64 = 0.5;

/// Result of measuring one file. The time vectors are for the M5 export test (cut mapping).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AvSync {
    pub flashes: usize,
    pub clicks: usize,
    pub pairs: usize,
    pub unmatched_flashes: usize,
    pub unmatched_clicks: usize,
    /// Largest `|click − flash|` over the pairs, in milliseconds (0 if no pairs).
    pub max_abs_offset_ms: f64,
    /// The same in frames of the analyzed frame rate.
    pub offsets_frames_max: f64,
    /// Mean signed offset, milliseconds (positive: audio late).
    pub mean_offset_ms: f64,
    /// Signed offset of every pair, milliseconds, in flash order.
    pub offsets_ms: Vec<f64>,
    /// Flash frame start times, seconds (file timeline).
    pub flash_times: Vec<f64>,
    /// Click onset times, seconds (file timeline).
    pub click_times: Vec<f64>,
    /// Duration of one frame, milliseconds.
    pub frame_ms: f64,
}

impl AvSync {
    /// Report rows for `kadr-bench`.
    pub fn report(&self, title: &str, case: &str) -> Report {
        let mut r = Report::new(title);
        r.push("avsync", case, "flashes", self.flashes as f64, "n");
        r.push("avsync", case, "clicks", self.clicks as f64, "n");
        r.push("avsync", case, "pairs", self.pairs as f64, "n");
        r.push("avsync", case, "unmatched flashes", self.unmatched_flashes as f64, "n");
        r.push("avsync", case, "unmatched clicks", self.unmatched_clicks as f64, "n");
        r.push("avsync", case, "max |offset|", self.max_abs_offset_ms, "ms");
        r.push("avsync", case, "max |offset|", self.offsets_frames_max, "frames");
        r.push("avsync", case, "mean offset", self.mean_offset_ms, "ms");
        r.push("avsync", case, "frame", self.frame_ms, "ms");
        r
    }

    /// Spec §8: every flash paired, nothing left over, all offsets within ±1 frame.
    pub fn within_one_frame(&self) -> bool {
        self.flashes > 0 && self.unmatched_flashes == 0 && self.unmatched_clicks == 0 && self.offsets_frames_max <= 1.0
    }
}

fn run_ffmpeg(args: &[&str]) -> io::Result<()> {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// lavfi expressions over the frame number `N`: (flash, second on screen). Frame `N` starts at
/// `N·den/num`; it is the first frame at or after a whole second exactly when `floor(t)` grows
/// (`N = 0` counts: `floor(-x) = -1`). `N·den` is an exact integer, and a quotient that is not
/// an integer is at least 1/num away from one, so double precision never misplaces a flash.
fn flash_expr(fps: FrameRate) -> (String, String) {
    let sec = |n: &str| format!("floor(({n})*{}/{})", fps.den, fps.num);
    (format!("gt({},{})", sec("N"), sec("N-1")), sec("N"))
}

/// Writes an H.264 + AAC (48 kHz stereo) mp4 of `secs` seconds, with a flash frame and a click
/// at the start of every whole second (see the module docs). Video is made at 16×16 by `geq`
/// (cheap) and scaled up, so the cost is the encode, not the generator.
pub fn generate_source(path: &Path, secs: u32, width: u32, height: u32, fps: FrameRate) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (flash, second) = flash_expr(fps);
    let lum = format!("if({flash},190+mod({second},45),16+mod({second},40)*2)");
    let video = format!(
        "color=c=black:s=16x16:r={}/{}:d={secs},format=yuv420p,geq=lum='{lum}':cb=128:cr=128,scale={width}:{height}:flags=neighbor,setsar=1,format=yuv420p",
        fps.num, fps.den
    );
    // `n` is the sample number, so the click starts on sample s·48000 with no float drift.
    let click = format!("if(lt(mod(n,{SAMPLE_RATE}),{}),0.8*cos(2*PI*1000*mod(n,{SAMPLE_RATE})/{SAMPLE_RATE}),0)", SAMPLE_RATE / 200);
    let audio = format!("aevalsrc=exprs='{click}|{click}':s={SAMPLE_RATE}:d={secs}");
    let gop = (fps.as_f64().round() as u32 * 2).to_string();
    let part = path.with_extension("part");
    let secs_s = secs.to_string();
    run_ffmpeg(&[
        "-f", "lavfi", "-i", &video, "-f", "lavfi", "-i", &audio,
        "-c:v", "libx264", "-preset", "ultrafast", "-crf", "10", "-g", &gop, "-sc_threshold", "0", "-pix_fmt", "yuv420p",
        "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709",
        "-c:a", "aac", "-b:a", "192k", "-t", &secs_s, "-f", "mp4", &part.to_string_lossy(),
    ])?;
    std::fs::rename(&part, path)
}

/// `start_time` of the first stream matching `selector` (`v:0`, `a:0`), seconds; 0 when unknown.
fn stream_start(path: &Path, selector: &str) -> f64 {
    Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", selector, "-show_entries", "stream=start_time", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().and_then(|l| l.trim().parse::<f64>().ok()))
        .unwrap_or(0.0)
}

/// Runs `ffmpeg -i path <args> -f <format> -` and returns stdout.
fn decode(path: &Path, args: &[&str], format: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(args)
        .args(["-f", format, "-"])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffmpeg failed on {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

/// Mean luma of every decoded video frame (16×16 gray), in presentation order, none dropped.
fn frame_lumas(path: &Path) -> Result<Vec<f64>, String> {
    let raw = decode(path, &["-an", "-map", "0:v:0", "-fps_mode", "passthrough", "-vf", "scale=16:16:flags=area,format=gray"], "rawvideo")?;
    Ok(raw.as_chunks::<256>().0.iter().map(|f| f.iter().map(|&b| b as f64).sum::<f64>() / 256.0).collect())
}

/// Click onsets as sample indices of the decoded mono 48 kHz audio.
fn click_onsets(path: &Path) -> Result<Vec<usize>, String> {
    let raw = decode(path, &["-vn", "-map", "0:a:0", "-ac", "1", "-ar", "48000", "-acodec", "pcm_s16le"], "s16le")?;
    let gap = (CLICK_GAP_SECS * SAMPLE_RATE as f64) as usize;
    let mut onsets: Vec<usize> = vec![];
    for (i, b) in raw.as_chunks::<2>().0.iter().enumerate() {
        if (i16::from_le_bytes(*b) as i32).abs() > CLICK_LEVEL && onsets.last().is_none_or(|&l| i >= l + gap) {
            onsets.push(i);
        }
    }
    Ok(onsets)
}

/// Measures flash/click alignment of `path`, whose video runs at the constant rate `fps`.
/// Times are on the file's own timeline: stream `start_time` + index / rate.
pub fn analyze(path: &Path, fps: FrameRate) -> Result<AvSync, String> {
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let lumas = frame_lumas(path)?;
    let (v0, a0) = (stream_start(path, "v:0"), stream_start(path, "a:0"));
    let frame_secs = fps.den as f64 / fps.num as f64;
    let mut flash_times: Vec<f64> = vec![];
    let mut was_flash = false;
    for (i, l) in lumas.iter().enumerate() {
        let is_flash = *l > FLASH_LUMA;
        if is_flash && !was_flash {
            flash_times.push(v0 + i as f64 * fps.den as f64 / fps.num as f64);
        }
        was_flash = is_flash;
    }
    let click_times: Vec<f64> = click_onsets(path)?.into_iter().map(|s| a0 + s as f64 / SAMPLE_RATE as f64).collect();

    let mut claimed = vec![false; click_times.len()];
    let mut offsets_ms = vec![];
    for &f in &flash_times {
        let nearest = click_times.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs()));
        if let Some((i, &c)) = nearest.filter(|(_, c)| (**c - f).abs() <= PAIR_WINDOW_SECS) {
            claimed[i] = true;
            offsets_ms.push((c - f) * 1000.0);
        }
    }
    let pairs = offsets_ms.len();
    let max_abs_offset_ms = offsets_ms.iter().fold(0.0f64, |m, o| m.max(o.abs()));
    Ok(AvSync {
        flashes: flash_times.len(),
        clicks: click_times.len(),
        pairs,
        unmatched_flashes: flash_times.len() - pairs,
        unmatched_clicks: claimed.iter().filter(|c| !**c).count(),
        max_abs_offset_ms,
        offsets_frames_max: max_abs_offset_ms / (frame_secs * 1000.0),
        mean_offset_ms: if pairs == 0 { 0.0 } else { offsets_ms.iter().sum::<f64>() / pairs as f64 },
        offsets_ms,
        flash_times,
        click_times,
        frame_ms: frame_secs * 1000.0,
    })
}

/// `kadr-bench avsync-selftest`: a 60 s 1280×720 29.97 fps source analyzed against itself.
pub fn selftest() -> Result<Report, String> {
    let fps = FrameRate::FPS_29_97;
    let path = crate::media::bench_dir().join("avsync_60s_2997.mp4");
    if !path.exists() {
        eprintln!("kadr-bench: generating {} …", path.display());
        generate_source(&path, 60, 1280, 720, fps).map_err(|e| e.to_string())?;
    }
    let m = analyze(&path, fps)?;
    let report = m.report("avsync-selftest", "source 60 s 1280x720 29.97");
    if m.flashes != 60 || m.clicks != 60 || !m.within_one_frame() {
        return Err(format!(
            "self-test failed: {} flashes, {} clicks, {} unmatched flashes, {} unmatched clicks, max offset {:.2} ms ({:.2} frames)\n{}",
            m.flashes,
            m.clicks,
            m.unmatched_flashes,
            m.unmatched_clicks,
            m.max_abs_offset_ms,
            m.offsets_frames_max,
            crate::report::markdown(&report)
        ));
    }
    Ok(report)
}

/// `kadr-bench avsync-analyze <file> [--fps N/D]`.
pub fn analyze_file(path: &Path, fps: FrameRate) -> Result<Report, String> {
    let m = analyze(path, fps)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(m.report("avsync-analyze", &name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have_ffmpeg() -> bool {
        let ok = ["ffmpeg", "ffprobe"].iter().all(|t| Command::new(t).arg("-version").output().is_ok_and(|o| o.status.success()));
        if !ok {
            eprintln!("avsync tests skipped: ffmpeg/ffprobe not on PATH");
        }
        ok
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kadr-avsync-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn flash_expression_marks_first_frame_at_or_after_each_second() {
        // Mirror the lavfi expression in integers and check it against exact rational time.
        for fps in [FrameRate::FPS_25, FrameRate::FPS_29_97, FrameRate::FPS_59_94, FrameRate::FPS_24] {
            let (num, den) = (fps.num as i64, fps.den as i64);
            let mut flashes = vec![];
            for n in 0..(num * 40 / den + 2) {
                if (n * den).div_euclid(num) > ((n - 1) * den).div_euclid(num) {
                    flashes.push(n);
                }
            }
            for (s, n) in flashes.iter().enumerate() {
                let s = s as i64;
                assert!(n * den >= s * num, "frame {n} starts before second {s}");
                assert!((n - 1) * den < s * num, "frame {n} is not the first at/after second {s}");
            }
            assert!(flashes.len() >= 40);
        }
    }

    #[test]
    fn source_has_one_flash_and_one_click_per_second_within_a_frame() {
        if !have_ffmpeg() {
            return;
        }
        for fps in [FrameRate::FPS_25, FrameRate::FPS_29_97] {
            let dir = scratch(&format!("src{}", fps.num));
            let src = dir.join("src.mp4");
            generate_source(&src, 12, 320, 180, fps).unwrap();
            let m = analyze(&src, fps).unwrap();
            assert_eq!((m.flashes, m.clicks, m.pairs), (12, 12, 12), "{m:?}");
            assert_eq!((m.unmatched_flashes, m.unmatched_clicks), (0, 0));
            assert!(m.within_one_frame(), "max offset {:.3} ms = {:.3} frames: {:?}", m.max_abs_offset_ms, m.offsets_frames_max, m.offsets_ms);
            // The flash frame starts at or after its second, so audio is never late in the source.
            assert!(m.offsets_ms.iter().all(|o| *o < 1.0), "{:?}", m.offsets_ms);
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn delayed_audio_is_reported_as_an_offset() {
        if !have_ffmpeg() {
            return;
        }
        let fps = FrameRate::FPS_25;
        let dir = scratch("shift");
        let src = dir.join("src.mp4");
        let shifted = dir.join("shifted.mp4");
        generate_source(&src, 12, 320, 180, fps).unwrap();
        let s = src.to_string_lossy().into_owned();
        run_ffmpeg(&["-i", &s, "-i", &s, "-map", "0:v", "-map", "1:a", "-af", "adelay=200:all=1", "-c:v", "copy", "-c:a", "aac", &shifted.to_string_lossy()]).unwrap();
        let m = analyze(&shifted, fps).unwrap();
        assert_eq!(m.flashes, 12, "{m:?}");
        assert_eq!(m.pairs, 12, "{m:?}");
        assert!((m.mean_offset_ms - 200.0).abs() < 5.0, "mean offset {} ms", m.mean_offset_ms);
        assert!((m.max_abs_offset_ms - 200.0).abs() < 5.0, "max offset {} ms", m.max_abs_offset_ms);
        assert!(m.offsets_frames_max > 4.5, "{} frames", m.offsets_frames_max);
        assert!(!m.within_one_frame());
        std::fs::remove_dir_all(&dir).ok();
    }
}
