//! "No hidden AI calls": every outbound request passes through
//! [`gate`], which decides from the user's policy alone.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    /// No AI at all, not even local intent routing to plans.
    Off,
    /// Local algorithms/models only. The default: nothing leaves the machine.
    #[default]
    LocalOnly,
    /// Cloud allowed, but each request needs confirmation.
    AskBeforeCloud,
    /// Providers marked allowed may be used without per-request prompts
    /// (budget limits still apply).
    AllowSelected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataKind {
    Text,
    Images,
    Audio,
    Video,
}

/// Per-provider data permissions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DataPermissions {
    pub allowed: bool,
    pub text: bool,
    pub images: bool,
    pub audio: bool,
    pub video: bool,
}

impl DataPermissions {
    pub fn permits(&self, k: DataKind) -> bool {
        match k {
            DataKind::Text => self.text,
            DataKind::Images => self.images,
            DataKind::Audio => self.audio,
            DataKind::Video => self.video,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateDecision {
    Allow,
    /// Allowed only after the user confirms this specific request.
    Ask,
    /// Reason as an i18n key (`privacy.deny.*`).
    Deny(String),
}

pub fn gate(mode: PrivacyMode, provider_is_local: bool, perms: &DataPermissions, kinds: &[DataKind]) -> GateDecision {
    if mode == PrivacyMode::Off {
        return GateDecision::Deny("privacy.deny.off".into());
    }
    if provider_is_local {
        // A local model (e.g. llama.cpp on localhost) never leaves the machine.
        return GateDecision::Allow;
    }
    if let Some(k) = kinds.iter().find(|k| !perms.permits(**k)) {
        return GateDecision::Deny(format!("privacy.deny.kind.{}", format!("{k:?}").to_lowercase()));
    }
    match mode {
        PrivacyMode::Off => unreachable!(),
        PrivacyMode::LocalOnly => GateDecision::Deny("privacy.deny.local_only".into()),
        PrivacyMode::AskBeforeCloud => GateDecision::Ask,
        PrivacyMode::AllowSelected if perms.allowed => GateDecision::Allow,
        PrivacyMode::AllowSelected => GateDecision::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_only_and_denies_cloud() {
        let p = DataPermissions { allowed: true, text: true, ..Default::default() };
        assert!(matches!(gate(PrivacyMode::default(), false, &p, &[DataKind::Text]), GateDecision::Deny(_)));
        assert_eq!(gate(PrivacyMode::default(), true, &p, &[DataKind::Text]), GateDecision::Allow);
    }

    #[test]
    fn per_kind_permissions() {
        let p = DataPermissions { allowed: true, text: true, ..Default::default() };
        assert_eq!(gate(PrivacyMode::AllowSelected, false, &p, &[DataKind::Text]), GateDecision::Allow);
        assert!(matches!(gate(PrivacyMode::AllowSelected, false, &p, &[DataKind::Text, DataKind::Images]), GateDecision::Deny(_)));
        assert_eq!(gate(PrivacyMode::AskBeforeCloud, false, &p, &[DataKind::Text]), GateDecision::Ask);
        assert!(matches!(gate(PrivacyMode::Off, true, &p, &[]), GateDecision::Deny(_)));
    }
}
