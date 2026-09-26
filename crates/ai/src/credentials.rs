//! API keys live in the OS credential store (Windows Credential Manager),
//! never in project files, settings JSON or logs.

use std::fmt;

const SERVICE: &str = "Kadr AI";

/// A secret whose `Debug`/`Display` never reveal the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}
impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

pub fn store_key(provider_id: &str, key: &Secret) -> Result<(), String> {
    keyring::Entry::new(SERVICE, provider_id)
        .and_then(|e| e.set_password(key.expose()))
        .map_err(|e| format!("credential store: {e}"))
}

pub fn load_key(provider_id: &str) -> Option<Secret> {
    keyring::Entry::new(SERVICE, provider_id).and_then(|e| e.get_password()).ok().map(Secret)
}

pub fn delete_key(provider_id: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, provider_id)
        .and_then(|e| e.delete_credential())
        .map_err(|e| format!("credential store: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_redacted() {
        let s = Secret::new("sk-live-123");
        assert_eq!(format!("{s:?} {s}"), "Secret(***) ***");
    }
}
