//! Text clips in the evaluator (titles spec §3, §5.2): the clock of a
//! clip's text animation at `t`, and its layer — sized by the text's
//! bounds, anchored at the text block's centre (so `Transform.x/y` place
//! that centre and growing text stays centred), placed, faded, keyframed,
//! corrected and filtered by effects exactly like a media clip — the
//! motion effects moving the text block (`TextFrame::block_box`: the
//! layout box, or a curved text's own block), not the room around it.

use kadr_core::Time;
use kadr_project::{Clip, TextDoc, Transform};
use kadr_scene::*;
use kadr_text::Bounds;
use std::sync::Arc;

/// Titles spec §3: time since the visible start and until the visible end
/// — clip-bound like the fades (`fade_at`): `t` clamped into the clip's
/// frames, so its last frame has `until_out` 0 (a disappearance has
/// finished there) and a transition handle beyond an edge gets the edge's
/// state; `frame` is the sequence's frame duration. The source time
/// (loops) is extrapolated like keyframes; the highlight's progress runs
/// linearly over its span from the clip's in to its out (continuous across
/// a split, like an effect's progress), held beyond the edges.
pub fn text_clock(clip: &Clip, doc: &TextDoc, t: Time, frame: Time) -> kadr_text::Clock {
    let last = (clip.timeline_out - frame).max(clip.timeline_in);
    let tt = t.max(clip.timeline_in).min(last);
    let frac = crate::fx::visible_fraction(clip, t.max(clip.timeline_in).min(clip.timeline_out)).clamp(0.0, 1.0);
    let highlight = doc.anim.highlight.as_ref().map_or(0.0, |h| h.span.0 + (h.span.1 - h.span.0) * frac);
    kadr_text::Clock { since_in: tt - clip.timeline_in, until_out: last - tt, source: clip.source_time_at(t), highlight }
}

/// The layer of text clip `clip` (its document `doc`) at `t`.
pub(crate) fn text_layer(clip: &Clip, doc: &TextDoc, t: Time, canvas: SizeU, frame: Time) -> Option<Layer> {
    let st = crate::scene::clip_state_at(clip, t, frame);
    if st.transform.scale <= 0.0 {
        return None;
    }
    let picture = kadr_text::TextFrame::new(doc, text_clock(clip, doc, t, frame));
    let b = picture.bounds();
    let (w, h) = (b.width(), b.height());
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    // The crop cuts fractions of the picture at rest: an animation that
    // widens the bounds does not move the crop's edges.
    let rest = if doc.is_animated() { kadr_text::TextFrame::new(doc, kadr_text::Clock::REST).bounds() } else { b };
    let crop = text_crop(&st.transform, b, rest)?;
    // The motion effects move the text block — the layout box, or a
    // curved text's placed glyphs with its centre (TX-R15), about the
    // layout centre; the crop's visible part of it is their window — so a
    // shadow, a plaque or an animated glyph never shifts a zoom's point or
    // scales a shake (TX-R7).
    let block = picture.block_box();
    let block = if block.width() > 0.0 && block.height() > 0.0 { block } else { b };
    let (bw, bh) = (block.width(), block.height());
    let mut base = crate::scene::placement_of(&st.transform, Vec2::new(bw, bh), canvas);
    base.anchor = Vec2::new(-block.x0 / bw, -block.y0 / bh);
    let window = {
        let f = |v: f32, o: f32, len: f32| (((v + o) / len) as f64).clamp(0.0, 1.0);
        let win = (f(crop.x0 + b.x0, -block.x0, bw), f(crop.y0 + b.y0, -block.y0, bh), f(crop.x1 + b.x0, -block.x0, bw), f(crop.y1 + b.y0, -block.y0, bh));
        if win.0 < win.2 && win.1 < win.3 { win } else { kadr_effects::CompileCtx::UNCROPPED }
    };
    let (moved, colour) = crate::fx::apply_effects(clip, t, canvas, base, window);
    // The whole picture where the moved block is, anchored at the layout
    // centre again (TX-R7): the preview's handles find the text there.
    let (cx, cy) = moved.to_canvas().apply(-block.x0 as f64, -block.y0 as f64);
    let placement = Placement { size: Vec2::new(w, h), anchor: Vec2::new(-b.x0 / w, -b.y0 / h), position: Vec2::new(cx as f32, cy as f32), scale: moved.scale, rotation: moved.rotation };
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

/// The crop of a picture with bounds `b` (local px): the transform's crop
/// fractions of the resting bounds `rest`, a side without crop reaching the
/// picture's own edge; `None` when nothing is left.
fn text_crop(tr: &Transform, b: Bounds, rest: Bounds) -> Option<RectF> {
    let (w, h) = (b.width(), b.height());
    if rest == b {
        return crate::scene::crop_of(tr, Vec2::new(w, h));
    }
    let r = crate::scene::crop_of(tr, Vec2::new(rest.width(), rest.height()))?;
    let (dx, dy) = (rest.x0 - b.x0, rest.y0 - b.y0);
    let side = |crop: f64, edge: f32, open: f32| if crop > 0.0 { edge } else { open };
    let c = RectF::new(side(tr.crop_left, r.x0 + dx, 0.0), side(tr.crop_top, r.y0 + dy, 0.0), side(tr.crop_right, r.x1 + dx, w), side(tr.crop_bottom, r.y1 + dy, h))
        .intersect(&RectF::new(0.0, 0.0, w, h));
    (!c.is_empty()).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4: the crop's fractions are of the bounds at rest; a side without
    /// crop follows the animated bounds (a letter flying in from outside
    /// is not cut).
    #[test]
    fn the_crop_is_measured_over_the_resting_bounds() {
        let rest = Bounds { x0: -100.0, y0: -50.0, x1: 100.0, y1: 50.0 };
        let wider = Bounds { x0: -150.0, y0: -50.0, x1: 100.0, y1: 80.0 };
        let mut tr = Transform::default();
        assert_eq!(text_crop(&tr, wider, rest), Some(RectF::new(0.0, 0.0, 250.0, 130.0)), "no crop: the whole picture");
        tr.crop_left = 0.25;
        assert_eq!(text_crop(&tr, wider, rest), Some(RectF::new(100.0, 0.0, 250.0, 130.0)), "a quarter of the resting width from its left edge");
        tr.crop_bottom = 0.5;
        assert_eq!(text_crop(&tr, wider, rest), Some(RectF::new(100.0, 0.0, 250.0, 50.0)));
        assert_eq!(text_crop(&tr, rest, rest), crate::scene::crop_of(&tr, Vec2::new(200.0, 100.0)), "at rest: the plain crop");
        tr.crop_left = 1.0;
        assert_eq!(text_crop(&tr, wider, rest), None, "nothing left");
    }
}
