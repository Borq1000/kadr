//! Strongly-typed ids. Serialized as plain UUID strings.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! define_id {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                $name(Uuid::new_v4())
            }
            pub fn parse(s: &str) -> Option<Self> {
                Uuid::parse_str(s).ok().map($name)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Short form keeps logs readable.
                write!(f, concat!(stringify!($name), "({})"), &self.0.simple().to_string()[..8])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    )*};
}

define_id!(
    ProjectId, AssetId, BinId, SequenceId, TrackId, ClipId, LinkId, TransitionId, EffectId,
    MarkerId, TranscriptId, ActionId, MulticamId,
);
