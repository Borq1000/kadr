//! Review focus 1 (D1, saved semantics): a project saved before E3 — its
//! transitions named by the old `kind` — opens and renders every frame of
//! every transition exactly as the E2 build did. The golden image was
//! rendered by the E2 evaluator (E3 Task 3, Step 2), before the model
//! changed; the migrated recipes must keep matching it byte for byte.

use kadr_core::color::AlphaMode;
use kadr_core::{AssetId, ColorInfo, CpuFrame, FrameRate, MediaInfo, MediaKind, Time, TimeRange, VideoInfo};
use kadr_playback::SceneSource;
use kadr_project::{io, Clip, MediaAsset, Project};
use kadr_project_scenes::ProjectScenes;
use kadr_render::{CpuRenderer, CpuTarget, LayerInput, MissingReason, PreparedFrame, RenderInputs, RenderTarget, Renderer};
use kadr_scene::{Layer, LayerContent, OutputSpec, RenderQuality, SizeU};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 36;

fn asset(name: &str) -> MediaAsset {
    let video = VideoInfo { width: W, height: H, frame_rate: Some(FrameRate::FPS_30), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None };
    MediaAsset::new(name, MediaInfo { kind: MediaKind::Video, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: Some(video), audio: None, timecode: None })
}

fn frame(px: impl Fn(u32, u32) -> [u8; 4]) -> Arc<CpuFrame> {
    let bytes = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).flat_map(|(x, y)| px(x, y)).collect();
    Arc::new(CpuFrame::from_rgba8(W, H, ColorInfo { alpha: AlphaMode::Opaque, ..ColorInfo::WORKING_SDR }, bytes))
}

/// A format-3 file as Kadr wrote it before E3: clips a (0..2 s) and b
/// (2..4 s) on V1 of a 64 × 36, 30 fps sequence and a 1 s transition of
/// `kind` at their cut — plus, for the dissolve, one at 1 s that sits on no
/// cut (the E2 evaluator skipped it; E3 drops it on load).
fn legacy_file(kind: &str) -> (Vec<u8>, HashMap<AssetId, Arc<CpuFrame>>) {
    let mut p = Project::new("legacy");
    let seq = p.sequence_mut();
    (seq.frame_rate, seq.width, seq.height) = (FrameRate::FPS_30, W, H);
    let (a, b) = (asset("a.mp4"), asset("b.mp4"));
    let ca = Clip::new(a.id, "a", TimeRange::new(Time::from_secs(10), Time::from_secs(12)), Time::ZERO);
    let cb = Clip::new(b.id, "b", TimeRange::new(Time::from_secs(20), Time::from_secs(22)), Time::from_secs(2));
    p.sequence_mut().tracks[0].clips.extend([ca, cb]);
    let frames = HashMap::from([(a.id, frame(|x, y| [(x * 4) as u8, (y * 7) as u8, 128, 255])), (b.id, frame(|x, y| [(255 - x * 3) as u8, 90, (y * 6) as u8, 255]))]);
    p.assets.extend([a, b]);
    let mut v = serde_json::to_value(&p).unwrap();
    let track = v["sequences"][0]["tracks"][0]["id"].clone();
    let f = |ms: i64| Time::from_millis(ms).flicks();
    let mut list = vec![serde_json::json!({"id": kadr_core::TransitionId::new(), "kind": kind, "track": track, "at": f(2_000), "duration": f(1_000)})];
    if kind == "cross_dissolve" {
        list.push(serde_json::json!({"id": kadr_core::TransitionId::new(), "kind": kind, "track": track, "at": f(1_000), "duration": f(400)}));
    }
    v["sequences"][0]["transitions"] = serde_json::Value::Array(list);
    v["format_version"] = serde_json::json!(3);
    (serde_json::to_vec(&v).unwrap(), frames)
}

fn inputs(layers: &[Layer], frames: &HashMap<AssetId, Arc<CpuFrame>>) -> Vec<LayerInput> {
    layers
        .iter()
        .map(|l| match &l.content {
            LayerContent::Media { media, .. } => frames.get(&media.media).map_or(LayerInput::Missing(MissingReason::Offline), |f| LayerInput::Cpu(f.clone())),
            LayerContent::Transition(t) => LayerInput::Transition { from: inputs(&t.from, frames), to: inputs(&t.to, frames) },
            _ => LayerInput::None,
        })
        .collect()
}

/// The three kinds (rows) at five frames of the window [1.5 s, 2.5 s) (columns).
fn strip() -> (u32, u32, Vec<u8>) {
    let at = [45, 54, 60, 67, 74];
    let (sw, sh) = (W * at.len() as u32, H * 3);
    let mut out = vec![0u8; (sw * sh * 4) as usize];
    for (row, kind) in ["cross_dissolve", "dip_to_black", "wipe"].iter().enumerate() {
        let (bytes, frames) = legacy_file(kind);
        let p = io::parse(&bytes, Path::new("legacy.kadr")).expect("a pre-E3 file opens");
        let scenes = ProjectScenes::new(&p, false);
        for (col, n) in at.iter().enumerate() {
            let scene = scenes.scene_at(FrameRate::FPS_30.frame_to_time(*n), &OutputSpec::new(SizeU::new(W, H), RenderQuality::Export));
            let ins = RenderInputs { layers: inputs(&scene.layers, &frames) };
            let mut buf = vec![0u8; (W * H * 4) as usize];
            CpuRenderer::new().render(&PreparedFrame { scene: &scene, inputs: &ins }, &mut RenderTarget::Cpu(CpuTarget::packed(W, H, &mut buf))).unwrap();
            for y in 0..H {
                let at = (((row as u32 * H + y) * sw + col as u32 * W) * 4) as usize;
                out[at..at + (W * 4) as usize].copy_from_slice(&buf[(y * W * 4) as usize..((y + 1) * W * 4) as usize]);
            }
        }
    }
    (sw, sh, out)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join("legacy_transitions.png")
}

fn read_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::fs::File::open(path).ok()?).read_info().expect("golden PNG header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("golden PNG data");
    buf.truncate(info.buffer_size());
    Some((info.width, info.height, buf))
}

fn write_png(path: &Path, w: u32, h: u32, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path).unwrap()), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(data).unwrap();
}

#[test]
fn legacy_transitions_render_as_before() {
    let (w, h, out) = strip();
    let path = golden_path();
    if std::env::var_os("KADR_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        write_png(&path, w, h, &out);
        return;
    }
    let (gw, gh, golden) = read_png(&path).expect("no golden: it is blessed once, at the E2 evaluator (E3 Task 3, Step 2)");
    assert_eq!((gw, gh), (w, h));
    let diff = out.iter().zip(&golden).filter(|(a, b)| a != b).count();
    assert_eq!(diff, 0, "{diff} bytes differ from the E2 picture");
}
