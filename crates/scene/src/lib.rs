//! Kadr's evaluated scene (render spec §4): what one output frame shows, as
//! plain data. Produced by the timeline's scene evaluator, consumed by
//! renderers through kadr-render. No pixels, no I/O, no timeline, no FFmpeg.
//!
//! # Rendering contract (CPU and GPU renderers must match within ±1 LSB, spec §6)
//!
//! Every renderer turns a [`FrameScene`] into pixels by exactly these rules;
//! two independent implementations must agree within ±1 LSB per channel.
//!
//! ## Values and buffers
//!
//! - Working values are non-linear Rec.709 R'G'B' with **premultiplied**
//!   alpha, components in [0, 1]. Texels arrive already converted to this
//!   space by the resolver (source matrix, range and transfer per spec §6).
//! - [`Rgba`] values in the scene ([`LayerContent::Solid`],
//!   [`FrameScene::background`], [`TransitionOp::DipToColor`]) are
//!   **straight** alpha; the renderer premultiplies them:
//!   `(r·a, g·a, b·a, a)`.
//! - Buffers (the output and the transition buffers below) hold 8 bits per
//!   channel, premultiplied. A value `v` is stored as
//!   `floor(clamp(v, 0, 1) · 255 + 0.5)` (nearest, half up); it is read back
//!   as `stored / 255`. Arithmetic in between is f32 or better. Fusing steps
//!   (keeping an intermediate in float instead of storing it) is allowed
//!   only **within one layer** — sample → effects → opacity and coverage →
//!   composite — and the ±1 LSB tolerance covers that. Between layers
//!   nothing is fused: the output buffer is stored (rounded) after every
//!   top-level layer, and each transition buffer (`from`, `to`) is stored
//!   after its last layer, before the two are mixed.
//! - Straight-alpha sources (images, alpha video) are premultiplied per
//!   texel **before** filtering.
//!
//! ## Coordinates
//!
//! - Pixel `(i, j)` covers `[i, i+1) × [j, j+1)`; its centre is
//!   `(i + 0.5, j + 0.5)`. This holds in output, canvas, local-layer and
//!   source-texel spaces alike. Every output pixel is evaluated at its centre.
//! - Output ↔ canvas: one uniform scale `k = min(out.w / canvas.w,
//!   out.h / canvas.h)` and a centring offset
//!   `o = ((out.w − k·canvas.w) / 2, (out.h − k·canvas.h) / 2)`;
//!   `output = k · canvas + o`. When the output has the canvas's aspect (the
//!   normal case) `o = (0, 0)`. Output pixels whose centre lies outside the
//!   canvas rectangle `[o.x, o.x + k·canvas.w] × [o.y, o.y + k·canvas.h]` are
//!   margins: opaque black `(0, 0, 0, 1)`, nothing is drawn there.
//! - Canvas → local: `placement.to_canvas().inverse()` (a layer whose
//!   transform has no inverse draws nothing).
//! - Local → texel: the local content rectangle `[0, size.x] × [0, size.y]`
//!   maps onto the whole decoded frame `[0, tw] × [0, th]` (`tw × th` texels):
//!   `texel = (local.x · tw / size.x, local.y · th / size.y)` with
//!   `size = placement.size`. The decoded frame is upright (rotation metadata
//!   already applied); its size may differ from the display size (proxies,
//!   non-square samples), the per-axis scale absorbs that. The crop rectangle
//!   in texels is `crop` scaled the same way.
//!
//! ## Sampling
//!
//! One bilinear sample per output pixel, no mipmaps or area filtering. For
//! the texel point `(u, v)`: clamp `u` to `[kx0 + 0.5, kx1 − 0.5]` and `v` to
//! `[ky0 + 0.5, ky1 − 0.5]`, where the crop in whole texels is
//! `kx0 = min(floor(cx0 + 0.5), tw − 1)`,
//! `kx1 = min(max(floor(cx1 + 0.5), kx0 + 1), tw)` (same for y with `th`)
//! and `(cx0, cy0, cx1, cy1)` is the crop rectangle in texels **intersected
//! with the decoded frame** `[0, tw] × [0, th]` (so `0 ≤ kx0 < kx1 ≤ tw`:
//! at least one texel, never one outside the frame). Then with
//! `i = floor(u − 0.5)`, `f = (u − 0.5) − i`, `j = floor(v − 0.5)`,
//! `g = (v − 0.5) − j` the sample is the bilinear mix of texels `(i, j)`, `(i+1, j)`, `(i, j+1)`,
//! `(i+1, j+1)` with weights `(1−f)(1−g)`, `f(1−g)`, `(1−f)g`, `fg`; an index
//! outside `[kx0, kx1 − 1]` (or `[ky0, ky1 − 1]`) is clamped into it, which
//! also caps it at `tw − 1` (or `th − 1`). So
//! nothing outside the crop is ever read (no bleeding). A
//! [`LayerContent::Solid`] layer has no texels: its sample is its
//! premultiplied colour. A [`LayerContent::Media`] layer whose frame is
//! missing (offline media, a failed decode) is drawn exactly like a `Solid`
//! layer of [`Rgba::MISSING`] with the same placement, crop, opacity, blend
//! and effects; the scene itself never changes because of it.
//!
//! Effects (point operations, formulas in the effects spec «Примитивы цвета»)
//! apply to that sample in `effects` order, on unpremultiplied colour:
//! `rgb / a` → the effect chain (clamped to [0, 1] once, at its end) →
//! `× a`; a sample with `a = 0` stays transparent. This happens before
//! opacity and coverage.
//!
//! ## Layer edge coverage
//!
//! `coverage = clamp(0.5 + dist, 0, 1)`, where `dist` is the signed distance,
//! in **output** pixels and positive inside, from the output pixel centre to
//! the crop rectangle mapped into output space (`to_canvas`, then canvas →
//! output; always a rectangle, possibly rotated). `dist` is the minimum over the
//! rectangle's four edges of the signed distance to that edge's line
//! (positive on the inner side). Colour and alpha are multiplied by
//! coverage. An axis-aligned layer whose edges lie on output pixel
//! boundaries therefore has coverage exactly 1 inside and 0 outside
//! (consistent with `cull`'s "covers the canvas").
//!
//! ## Compositing
//!
//! Inside the canvas rectangle the output starts as the premultiplied
//! `background`; `layers` are composited bottom to top. Premultiplied, per
//! component **including alpha**, with `s = sample · opacity · coverage` and
//! `d` the destination:
//!
//! - Normal: `s + d·(1 − s.a)`
//! - Add: `min(s + d, 1)`
//! - Multiply: `s·d + s·(1 − d.a) + d·(1 − s.a)`
//! - Screen: `s + d − s·d`
//!
//! ## Transitions
//!
//! `from` and `to` are each rendered, bottom to top with the rules above,
//! into their own buffer on the output's pixel grid (same size), which
//! starts transparent `(0, 0, 0, 0)` everywhere (no background; margin
//! pixels stay transparent and are not drawn). The two are mixed per pixel into one
//! value, which is composited pixel for pixel (no resampling, coverage 1)
//! as a layer with `s = mixed · opacity` and the transition layer's `blend`;
//! the evaluator emits full-canvas placement and crop, opacity 1, Normal
//! and no effects for transition layers. With `p = progress`,
//! `lerp(a, b, t) = a + (b − a)·t` per premultiplied component, alpha
//! included:
//!
//! - Dissolve: `out = lerp(from, to, p)`.
//! - DipToColor(c): let `C(x) = (c.r·c.a, c.g·c.a, c.b·c.a, c.a) · x.a`,
//!   the premultiplied dip colour scaled by the alpha of buffer `x` (for the
//!   opaque colours the evaluator emits: `(c.r·x.a, c.g·x.a, c.b·x.a, x.a)`).
//!   For `p < 0.5`: `out = lerp(from, C(from), 2p)`; otherwise
//!   `out = lerp(C(to), to, 2p − 1)`. The colour therefore appears only where
//!   the clip has coverage: a picture-in-picture clip dips to a coloured
//!   rectangle and the track below is untouched.
//! - Wipe { angle, softness }: `u = (cos angle, sin angle)` in canvas pixels
//!   (y down). Project the four canvas corners `(0, 0)`, `(cw, 0)`,
//!   `(0, ch)`, `(cw, ch)` (`cw × ch` = canvas) onto `u`: `lo` = smallest,
//!   `hi` = largest `dot(corner, u)`. With `w = softness` (canvas pixels) the
//!   edge is at `E = lerp(lo − w/2, hi + w/2, p)`: the sweep is extended by
//!   half the band at both ends so `p = 0` shows only `from` and `p = 1` only
//!   `to`; for `w = 0` this is `lerp(lo, hi, p)`. For an output pixel whose
//!   centre is at canvas position `x`: `e = (E − dot(x, u)) · k` (in output
//!   pixels; positive on the side the edge has passed, where
//!   `dot(x, u) < E`), `w_out = w · k`,
//!   `m = clamp(0.5 + e / max(w_out, 1), 0, 1)`, `out = lerp(from, to, m)`.
//!   So with no softness the edge is anti-aliased over one output pixel,
//!   like a layer edge.
//! - The output alpha is whatever the formula yields; it is never forced
//!   opaque.

pub mod geom;
pub mod scene;

pub use geom::*;
pub use scene::*;
