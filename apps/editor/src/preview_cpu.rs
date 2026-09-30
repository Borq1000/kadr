//! The preview on the new pipeline (render foundation M4): project snapshot
//! (`ProjectScenes`) → `PreviewPlayer` (resolver + `CpuRenderer`) → a Slint
//! sink that renders straight into the buffer the UI displays.
//!
//! - The sink hands the renderer a `SharedPixelBuffer` from a ring of four
//!   on the player thread; the UI thread only wraps it in an `Image` (no
//!   pixel copy there). A buffer is reused only when Slint no longer holds
//!   it: `make_mut_bytes` would copy a shared one, and that copy is detected
//!   (pointer before/after) and counted as an allocation and a copy.
//! - Play pre-roll: the player renders the first frame with the audio
//!   stopped and reports `ready`; the audio starts then, or after
//!   [`PREROLL_FALLBACK`] at the latest.

use crate::app::post;
use crate::scene_source::ProjectScenes;
use kadr_audio::{AudioClock, MixSource};
use kadr_core::perf::PerfRing;
use kadr_core::Time;
use kadr_media::MediaBackend;
use kadr_playback::{Clock, FfmpegDecoders, FrameInfo, FrameSink, PlayerConfig, PreviewPlayer, Resolver, ResolverConfig};
use kadr_render::{CpuRenderer, CpuTarget};
use kadr_scene::{OutputSpec, SizeU};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The app starts the audio at most this long after `play` even if the
/// first frame is not ready (the player gives up waiting a bit earlier).
pub const PREROLL_FALLBACK: Duration = Duration::from_millis(250);
const PLAYER_PREROLL: Duration = Duration::from_millis(200);

/// The audio engine's clock as the player's master clock.
struct AudioMaster(AudioClock);

impl Clock for AudioMaster {
    fn now(&self) -> Option<Time> {
        self.0.now()
    }
}

/// A frame handed from the player thread to the UI thread.
pub struct PlayerFrame {
    pub generation: u64,
    /// Presentation order (posts may be retried out of order when the app is busy).
    pub seq: u64,
    pub buf: SharedPixelBuffer<Rgba8Pixel>,
    pub posted: Instant,
}

/// Ring of display buffers the renderer draws into (player thread).
pub struct SlintSink {
    ring: Vec<Option<SharedPixelBuffer<Rgba8Pixel>>>,
    /// The slot handed out by the last `target`.
    drawing: Option<usize>,
    /// The last two presented slots: on screen, or posted and about to be.
    recent: [Option<usize>; 2],
    /// Per slot, the `seq` it was last presented with (0 = never).
    presented_at: Vec<u64>,
    /// Allocations and copies the last `target` made.
    allocs: u32,
    copies: u32,
    copied_bytes: u64,
    seq: u64,
    deliver: Box<dyn Fn(PlayerFrame) + Send>,
    ready: Box<dyn Fn(u64) + Send>,
}

impl SlintSink {
    pub fn new(slots: usize, deliver: Box<dyn Fn(PlayerFrame) + Send>, ready: Box<dyn Fn(u64) + Send>) -> Self {
        let n = slots.max(3);
        SlintSink { ring: vec![None; n], drawing: None, recent: [None; 2], presented_at: vec![0; n], allocs: 0, copies: 0, copied_bytes: 0, seq: 0, deliver, ready }
    }

    /// The least recently presented slot that is neither on screen nor just
    /// posted: the one the display most likely let go of.
    fn pick(&self) -> usize {
        (0..self.ring.len()).filter(|i| !self.recent.contains(&Some(*i))).min_by_key(|&i| self.presented_at[i]).unwrap_or(0)
    }
}

impl FrameSink for SlintSink {
    fn target(&mut self, size: SizeU) -> Option<CpuTarget<'_>> {
        if size.w == 0 || size.h == 0 {
            return None;
        }
        // A slot handed out but never presented (dropped frame) is reused first.
        let i = self.drawing.unwrap_or_else(|| self.pick());
        self.drawing = Some(i);
        let (mut allocs, mut copies, mut bytes) = (0, 0, 0);
        let slot = &mut self.ring[i];
        match slot {
            Some(b) if b.width() == size.w && b.height() == size.h => {
                let before = b.as_bytes().as_ptr();
                let after = b.make_mut_bytes().as_ptr();
                if before != after {
                    // Slint still held it: make_mut copied the old picture into a new buffer.
                    allocs += 1;
                    copies += 1;
                    bytes += b.as_bytes().len() as u64;
                }
            }
            _ => {
                *slot = Some(SharedPixelBuffer::new(size.w, size.h));
                allocs += 1;
            }
        }
        (self.allocs, self.copies, self.copied_bytes) = (self.allocs + allocs, self.copies + copies, self.copied_bytes + bytes);
        let b = slot.as_mut()?;
        Some(CpuTarget::packed(size.w, size.h, b.make_mut_bytes()))
    }

    fn present(&mut self, info: &mut FrameInfo) {
        let Some(i) = self.drawing.take() else { return };
        let Some(buf) = self.ring[i].clone() else { return };
        info.perf.frame_allocs += std::mem::take(&mut self.allocs);
        info.perf.frame_copies += std::mem::take(&mut self.copies);
        info.perf.bytes_copied += std::mem::take(&mut self.copied_bytes);
        self.recent = [Some(i), self.recent[0]];
        self.seq += 1;
        self.presented_at[i] = self.seq;
        (self.deliver)(PlayerFrame { generation: info.generation, seq: self.seq, buf, posted: Instant::now() });
    }

    fn ready(&mut self, generation: u64) {
        (self.ready)(generation);
    }
}

/// A play waiting for its first frame before the audio starts.
pub struct PendingPlay {
    pub generation: u64,
    pub from: Time,
    pub sources: Vec<MixSource>,
}

pub struct CpuPreview {
    player: PreviewPlayer,
    resolver: Arc<Resolver>,
    pub scenes: Option<Arc<ProjectScenes>>,
    /// The project changed since `scenes` was taken.
    pub dirty: bool,
    pub output: Option<OutputSpec>,
    pub pending: Option<PendingPlay>,
    /// Newest frame shown (see [`PlayerFrame::seq`]).
    pub shown_seq: u64,
}

impl CpuPreview {
    pub fn new(media: Arc<dyn MediaBackend>, clock: AudioClock, perf: Arc<PerfRing>) -> Self {
        let resolver = Arc::new(Resolver::new(Arc::new(FfmpegDecoders::new(media)), ResolverConfig::default()));
        let sink = SlintSink::new(
            4,
            Box::new(|f| post(move |app| app.on_player_frame(f))),
            Box::new(|g| post(move |app| app.start_pending_audio(g))),
        );
        let config = PlayerConfig { preroll: PLAYER_PREROLL, ..Default::default() };
        let output = OutputSpec::new(SizeU::new(2, 2), kadr_scene::RenderQuality::PreviewFast);
        let player = PreviewPlayer::new(resolver.clone(), Box::new(CpuRenderer::new()), Arc::new(AudioMaster(clock)), Box::new(sink), output, perf, config);
        CpuPreview { player, resolver, scenes: None, dirty: true, output: None, pending: None, shown_seq: 0 }
    }

    pub fn player(&self) -> &PreviewPlayer {
        &self.player
    }

    /// Makes `scenes` current. Media whose file moved (relink) or came back
    /// online forget their cached frames and failures.
    pub fn set_scenes(&mut self, scenes: Arc<ProjectScenes>, also: Option<&Resolver>) {
        if let Some(old) = &self.scenes {
            for (id, m) in scenes.media_sources() {
                if let Some(o) = old.media_sources().get(id)
                    && (o.path != m.path || (!o.online && m.online))
                {
                    self.resolver.invalidate_media(*id);
                    if let Some(r) = also {
                        r.invalidate_media(*id);
                    }
                }
            }
        }
        self.player.set_source(scenes.clone());
        self.scenes = Some(scenes);
        self.dirty = false;
    }

    pub fn set_output(&mut self, out: OutputSpec) {
        if self.output != Some(out) {
            self.player.set_output(out);
            self.output = Some(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::perf::FramePerf;
    use std::sync::mpsc;

    fn sink() -> (SlintSink, mpsc::Receiver<PlayerFrame>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        (SlintSink::new(3, Box::new(move |f| tx.lock().unwrap().send(f).unwrap()), Box::new(|_| {})), rx)
    }

    fn info(g: u64) -> FrameInfo {
        FrameInfo { generation: g, time: Time::ZERO, playing: false, latest: true, perf: FramePerf::default() }
    }

    fn draw(s: &mut SlintSink, v: u8) -> FrameInfo {
        let t = s.target(SizeU::new(4, 2)).unwrap();
        t.data.fill(v);
        let mut i = info(1);
        s.present(&mut i);
        i
    }

    #[test]
    fn buffers_are_reused_without_copies_once_the_display_let_go() {
        let (mut s, rx) = sink();
        let mut firsts = vec![];
        for v in 0..3 {
            firsts.push(draw(&mut s, v).perf.frame_allocs);
            drop(rx.recv().unwrap()); // the UI dropped it (next frame replaced it)
        }
        assert_eq!(firsts, [1, 1, 1], "three slots allocated once each");
        for v in 3..30 {
            let i = draw(&mut s, v);
            assert_eq!((i.perf.frame_allocs, i.perf.frame_copies), (0, 0), "frame {v}");
            let f = rx.recv().unwrap();
            assert!(f.buf.as_bytes().iter().all(|b| *b == v), "the frame shows what was drawn");
            assert_eq!(f.seq, v as u64 + 1);
        }
    }

    #[test]
    fn the_buffer_on_screen_is_never_drawn_into_and_a_held_one_is_counted() {
        let (mut s, rx) = sink();
        // The UI keeps every frame (a slow display): the ring must not overwrite them.
        let mut held = vec![];
        for v in 0..6 {
            let i = draw(&mut s, v);
            let f = rx.recv().unwrap();
            if v >= 3 {
                assert_eq!((i.perf.frame_allocs, i.perf.frame_copies), (1, 1), "reusing a held buffer copies, and is counted");
            }
            held.push(f);
        }
        for (v, f) in held.iter().enumerate() {
            assert!(f.buf.as_bytes().iter().all(|b| *b == v as u8), "frame {v} was not overwritten while shown");
        }
    }

    #[test]
    fn a_target_not_presented_is_reused_and_a_new_size_allocates() {
        let (mut s, rx) = sink();
        draw(&mut s, 1);
        drop(rx.recv().unwrap());
        let _ = s.target(SizeU::new(4, 2)).unwrap(); // dropped frame: never presented
        let i = draw(&mut s, 2);
        assert_eq!(i.perf.frame_allocs, 1, "the dropped frame's slot is reused; its allocation is reported with the next presented frame");
        drop(rx.recv().unwrap());
        let t = s.target(SizeU::new(8, 4)).unwrap();
        assert_eq!(t.data.len(), 8 * 4 * 4);
        assert!(s.target(SizeU::new(0, 4)).is_none());
    }
}
