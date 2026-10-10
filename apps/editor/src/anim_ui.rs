//! Pure keyframe/fade UI logic shared by the inspector and the timeline.

use kadr_core::Time;
use kadr_project::{AnimParam, Clip, Ease, FadeEdge, Keyframe, Sequence};

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

// ---------------------------------------------------------- fade handles
//
// Timeline fade handles (spec §6.2), in lane pixels. A handle's grab zone
// is `2 · FADE_R` wide and the clip's top strip high; it starts
// `HANDLE_INSET − FADE_R` inside the clip, i.e. right where the trim zone
// (`timeline_ui::TRIM_ZONE` around the edge) ends, so the two never overlap.

/// Half the width of a handle's grab zone.
pub const FADE_R: f64 = 6.0;
/// Closest a handle's centre comes to the clip's edge.
pub const HANDLE_INSET: f64 = 12.0;
/// The clip's top strip (its 2 px margin + the 16 px name bar) where handles are hit.
pub const HEADER_H: f64 = 18.0;
/// Centre of a drawn handle below the top of the clip's rectangle (which
/// starts 2 px into its row): the middle of the 16 px name bar.
pub const HANDLE_Y: f64 = 8.0;
/// Narrower clips show no handles (trimming and moving keep the room).
pub const FADE_MIN_W: f64 = 40.0;

/// Centre of a handle: on the fade boundary, kept `HANDLE_INSET` inside the clip.
pub fn handle_x(x0: f64, x1: f64, fade_px: f64, edge: FadeEdge) -> f64 {
    match edge {
        FadeEdge::In => (x0 + fade_px).max(x0 + HANDLE_INSET).min(x1 - HANDLE_INSET),
        FadeEdge::Out => (x1 - fade_px).min(x1 - HANDLE_INSET).max(x0 + HANDLE_INSET),
    }
}

/// The fade boundary itself (a handle without its inset). A drag moves it
/// by the pointer's movement, so grabbing a handle never makes the fade jump.
pub fn fade_edge_x(x0: f64, x1: f64, fade_px: f64, edge: FadeEdge) -> f64 {
    match edge {
        FadeEdge::In => x0 + fade_px,
        FadeEdge::Out => x1 - fade_px,
    }
}

/// Which handles (fade-in, fade-out) a clip `width` px wide shows (spec
/// §6.2, per fade — R18): one whose own fade is set always, a zero one only
/// while the clip is `active` (hovered or selected); none below `FADE_MIN_W`.
pub fn shown_handles(width: f64, active: bool, fi: Time, fo: Time) -> (bool, bool) {
    if width < FADE_MIN_W {
        return (false, false);
    }
    (active || fi > Time::ZERO, active || fo > Time::ZERO)
}

/// What a press on a clip's handles grabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleHit {
    Edge(FadeEdge),
    /// Two handles on (nearly) one spot — e.g. fades that fill the clip:
    /// the first movement picks the one it shortens (`edge_for_move`).
    Both,
}

/// The shown handle at `x`: the nearest within `FADE_R`; a hidden one is
/// never picked. Handles closer than `FADE_R` to each other overlap on
/// screen, so a press near both is `Both`.
pub fn pick_handle(x: f64, x0: f64, x1: f64, fi_px: f64, fo_px: f64, (show_in, show_out): (bool, bool)) -> Option<HandleHit> {
    let (h_in, h_out) = (handle_x(x0, x1, fi_px, FadeEdge::In), handle_x(x0, x1, fo_px, FadeEdge::Out));
    let near_in = show_in && (x - h_in).abs() <= FADE_R;
    let near_out = show_out && (x - h_out).abs() <= FADE_R;
    match (near_in, near_out) {
        (true, true) if (h_out - h_in).abs() <= FADE_R => Some(HandleHit::Both),
        (true, true) => Some(HandleHit::Edge(if (x - h_in).abs() <= (x - h_out).abs() { FadeEdge::In } else { FadeEdge::Out })),
        (true, false) => Some(HandleHit::Edge(FadeEdge::In)),
        (false, true) => Some(HandleHit::Edge(FadeEdge::Out)),
        (false, false) => None,
    }
}

/// For `HandleHit::Both`: the handle a first movement by `dx` px drags —
/// the one it shortens (left: the fade-in, right: the fade-out), so either
/// fade can be shortened from a shared spot, which separates the handles.
pub fn edge_for_move(dx: f64) -> FadeEdge {
    if dx < 0.0 { FadeEdge::In } else { FadeEdge::Out }
}

/// The fade a handle dragged to `x` sets: whole frames, in `[0, dur − other]`.
#[allow(clippy::too_many_arguments)]
pub fn fade_drag_value(edge: FadeEdge, x: f64, x0: f64, x1: f64, pps: f64, frame: Time, other: Time, dur: Time) -> Time {
    let px = match edge {
        FadeEdge::In => x - x0,
        FadeEdge::Out => x1 - x,
    };
    let f = frame.flicks().max(1);
    let raw = Time::from_secs_f64((px / pps).max(0.0));
    let snapped = Time((raw.flicks() + f / 2) / f * f);
    snapped.min((dur - other).max(Time::ZERO))
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
        // The Slint rows read `keys[i]` in this order: it must be `AnimParam::index()`.
        for (i, (name, p)) in PROPS.iter().enumerate() {
            assert_eq!(p.index(), i, "row {name}");
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

    #[test]
    fn handles_stay_inside_the_clip_and_track_the_fade() {
        use kadr_project::FadeEdge;
        assert_eq!(handle_x(100.0, 300.0, 0.0, FadeEdge::In), 112.0, "zero fade: inset from the edge");
        assert_eq!(handle_x(100.0, 300.0, 50.0, FadeEdge::In), 150.0);
        assert_eq!(handle_x(100.0, 300.0, 0.0, FadeEdge::Out), 288.0);
        assert_eq!(handle_x(100.0, 300.0, 80.0, FadeEdge::Out), 220.0);
        assert_eq!(handle_x(100.0, 300.0, 500.0, FadeEdge::In), 288.0, "never past the other inset");
    }

    #[test]
    fn fade_drag_snaps_to_frames_and_respects_the_other_fade() {
        use kadr_project::FadeEdge;
        let pps = 100.0; // px per second
        let dur = Time::from_secs(4);
        let v = fade_drag_value(FadeEdge::In, 100.0 + 51.0, 100.0, 500.0, pps, F, Time::ZERO, dur);
        assert_eq!(v, F.mul_ratio(15, 1), "0.51 s → 15 frames at 30 fps");
        let v = fade_drag_value(FadeEdge::Out, 500.0 - 390.0, 100.0, 500.0, pps, F, Time::from_secs(1), dur);
        assert_eq!(v, Time::from_secs(3), "capped at duration − fade_in");
        assert_eq!(fade_drag_value(FadeEdge::In, 50.0, 100.0, 500.0, pps, F, Time::ZERO, dur), Time::ZERO);
    }

    /// A drag moves the fade boundary by the pointer's movement: grabbing a
    /// zero fade's handle (drawn at the inset) does not jump to the inset.
    #[test]
    fn grabbing_a_handle_off_its_boundary_does_not_jump() {
        use kadr_project::FadeEdge;
        let (x0, x1, pps) = (100.0, 500.0, 100.0);
        let press = handle_x(x0, x1, 0.0, FadeEdge::In);
        let grab = press - fade_edge_x(x0, x1, 0.0, FadeEdge::In);
        let v = fade_drag_value(FadeEdge::In, press + 3.4 - grab, x0, x1, pps, F, Time::ZERO, Time::from_secs(4));
        assert_eq!(v, F, "3.4 px = 0.034 s → 1 frame, not the 12 px inset");
        let press = handle_x(x0, x1, 150.0, FadeEdge::Out);
        let grab = press - fade_edge_x(x0, x1, 150.0, FadeEdge::Out);
        assert_eq!(grab, 0.0, "a handle on its boundary is grabbed as it is");
    }

    /// The handle under the pointer: the nearest within `FADE_R`; two
    /// handles on (nearly) one spot are `Both` (the drag direction picks).
    #[test]
    fn the_nearest_handle_is_picked_and_overlapping_ones_wait_for_the_drag() {
        use kadr_project::FadeEdge;
        const BOTH: (bool, bool) = (true, true);
        let one = |e| Some(HandleHit::Edge(e));
        assert_eq!(pick_handle(112.0, 100.0, 300.0, 0.0, 0.0, BOTH), one(FadeEdge::In));
        assert_eq!(pick_handle(106.0, 100.0, 300.0, 0.0, 0.0, BOTH), one(FadeEdge::In), "the grab zone is 2·FADE_R wide");
        assert_eq!(pick_handle(105.0, 100.0, 300.0, 0.0, 0.0, BOTH), None, "the clip edge belongs to trimming");
        assert_eq!(pick_handle(288.0, 100.0, 300.0, 0.0, 0.0, BOTH), one(FadeEdge::Out));
        assert_eq!(pick_handle(200.0, 100.0, 300.0, 0.0, 0.0, BOTH), None);
        assert_eq!(pick_handle(112.0, 100.0, 300.0, 0.0, 200.0, BOTH), Some(HandleHit::Both), "fade-out fills the clip");
        assert_eq!(pick_handle(288.0, 100.0, 300.0, 200.0, 0.0, BOTH), Some(HandleHit::Both), "fade-in fills the clip");
        // The narrowest clip with handles: both zones apart, the middle moves the clip.
        assert_eq!(pick_handle(118.0, 100.0, 100.0 + FADE_MIN_W, 0.0, 0.0, BOTH), one(FadeEdge::In));
        assert_eq!(pick_handle(120.0, 100.0, 100.0 + FADE_MIN_W, 0.0, 0.0, BOTH), None);
        assert_eq!(pick_handle(122.0, 100.0, 100.0 + FADE_MIN_W, 0.0, 0.0, BOTH), one(FadeEdge::Out));
        // Handles 4 px apart overlap: `Both` where the pointer is near both,
        // the near one outside that.
        assert_eq!(pick_handle(150.0, 100.0, 300.0, 48.0, 148.0, BOTH), Some(HandleHit::Both));
        assert_eq!(pick_handle(143.0, 100.0, 300.0, 48.0, 148.0, BOTH), one(FadeEdge::In));
        assert_eq!(pick_handle(157.0, 100.0, 300.0, 48.0, 148.0, BOTH), one(FadeEdge::Out));
        // Handles further apart than FADE_R: the nearest, even between them.
        assert_eq!(pick_handle(154.0, 100.0, 300.0, 50.0, 140.0, BOTH), one(FadeEdge::In));
        assert_eq!(pick_handle(156.0, 100.0, 300.0, 50.0, 140.0, BOTH), one(FadeEdge::Out));
    }

    /// Meeting handles (fade_in + fade_out = duration): moving left
    /// shortens the fade-in, moving right the fade-out — either fade can be
    /// shortened from the spot, which separates the handles.
    #[test]
    fn meeting_handles_are_told_apart_by_the_first_movement() {
        use kadr_project::FadeEdge;
        let (x0, x1, pps, dur) = (100.0, 300.0, 100.0, Time::from_secs(2));
        let (fi, fo) = (F.mul_ratio(36, 1), F.mul_ratio(24, 1)); // 1.2 s + 0.8 s
        let px = |t: Time| t.as_secs_f64() * pps;
        let spot = handle_x(x0, x1, px(fi), FadeEdge::In);
        assert_eq!(spot, handle_x(x0, x1, px(fo), FadeEdge::Out));
        assert_eq!(pick_handle(spot, x0, x1, px(fi), px(fo), (true, true)), Some(HandleHit::Both));
        assert_eq!(edge_for_move(-4.0), FadeEdge::In);
        assert_eq!(edge_for_move(4.0), FadeEdge::Out);
        // Each grabbed at the spot and moved 10 px its way gets 10 px (0.1 s) shorter.
        let grab_in = spot - fade_edge_x(x0, x1, px(fi), FadeEdge::In);
        assert_eq!(fade_drag_value(FadeEdge::In, spot - 10.0 - grab_in, x0, x1, pps, F, fo, dur), F.mul_ratio(33, 1));
        let grab_out = spot - fade_edge_x(x0, x1, px(fo), FadeEdge::Out);
        assert_eq!(fade_drag_value(FadeEdge::Out, spot + 10.0 - grab_out, x0, x1, pps, F, fi, dur), F.mul_ratio(21, 1));
    }

    /// R18 (spec §6.2 per fade): a handle whose own fade is set always
    /// shows; a zero one only while the clip is hovered or selected; none
    /// on a clip narrower than `FADE_MIN_W`.
    #[test]
    fn each_handle_shows_on_its_own_fade_or_while_the_clip_is_active() {
        let s = Time::from_secs(1);
        assert_eq!(shown_handles(100.0, false, Time::ZERO, Time::ZERO), (false, false), "idle, no fades");
        assert_eq!(shown_handles(100.0, false, s, Time::ZERO), (true, false), "only the fade-in's handle");
        assert_eq!(shown_handles(100.0, false, Time::ZERO, s), (false, true), "only the fade-out's handle");
        assert_eq!(shown_handles(100.0, true, Time::ZERO, Time::ZERO), (true, true), "hovered or selected");
        assert_eq!(shown_handles(FADE_MIN_W, false, s, s), (true, true));
        assert_eq!(shown_handles(FADE_MIN_W - 1.0, true, s, s), (false, false), "too narrow");
    }

    /// A hidden handle is not there for the pointer: its spot belongs to
    /// the clip (or the other handle) as without handles.
    #[test]
    fn a_hidden_handle_cannot_be_grabbed() {
        use kadr_project::FadeEdge;
        let out_only = (false, true);
        assert_eq!(pick_handle(112.0, 100.0, 300.0, 0.0, 50.0, out_only), None, "the zero fade-in's spot");
        assert_eq!(pick_handle(250.0, 100.0, 300.0, 0.0, 50.0, out_only), Some(HandleHit::Edge(FadeEdge::Out)));
        // A fade-out filling the clip sits where the hidden fade-in would: no tie.
        assert_eq!(pick_handle(112.0, 100.0, 300.0, 0.0, 200.0, out_only), Some(HandleHit::Edge(FadeEdge::Out)));
        assert_eq!(pick_handle(112.0, 100.0, 300.0, 0.0, 0.0, (false, false)), None);
        assert_eq!(pick_handle(288.0, 100.0, 300.0, 200.0, 0.0, (true, false)), Some(HandleHit::Edge(FadeEdge::In)));
    }
}
