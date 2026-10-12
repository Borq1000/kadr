//! The sequence format from the first video clip (TX-R12): while the
//! sequence holds no video clip, the first video placed on it gives it its
//! size and rate — titles, stills and sounds don't count, and a still never
//! sets it. Titles already on the timeline follow in the same step: their
//! in / out snap to the new rate (at least a frame long), their x / y scale
//! per axis and their scale by the shorter side, keyframes too. The format
//! is adopted before the placement time is snapped, so the placed clip lands
//! on the new rate's grid.

use crate::app::App;
use kadr_core::{MediaKind, Time, TimeRange, VideoInfo};
use kadr_i18n::tf;
use kadr_project::{AnimParam, MediaAsset, Project, Sequence, TrackKind};

/// The format placing `asset` now gives the sequence: its picture's when it
/// is a video and no video clip lies on a video track yet (titles, stills
/// and sounds don't count); `None` otherwise — a still never sets it.
pub fn first_media_format<'a>(project: &Project, asset: &'a MediaAsset) -> Option<&'a VideoInfo> {
    let has_video = project
        .sequence()
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| &t.clips)
        .any(|c| !c.is_text() && project.asset(c.asset).is_some_and(|a| a.kind() == MediaKind::Video));
    asset.info.video.as_ref().filter(|_| asset.kind() == MediaKind::Video && !has_video)
}

/// The sequence takes `v`'s size (even, at least 2) and its rate when
/// declared; the titles on it follow ([`fit_titles`]). Returns how many
/// titles there are.
pub fn adopt_format(seq: &mut Sequence, v: &VideoInfo) -> usize {
    let (ow, oh) = (seq.width, seq.height);
    seq.width = v.width.max(2) & !1;
    seq.height = v.height.max(2) & !1;
    if let Some(fr) = v.frame_rate {
        seq.frame_rate = fr;
    }
    fit_titles(seq, ow, oh)
}

/// Titles follow the format's change from `ow × oh` to the sequence's own:
/// in / out on the sequence's frame grid (at least a frame long; a track
/// where that would make clips overlap keeps its titles' times), x / y
/// scaled per axis, scale by the shorter side — static values and
/// keyframes. Returns how many titles there are.
fn fit_titles(seq: &mut Sequence, ow: u32, oh: u32) -> usize {
    let (sx, sy) = (seq.width as f64 / ow.max(1) as f64, seq.height as f64 / oh.max(1) as f64);
    let ss = seq.width.min(seq.height) as f64 / ow.min(oh).max(1) as f64;
    let (fr, frame) = (seq.frame_rate, seq.frame_rate.frame_duration());
    let mut titles = 0;
    for track in &mut seq.tracks {
        let times: Vec<TimeRange> = track
            .clips
            .iter()
            .map(|c| {
                if !c.is_text() {
                    return c.timeline_range();
                }
                let start = fr.snap(c.timeline_in).max(Time::ZERO);
                TimeRange::new(start, fr.snap(c.timeline_out).max(start + frame))
            })
            .collect();
        let retime = times.windows(2).all(|w| w[0].end <= w[1].start);
        for (c, r) in track.clips.iter_mut().zip(times) {
            if !c.is_text() {
                continue;
            }
            titles += 1;
            if retime {
                c.move_to(r.start);
                c.timeline_out = r.end;
                c.source_out = c.source_in + (r.end - r.start);
            }
            c.transform.x *= sx;
            c.transform.y *= sy;
            c.transform.scale *= ss;
            for (p, k) in [(AnimParam::X, sx), (AnimParam::Y, sy), (AnimParam::Scale, ss)] {
                for key in c.animation.track(p).to_vec() {
                    c.animation.set_key(p, key.at, key.value * k, None);
                }
            }
        }
    }
    titles
}

impl App {
    /// Before placing `asset` (shown to the user as `name`): when it is the
    /// first video, the sequence takes its format now ([`adopt_format`]).
    /// Returns the toast to show once the placement succeeds when titles
    /// followed a format that changed. (Like the format itself, the titles'
    /// change is not an undo step.)
    pub fn adopt_first_format(&mut self, asset: &MediaAsset, name: &str) -> Option<String> {
        let v = first_media_format(&self.project, asset)?.clone();
        let seq = self.project.sequence();
        let before = (seq.width, seq.height, seq.frame_rate);
        let titles = adopt_format(self.project.sequence_mut(), &v);
        self.meta_dirty = true;
        let seq = self.project.sequence();
        let changed = (seq.width, seq.height, seq.frame_rate) != before;
        (changed && titles > 0).then(|| {
            tf("toast.seq_format_from", &[("w", &seq.width.to_string()), ("h", &seq.height.to_string()), ("fps", &seq.frame_rate.to_string()), ("clip", name)])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{ClipId, FrameRate, MediaInfo};
    use kadr_project::{Clip, Ease, SourceTime, TextDoc, Track};

    fn video(w: u32, h: u32, fr: FrameRate) -> VideoInfo {
        VideoInfo { width: w, height: h, frame_rate: Some(fr), variable_frame_rate: false, codec: "h264".into(), pixel_format: "yuv420p".into(), rotation: 0, sar: (1, 1), color: None }
    }

    fn asset(kind: MediaKind, v: Option<VideoInfo>) -> MediaAsset {
        MediaAsset::new("m", MediaInfo { kind, duration: Time::from_secs(60), container: "mp4".into(), size_bytes: 0, video: v, audio: None, timecode: None })
    }

    /// `a` on the first track of `kind`, 5 s from `at` s.
    fn place(p: &mut Project, a: &MediaAsset, kind: TrackKind, at: i64) {
        p.assets.push(a.clone());
        let ti = p.sequence().tracks.iter().position(|t| t.kind == kind).unwrap();
        let c = Clip::new(a.id, "c", TimeRange::new(Time::ZERO, Time::from_secs(5)), Time::from_secs(at));
        let clips = &mut p.sequence_mut().tracks[ti].clips;
        clips.push(c);
        clips.sort_by_key(|c| c.timeline_in);
    }

    /// A title over `at..out` on the topmost video track.
    fn title(p: &mut Project, at: Time, out: Time) -> ClipId {
        let c = Clip::new_text(TextDoc::new("Гол!"), at, out - at);
        let id = c.id;
        let ti = p.sequence().tracks.iter().rposition(|t| t.kind == TrackKind::Video).unwrap();
        let clips = &mut p.sequence_mut().tracks[ti].clips;
        clips.push(c);
        clips.sort_by_key(|c| c.timeline_in);
        id
    }

    #[test]
    fn the_first_video_sets_the_format_and_nothing_else_does() {
        let mut p = Project::new("t");
        let phone = asset(MediaKind::Video, Some(video(1080, 1920, FrameRate::FPS_30)));
        let still = asset(MediaKind::Image, Some(video(4000, 3000, FrameRate::FPS_25)));
        let sound = asset(MediaKind::Audio, None);
        title(&mut p, Time::ZERO, Time::from_secs(5));
        assert_eq!(first_media_format(&p, &phone), phone.info.video.as_ref(), "titles only: adopt");
        place(&mut p, &sound, TrackKind::Audio, 0);
        assert_eq!(first_media_format(&p, &phone), phone.info.video.as_ref(), "a sound placed: adopt");
        assert_eq!(first_media_format(&p, &still), None, "a still never sets the format");
        place(&mut p, &still, TrackKind::Video, 10);
        assert_eq!(first_media_format(&p, &phone), phone.info.video.as_ref(), "a still placed: adopt");
        assert_eq!(first_media_format(&p, &sound), None, "a sound has no picture");
        place(&mut p, &phone, TrackKind::Video, 20);
        assert_eq!(first_media_format(&p, &phone), None, "a video already placed");
    }

    /// TX-R12: titles on a 1920×1080 · 25 sequence follow it to 1080×1920 ·
    /// 30 — on the new grid, at least a frame long, x / y per axis, scale
    /// by the shorter side (1080 → 1080: unchanged), keyframes too.
    #[test]
    fn titles_follow_the_adopted_format() {
        let mut p = Project::new("t");
        (p.sequence_mut().width, p.sequence_mut().height, p.sequence_mut().frame_rate) = (1920, 1080, FrameRate::FPS_25);
        let a = title(&mut p, Time::from_millis(1_040), Time::from_millis(3_960));
        let b = title(&mut p, Time::from_millis(10_000), Time::from_millis(10_010));
        {
            let (ti, ci) = p.sequence().locate_clip(a).unwrap();
            let c = &mut p.sequence_mut().tracks[ti].clips[ci];
            (c.transform.x, c.transform.y, c.transform.scale) = (480.0, -270.0, 1.5);
            c.animation.set_key(AnimParam::X, SourceTime(Time::ZERO), 960.0, Some(Ease::Linear));
            c.animation.set_key(AnimParam::Scale, SourceTime(Time::from_secs(1)), 2.0, None);
        }
        assert_eq!(adopt_format(p.sequence_mut(), &video(1080, 1920, FrameRate::FPS_30)), 2);
        let seq = p.sequence();
        assert_eq!((seq.width, seq.height, seq.frame_rate), (1080, 1920, FrameRate::FPS_30));
        let fr = FrameRate::FPS_30;
        let ca = seq.clip(a).unwrap();
        assert_eq!((ca.timeline_in, ca.timeline_out), (fr.snap(Time::from_millis(1_040)), fr.snap(Time::from_millis(3_960))), "on the 30 fps grid");
        assert_ne!(ca.timeline_in, Time::from_millis(1_040), "it moved onto the grid");
        assert_eq!(ca.source_out - ca.source_in, ca.duration(), "speed 1");
        assert_eq!((ca.transform.x, ca.transform.y, ca.transform.scale), (480.0 * 1080.0 / 1920.0, -270.0 * 1920.0 / 1080.0, 1.5));
        assert_eq!(ca.animation.track(AnimParam::X)[0].value, 960.0 * 1080.0 / 1920.0);
        assert_eq!(ca.animation.track(AnimParam::X)[0].ease, Ease::Linear, "the ease stays");
        assert_eq!(ca.animation.track(AnimParam::Scale)[0].value, 2.0);
        let cb = seq.clip(b).unwrap();
        assert_eq!(cb.duration(), fr.frame_duration(), "a sliver becomes one frame: {:?}", cb.timeline_range());
        assert!(seq.tracks.iter().all(Track::is_sorted_and_disjoint));
    }

    /// The shorter side scales a title: 1080p → 2160p doubles it.
    #[test]
    fn a_bigger_frame_scales_titles_by_the_shorter_side() {
        let mut p = Project::new("t");
        (p.sequence_mut().width, p.sequence_mut().height) = (1920, 1080);
        let a = title(&mut p, Time::ZERO, Time::from_secs(5));
        adopt_format(p.sequence_mut(), &video(3840, 2160, FrameRate::FPS_25));
        assert_eq!(p.sequence().clip(a).unwrap().transform.scale, 2.0);
    }

    /// Titles that would overlap on the new grid keep their times rather
    /// than break the track (and still follow the frame's size).
    #[test]
    fn titles_too_close_for_the_new_grid_keep_their_times() {
        let mut p = Project::new("t");
        (p.sequence_mut().width, p.sequence_mut().height, p.sequence_mut().frame_rate) = (1920, 1080, FrameRate::FPS_60);
        let sixtieth = FrameRate::FPS_60.frame_duration();
        let a = title(&mut p, Time::ZERO, sixtieth);
        let b = title(&mut p, sixtieth, Time::from_secs(1));
        adopt_format(p.sequence_mut(), &video(3840, 2160, FrameRate::FPS_24));
        let seq = p.sequence();
        assert_eq!((seq.clip(a).unwrap().timeline_out, seq.clip(b).unwrap().timeline_in), (sixtieth, sixtieth), "unchanged");
        assert_eq!(seq.clip(b).unwrap().transform.scale, 2.0);
        assert!(seq.tracks.iter().all(Track::is_sorted_and_disjoint));
    }
}
