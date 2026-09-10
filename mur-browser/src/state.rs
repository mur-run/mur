//! Encrypted Playwright storage-state files for browser profiles.
//!
//! The age identity is kept in the OS Keychain, never beside the encrypted
//! state.  The on-disk `state.json.age` file is always mode 0600.

use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, bail};
use std::{
    io::{Cursor, Read, Write},
    path::Path,
};

use crate::broker::KEYCHAIN_SERVICE;

/// Keychain account holding the age X25519 identity for all browser profiles.
pub const STATE_KEY_ACCOUNT: &str = "browser/state-key";

/// Narrow seam for tests; production uses [`KeychainStateKeyStore`].
pub trait StateKeyStore {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> Result<()>;
}

/// OS-native storage for the browser state encryption identity.
pub struct KeychainStateKeyStore;

impl StateKeyStore for KeychainStateKeyStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
            .with_context(|| format!("open Keychain entry {KEYCHAIN_SERVICE}:{account}"))?;
        match entry.get_password() {
            Ok(value) if !value.is_empty() => Ok(Some(value)),
            Ok(_) => bail!("Keychain entry {KEYCHAIN_SERVICE}:{account} is empty"),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("read Keychain entry {KEYCHAIN_SERVICE}:{account}")),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        keyring::Entry::new(KEYCHAIN_SERVICE, account)?
            .set_password(value)
            .with_context(|| format!("write Keychain entry {KEYCHAIN_SERVICE}:{account}"))
    }
}

fn identity(store: &impl StateKeyStore) -> Result<age::x25519::Identity> {
    let encoded = match store.get(STATE_KEY_ACCOUNT)? {
        Some(value) => value,
        None => {
            let created = age::x25519::Identity::generate();
            let encoded = created.to_string();
            store.set(STATE_KEY_ACCOUNT, encoded.expose_secret())?;
            return Ok(created);
        }
    };
    encoded.trim().parse().map_err(|error: &str| {
        anyhow::anyhow!("parse Keychain entry {KEYCHAIN_SERVICE}:{STATE_KEY_ACCOUNT}: {error}")
    })
}

/// Encrypt a Playwright `storageState` JSON document and atomically replace
/// `path`. The caller must pass only transient plaintext; this function never
/// writes that plaintext to disk.
pub fn write_state(path: &Path, plaintext: &[u8], store: &impl StateKeyStore) -> Result<()> {
    let identity = identity(store)?;
    let recipient = identity.to_public();
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?;
    let mut encrypted = Vec::new();
    {
        let mut writer = encryptor.wrap_output(&mut encrypted)?;
        writer.write_all(plaintext)?;
        writer.finish()?;
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("state path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".state-{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temp, encrypted)?;
    #[cfg(unix)]
    std::fs::set_permissions(&temp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    Ok(())
}

/// Decrypt a profile state document wholly in memory.
pub fn read_state(path: &Path, store: &impl StateKeyStore) -> Result<Vec<u8>> {
    let identity = identity(store)?;
    let encrypted = std::fs::read(path)
        .with_context(|| format!("read encrypted browser state {}", path.display()))?;
    let decryptor = age::Decryptor::new(Cursor::new(encrypted))?;
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
    let mut plaintext = Vec::new();
    reader.read_to_end(&mut plaintext)?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    #[derive(Default)]
    struct MemoryStore(Mutex<HashMap<String, String>>);
    impl StateKeyStore for MemoryStore {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
    }

    #[test]
    fn state_round_trip_never_contains_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile/state.json.age");
        let store = MemoryStore::default();
        let plain = br#"{\"cookies\":[{\"value\":\"secret-cookie\"}]}"#;
        write_state(&path, plain, &store).unwrap();
        let ciphertext = std::fs::read(&path).unwrap();
        assert!(
            !ciphertext
                .windows(b"secret-cookie".len())
                .any(|window| window == b"secret-cookie")
        );
        assert_eq!(read_state(&path, &store).unwrap(), plain);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
    }
}
