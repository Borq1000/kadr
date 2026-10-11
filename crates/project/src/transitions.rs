//! Transitions on cuts (effects spec §2, D1, D2): a recipe of `kadr-effects`
//! (`Target::Cut`) with its parameters, anchored to the pair of clips that
//! meet at the cut — the outgoing `from` and the incoming `to` — and a
//! length the evaluator clamps to both. Everything here is data and its
//! consistency: a transition follows its clips (`reconcile_transitions`),
//! what it looks like is the recipe's. A recipe, version, parameter or field
//! this build does not know is kept verbatim (spec §2).

use crate::{ParamValue, Sequence, Track, TrackKind};
use kadr_core::{ClipId, Time, TrackId, TransitionId};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

fn one() -> u16 {
    1
}

/// The three kinds Kadr wrote before E3 (format 3 kept them as `kind`):
/// understood when a file is loaded and from MCP / AI clients, never written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    CrossDissolve,
    DipToBlack,
    Wipe,
}

impl TransitionKind {
    /// The recipe and parameters a kind became (D1, spec §2) — drawing the
    /// picture the kind drew: `cross_dissolve` → `clean_fade {style: clean}`,
    /// `dip_to_black` → `fade_through_color {color: black}`, `wipe` →
    /// `wipe {direction: left, softness: 0}`.
    pub fn recipe(self) -> (&'static str, BTreeMap<String, ParamValue>) {
        let choice = |k: &str, v: &str| (k.to_string(), ParamValue::Choice(v.to_string()));
        match self {
            TransitionKind::CrossDissolve => ("clean_fade", BTreeMap::from([choice("style", "clean")])),
            TransitionKind::DipToBlack => ("fade_through_color", BTreeMap::from([choice("color", "black")])),
            TransitionKind::Wipe => ("wipe", BTreeMap::from([choice("direction", "left"), ("softness".to_string(), ParamValue::Number(0.0))])),
        }
    }
}

/// A transition on the cut between `from` and `to` (D1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    /// Registry id of a cut recipe, e.g. "clean_fade" (`kadr-effects`).
    pub recipe: String,
    /// The recipe's `compile` version this transition was made with.
    #[serde(default = "one")]
    pub version: u16,
    #[serde(default)]
    pub params: BTreeMap<String, ParamValue>,
    /// The outgoing clip: the cut is its `timeline_out`.
    pub from: ClipId,
    /// The incoming clip, starting at the cut.
    pub to: ClipId,
    /// As set; the evaluator clamps it to both clips.
    pub duration: Time,
    /// Fields from a newer Kadr, kept verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Transition {
    pub fn new(recipe: impl Into<String>, version: u16, params: BTreeMap<String, ParamValue>, from: ClipId, to: ClipId, duration: Time) -> Transition {
        Transition { id: TransitionId::new(), recipe: recipe.into(), version, params, from, to, duration, extra: BTreeMap::new() }
    }

    /// A transition of an old `kind` on the cut `from` | `to` (D1).
    pub fn legacy(kind: TransitionKind, from: ClipId, to: ClipId, duration: Time) -> Transition {
        let (recipe, params) = kind.recipe();
        Transition::new(recipe, 1, params, from, to, duration)
    }
}

/// `Sequence::transitions` from a file; never fails the load. An entry
/// that is not a transition — not an object, a missing or mistyped field,
/// an old `kind` the migration could not place — is dropped with a warning,
/// and so is a later one with an id already seen.
pub fn deserialize_transitions<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Transition>, D::Error> {
    let Value::Array(items) = Value::deserialize(d)? else {
        tracing::warn!("ignoring sequence transitions that are not a list");
        return Ok(vec![]);
    };
    let mut out: Vec<Transition> = Vec::with_capacity(items.len());
    for item in items {
        match serde_json::from_value::<Transition>(item) {
            Ok(t) if out.iter().any(|x| x.id == t.id) => tracing::warn!(id = %t.id, "dropping a duplicate transition"),
            Ok(t) => out.push(t),
            Err(err) => tracing::warn!(error = %err, "dropping an unreadable transition"),
        }
    }
    Ok(out)
}

impl Sequence {
    /// The one cut of `track` within `slack` of `at` — a clip and the next
    /// one meeting there — as `(from, to, cut)`; `None` when there is none
    /// or more than one (MCP and AI name a cut by a millisecond time, D9).
    pub fn cut_near(&self, track: TrackId, at: Time, slack: Time) -> Option<(ClipId, ClipId, Time)> {
        let t = self.track(track)?;
        let mut cuts = t.clips.windows(2).filter(|w| w[0].timeline_out == w[1].timeline_in && (w[0].timeline_out - at).abs() <= slack);
        match (cuts.next(), cuts.next()) {
            (Some(w), None) => Some((w[0].id, w[1].id, w[0].timeline_out)),
            _ => None,
        }
    }

    pub fn transition(&self, id: TransitionId) -> Option<&Transition> {
        self.transitions.iter().find(|t| t.id == id)
    }

    /// Where `tr` is: its track's index and its cut — `None` unless its
    /// clips meet on one video track (`from` right before `to`).
    pub fn transition_cut(&self, tr: &Transition) -> Option<(usize, Time)> {
        let (ti, ci) = self.locate_clip(tr.from)?;
        let t = &self.tracks[ti];
        let next = t.clips.get(ci + 1).filter(|n| n.id == tr.to && n.timeline_in == t.clips[ci].timeline_out)?;
        (t.kind == TrackKind::Video).then_some((ti, next.timeline_in))
    }

    /// The longest `tr` can be: the shorter of its two clips (`None` where it has no cut).
    pub fn transition_limit(&self, tr: &Transition) -> Option<Time> {
        self.transition_cut(tr)?;
        Some(self.clip(tr.from)?.duration().min(self.clip(tr.to)?.duration()))
    }

    /// Keeps every transition on its cut after the clips changed (D2,
    /// refinement 10): one stays while `to` is on a video track and the clip
    /// now ending where `to` starts is `from` — or a piece `from` was split
    /// into, reached from it through pieces each continuing the one before
    /// (same medium, next timeline and source time) — to which it is
    /// re-anchored; one transition per cut (the first). The rest are removed
    /// and returned. A switched-off clip keeps its transition.
    pub fn reconcile_transitions(&mut self) -> Vec<Transition> {
        let mut dropped = vec![];
        let mut cuts: Vec<ClipId> = vec![];
        let tracks = &self.tracks;
        self.transitions.retain_mut(|tr| match follow(tracks, tr.from, tr.to) {
            Some(from) if !cuts.contains(&tr.to) => {
                cuts.push(tr.to);
                tr.from = from;
                true
            }
            _ => {
                dropped.push(tr.clone());
                false
            }
        });
        dropped
    }
}

/// The clip ending where `to` starts, reached from `from` through the
/// pieces a split cut `from` into; `None` when `to` is gone or not on a
/// video track, or the chain breaks before the cut.
fn follow(tracks: &[Track], from: ClipId, to: ClipId) -> Option<ClipId> {
    let track = tracks.iter().find(|t| t.clips.iter().any(|c| c.id == to))?;
    if track.kind != TrackKind::Video {
        return None;
    }
    let cut = track.clips.iter().find(|c| c.id == to)?.timeline_in;
    let mut i = track.clips.iter().position(|c| c.id == from)?;
    loop {
        let c = &track.clips[i];
        if c.timeline_out == cut {
            return Some(c.id);
        }
        let next = track.clips.get(i + 1)?;
        if next.timeline_in != c.timeline_out || next.asset != c.asset || next.source_in != c.source_out {
            return None;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clip;
    use kadr_core::{AssetId, FrameRate, TimeRange};

    fn s(x: i64) -> Time {
        Time::from_secs(x)
    }

    /// V1: a 0..4 s, b 4..8 s, c 8..12 s (each its own medium), A1 empty;
    /// a dissolve on a|b, a wipe on b|c.
    fn seq3() -> (Sequence, [ClipId; 3]) {
        let mut seq = Sequence::new("s", FrameRate::FPS_25, 1920, 1080);
        let clip = |a: i64, b: i64| Clip::new(AssetId::new(), "c", TimeRange::new(s(100 + a), s(100 + b)), s(a));
        let cs = [clip(0, 4), clip(4, 8), clip(8, 12)];
        let ids = cs.each_ref().map(|c| c.id);
        seq.tracks[0].clips.extend(cs);
        seq.transitions = vec![Transition::legacy(TransitionKind::CrossDissolve, ids[0], ids[1], s(1)), Transition::legacy(TransitionKind::Wipe, ids[1], ids[2], s(1))];
        (seq, ids)
    }

    /// A split as the engine makes it: the left half keeps the id, the
    /// right one continues it (same medium, next timeline and source time).
    fn split(seq: &mut Sequence, id: ClipId, at: Time) -> ClipId {
        let (ti, ci) = seq.locate_clip(id).unwrap();
        let mut right = seq.tracks[ti].clips[ci].clone();
        let left = &mut seq.tracks[ti].clips[ci];
        let src = left.source_time_at(at);
        (left.timeline_out, left.source_out) = (at, src);
        right.id = ClipId::new();
        (right.timeline_in, right.source_in) = (at, src);
        let rid = right.id;
        seq.tracks[ti].clips.insert(ci + 1, right);
        rid
    }

    #[test]
    fn a_split_of_from_re_anchors_to_the_piece_at_the_cut_and_one_of_to_keeps_it() {
        let (mut seq, [a, b, _]) = seq3();
        let a2 = split(&mut seq, a, s(2));
        let a3 = split(&mut seq, a2, s(3));
        assert!(seq.reconcile_transitions().is_empty());
        assert_eq!((seq.transitions[0].from, seq.transitions[0].to), (a3, b), "the piece that now ends at the cut");
        let b2 = split(&mut seq, b, s(6));
        assert!(seq.reconcile_transitions().is_empty());
        assert_eq!((seq.transitions[0].to, seq.transitions[1].from), (b, b2), "b keeps its start, and b|c follows b's right half");
    }

    #[test]
    fn a_pair_that_no_longer_meets_is_dropped_and_a_disabled_one_stays() {
        let (mut seq, _) = seq3();
        seq.tracks[0].clips[0].timeline_out = s(3);
        assert_eq!(seq.reconcile_transitions().len(), 1, "a gap at the cut");
        assert_eq!((seq.transitions.len(), seq.transitions[0].to), (1, seq.tracks[0].clips[2].id), "b|c stays");
        let (mut seq, [_, b, _]) = seq3();
        seq.tracks[0].clips.retain(|x| x.id != b);
        assert_eq!(seq.reconcile_transitions().len(), 2, "both cuts of a deleted clip go");
        let (mut seq, [a, ..]) = seq3();
        let a2 = split(&mut seq, a, s(2));
        seq.tracks[0].clips.retain(|x| x.id != a2);
        assert_eq!(seq.reconcile_transitions().len(), 1, "a piece of `from` removed: a gap again");
        let (mut seq, _) = seq3();
        seq.tracks[1].clips = std::mem::take(&mut seq.tracks[0].clips);
        assert_eq!(seq.reconcile_transitions().len(), 2, "no transitions on sound (D11)");
        let (mut seq, _) = seq3();
        seq.tracks[0].clips[0].enabled = false;
        assert!(seq.reconcile_transitions().is_empty(), "a switched-off clip keeps its transition (it draws nothing while off)");
    }

    #[test]
    fn one_transition_per_cut_and_the_lookups() {
        let (mut seq, [a, b, c]) = seq3();
        let extra = Transition::legacy(TransitionKind::DipToBlack, a, b, s(2));
        seq.transitions.push(extra.clone());
        assert_eq!(seq.reconcile_transitions(), vec![extra], "the first on a cut stays");
        let t = seq.transitions[1].clone();
        assert_eq!(seq.transition(t.id), Some(&t));
        assert_eq!((seq.transition_cut(&t), seq.transition_limit(&t)), (Some((0, s(8))), Some(s(4))));
        let stale = Transition::legacy(TransitionKind::Wipe, c, a, s(1));
        assert_eq!((seq.transition_cut(&stale), seq.transition_limit(&stale)), (None, None), "c and a do not meet");
    }

    #[test]
    fn a_cut_is_named_by_a_time_within_slack_only_when_it_is_the_one() {
        let (seq, [a, b, _]) = seq3();
        let track = seq.tracks[0].id;
        assert_eq!(seq.cut_near(track, s(4), Time::ZERO), Some((a, b, s(4))));
        let half = Time(FrameRate::FPS_25.frame_duration().flicks() / 2);
        assert_eq!(seq.cut_near(track, s(4) + Time(half.flicks() - 1), half).map(|c| c.2), Some(s(4)));
        assert_eq!(seq.cut_near(track, s(4) + half + Time(1), half), None, "more than half a frame away");
        assert_eq!(seq.cut_near(track, s(6), s(3)), None, "two cuts within reach: neither is named");
        assert_eq!(seq.cut_near(seq.tracks[1].id, s(4), half), None, "the audio track has no cut");
    }

    #[test]
    fn the_old_kinds_name_their_recipes() {
        let (r, p) = TransitionKind::Wipe.recipe();
        assert_eq!((r, p["direction"].clone(), p["softness"].clone()), ("wipe", ParamValue::Choice("left".into()), ParamValue::Number(0.0)));
        assert_eq!(TransitionKind::CrossDissolve.recipe(), ("clean_fade", BTreeMap::from([("style".to_string(), ParamValue::Choice("clean".into()))])));
        assert_eq!(TransitionKind::DipToBlack.recipe().1["color"], ParamValue::Choice("black".into()));
        let t = Transition::legacy(TransitionKind::DipToBlack, ClipId::new(), ClipId::new(), s(1));
        assert_eq!((t.recipe.as_str(), t.version, t.extra.is_empty()), ("fade_through_color", 1, true));
    }

    #[derive(serde::Deserialize)]
    struct Holder {
        #[serde(deserialize_with = "deserialize_transitions")]
        transitions: Vec<Transition>,
    }

    #[test]
    fn transitions_load_leniently_and_keep_ids_unique() {
        let id = TransitionId::new();
        let ok = |id: TransitionId| serde_json::json!({"id": id, "recipe": "wipe", "from": ClipId::new(), "to": ClipId::new(), "duration": 100});
        let list = serde_json::json!([7, {"recipe": "x"}, ok(id), ok(id), {"id": TransitionId::new(), "recipe": "a", "from": "nope", "to": ClipId::new(), "duration": 1}]);
        let h: Holder = serde_json::from_value(serde_json::json!({ "transitions": list })).unwrap();
        assert_eq!(h.transitions.len(), 1, "the first of an id wins, the unreadable go");
        assert_eq!((h.transitions[0].version, h.transitions[0].params.is_empty()), (1, true), "defaults");
        for bad in [serde_json::json!(null), serde_json::json!("x"), serde_json::json!({"a": 1})] {
            let h: Holder = serde_json::from_value(serde_json::json!({ "transitions": bad })).unwrap();
            assert!(h.transitions.is_empty());
        }
    }
}
