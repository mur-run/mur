//! Credential storage for adapters.
//!
//! We abstract over the OS keyring so unit tests don't need Keychain /
//! Secret Service. Production uses `OsKeyring`; tests use `InMemoryCreds`.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait CredentialStore: Send + Sync {
    /// Store `value` under `(service, account)`.
    fn set(&self, service: &str, account: &str, value: &str) -> Result<()>;
    /// Retrieve `(service, account)` or return `Ok(None)` if not present.
    fn get(&self, service: &str, account: &str) -> Result<Option<String>>;
    /// Delete if present; no error if absent.
    fn delete(&self, service: &str, account: &str) -> Result<()>;
}

/// Production implementation backed by the OS keyring (macOS Keychain,
/// Linux Secret Service via libsecret, Windows Credential Manager).
pub struct OsKeyring;

impl CredentialStore for OsKeyring {
    fn set(&self, service: &str, account: &str, value: &str) -> Result<()> {
        let entry = keyring::Entry::new(service, account)
            .with_context(|| format!("open keyring entry {service}:{account}"))?;
        entry
            .set_password(value)
            .with_context(|| format!("set keyring entry {service}:{account}"))?;
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(service, account)
            .with_context(|| format!("open keyring entry {service}:{account}"))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("get keyring entry {service}:{account}")),
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let entry = keyring::Entry::new(service, account)
            .with_context(|| format!("open keyring entry {service}:{account}"))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete keyring entry {service}:{account}")),
        }
    }
}

/// In-memory implementation for tests.
#[derive(Default)]
pub struct InMemoryCreds {
    store: Mutex<HashMap<(String, String), String>>,
}

impl CredentialStore for InMemoryCreds {
    fn set(&self, service: &str, account: &str, value: &str) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .insert((service.into(), account.into()), value.into());
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .get(&(service.into(), account.into()))
            .cloned())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .remove(&(service.into(), account.into()));
        Ok(())
    }
}

/// Canonical helper: derive the keyring account name from source id + field.
pub fn account(source_id: &str, field: &str) -> String {
    format!("{source_id}:{field}")
}

pub const SERVICE: &str = "mur";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_roundtrip() {
        let c = InMemoryCreds::default();
        c.set("mur", "notion:work:access_token", "secret-123").unwrap();
        assert_eq!(
            c.get("mur", "notion:work:access_token").unwrap().as_deref(),
            Some("secret-123")
        );
        c.delete("mur", "notion:work:access_token").unwrap();
        assert_eq!(c.get("mur", "notion:work:access_token").unwrap(), None);
    }

    #[test]
    fn in_memory_missing_returns_none() {
        let c = InMemoryCreds::default();
        assert_eq!(c.get("mur", "nope").unwrap(), None);
    }

    #[test]
    fn account_helper_formats_canonically() {
        assert_eq!(account("notion:work", "access_token"), "notion:work:access_token");
    }
}
