//! Sequence / track / clip model. Everything here is plain data; the edit
//! rules (ripple, overlap resolution, undo) live in `kadr-timeline`.

use kadr_core::{
    AssetId, ClipId, EffectId, FrameRate, LinkId, MarkerId, SequenceId, Time, TimeRange, TrackId,
    TransitionId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub frame_rate: FrameRate,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    /// Video tracks first (V1 = bottom), then audio tracks (A1 = top).
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    #[serde(default)]
    pub in_out: Option<TimeRange>,
}

impl Sequence {
    pub fn new(name: impl Into<String>, frame_rate: FrameRate, width: u32, height: u32) -> Self {
        Sequence {
            id: SequenceId::new(),
            name: name.into(),
            frame_rate,
            width,
            height,
            sample_rate: 48_000,
            tracks: vec![
                Track::new(TrackKind::Video, "V1"),
                Track::new(TrackKind::Audio, "A1"),
            ],
            markers: vec![],
            transitions: vec![],
            in_out: None,
        }
    }

    /// End of the last clip on any track.
    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .filter_map(|t| t.clips.last().map(|c| c.timeline_out))
            .max()
            .unwrap_or(Time::ZERO)
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Returns `(track index, clip index)` of the clip.
    pub fn locate_clip(&self, id: ClipId) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(ti, t)| {
            t.clips.iter().position(|c| c.id == id).map(|ci| (ti, ci))
        })
    }
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.locate_clip(id).map(|(t, c)| &self.tracks[t].clips[c])
    }

    pub fn tracks_of(&self, kind: TrackKind) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(move |t| t.kind == kind)
    }

    /// Clips that share a link group with `id` (linked A/V), excluding `id`.
    pub fn linked_clips(&self, id: ClipId) -> Vec<ClipId> {
        let Some(link) = self.clip(id).and_then(|c| c.link) else {
            return vec![];
        };
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.link == Some(link) && c.id != id)
            .map(|c| c.id)
            .collect()
    }

    pub fn any_solo(&self, kind: TrackKind) -> bool {
        self.tracks.iter().any(|t| t.kind == kind && t.solo)
    }

    /// Whether a track contributes to playback/export after mute/solo.
    pub fn track_audible(&self, t: &Track) -> bool {
        !t.muted && (!self.any_solo(t.kind) || t.solo)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub locked: bool,
    /// Sorted by `timeline_in`, never overlapping (enforced by the engine).
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Track { id: TrackId::new(), kind, name: name.into(), muted: false, solo: false, locked: false, clips: vec![] }
    }

    pub fn clip_at(&self, t: Time) -> Option<&Clip> {
        // Clips are sorted: binary search on timeline_in.
        let idx = self.clips.partition_point(|c| c.timeline_in <= t);
        idx.checked_sub(1).map(|i| &self.clips[i]).filter(|c| t < c.timeline_out)
    }

    pub fn is_sorted_and_disjoint(&self) -> bool {
        self.clips.windows(2).all(|w| w[0].timeline_out <= w[1].timeline_in)
            && self.clips.iter().all(|c| c.timeline_in < c.timeline_out && c.source_in < c.source_out)
    }
}

/// A reference to a range of a media asset placed on a track.
///
/// Speed is *derived*: `(source_out - source_in) / (timeline_out - timeline_in)`.
/// Storing all four endpoints (as the spec requires) and deriving speed means
/// there is no redundant field that could disagree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub asset: AssetId,
    pub name: String,
    pub source_in: Time,
    pub source_out: Time,
    pub timeline_in: Time,
    pub timeline_out: Time,
    /// Clips with the same link id (typically video + its audio) move,
    /// trim, split and delete together.
    #[serde(default)]
    pub link: Option<LinkId>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default)]
    pub color: ColorAdjust,
    #[serde(default)]
    pub audio: AudioProps,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
    /// Set when this clip is an angle of a multicam group; switching camera
    /// swaps `asset`/source range using the group's sync offsets.
    #[serde(default)]
    pub multicam: Option<MulticamSelection>,
}

fn yes() -> bool {
    true
}

/// Clips on audio tracks are regular clips whose `audio` props apply.
pub type AudioClip = Clip;

impl Clip {
    pub fn new(asset: AssetId, name: impl Into<String>, source: TimeRange, timeline_in: Time) -> Self {
        Clip {
            id: ClipId::new(),
            asset,
            name: name.into(),
            source_in: source.start,
            source_out: source.end,
            timeline_in,
            timeline_out: timeline_in + source.duration(),
            link: None,
            enabled: true,
            transform: Transform::default(),
            color: ColorAdjust::default(),
            audio: AudioProps::default(),
            effects: vec![],
            keyframes: vec![],
            multicam: None,
        }
    }

    pub fn timeline_range(&self) -> TimeRange {
        TimeRange::new(self.timeline_in, self.timeline_out)
    }
    pub fn source_range(&self) -> TimeRange {
        TimeRange::new(self.source_in, self.source_out)
    }
    pub fn duration(&self) -> Time {
        self.timeline_out - self.timeline_in
    }
    pub fn speed(&self) -> f64 {
        (self.source_out - self.source_in).as_secs_f64() / self.duration().as_secs_f64()
    }

    /// Maps a timeline time inside the clip to the source time, exactly.
    pub fn source_time_at(&self, t: Time) -> Time {
        let src = (self.source_out - self.source_in).flicks();
        let tl = self.duration().flicks();
        self.source_in + (t - self.timeline_in).mul_ratio(src, tl)
    }

    /// Timeline time at which source time `s` is shown.
    pub fn timeline_time_of(&self, s: Time) -> Time {
        let src = (self.source_out - self.source_in).flicks();
        let tl = self.duration().flicks();
        self.timeline_in + (s - self.source_in).mul_ratio(tl, src)
    }

    /// Moves the clip so it starts at `t`, keeping duration.
    pub fn move_to(&mut self, t: Time) {
        let d = self.duration();
        self.timeline_in = t;
        self.timeline_out = t + d;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Offset from centre in sequence pixels.
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub rotation_deg: f64,
    pub crop_left: f64,
    pub crop_right: f64,
    pub crop_top: f64,
    pub crop_bottom: f64,
    pub opacity: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation_deg: 0.0,
            crop_left: 0.0,
            crop_right: 0.0,
            crop_top: 0.0,
            crop_bottom: 0.0,
            opacity: 1.0,
        }
    }
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        *self == Transform::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorAdjust {
    pub exposure: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub temperature: f64,
}

impl Default for ColorAdjust {
    fn default() -> Self {
        ColorAdjust { exposure: 0.0, contrast: 1.0, saturation: 1.0, temperature: 0.0 }
    }
}

impl ColorAdjust {
    pub fn is_identity(&self) -> bool {
        *self == ColorAdjust::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioProps {
    pub gain_db: f64,
    /// -1 (left) .. 1 (right).
    pub pan: f64,
    pub fade_in: Time,
    pub fade_out: Time,
}

impl Default for AudioProps {
    fn default() -> Self {
        AudioProps { gain_db: 0.0, pan: 0.0, fade_in: Time::ZERO, fade_out: Time::ZERO }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    /// Registry key, e.g. "blur", "stabilize".
    pub kind: String,
    pub enabled: bool,
    pub params: BTreeMap<String, f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    Linear,
    Hold,
    EaseInOut,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Property path, e.g. "transform.scale", "audio.gain_db".
    pub property: String,
    /// Relative to the clip's timeline_in.
    pub time: Time,
    pub value: f64,
    pub interpolation: Interpolation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    CrossDissolve,
    DipToBlack,
    Wipe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    pub kind: TransitionKind,
    pub track: TrackId,
    /// The cut point the transition is centred on.
    pub at: Time,
    pub duration: Time,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub time: Time,
    #[serde(default)]
    pub duration: Time,
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// RGB hex, e.g. "#e5a50a".
    pub color: String,
}

impl Marker {
    pub fn new(time: Time, name: impl Into<String>) -> Self {
        Marker { id: MarkerId::new(), time, duration: Time::ZERO, name: name.into(), note: String::new(), color: "#e5a50a".into() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamSelection {
    pub group: kadr_core::MulticamId,
    pub angle: u32,
}
