//! Source decoding (`open_source`, `decode_still`) against real FFmpeg:
//! frame numbering, colour, size and orientation, allocations, stills and
//! errors (M3 plan, task 2). Skipped (with a message) if FFmpeg is missing.

use kadr_core::color::{AlphaMode, ColorInfo, Matrix, Primaries, Range, Transfer};
use kadr_core::FrameRate;
use kadr_media::ffmpeg::FfmpegCli;
use kadr_media::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---- frame-sized allocations on the calling thread ------------------------

/// Counts allocations of at least [`BIG`] bytes per thread, so a test can
/// prove that reading frames allocates no frame-sized buffers.
struct CountingAlloc;

const BIG: usize = 64 * 1024;

thread_local! {
    static BIG_ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn note(size: usize) {
    if size >= BIG {
        let _ = BIG_ALLOCS.try_with(|c| c.set(c.get() + 1));
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn big_allocs() -> u64 {
    BIG_ALLOCS.with(|c| c.get())
}

// ---- helpers -----------------------------------------------------------------

fn backend() -> Option<FfmpegCli> {
    match FfmpegCli::locate() {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-nostdin", "-v", "error", "-y"]).args(args).output().unwrap();
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn request(path: &Path, rate: FrameRate, start_frame: i64, width: u32, height: u32, color: ColorInfo) -> SourceRequest {
    SourceRequest { path: path.to_path_buf(), rate, start_frame, width, height, color, hwaccel: false }
}

fn video_color(matrix: Matrix, range: Range) -> ColorInfo {
    let primaries = if matrix == Matrix::Bt601 { Primaries::Bt601_525 } else { Primaries::Bt709 };
    ColorInfo { primaries, transfer: Transfer::Bt709, matrix, range, alpha: AlphaMode::Opaque }
}

fn px(buf: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * width as usize + x as usize) * 4;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

// ---- 1. frame numbering ------------------------------------------------------

const NUMBERED_FRAMES: i64 = 300;
const NUMBERED_SIZE: u32 = 64;

/// 300 frames of 64×64 grey whose three vertical stripes are the octal
/// digits of the frame number (luma 16 + 31·digit, limited range). Near-
/// lossless x264 with B-frames (negative DTS, reordering) and a GOP of 50,
/// so most seeks land mid-GOP.
fn numbered_clip(dir: &Path, rate: &str) -> PathBuf {
    let out = dir.join(format!("numbered_{}.mp4", rate.replace('/', "_")));
    let src = format!(
        "nullsrc=s=64x64:r={rate},format=yuv420p,\
         geq=lum='16+31*(mod(floor(N/64)\\,8)*lt(X\\,22)+mod(floor(N/8)\\,8)*between(X\\,22\\,43)+mod(N\\,8)*gt(X\\,43))':cb=128:cr=128"
    );
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &src,
        "-frames:v",
        &NUMBERED_FRAMES.to_string(),
        "-c:v",
        "libx264",
        "-preset",
        "medium",
        "-qp",
        "4",
        "-x264-params",
        "bframes=3:b-adapt=0:keyint=50:min-keyint=50:scenecut=0",
        "-color_range",
        "tv",
        "-colorspace",
        "bt709",
        path_str(&out),
    ]);
    out
}

/// Frame number read back from a decoded numbered frame. Full-range grey
/// `v = (Y − 16)·255/219`, so a digit step of 31 luma codes is 36.1 in RGB.
fn frame_number(buf: &[u8], width: u32) -> i64 {
    let digit = |x: u32| -> i64 {
        let [r, g, b, a] = px(buf, width, x * width / NUMBERED_SIZE, width / 2);
        assert!(r.abs_diff(g) <= 3 && g.abs_diff(b) <= 3 && a == 255, "neutral opaque grey expected, got {r} {g} {b} {a}");
        (r as f64 / (31.0 * 255.0 / 219.0)).round() as i64
    };
    digit(10) * 64 + digit(32) * 8 + digit(54)
}

fn read_numbers(ff: &FfmpegCli, clip: &Path, rate: FrameRate, start: i64, count: usize) -> (Vec<i64>, bool) {
    let mut s = ff.open_source(&request(clip, rate, start, NUMBERED_SIZE, NUMBERED_SIZE, video_color(Matrix::Bt709, Range::Limited))).unwrap();
    let mut buf = vec![0u8; (NUMBERED_SIZE * NUMBERED_SIZE * 4) as usize];
    let mut got = vec![];
    while got.len() < count {
        if !s.read_into(&mut buf).unwrap() {
            // The end is sticky.
            assert!(!s.read_into(&mut buf).unwrap(), "read after the end");
            return (got, true);
        }
        got.push(frame_number(&buf, NUMBERED_SIZE));
    }
    (got, false)
}

fn check_frame_exactness(rate_arg: &str, rate: FrameRate) {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = numbered_clip(dir.path(), rate_arg);

    // Every frame, in order, from the start to the end.
    let (all, ended) = read_numbers(&ff, &clip, rate, 0, NUMBERED_FRAMES as usize + 10);
    assert!(ended);
    assert_eq!(all, (0..NUMBERED_FRAMES).collect::<Vec<_>>(), "sequential read at {rate_arg}");

    // Starts at, after, in and just before GOP boundaries, and near the end.
    for start in [0, 1, 2, 3, 24, 25, 37, 48, 49, 50, 51, 99, 100, 101, 137, 199, 200, 249, 250, 251, 290, 295, 297, 299] {
        let (got, ended) = read_numbers(&ff, &clip, rate, start, 5);
        let want: Vec<i64> = (start..(start + 5).min(NUMBERED_FRAMES)).collect();
        assert_eq!(got, want, "start_frame {start} at {rate_arg}");
        assert_eq!(ended, start + 5 > NUMBERED_FRAMES, "end of stream from {start} at {rate_arg}");
    }

    // Past the end: an empty stream, not an error.
    for start in [NUMBERED_FRAMES, NUMBERED_FRAMES + 40] {
        let (got, ended) = read_numbers(&ff, &clip, rate, start, 5);
        assert!(got.is_empty() && ended, "start_frame {start} past the end gave {got:?}");
    }
}

#[test]
fn frames_are_numbered_exactly_at_25_fps() {
    check_frame_exactness("25", FrameRate::FPS_25);
}

#[test]
fn frames_are_numbered_exactly_at_29_97_fps() {
    check_frame_exactness("30000/1001", FrameRate::FPS_29_97);
}

#[test]
fn transport_stream_frames_are_numbered_exactly() {
    // MPEG-TS has no index: a seek lands on any packet near the target. The
    // name hides the format (like a `.part` download): it is sniffed.
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mp4 = numbered_clip(dir.path(), "30000/1001");
    let ts = dir.path().join("numbered.mp4.part");
    ffmpeg(&["-i", path_str(&mp4), "-c", "copy", "-f", "mpegts", path_str(&ts)]);
    for start in [0, 4, 30, 49, 50, 51, 120, 199, 295] {
        let (got, _) = read_numbers(&ff, &ts, FrameRate::FPS_29_97, start, 5);
        let want: Vec<i64> = (start..(start + 5).min(NUMBERED_FRAMES)).collect();
        assert_eq!(got, want, "start_frame {start} in MPEG-TS");
    }
}

#[test]
fn variable_frame_rate_seeks_agree_with_a_sequential_read() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    // 25 fps numbering with 4 of every 10 frames dropped (held gaps of 5
    // frames) and every third frame 13 ms late.
    let vfr = dir.path().join("vfr.mp4");
    let src = "nullsrc=s=64x64:r=25,format=yuv420p,\
               geq=lum='16+31*(mod(floor(N/64)\\,8)*lt(X\\,22)+mod(floor(N/8)\\,8)*between(X\\,22\\,43)+mod(N\\,8)*gt(X\\,43))':cb=128:cr=128,\
               select='not(between(mod(n\\,10)\\,3\\,6))',setpts='PTS+if(mod(N\\,3)\\,0.013/TB\\,0)'";
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        src,
        "-frames:v",
        "200",
        "-fps_mode",
        "vfr",
        "-c:v",
        "libx264",
        "-qp",
        "4",
        "-x264-params",
        "bframes=3:b-adapt=0:keyint=50:min-keyint=50:scenecut=0",
        path_str(&vfr),
    ]);
    let rate = FrameRate::FPS_25;
    let (seq, ended) = read_numbers(&ff, &vfr, rate, 0, 1000);
    assert!(ended && seq.len() > 300, "{} frames", seq.len());
    assert!(seq.windows(2).all(|w| w[0] <= w[1]), "sequential read goes forward");
    assert!(seq.windows(2).any(|w| w[0] == w[1]), "gaps hold the previous frame");

    for start in (0..seq.len() as i64).step_by(7).chain([1, 2, 3, 4, 5, 6]) {
        let (got, _) = read_numbers(&ff, &vfr, rate, start, 5);
        let want = &seq[start as usize..(start as usize + 5).min(seq.len())];
        assert_eq!(got, want, "start_frame {start}");
    }
}

// ---- 2. colour -----------------------------------------------------------------

const BAR_W: u32 = 32;
const BARS_H: u32 = 32;
/// 75 % bars (white, yellow, cyan, green, magenta, red, blue, black), then
/// 100 % white and mid grey.
const BARS: [[u8; 3]; 10] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
    [0, 0, 0],
    [255, 255, 255],
    [128, 128, 128],
];

fn bars_rgb24(dir: &Path) -> PathBuf {
    let w = BAR_W * BARS.len() as u32;
    let mut raw = Vec::with_capacity((w * BARS_H * 3) as usize);
    for _ in 0..BARS_H {
        for bar in BARS {
            for _ in 0..BAR_W {
                raw.extend_from_slice(&bar);
            }
        }
    }
    let path = dir.join("bars.rgb");
    std::fs::write(&path, raw).unwrap();
    path
}

/// The bars as H.264 4:4:4 (no chroma subsampling blur at bar edges) in
/// `matrix` / `range`; `tags` are the container colour tags (none: untagged).
fn encode_bars(dir: &Path, name: &str, matrix: &str, range: &str, tags: Option<(&str, &str)>) -> PathBuf {
    let raw = bars_rgb24(dir);
    let out = dir.join(name);
    let size = format!("{}x{BARS_H}", BAR_W * BARS.len() as u32);
    let vf = format!("scale=flags=bicubic+accurate_rnd+full_chroma_int:out_color_matrix={matrix}:out_range={range},format=yuv444p");
    let mut args: Vec<String> = ["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &size, "-r", "25", "-i", path_str(&raw), "-vf", &vf]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(["-c:v", "libx264", "-qp", "0"].map(String::from));
    if let Some((space, primaries)) = tags {
        args.extend(["-colorspace", space, "-color_primaries", primaries, "-color_trc", primaries, "-color_range", range].map(String::from));
    }
    args.push(path_str(&out).into());
    ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>());
    out
}

/// Decoded bar centres.
fn decode_bars(ff: &FfmpegCli, path: &Path, color: ColorInfo) -> Vec<[u8; 4]> {
    let w = BAR_W * BARS.len() as u32;
    let mut s = ff.open_source(&request(path, FrameRate::FPS_25, 0, w, BARS_H, color)).unwrap();
    let mut buf = vec![0u8; (w * BARS_H * 4) as usize];
    assert!(s.read_into(&mut buf).unwrap(), "{}: no frame", path.display());
    (0..BARS.len() as u32).map(|i| px(&buf, w, i * BAR_W + BAR_W / 2, BARS_H / 2)).collect()
}

/// Largest channel error of the decoded bars against the originals; alpha
/// must be opaque.
fn bars_error(decoded: &[[u8; 4]]) -> u8 {
    decoded
        .iter()
        .zip(BARS)
        .map(|(d, want)| {
            assert_eq!(d[3], 255, "opaque video decodes with alpha 255");
            (0..3).map(|c| d[c].abs_diff(want[c])).max().unwrap()
        })
        .max()
        .unwrap()
}

#[test]
fn rec709_and_rec601_bars_decode_to_the_original_rgb() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let rec709 = encode_bars(dir.path(), "709.mp4", "bt709", "tv", Some(("bt709", "bt709")));
    let rec601 = encode_bars(dir.path(), "601.mp4", "bt601", "tv", Some(("smpte170m", "smpte170m")));
    let full709 = encode_bars(dir.path(), "709full.mp4", "bt709", "pc", Some(("bt709", "bt709")));
    // Untagged: FFmpeg alone would assume BT.601 — the explicit matrix wins.
    let untagged709 = encode_bars(dir.path(), "709untagged.mp4", "bt709", "tv", None);

    for (path, color) in [
        (&rec709, video_color(Matrix::Bt709, Range::Limited)),
        (&rec601, video_color(Matrix::Bt601, Range::Limited)),
        (&full709, video_color(Matrix::Bt709, Range::Full)),
        (&untagged709, video_color(Matrix::Bt709, Range::Limited)),
    ] {
        let decoded = decode_bars(&ff, path, color);
        let err = bars_error(&decoded);
        assert!(err <= 2, "{}: max error {err}, decoded {decoded:?}", path.display());
    }

    // The matrix and range are really applied: the wrong ones are visibly off.
    let err = bars_error(&decode_bars(&ff, &rec601, video_color(Matrix::Bt709, Range::Limited)));
    assert!(err > 2, "Rec.601 bars decoded as Rec.709 should differ, max error {err}");
    let err = bars_error(&decode_bars(&ff, &rec709, video_color(Matrix::Bt709, Range::Full)));
    assert!(err > 2, "limited-range bars decoded as full range should differ, max error {err}");
}

// ---- 3. size and orientation --------------------------------------------------

fn is_red(p: [u8; 4]) -> bool {
    p[0] > 200 && p[1] < 40 && p[2] < 40
}
fn is_blue(p: [u8; 4]) -> bool {
    p[0] < 40 && p[1] < 40 && p[2] > 200
}

fn read_one(ff: &FfmpegCli, path: &Path, width: u32, height: u32) -> Vec<u8> {
    let mut s = ff.open_source(&request(path, FrameRate::FPS_25, 0, width, height, video_color(Matrix::Bt709, Range::Limited))).unwrap();
    let mut buf = vec![0u8; (width * height * 4) as usize];
    assert!(s.read_into(&mut buf).unwrap(), "{}: no frame", path.display());
    buf
}

/// 64×48, red on the left half and blue on the right, `extra` filters after.
fn half_red_clip(dir: &Path, name: &str, pattern: &str) -> PathBuf {
    let out = dir.join(name);
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        pattern,
        "-frames:v",
        "5",
        "-c:v",
        "libx264",
        "-qp",
        "0",
        "-pix_fmt",
        "yuv444p",
        path_str(&out),
    ]);
    out
}

#[test]
fn non_square_pixels_scale_to_the_exact_requested_size() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = half_red_clip(dir.path(), "sar.mp4", "color=c=blue:s=64x48:r=25,drawbox=x=0:y=0:w=32:h=48:c=red:t=fill,setsar=4/3");
    let info = ff.probe(&clip).unwrap();
    let v = info.video.unwrap();
    assert_eq!((v.width, v.height), (64, 48));

    // Display size 85⅓×48: the requested 85×48 is filled edge to edge.
    let (w, h) = (85, 48);
    let buf = read_one(&ff, &clip, w, h);
    assert_eq!(buf.len(), (w * h * 4) as usize);
    for (x, red) in [(2, true), (38, true), (47, false), (82, false)] {
        let p = px(&buf, w, x, h / 2);
        assert!(if red { is_red(p) } else { is_blue(p) }, "x {x}: {p:?}");
    }
}

#[test]
fn rotated_video_decodes_upright_at_the_display_size() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    // Stored 64×48: blue with a red 16×16 block in the top-left corner.
    let plain = half_red_clip(dir.path(), "plain.mp4", "color=c=blue:s=64x48:r=25,drawbox=x=0:y=0:w=16:h=16:c=red:t=fill");
    let rotated = dir.path().join("rotated.mp4");
    // Display matrix: rotate 90° counter-clockwise for display.
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y", "-display_rotation", "90", "-i", path_str(&plain), "-c", "copy", path_str(&rotated)])
        .output()
        .unwrap();
    if !st.status.success() {
        eprintln!("skipping rotation: this FFmpeg cannot write rotation metadata: {}", String::from_utf8_lossy(&st.stderr));
        return;
    }

    // Upright display is 48×64 and the red block, turned counter-clockwise,
    // sits in the bottom-left corner.
    let (w, h) = (48, 64);
    let buf = read_one(&ff, &rotated, w, h);
    assert!(is_red(px(&buf, w, 4, h - 5)), "bottom-left: {:?}", px(&buf, w, 4, h - 5));
    for (x, y) in [(4, 4), (w - 5, 4), (w - 5, h - 5)] {
        assert!(is_blue(px(&buf, w, x, y)), "({x}, {y}): {:?}", px(&buf, w, x, y));
    }
}

// ---- 4. no allocation per frame -------------------------------------------------

#[test]
fn reading_frames_allocates_no_frame_buffers() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = numbered_clip(dir.path(), "25");
    let (w, h) = (256, 256);
    let mut s = ff.open_source(&request(&clip, FrameRate::FPS_25, 10, w, h, video_color(Matrix::Bt709, Range::Limited))).unwrap();
    let mut buf = vec![0u8; (w * h * 4) as usize];
    assert!(s.read_into(&mut buf).unwrap());

    let (big, frames) = (big_allocs(), stats::frame_allocs_on_this_thread());
    for i in 0..50 {
        assert!(s.read_into(&mut buf).unwrap());
        assert_eq!(frame_number(&buf, w), 11 + i);
    }
    assert_eq!(big_allocs() - big, 0, "allocations of ≥ {BIG} bytes while reading 50 frames");
    assert_eq!(stats::frame_allocs_on_this_thread() - frames, 0);

    // A buffer of the wrong size is an error, not a panic or a short read.
    assert!(s.read_into(&mut [0u8; 16]).is_err());
}

// ---- 5. stills ------------------------------------------------------------------

#[test]
fn still_decodes_to_the_exact_size_with_straight_alpha() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("half_red.png");
    ffmpeg(&["-f", "lavfi", "-i", "color=c=red@0.5:s=32x32,format=rgba", "-frames:v", "1", path_str(&png)]);

    let (w, h) = (16, 12);
    let mut out = vec![0u8; (w * h * 4) as usize];
    ff.decode_still(&png, w, h, &mut out).unwrap();
    for p in out.as_chunks::<4>().0 {
        assert!(p[0] >= 253 && p[1] <= 2 && p[2] <= 2, "straight (not premultiplied) red: {p:?}");
        assert!(p[3].abs_diff(128) <= 1, "alpha ≈ 50 %: {p:?}");
    }

    assert!(ff.decode_still(&png, w, h, &mut [0u8; 8]).is_err(), "buffer size is checked");
}

// ---- 6. errors ------------------------------------------------------------------

#[test]
fn missing_and_unreadable_files_are_errors() {
    let Some(ff) = backend() else { return };
    let dir = tempfile::tempdir().unwrap();
    let color = video_color(Matrix::Bt709, Range::Limited);
    let mut buf = vec![0u8; 16 * 16 * 4];

    let missing = dir.path().join("missing.mp4");
    let opened = ff.open_source(&request(&missing, FrameRate::FPS_25, 0, 16, 16, color));
    assert!(opened.is_err() || opened.unwrap().read_into(&mut buf).is_err());
    assert!(ff.decode_still(&missing, 16, 16, &mut buf).is_err());

    let garbage = dir.path().join("garbage.mp4");
    std::fs::write(&garbage, b"this is not a video file at all, just some bytes").unwrap();
    let err = ff.open_source(&request(&garbage, FrameRate::FPS_25, 0, 16, 16, color)).and_then(|mut s| s.read_into(&mut buf));
    match err {
        Err(MediaError::ToolFailed { stderr, .. }) => assert!(!stderr.is_empty(), "FFmpeg's message is kept"),
        other => panic!("unreadable file: {other:?}"),
    }
    assert!(ff.decode_still(&garbage, 16, 16, &mut buf).is_err());
}

// ---- real footage (manual) ------------------------------------------------------

/// Every video in `$KADR_REAL_MEDIA`: frames reached by seeking equal the
/// same frames read sequentially from frame 0; every image decodes as a
/// still. Run with `KADR_REAL_MEDIA=<dir> cargo test -p kadr-media --test
/// source_decode -- --ignored --nocapture`.
#[test]
#[ignore = "needs KADR_REAL_MEDIA=<directory of real clips>"]
fn real_media_seeks_agree_with_sequential_reads() {
    let Some(ff) = backend() else { return };
    let Some(dir) = std::env::var_os("KADR_REAL_MEDIA") else {
        eprintln!("skipping: KADR_REAL_MEDIA is not set");
        return;
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_file()).collect();
    entries.sort();
    for path in entries {
        let Ok(info) = ff.probe(&path) else { continue };
        let Some(v) = info.video.as_ref() else { continue };
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (mut dw, mut dh) = ((v.width as f64 * v.sar.0 as f64 / v.sar.1 as f64).round() as u32, v.height);
        if v.rotation.rem_euclid(180) == 90 {
            (dw, dh) = (dh, dw);
        }
        let (w, h) = ((dw / 4).max(1), (dh / 4).max(1));
        let color = v.color.unwrap_or_else(|| ColorInfo::guess_video(v.width, v.height));
        if info.kind == kadr_core::MediaKind::Image {
            let mut out = vec![0u8; (w * h * 4) as usize];
            ff.decode_still(&path, w, h, &mut out).unwrap();
            let alphas: std::collections::BTreeSet<u8> = out.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
            eprintln!("{name}: still {w}×{h}, {} distinct alpha values, min {:?}", alphas.len(), alphas.first());
            continue;
        }
        let Some(rate) = v.frame_rate else { continue };
        let len = (w * h * 4) as usize;
        let read = |start: i64, count: usize| -> Vec<Vec<u8>> {
            let mut s = ff.open_source(&SourceRequest { path: path.clone(), rate, start_frame: start, width: w, height: h, color, hwaccel: false }).unwrap();
            let mut frames = vec![];
            let mut buf = vec![0u8; len];
            while frames.len() < count && s.read_into(&mut buf).unwrap() {
                frames.push(buf.clone());
            }
            frames
        };
        let t = std::time::Instant::now();
        let seq = read(0, 400);
        let seq_ms = t.elapsed().as_millis();
        let mut worst = 0.0f64;
        let mut mismatched = vec![];
        for start in (1..seq.len() as i64 - 3).step_by(23).chain([1, 2, 3]) {
            for (i, f) in read(start, 3).iter().enumerate() {
                let k = start as usize + i;
                let diff = f.iter().zip(&seq[k]).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / len as f64;
                worst = worst.max(diff);
                if diff > 0.5 {
                    // Which sequential frame did we get instead?
                    let near = (k.saturating_sub(4)..(k + 5).min(seq.len())).find(|&j| seq[j] == *f);
                    mismatched.push((k, near));
                }
            }
        }
        let c = &seq[seq.len() / 2];
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        eprintln!(
            "{name}: {}×{} rot {} sar {:?} → {w}×{h} at {}/{} ({:?} {:?}), {} frames read sequentially in {seq_ms} ms, worst seek/sequential diff {worst:.3}, centre px {:?}, mismatches {mismatched:?}",
            v.width, v.height, v.rotation, v.sar, rate.num, rate.den, color.matrix, color.range, seq.len(), &c[mid..mid + 4]
        );
        assert!(mismatched.is_empty(), "{name}: seek and sequential frames differ at {mismatched:?}");
    }
}
