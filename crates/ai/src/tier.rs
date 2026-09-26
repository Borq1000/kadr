use serde::{Deserialize, Serialize};

/// Explicit AI levels. Every request is tagged with one, and the UI shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Local algorithms/models. Always $0, never leaves the machine.
    Local,
    /// Very cheap models (and Jev) for bulk classification/ranking.
    Economy,
    /// A regular LLM for harder analysis.
    Smart,
    /// Strong reasoning model; only for whole-story tasks.
    Director,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Local => "LOCAL",
            Tier::Economy => "ECONOMY",
            Tier::Smart => "SMART",
            Tier::Director => "DIRECTOR",
        }
    }
    pub fn is_cloud(self) -> bool {
        self != Tier::Local
    }
    pub fn cheaper(self) -> Option<Tier> {
        match self {
            Tier::Director => Some(Tier::Smart),
            Tier::Smart => Some(Tier::Economy),
            Tier::Economy => Some(Tier::Local),
            Tier::Local => None,
        }
    }
}
