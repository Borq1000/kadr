//! Pure keyframe/fade UI logic shared by the inspector and the timeline.

use kadr_core::{FrameRate, Time};
use kadr_project::{AnimParam, Clip, Ease, FadeEdge, Keyframe, SourceTime, TrackKey};

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

/// Half a sequence `frame`: the tolerance of "at the playhead" (spec §5).
pub fn half_frame(frame: Time) -> Time {
    Time(frame.flicks() / 2)
}

/// `t` (≥ 0) in whole `frame`s, rounded to the nearest (half a frame up):
/// fade lengths set in the inspector or by a timeline handle.
pub fn whole_frames(t: Time, frame: Time) -> Time {
    let f = frame.flicks().max(1);
    Time((t.flicks() + f / 2) / f * f)
}

/// "The keyframe here" (spec §5): the keyframe moment of `p` (of any track
/// — clip property or effect parameter — when `None`) whose timeline time
/// is nearest to `t` and strictly within half a `frame` of it — among the
/// **visible** ones only. A key hidden past an edge of the clip never
/// counts, even within half a frame of it (review focus 1): it is not on
/// screen, and only MCP `source_ms` addresses it. The inspector and MCP
/// `at_ms` share this rule.
pub fn visible_key_near(c: &Clip, p: Option<TrackKey>, t: Time, frame: Time) -> Option<SourceTime> {
    let half = half_frame(frame);
    let keys = p.map_or_else(|| c.animation.keys(), |p| vec![p]);
    keys.iter()
        .flat_map(|q| c.animation.track(q).iter().map(|k| k.at))
        .map(|at| (at, c.timeline_time_of(at.0)))
        .filter(|(_, tl)| *tl >= c.timeline_in && *tl < c.timeline_out && (*tl - t).abs() < half)
        .min_by_key(|(_, tl)| (*tl - t).abs())
        .map(|(at, _)| at)
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

/// The keyframe of `p` at the playhead (`visible_key_near`), only while
/// the playhead is on the clip.
pub fn key_here(clip: &Clip, p: impl Into<TrackKey>, playhead: Time, frame: Time) -> Option<&Keyframe> {
    let p = p.into();
    let t = edit_time(clip, playhead, frame)?;
    let at = visible_key_near(clip, Some(p.clone()), t, frame)?;
    clip.animation.track(&p).iter().find(|k| k.at == at)
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
pub fn key_info(clip: &Clip, p: impl Into<TrackKey>, playhead: Time, frame: Time) -> KeyInfo {
    let p = p.into();
    if !clip.animation.is_animated(&p) {
        return KeyInfo { state: 0, prev: None, next: None };
    }
    let half = half_frame(frame);
    let visible: Vec<Time> = clip
        .animation
        .track(&p)
        .iter()
        .map(|k| clip.timeline_time_of(k.at.0))
        .filter(|t| *t >= clip.timeline_in && *t < clip.timeline_out)
        .collect();
    KeyInfo {
        state: if key_here(clip, &p, playhead, frame).is_some() { 2 } else { 1 },
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
/// A zero fade cannot get shorter: then the movement lengthens the other
/// one (a fade within `HANDLE_INSET` of filling the clip shares its spot
/// with the other, zero fade's handle).
pub fn edge_for_move(dx: f64, fade_in: Time, fade_out: Time) -> FadeEdge {
    let (shortened, fade, other) = if dx < 0.0 { (FadeEdge::In, fade_in, FadeEdge::Out) } else { (FadeEdge::Out, fade_out, FadeEdge::In) };
    if fade > Time::ZERO { shortened } else { other }
}

/// The fade a handle dragged to `x` sets: whole frames, in `[0, dur − other]`.
#[allow(clippy::too_many_arguments)]
pub fn fade_drag_value(edge: FadeEdge, x: f64, x0: f64, x1: f64, pps: f64, frame: Time, other: Time, dur: Time) -> Time {
    let px = match edge {
        FadeEdge::In => x - x0,
        FadeEdge::Out => x1 - x,
    };
    whole_frames(Time::from_secs_f64((px / pps).max(0.0)), frame).min((dur - other).max(Time::ZERO))
}

// --------------------------------------------------------------- diamonds
//
// Timeline keyframe diamonds (spec §6.2), in lane pixels: one per exact
// source moment with a keyframe of any property, along the bottom of a
// video clip. Clusters exist only on screen; hit-testing, dragging and the
// menu always work on one real moment.

/// Diamonds closer than this merge into one drawn cluster.
pub const CLUSTER_PX: f64 = 6.0;
/// The strip where diamonds are hit: this high above the 2 px margin under
/// a video clip (the fade handles' strip is at the top: `HEADER_H`).
pub const KEY_STRIP_H: f64 = 14.0;
/// A drawn diamond's centre above the bottom of the clip's rectangle: the
/// middle of `KEY_STRIP_H`.
pub const KEY_CY: f64 = KEY_STRIP_H / 2.0;

/// Render-only clusters of sorted diamond positions: (x of the first
/// member, `true` when it stands for more than one moment). A cluster is
/// anchored at its first member: a key less than `CLUSTER_PX` right of the
/// anchor joins it, the next one starts a new mark — so a dense run shows a
/// diamond every ≥ `CLUSTER_PX` (R21). The moments are never merged.
pub fn key_marks(xs: &[f64]) -> Vec<(f64, bool)> {
    let mut out: Vec<(f64, bool)> = vec![];
    for &x in xs {
        match out.last_mut() {
            Some(m) if x - m.0 < CLUSTER_PX => m.1 = true,
            _ => out.push((x, false)),
        }
    }
    out
}

/// The real moment nearest to `x` among (moment, x) within `r` px — a
/// press on a cluster picks one of its moments, never the group.
pub fn nearest_key(xs: &[(SourceTime, f64)], x: f64, r: f64) -> Option<SourceTime> {
    xs.iter()
        .map(|(at, kx)| (*at, (kx - x).abs()))
        .filter(|(_, d)| *d <= r)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(at, _)| at)
}

/// Alt+←/→: the previous / next of the sorted visible keyframe `times`
/// at least `half` a frame away from the playhead (a keyframe at the
/// playhead is the current one, not a neighbour).
pub fn visible_neighbour(times: &[Time], playhead: Time, half: Time, forward: bool) -> Option<Time> {
    if forward {
        times.iter().copied().find(|t| *t >= playhead + half)
    } else {
        times.iter().rev().copied().find(|t| *t <= playhead - half)
    }
}

/// The clip's keyframe moments as (source time, timeline time), shifted by
/// `shift` and split by the shown range `[tin, tout)`: the visible ones in
/// time order, and whether any are hidden before / after it (the ‹ ›
/// chevrons). Without a drag the range is the clip's own and `shift` zero;
/// a move or trim drag previews its own. A hidden moment never gets a diamond.
pub fn split_keys(c: &Clip, shift: Time, tin: Time, tout: Time) -> (Vec<(SourceTime, Time)>, bool, bool) {
    let (mut before, mut after) = (false, false);
    let mut shown = vec![];
    for at in c.animation.times() {
        let t = c.timeline_time_of(at.0) + shift;
        if t < tin {
            before = true;
        } else if t >= tout {
            after = true;
        } else {
            shown.push((at, t));
        }
    }
    (shown, before, after)
}

/// The clip's visible keyframe moments (on `[timeline_in, timeline_out)`).
pub fn visible_keys(c: &Clip) -> Vec<(SourceTime, Time)> {
    split_keys(c, Time::ZERO, c.timeline_in, c.timeline_out).0
}

/// Where a diamond shown at `from` lands when dragged by `dt`: on whole
/// sequence frames, inside the clip's visible range `[timeline_in,
/// timeline_out − frame]` (review focus 4). With `dt` zero it is the
/// keyframe's own frame — the "not moved" position.
pub fn key_drag_to(c: &Clip, from: Time, dt: Time, fr: FrameRate) -> Time {
    let last = (c.timeline_out - fr.frame_duration()).max(c.timeline_in);
    fr.snap(from + dt).max(c.timeline_in).min(last)
}

/// The UI name of the curve every track's keyframe at `at` shares;
/// `""` when they differ or there is none (the menu marks nothing).
pub fn moment_ease(c: &Clip, at: SourceTime) -> &'static str {
    let mut keys = c.animation.keys_at(at).into_iter().filter_map(|p| c.animation.track(&p).iter().find(|k| k.at == at).map(|k| ease_key(&k.ease)));
    match keys.next() {
        Some(first) if keys.all(|k| k == first) => first,
        _ => "",
    }
}

/// The tracks whose keyframe at `at` does not have curve `e` yet: the
/// keyframe menu changes only those (none — no command at all).
pub fn ease_targets(c: &Clip, at: SourceTime, e: &Ease) -> Vec<TrackKey> {
    c.animation.keys_at(at).into_iter().filter(|p| c.animation.track(p).iter().any(|k| k.at == at && k.ease != *e)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{AssetId, TimeRange};

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

    /// Review focus 1: a key hidden less than half a frame past an edge of
    /// the clip is never "the keyframe here" — not in the inspector (◆, the
    /// slider would edit it) and not for MCP `at_ms`.
    #[test]
    fn a_hidden_key_within_half_a_frame_of_an_edge_is_not_here() {
        let mut c = clip(); // timeline 10..20 s = source 0..10 s
        let third = Time(F.flicks() / 3);
        c.animation.set_key(AnimParam::X, SourceTime(Time::ZERO - third), 1.0, None); // just before timeline_in
        c.animation.set_key(AnimParam::X, SourceTime(Time::from_secs(5)), 2.0, None);
        c.animation.set_key(AnimParam::Y, SourceTime(Time::from_secs(10)), 1.0, None); // at timeline_out: hidden
        let (tin, last) = (Time::from_secs(10), Time::from_secs(20) - third);
        assert!(key_here(&c, AnimParam::X, tin, F).is_none());
        assert_eq!(key_info(&c, AnimParam::X, tin, F).state, 1, "animated, no keyframe here");
        assert!(key_here(&c, AnimParam::Y, last, F).is_none());
        assert_eq!(visible_key_near(&c, None, tin, F), None);
        assert_eq!(visible_key_near(&c, None, last, F), None);
        assert_eq!(visible_key_near(&c, None, Time::from_secs(15) + third, F), Some(SourceTime(Time::from_secs(5))), "a visible one is");
    }

    /// The distance is timeline time (a 2× clip halves source distances),
    /// strictly inside half a frame; `None` looks at every property.
    #[test]
    fn the_key_near_is_measured_on_the_timeline_strictly_within_half_a_frame() {
        let mut c = clip();
        c.source_out = Time::from_secs(20); // 2×: timeline 10..20 s = source 0..20 s
        c.animation.set_key(AnimParam::Scale, SourceTime(Time::from_secs(4)), 1.0, None); // timeline 12 s
        let half = half_frame(F);
        assert_eq!(half, Time(F.flicks() / 2));
        let at = |t: Time, p| visible_key_near(&c, p, t, F);
        assert_eq!(at(Time::from_secs(12) + half - Time(1), Some(AnimParam::Scale.into())), Some(SourceTime(Time::from_secs(4))));
        assert_eq!(at(Time::from_secs(12) - half + Time(1), None), Some(SourceTime(Time::from_secs(4))));
        assert_eq!(at(Time::from_secs(12) + half, Some(AnimParam::Scale.into())), None, "exactly half a frame away is not near");
        assert_eq!(at(Time::from_secs(12), Some(AnimParam::X.into())), None, "another property");
    }

    /// Fade lengths are whole frames, rounded to the nearest (ties up).
    #[test]
    fn whole_frames_rounds_to_the_nearest_frame() {
        assert_eq!(whole_frames(Time::ZERO, F), Time::ZERO);
        assert_eq!(whole_frames(Time(F.flicks() / 2 - 1), F), Time::ZERO);
        assert_eq!(whole_frames(Time(F.flicks() / 2), F), F, "half a frame rounds up");
        assert_eq!(whole_frames(Time::from_millis(510), F), F.mul_ratio(15, 1));
        assert_eq!(whole_frames(Time::from_secs(1), Time::ZERO), Time::from_secs(1), "a zero frame never divides by zero");
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
        assert_eq!(edge_for_move(-4.0, fi, fo), FadeEdge::In);
        assert_eq!(edge_for_move(4.0, fi, fo), FadeEdge::Out);
        // Each grabbed at the spot and moved 10 px its way gets 10 px (0.1 s) shorter.
        let grab_in = spot - fade_edge_x(x0, x1, px(fi), FadeEdge::In);
        assert_eq!(fade_drag_value(FadeEdge::In, spot - 10.0 - grab_in, x0, x1, pps, F, fo, dur), F.mul_ratio(33, 1));
        let grab_out = spot - fade_edge_x(x0, x1, px(fo), FadeEdge::Out);
        assert_eq!(fade_drag_value(FadeEdge::Out, spot + 10.0 - grab_out, x0, x1, pps, F, fi, dur), F.mul_ratio(21, 1));
    }

    /// A fade within `HANDLE_INSET` of filling the clip, the other one zero:
    /// both handles clamp to one spot. A movement that would shorten the zero
    /// fade takes the other edge, so the near-full fade can still be lengthened.
    #[test]
    fn a_near_full_fade_lengthens_from_the_shared_spot() {
        use kadr_project::FadeEdge;
        let (x0, x1, pps, dur) = (100.0, 300.0, 100.0, Time::from_secs(2));
        let (fi, fo) = (Time::from_millis(1_950), Time::ZERO);
        let px = |t: Time| t.as_secs_f64() * pps;
        assert_eq!(pick_handle(288.0, x0, x1, px(fi), px(fo), (true, true)), Some(HandleHit::Both));
        let edge = edge_for_move(298.0 - 288.0, fi, fo);
        assert_eq!(edge, FadeEdge::In, "the zero fade-out cannot get shorter");
        let grab = 288.0 - fade_edge_x(x0, x1, px(fi), edge);
        let v = fade_drag_value(edge, 298.0 - grab, x0, x1, pps, F, fo, dur);
        assert!(v > fi, "the fade-in got longer: {v:?}");
        assert_eq!(v, dur, "up to the whole clip");
        // Mirrored: a near-full fade-out and a move to the left.
        assert_eq!(edge_for_move(-10.0, Time::ZERO, fi), FadeEdge::Out);
        // Shortening a set fade keeps the old rule.
        assert_eq!(edge_for_move(-10.0, fi, fo), FadeEdge::In);
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

    // ------------------------------------------------------------ diamonds

    #[test]
    fn marks_cluster_only_for_drawing() {
        assert_eq!(key_marks(&[10.0, 13.0, 30.0, 35.9, 50.0]), vec![(10.0, true), (30.0, true), (50.0, false)]);
        assert_eq!(key_marks(&[10.0, 17.0]), vec![(10.0, false), (17.0, false)], "6 px apart or more: separate");
    }

    /// R21: a cluster is anchored at its first member, so a dense run of
    /// keyframes shows a diamond every ≥ CLUSTER_PX instead of one at its start.
    #[test]
    fn a_dense_run_shows_a_diamond_every_cluster_width() {
        let run: Vec<f64> = (0..=10).map(|i| i as f64 * 2.0).collect(); // every 2 px over 20 px
        assert_eq!(key_marks(&run), vec![(0.0, true), (6.0, true), (12.0, true), (18.0, true)]);
        assert_eq!(key_marks(&[0.0, 2.0, 6.0]), vec![(0.0, true), (6.0, false)], "measured from the anchor, not the previous key");
    }

    /// A click on a cluster picks the nearest *real* moment, never the group.
    #[test]
    fn nearest_key_returns_a_real_time() {
        let s = |ms| SourceTime(Time::from_millis(ms));
        let xs = [(s(1_000), 10.0), (s(1_040), 13.0), (s(9_000), 90.0)];
        assert_eq!(nearest_key(&xs, 12.0, 6.0), Some(s(1_040)));
        assert_eq!(nearest_key(&xs, 10.4, 6.0), Some(s(1_000)));
        assert_eq!(nearest_key(&xs, 50.0, 6.0), None);
    }

    #[test]
    fn alt_arrows_step_between_visible_keyframes() {
        let ts = [Time::from_secs(1), Time::from_secs(3), Time::from_secs(5)];
        let half = Time::from_millis(16);
        assert_eq!(visible_neighbour(&ts, Time::from_secs(3), half, true), Some(Time::from_secs(5)));
        assert_eq!(visible_neighbour(&ts, Time::from_secs(3), half, false), Some(Time::from_secs(1)));
        assert_eq!(visible_neighbour(&ts, Time::from_secs(5), half, true), None);
    }

    /// One moment per exact source time; a hidden one (outside
    /// `[timeline_in, timeline_out)`) never gets a diamond, only a chevron.
    #[test]
    fn hidden_keys_get_a_chevron_never_a_diamond() {
        let mut c = clip(); // timeline 10..20 s = source 0..10 s
        for (src, p) in [(-5, AnimParam::X), (0, AnimParam::X), (4, AnimParam::X), (4, AnimParam::Opacity), (10, AnimParam::Opacity)] {
            c.animation.set_key(p, SourceTime(Time::from_secs(src)), 1.0, None);
        }
        let s = |x| SourceTime(Time::from_secs(x));
        assert_eq!(visible_keys(&c), vec![(s(0), Time::from_secs(10)), (s(4), Time::from_secs(14))], "−5 s and timeline_out are hidden");
        let (_, before, after) = split_keys(&c, Time::ZERO, c.timeline_in, c.timeline_out);
        assert!(before && after, "‹ and › instead");
        // A trim drag previewing a longer clip shows the hidden moments.
        let (shown, before, after) = split_keys(&c, Time::ZERO, Time::from_secs(4), Time::from_secs(21));
        assert_eq!(shown.len(), 4);
        assert!(!before && !after);
        // A move drag shifts the moments with the clip.
        let moved = split_keys(&c, Time::from_secs(1), Time::from_secs(11), Time::from_secs(21));
        assert_eq!(moved, (vec![(s(0), Time::from_secs(11)), (s(4), Time::from_secs(15))], true, true));
        let bare = clip();
        assert_eq!(split_keys(&bare, Time::ZERO, bare.timeline_in, bare.timeline_out), (vec![], false, false));
    }

    /// Review focus 4: a dragged diamond lands on whole frames and never
    /// leaves the visible clip range `[timeline_in, timeline_out − frame]`.
    #[test]
    fn a_dragged_diamond_stays_on_frames_inside_the_clip() {
        use kadr_core::FrameRate;
        let c = clip(); // timeline 10..20 s
        let fr = FrameRate::FPS_30;
        let from = Time::from_secs(14);
        assert_eq!(key_drag_to(&c, from, Time::ZERO, fr), from);
        assert_eq!(key_drag_to(&c, from, Time(F.flicks() * 5 / 2), fr), from + F.mul_ratio(3, 1), "2.5 frames → 3 frames (nearest)");
        assert_eq!(key_drag_to(&c, from, Time::from_secs(-30), fr), Time::from_secs(10), "not before the clip");
        assert_eq!(key_drag_to(&c, from, Time::from_secs(30), fr), Time::from_secs(20) - F, "not past its last frame");
        // A keyframe between frames (e.g. after a speed change) snaps when moved.
        let off = from + Time(F.flicks() / 4);
        assert_eq!(key_drag_to(&c, off, F, fr), from + F);
        assert_eq!(key_drag_to(&c, off, Time::ZERO, fr), from, "its own frame: no move");
    }

    /// R14: the keyframe menu marks the curve every property of the moment
    /// shares, and setting a curve touches only the keyframes that differ.
    #[test]
    fn the_keyframe_menu_marks_a_shared_curve_and_sets_only_the_others() {
        let mut c = clip();
        let at = SourceTime(Time::from_secs(4));
        assert_eq!(moment_ease(&c, at), "", "no keyframe there");
        c.animation.set_key(AnimParam::X, at, 1.0, Some(Ease::Linear));
        c.animation.set_key(AnimParam::Opacity, at, 1.0, Some(Ease::Linear));
        c.animation.set_key(AnimParam::Scale, SourceTime(Time::from_secs(5)), 1.0, Some(Ease::Hold));
        assert_eq!(moment_ease(&c, at), "linear", "another moment's curve does not count");
        assert_eq!(ease_targets(&c, at, &Ease::Linear), vec![], "nothing to change: no command");
        c.animation.set_key(AnimParam::Y, at, 1.0, Some(Ease::Hold));
        assert_eq!(moment_ease(&c, at), "", "mixed: nothing marked");
        assert_eq!(ease_targets(&c, at, &Ease::Linear), vec![TrackKey::from(AnimParam::Y)]);
        assert_eq!(ease_targets(&c, at, &Ease::Hold), vec![TrackKey::from(AnimParam::X), TrackKey::from(AnimParam::Opacity)]);
        let other = SourceTime(Time::from_secs(6));
        c.animation.set_key(AnimParam::X, other, 1.0, Some(Ease::Unknown(Default::default())));
        assert_eq!(moment_ease(&c, other), "other");
    }
}
