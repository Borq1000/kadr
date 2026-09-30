//! Synthetic test media, generated once with FFmpeg (lavfi) and cached.
//! Long-GOP (2 s) like camera footage, tagged BT.709.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TestClip {
    pub name: &'static str,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub secs: u32,
}

struct Spec {
    name: &'static str,
    width: u32,
    height: u32,
    secs: u32,
    video_args: &'static [&'static str],
}

const X264: &[&str] = &["-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-pix_fmt", "yuv420p"];
const X265: &[&str] = &["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error", "-g", "60", "-pix_fmt", "yuv420p", "-tag:v", "hvc1"];

const SPECS: &[Spec] = &[
    Spec { name: "h264_1080p30", width: 1920, height: 1080, secs: 60, video_args: X264 },
    Spec { name: "h264_2160p30", width: 3840, height: 2160, secs: 20, video_args: X264 },
    Spec { name: "hevc_2160p30", width: 3840, height: 2160, secs: 20, video_args: X265 },
];

/// `<exe dir>/bench-media` (e.g. `target/release/bench-media`).
pub fn bench_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("bench-media"))).unwrap_or_else(|| PathBuf::from("bench-media"))
}

/// Generates the missing clips in `dir` and returns all of them.
pub fn ensure(dir: &Path) -> io::Result<Vec<TestClip>> {
    std::fs::create_dir_all(dir)?;
    SPECS
        .iter()
        .map(|s| {
            let path = dir.join(format!("{}.mp4", s.name));
            if !path.exists() {
                eprintln!("kadr-bench: generating {} …", path.display());
                generate(s, &path)?;
            }
            Ok(TestClip { name: s.name, path, width: s.width, height: s.height, secs: s.secs })
        })
        .collect()
}

fn generate(s: &Spec, path: &Path) -> io::Result<()> {
    let part = path.with_extension("part");
    let src = format!("testsrc2=size={}x{}:rate=30:duration={}", s.width, s.height, s.secs);
    let tone = format!("sine=frequency=440:sample_rate=48000:duration={}", s.secs);
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-f", "lavfi", "-i", &tone])
        .args(s.video_args)
        .args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-c:a", "aac", "-shortest", "-f", "mp4"])
        .arg(&part)
        .status()?;
    if !st.success() {
        return Err(io::Error::other(format!("ffmpeg failed to generate {}", s.name)));
    }
    std::fs::rename(&part, path)
}
