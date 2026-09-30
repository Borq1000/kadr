//! What the playback layer needs from the application: scenes and the media
//! they reference (render spec §4.2). Implemented over the project by the
//! editor (M4) and by `kadr-bench`; tests build scenes by hand.

use kadr_core::color::AlphaMode;
use kadr_core::{AssetId, ColorInfo, FrameRate, Time, FLICKS_PER_SECOND};
use kadr_scene::{FrameScene, OutputSpec, SizeU, SourceKind};
use std::path::PathBuf;

/// A decodable source as the resolver sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaSource {
    pub path: PathBuf,
    pub kind: SourceKind,
    /// The source's own constant rate; source frame `k` is shown at `rate.frame_to_time(k)`.
    pub rate: FrameRate,
    /// `Time::ZERO` (or less) = unknown: no upper clamp, the stream's end decides.
    pub duration: Time,
    /// Size as displayed (sample aspect ratio and rotation applied); decode sizes are fractions of it.
    pub display_size: SizeU,
    pub color: ColorInfo,
    /// False when the file is missing: the layer becomes `Missing(Offline)` without touching a decoder.
    pub online: bool,
}

impl MediaSource {
    fn rate_ok(&self) -> bool {
        self.rate.num > 0 && self.rate.den > 0
    }

    /// Index of the last frame that starts before `duration`; `None` when
    /// the duration is unknown. Exact: `frame_to_time(k) < duration` ⇔
    /// `k·den·F < duration·num`, so `last = ⌊(duration·num − 1) / (den·F)⌋`.
    pub fn last_frame(&self) -> Option<i64> {
        if self.kind == SourceKind::Image {
            return Some(0);
        }
        if self.duration.flicks() <= 0 || !self.rate_ok() {
            return None;
        }
        let n = self.duration.flicks() as i128 * self.rate.num as i128 - 1;
        Some((n / (FLICKS_PER_SECOND as i128 * self.rate.den as i128)) as i64)
    }

    /// The source frame shown at `source_time`: the frame containing it,
    /// clamped to `[0, last]` (at or after the end → the last frame).
    pub fn frame_at(&self, source_time: Time) -> i64 {
        if self.kind == SourceKind::Image || !self.rate_ok() {
            return 0;
        }
        let f = self.rate.time_to_frame(source_time).max(0);
        match self.last_frame() {
            Some(last) => f.min(last),
            None => f,
        }
    }

    /// Colour of the frames the decoder delivers: the working space (full
    /// range non-linear R'G'B'), alpha opaque for opaque sources and
    /// straight otherwise (FFmpeg's RGBA is never premultiplied).
    pub fn frame_color(&self) -> ColorInfo {
        let alpha = if self.color.alpha == AlphaMode::Opaque { AlphaMode::Opaque } else { AlphaMode::Straight };
        ColorInfo { alpha, ..ColorInfo::WORKING_SDR }
    }

    /// Same file decoded the same way (a relink or re-probe changes this).
    pub(crate) fn same_decode(&self, o: &MediaSource) -> bool {
        self.path == o.path && self.kind == o.kind && self.rate == o.rate && self.color == o.color && self.display_size == o.display_size
    }
}

/// Scenes and media for playback. Called on the player thread (and by
/// whoever calls the resolver), so implementations are usually an immutable
/// snapshot of the project behind an `Arc`, replaced with
/// `PreviewPlayer::set_source` after an edit.
pub trait SceneSource: Send + Sync {
    /// The evaluated scene at timeline time `t` for output `out`.
    fn scene_at(&self, t: Time, out: &OutputSpec) -> FrameScene;
    /// The media behind `MediaRef::media`; `None` = unknown (treated as offline).
    fn media(&self, id: AssetId) -> Option<MediaSource>;
    /// Playback stops at the first frame starting at or after this.
    fn duration(&self) -> Time;
    /// Timeline rate: playback shows frame `n` at `from + frame_rate.frame_to_time(n)`.
    fn frame_rate(&self) -> FrameRate;
    fn canvas(&self) -> SizeU;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(rate: FrameRate, duration: Time) -> MediaSource {
        MediaSource {
            path: "x.mp4".into(),
            kind: SourceKind::Video,
            rate,
            duration,
            display_size: SizeU::new(64, 36),
            color: ColorInfo::guess_video(64, 36),
            online: true,
        }
    }

    #[test]
    fn frame_index_is_exact_and_clamped_to_the_last_frame() {
        let m = video(FrameRate::FPS_25, Time::from_secs(4)); // frames 0..=99
        assert_eq!(m.last_frame(), Some(99));
        assert_eq!(m.frame_at(Time::from_millis(-5)), 0);
        assert_eq!(m.frame_at(Time::from_millis(39)), 0);
        assert_eq!(m.frame_at(Time::from_millis(40)), 1);
        assert_eq!(m.frame_at(Time::from_millis(3999)), 99);
        assert_eq!(m.frame_at(Time::from_secs(4)), 99, "the end itself is past the last frame");
        assert_eq!(m.frame_at(Time::from_secs(100)), 99);
    }

    #[test]
    fn last_frame_for_ntsc_rates_and_partial_frames() {
        // 1001 frames of 30000/1001 last exactly 33.3667 s.
        let r = FrameRate::FPS_29_97;
        let exact = video(r, r.frame_to_time(1001));
        assert_eq!(exact.last_frame(), Some(1000));
        let partial = video(r, r.frame_to_time(1001) + Time(1));
        assert_eq!(partial.last_frame(), Some(1001), "a frame starting before the end counts");
        assert_eq!(video(r, Time::ZERO).last_frame(), None, "unknown duration: no clamp");
        assert_eq!(video(r, Time::ZERO).frame_at(Time::from_secs(1000)), 29970);
    }

    #[test]
    fn frame_colour_is_the_working_space_with_the_source_alpha() {
        let v = video(FrameRate::FPS_25, Time::from_secs(1));
        assert_eq!(v.frame_color().alpha, AlphaMode::Opaque);
        assert_eq!(v.frame_color().matrix, ColorInfo::WORKING_SDR.matrix);
        let img = MediaSource { kind: SourceKind::Image, color: ColorInfo::IMAGE_SRGB, ..v };
        assert_eq!(img.frame_color().alpha, AlphaMode::Straight);
        assert_eq!(img.frame_at(Time::from_secs(3)), 0);
    }
}
