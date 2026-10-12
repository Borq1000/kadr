//! Text clips in the evaluator (titles spec §3, §5.2): the clock of a
//! clip's text animation at `t`, and its layer — sized by the text's
//! bounds, anchored at the text block's centre (so `Transform.x/y` place
//! that centre and growing text stays centred), placed, faded, keyframed,
//! corrected and filtered by effects exactly like a media clip.

use kadr_core::Time;
use kadr_project::{Clip, TextDoc};
use kadr_scene::*;
use std::sync::Arc;

/// Titles spec §3: time since the visible start and until the visible end
/// (`t` clamped into the clip: a transition handle beyond an edge gets the
/// edge's state), the source time (loops; extrapolated like keyframes) and
/// the highlight's progress over its span.
pub fn text_clock(clip: &Clip, doc: &TextDoc, t: Time) -> kadr_text::Clock {
    let tt = t.max(clip.timeline_in).min(clip.timeline_out);
    let frac = crate::fx::visible_fraction(clip, tt).clamp(0.0, 1.0);
    let highlight = doc.anim.highlight.as_ref().map_or(0.0, |h| h.span.0 + (h.span.1 - h.span.0) * frac);
    kadr_text::Clock { since_in: tt - clip.timeline_in, until_out: clip.timeline_out - tt, source: clip.source_time_at(t), highlight }
}

/// The layer of text clip `clip` (its document `doc`) at `t`.
pub(crate) fn text_layer(clip: &Clip, doc: &TextDoc, t: Time, canvas: SizeU, frame: Time) -> Option<Layer> {
    let st = crate::scene::clip_state_at(clip, t, frame);
    if st.transform.scale <= 0.0 {
        return None;
    }
    let picture = kadr_text::TextFrame::new(doc, text_clock(clip, doc, t));
    let b = picture.bounds();
    let (w, h) = (b.width(), b.height());
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let mut placement = crate::scene::placement_of(&st.transform, Vec2::new(w, h), canvas);
    placement.anchor = Vec2::new(-b.x0 / w, -b.y0 / h);
    let crop = crate::scene::crop_of(&st.transform, placement.size)?;
    let (placement, colour) = crate::fx::apply_effects(clip, t, canvas, placement, crate::fx::crop_window(&st.transform));
    let mut effects = vec![];
    let color = crate::scene::color_of(&st.color);
    if !color.is_neutral() {
        effects.push(Effect::ColorAdjust(color));
    }
    effects.extend(colour);
    Some(Layer {
        id: LayerId::from(clip.id),
        content: LayerContent::Text(TextLayer { picture: Arc::new(picture) }),
        placement,
        crop,
        opacity: (st.transform.opacity * st.fade) as f32,
        blend: BlendMode::Normal,
        effects,
    })
}
