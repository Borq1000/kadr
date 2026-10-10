//! Pure keyframe/fade UI logic shared by the inspector and the timeline.

use kadr_core::Time;
use kadr_project::{AnimParam, Clip, Ease, Keyframe, Sequence};

/// Inspector property names of the animatable rows (Temperature has no row).
pub const PROPS: [(&str, AnimParam); 12] = [
    ("x", AnimParam::X),
    ("y", AnimParam::Y),
    ("scale", AnimParam::Scale),
    ("rotation", AnimParam::Rotation),
    ("opacity", AnimParam::Opacity),
    ("crop-l", AnimParam::CropLeft),
    ("crop-r", AnimParam::CropRight),
    ("crop-t", AnimParam::CropTop),
    ("crop-b", AnimParam::CropBottom),
    ("exposure", AnimParam::Exposure),
    ("contrast", AnimParam::Contrast),
    ("saturation", AnimParam::Saturation),
];

pub fn param_of(prop: &str) -> Option<AnimParam> {
    PROPS.iter().find(|(n, _)| *n == prop).map(|(_, p)| *p)
}

/// i18n key of the property's label.
pub fn label_key(p: AnimParam) -> &'static str {
    match p {
        AnimParam::X => "inspector.pos_x",
        AnimParam::Y => "inspector.pos_y",
        AnimParam::Scale => "inspector.scale",
        AnimParam::Rotation => "inspector.rotation",
        AnimParam::Opacity => "inspector.opacity",
        AnimParam::CropLeft => "inspector.crop_l",
        AnimParam::CropRight => "inspector.crop_r",
        AnimParam::CropTop => "inspector.crop_t",
        AnimParam::CropBottom => "inspector.crop_b",
        AnimParam::Exposure => "inspector.exposure",
        AnimParam::Contrast => "inspector.contrast",
        AnimParam::Saturation => "inspector.saturation",
        AnimParam::Temperature => "inspector.temperature",
    }
}

/// Tolerance of "at the playhead" (spec §5).
#[expect(dead_code, reason = "first used by the timeline's keyframe navigation")]
pub fn half_frame(seq: &Sequence) -> Time {
    Time(seq.frame_rate.frame_duration().flicks() / 2)
}

/// Where a keyframe may be set or changed: the playhead, only while it is
/// on the clip (`timeline_in ≤ playhead < timeline_out`). Never a hidden
/// time (review focus 1).
pub fn edit_time(clip: &Clip, playhead: Time, _frame: Time) -> Option<Time> {
    (clip.timeline_in <= playhead && playhead < clip.timeline_out).then_some(playhead)
}

/// The moment whose values the inspector shows: the playhead on the clip,
/// otherwise the nearest edge frame (`[timeline_in, timeline_out − frame]`).
pub fn shown_time(clip: &Clip, playhead: Time, frame: Time) -> Time {
    edit_time(clip, playhead, frame)
        .unwrap_or_else(|| playhead.max(clip.timeline_in).min((clip.timeline_out - frame).max(clip.timeline_in)))
}

/// Show "move the playhead onto the clip to edit its animation": the clip
/// has animation and the playhead is off it.
pub fn outside_hint(clip: &Clip, playhead: Time, frame: Time) -> bool {
    !clip.animation.is_empty() && edit_time(clip, playhead, frame).is_none()
}

/// The keyframe of `p` at the playhead (within half a frame, spec §5),
/// only while the playhead is on the clip.
pub fn key_here(clip: &Clip, p: AnimParam, playhead: Time, frame: Time) -> Option<&Keyframe> {
    let t = edit_time(clip, playhead, frame)?;
    let at = clip.key_near(p, t, Time(frame.flicks() / 2))?;
    clip.animation.track(p).iter().find(|k| k.at == at)
}

/// The UI name of a curve: its saved key, `"other"` for one from a newer Kadr.
pub fn ease_key(e: &Ease) -> &'static str {
    e.key().unwrap_or("other")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyInfo {
    /// 0 not animated, 1 animated without a keyframe here, 2 keyframe here.
    pub state: i32,
    pub prev: Option<Time>,
    pub next: Option<Time>,
}

/// Keyframe state of `p` at the playhead; `prev`/`next` are the nearest
/// **visible** keyframes (timeline time in `[in, out)`) other than the one here.
pub fn key_info(clip: &Clip, p: AnimParam, playhead: Time, frame: Time) -> KeyInfo {
    if !clip.animation.is_animated(p) {
        return KeyInfo { state: 0, prev: None, next: None };
    }
    let half = Time(frame.flicks() / 2);
    let visible: Vec<Time> = clip
        .animation
        .track(p)
        .iter()
        .map(|k| clip.timeline_time_of(k.at.0))
        .filter(|t| *t >= clip.timeline_in && *t < clip.timeline_out)
        .collect();
    KeyInfo {
        state: if key_here(clip, p, playhead, frame).is_some() { 2 } else { 1 },
        prev: visible.iter().rev().copied().find(|t| *t <= playhead - half),
        next: visible.iter().copied().find(|t| *t >= playhead + half),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, TimeRange};
    use kadr_project::{Ease, SourceTime};

    const F: Time = Time(705_600_000 / 30);

    fn clip() -> Clip {
        // timeline 10..20 s, source 0..10 s
        Clip::new(AssetId::new(), "c", TimeRange::new(Time::ZERO, Time::from_secs(10)), Time::from_secs(10))
    }

    #[test]
    fn inspector_props_map_to_params() {
        assert_eq!(param_of("crop-l"), Some(AnimParam::CropLeft));
        assert_eq!(param_of("speed"), None);
        for p in AnimParam::ALL.into_iter().filter(|p| *p != AnimParam::Temperature) {
            assert!(PROPS.iter().any(|(_, q)| *q == p), "{p:?} has an inspector row");
        }
    }

    /// Review focus 1.
    #[test]
    fn outside_the_clip_there_is_no_edit_time_and_values_show_the_edge() {
        let c = clip();
        assert_eq!(edit_time(&c, Time::from_secs(5), F), None);
        assert_eq!(edit_time(&c, Time::from_secs(20), F), None, "timeline_out is outside");
        assert_eq!(edit_time(&c, Time::from_secs(12), F), Some(Time::from_secs(12)));
        assert_eq!(shown_time(&c, Time::from_secs(5), F), Time::from_secs(10));
        assert_eq!(shown_time(&c, Time::from_secs(25), F), Time::from_secs(20) - F);
    }

    #[test]
    fn key_info_states_and_visible_neighbours() {
        let mut c = clip();
        assert_eq!(key_info(&c, AnimParam::X, Time::from_secs(12), F).state, 0);
        for (src, v) in [(-5, 0.0), (2, 1.0), (5, 2.0), (15, 3.0)] {
            c.animation.set_key(AnimParam::X, SourceTime(Time::from_secs(src)), v, Some(Ease::Linear));
        }
        let at = key_info(&c, AnimParam::X, Time::from_secs(15), F);
        assert_eq!((at.state, at.prev, at.next), (2, Some(Time::from_secs(12)), None), "hidden −5 s and 25 s are not neighbours");
        let between = key_info(&c, AnimParam::X, Time::from_secs(13), F);
        assert_eq!((between.state, between.prev, between.next), (1, Some(Time::from_secs(12)), Some(Time::from_secs(15))));
    }

    /// On the clip the inspector shows the playhead's own moment, also
    /// inside the clip's last (partial) frame — the frame the preview shows.
    #[test]
    fn on_the_clip_values_are_shown_at_the_playhead() {
        let c = clip();
        let last = Time::from_secs(20) - Time(F.flicks() / 3);
        assert_eq!(shown_time(&c, Time::from_secs(12), F), Time::from_secs(12));
        assert_eq!(shown_time(&c, last, F), last);
    }

    /// "Move the playhead onto the clip" only matters for a clip with animation.
    #[test]
    fn the_outside_hint_needs_animation_and_a_playhead_off_the_clip() {
        let mut c = clip();
        assert!(!outside_hint(&c, Time::from_secs(5), F), "no animation: nothing to explain");
        c.animation.set_key(AnimParam::Opacity, SourceTime(Time::from_secs(2)), 1.0, None);
        assert!(outside_hint(&c, Time::from_secs(5), F));
        assert!(outside_hint(&c, Time::from_secs(20), F), "timeline_out is outside");
        assert!(!outside_hint(&c, Time::from_secs(12), F));
    }

    /// The interpolation menu marks the curve of the keyframe at the playhead;
    /// a curve from a newer Kadr shows as "other" (spec §2.1, §6.1).
    #[test]
    fn the_keyframe_here_is_within_half_a_frame_and_only_on_the_clip() {
        let mut c = clip();
        c.animation.set_key(AnimParam::X, SourceTime(Time::from_secs(2)), 1.0, Some(Ease::Hold));
        c.animation.set_key(AnimParam::X, SourceTime(Time::from_secs(-5)), 0.0, Some(Ease::Unknown(Default::default())));
        c.animation.set_key(AnimParam::Y, SourceTime(Time::from_secs(4)), 0.0, Some(Ease::Unknown(Default::default())));
        let near = Time::from_secs(12) + Time(F.flicks() / 3);
        assert_eq!(key_here(&c, AnimParam::X, near, F).map(|k| (k.at, ease_key(&k.ease))), Some((SourceTime(Time::from_secs(2)), "hold")));
        assert!(key_here(&c, AnimParam::X, Time::from_secs(12) + F, F).is_none(), "a frame away is not here");
        assert!(key_here(&c, AnimParam::X, Time::from_secs(5), F).is_none(), "a hidden keyframe is never here");
        assert_eq!(key_here(&c, AnimParam::Y, Time::from_secs(14), F).map(|k| ease_key(&k.ease)), Some("other"));
    }
}
