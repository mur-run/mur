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

/// Env var naming an inherited fd that carries the state key (#2369). Set by
/// the agent runtime, which reads the key before its sandbox seals, for a
/// live-browser child that could not reach the Keychain itself. Mirrors
/// `mur_common::browser_state_key::FD_ENV`; a `mur-core` test pins them equal.
pub const STATE_KEY_FD_ENV: &str = "MUR_BROWSER_STATE_KEY_FD";

/// The key handed over on [`STATE_KEY_FD_ENV`], read to EOF once and kept for
/// the process: startup decrypts with it and the exit write-back re-encrypts
/// with it, long after the fd is gone.
fn handed_key() -> Option<&'static str> {
    static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let raw = std::env::var(STATE_KEY_FD_ENV).ok()?;
        match read_handed_fd(&raw) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::warn!(%error, "ignoring {STATE_KEY_FD_ENV}; falling back to the Keychain");
                None
            }
        }
    })
    .as_deref()
}

#[cfg(unix)]
fn read_handed_fd(raw: &str) -> Result<String> {
    use std::os::fd::FromRawFd;
    let fd: i32 = raw.trim().parse().context("not an fd number")?;
    // 0-2 are stdio, which the MCP transport owns; never take one of them.
    if fd <= 2 {
        bail!("fd {fd} is a stdio stream");
    }
    // SAFETY: the runtime opened this fd for us alone and nothing else in this
    // process refers to it; the OnceLock makes this the only take.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut key = String::new();
    file.read_to_string(&mut key).context("read handed key")?;
    let key = key.trim().to_owned();
    if key.is_empty() {
        bail!("handed key is empty");
    }
    Ok(key)
}

#[cfg(not(unix))]
fn read_handed_fd(_raw: &str) -> Result<String> {
    bail!("fd handover is unix-only")
}

impl StateKeyStore for KeychainStateKeyStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        if account == STATE_KEY_ACCOUNT
            && let Some(key) = handed_key()
        {
            return Ok(Some(key.to_owned()));
        }
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

/// A profile state's version: the SHA-256 of its encrypted file (#2360,
/// spec "Newer-state guard"). `age` draws a fresh file key on every
/// encryption, so every write by any writer changes it.
pub type Version = [u8; 32];

/// Decrypt a profile state document wholly in memory.
pub fn read_state(path: &Path, store: &impl StateKeyStore) -> Result<Vec<u8>> {
    read_versioned(path, store).map(|(plain, _)| plain)
}

/// [`read_state`], plus the [`Version`] of exactly the bytes decrypted: the
/// file is read once and that one buffer is both hashed and decrypted, so a
/// write landing in between cannot make the two disagree.
pub fn read_versioned(path: &Path, store: &impl StateKeyStore) -> Result<(Vec<u8>, Version)> {
    let identity = identity(store)?;
    let encrypted = std::fs::read(path)
        .with_context(|| format!("read encrypted browser state {}", path.display()))?;
    let version = version_of(&encrypted);
    let decryptor = age::Decryptor::new(Cursor::new(encrypted))?;
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
    let mut plaintext = Vec::new();
    reader.read_to_end(&mut plaintext)?;
    Ok((plaintext, version))
}

fn version_of(encrypted: &[u8]) -> Version {
    use sha2::Digest;
    sha2::Sha256::digest(encrypted).into()
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
        let (_, first) = read_versioned(&path, &store).unwrap();
        assert_eq!(
            read_versioned(&path, &store).unwrap().1,
            first,
            "a read is stable"
        );
        write_state(&path, plain, &store).unwrap();
        assert_ne!(
            read_versioned(&path, &store).unwrap().1,
            first,
            "re-encrypting the same plaintext is a new version"
        );
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
    }
}
