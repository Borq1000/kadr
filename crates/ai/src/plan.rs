//! Two-phase AI edits: a [`Plan`] describes what was found and what will
//! change (with cost and confidence); only an explicit Apply executes it.

use crate::command::AiCommand;
use crate::tier::Tier;
use kadr_core::{ActionId, Time};

#[derive(Clone, Debug, PartialEq)]
pub struct PlanItem {
    pub label: String,
    pub start: Time,
    pub end: Time,
    /// Toggled in Review; disabled items are not applied.
    pub enabled: bool,
    pub command: AiCommand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub id: ActionId,
    pub prompt: String,
    pub title: String,
    /// What was detected ("Обнаружено 43 паузы…").
    pub findings: Vec<String>,
    /// What will be done.
    pub proposal: String,
    pub items: Vec<PlanItem>,
    pub tier: Tier,
    pub provider: String,
    pub model: String,
    pub cost_usd: f64,
    pub confidence: f32,
    /// Estimated resulting sequence duration, if relevant.
    pub before_duration: Option<Time>,
    pub after_duration: Option<Time>,
}

impl Plan {
    pub fn enabled_commands(&self) -> Vec<AiCommand> {
        self.items.iter().filter(|i| i.enabled).map(|i| i.command.clone()).collect()
    }
    pub fn enabled_count(&self) -> usize {
        self.items.iter().filter(|i| i.enabled).count()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
