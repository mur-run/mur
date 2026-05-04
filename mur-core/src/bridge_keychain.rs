//! Track C2 — bridge keychain abstraction.
//!
//! `Keychain` is the put/get trait the Telegram (and future) bridge setup
//! flow uses to stash provider secrets such as bot tokens. `SystemKeychain`
//! delegates to the OS-native secret store via the `keyring` crate (macOS
//! Keychain, Windows Credential Manager, Linux Secret Service / dbus).
//! `MockKeychain` is an in-memory `HashMap` used by the integration tests so
//! CI never has to touch a real keychain backend.
//!
//! The account-name convention is `{bridge_id}/{KEY}` (e.g.
//! `tg-bridge-1/telegram_bot_token`); the service is fixed to `mur-agent` to
//! line up with `mur agent secret` and the runtime's lookup behaviour.

use std::collections::HashMap;
use std::sync::Mutex;

/// Abstract put/get for an OS-keychain-backed secret store. Implemented by
/// `SystemKeychain` (real backend) and `MockKeychain` (test double).
pub trait Keychain: Send + Sync {
    fn put(&self, account: &str, secret: &str) -> anyhow::Result<()>;
    /// Read a secret. Used by tests and by the runtime poller (M-c2.2+);
    /// the bin target's CLI flow only uses `put`, hence the allow.
    #[allow(dead_code)]
    fn get(&self, account: &str) -> anyhow::Result<String>;
}

/// Real OS-keychain-backed implementation. Service is fixed to `mur-agent`.
pub struct SystemKeychain;

impl Keychain for SystemKeychain {
    fn put(&self, account: &str, secret: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new("mur-agent", account)?;
        entry.set_password(secret)?;
        Ok(())
    }
    fn get(&self, account: &str) -> anyhow::Result<String> {
        let entry = keyring::Entry::new("mur-agent", account)?;
        Ok(entry.get_password()?)
    }
}

/// In-memory `Keychain` test double. Deterministic and CI-safe; use this in
/// integration tests via `MUR_TELEGRAM_KEYCHAIN_BACKEND=mock`.
#[derive(Default)]
pub struct MockKeychain {
    inner: Mutex<HashMap<String, String>>,
}

impl Keychain for MockKeychain {
    fn put(&self, account: &str, secret: &str) -> anyhow::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert(account.into(), secret.into());
        Ok(())
    }
    fn get(&self, account: &str) -> anyhow::Result<String> {
        self.inner
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("keychain: no entry for {}", account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_keychain_round_trip() {
        let kc = MockKeychain::default();
        kc.put("agent-1/telegram_bot_token", "secret-token")
            .unwrap();
        assert_eq!(
            kc.get("agent-1/telegram_bot_token").unwrap(),
            "secret-token"
        );
    }

    #[test]
    fn mock_keychain_missing_errors() {
        let kc = MockKeychain::default();
        assert!(kc.get("nope").is_err());
    }
}
