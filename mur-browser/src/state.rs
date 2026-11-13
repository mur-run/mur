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

/// The key handed over on [`STATE_KEY_FD_ENV`], read once (up to its newline)
/// and kept for the process: startup decrypts with it and the exit write-back
/// re-encrypts with it, long after the fd is gone.
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

/// The handed key wins over the Keychain; only without one is `keychain`
/// asked. Split out so the order is testable without a Keychain.
fn handed_or(
    handed: Option<&str>,
    keychain: impl FnOnce() -> Result<Option<String>>,
) -> Result<Option<String>> {
    match handed {
        Some(key) => Ok(Some(key.to_owned())),
        None => keychain(),
    }
}

/// Upper bound on a handed key. An age identity is ~74 bytes; anything this
/// long is not one, and the bound keeps a misbehaving writer from growing us.
#[cfg(unix)]
const MAX_HANDED_KEY: u64 = 4096;

/// Take the handed key now, closing its fd, before this process spawns
/// anything (#2369 review). The runtime clears close-on-exec on that fd so
/// this process can inherit it, which also means every child spawned while
/// it is still open inherits it too — `record` runs `<shell> --version`
/// before it ever needs the key. Idempotent: the same `OnceLock` as the
/// lazy path, so there is never a second reader or a second close.
pub fn prime_handed_key() {
    let _ = handed_key();
}

/// Read the key from the fd named by [`STATE_KEY_FD_ENV`] — the reader half
/// of the runtime's handover (`mur_common::browser_state_key`).
///
/// The env value is not trusted to name our pipe: a stale or inherited value
/// could point at any fd this process holds. So stdio is refused, and the fd
/// is inspected through a duplicate before anything is taken — only a pipe is
/// consumed (and closed); anything else is left exactly as it was. The key
/// is read up to its terminating newline, never to EOF, so a stray holder of
/// the write end cannot stall startup.
#[cfg(unix)]
pub fn read_handed_fd(raw: &str) -> Result<String> {
    use std::io::BufRead;
    use std::os::fd::{BorrowedFd, FromRawFd};
    use std::os::unix::fs::FileTypeExt;
    let fd: i32 = raw.trim().parse().context("not an fd number")?;
    // 0-2 are stdio, which the MCP transport owns; never take one of them.
    if fd <= 2 {
        bail!("fd {fd} is a stdio stream");
    }
    // SAFETY: borrowed only for the duration of the dup; `try_clone_to_owned`
    // fails cleanly (EBADF) when the fd is not open.
    let probe = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .with_context(|| format!("fd {fd} is not open"))?;
    let is_pipe = std::fs::File::from(probe)
        .metadata()
        .with_context(|| format!("stat fd {fd}"))?
        .file_type()
        .is_fifo();
    if !is_pipe {
        bail!("fd {fd} is not a pipe; leaving it alone");
    }
    // SAFETY: the runtime opened this pipe for us alone and nothing else in
    // this process refers to it; the OnceLock in `handed_key` makes this the
    // only take.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    // Up to the newline the runtime ends the key with, not to EOF: a write
    // end held by some other process must not be able to stall startup.
    let mut key = String::new();
    std::io::BufReader::new(file.take(MAX_HANDED_KEY))
        .read_line(&mut key)
        .context("read handed key")?;
    if !key.ends_with('\n') && key.len() as u64 >= MAX_HANDED_KEY {
        bail!("handed key exceeds {MAX_HANDED_KEY} bytes");
    }
    let key = key.trim().to_owned();
    if key.is_empty() {
        bail!("handed key is empty");
    }
    Ok(key)
}

#[cfg(not(unix))]
pub fn read_handed_fd(_raw: &str) -> Result<String> {
    bail!("fd handover is unix-only")
}

impl StateKeyStore for KeychainStateKeyStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        let handed = (account == STATE_KEY_ACCOUNT).then(handed_key).flatten();
        handed_or(handed, || keychain_get(account))
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        keyring::Entry::new(KEYCHAIN_SERVICE, account)?
            .set_password(value)
            .with_context(|| format!("write Keychain entry {KEYCHAIN_SERVICE}:{account}"))
    }
}

fn keychain_get(account: &str) -> Result<Option<String>> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .with_context(|| format!("open Keychain entry {KEYCHAIN_SERVICE}:{account}"))?;
    match entry.get_password() {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) => bail!("Keychain entry {KEYCHAIN_SERVICE}:{account} is empty"),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read Keychain entry {KEYCHAIN_SERVICE}:{account}"))
        }
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
    fn a_handed_key_wins_over_the_keychain() {
        let got = handed_or(Some("handed"), || panic!("keychain must not be asked")).unwrap();
        assert_eq!(got.as_deref(), Some("handed"));
        let got = handed_or(None, || Ok(Some("keychain".into()))).unwrap();
        assert_eq!(got.as_deref(), Some("keychain"));
    }

    #[cfg(unix)]
    mod handover {
        use super::super::*;
        use std::io::Write;
        use std::os::fd::{AsRawFd, IntoRawFd};

        #[test]
        fn stdio_and_garbage_are_refused() {
            for raw in ["0", "1", "2", "-1", "x", ""] {
                assert!(read_handed_fd(raw).is_err(), "{raw:?} accepted");
            }
        }

        #[test]
        fn a_closed_fd_is_refused() {
            // An fd at the soft limit can never be open, so no other test
            // thread's pipe can be sitting on this number (#2369 review).
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
            let fd = i32::try_from(lim.rlim_cur).unwrap_or(i32::MAX);
            assert!(read_handed_fd(&fd.to_string()).is_err());
        }

        /// The key arrives newline-terminated while some write end is still
        /// open elsewhere: the read must not wait for an EOF that never comes.
        #[test]
        fn the_read_stops_at_the_newline() {
            let (r, mut w) = std::io::pipe().unwrap();
            w.write_all(b"AGE-SECRET-KEY-1TEST\n").unwrap();
            let fd = r.into_raw_fd();
            assert_eq!(
                read_handed_fd(&fd.to_string()).unwrap(),
                "AGE-SECRET-KEY-1TEST"
            );
            drop(w);
        }

        /// Item 2 of the #2369 review: the handed fd arrives without
        /// close-on-exec, so until it is read every child inherits it. After
        /// the read it is closed, and a child spawned then sees nothing.
        #[test]
        fn a_child_spawned_after_the_read_does_not_inherit_the_fd() {
            let (r, mut w) = std::io::pipe().unwrap();
            w.write_all(b"AGE-SECRET-KEY-1TEST\n").unwrap();
            drop(w);
            let fd = r.into_raw_fd();
            // As the runtime hands it over: inheritable.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0);
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
                0
            );
            let probe = format!("test -e /dev/fd/{fd}");
            let sees = || {
                std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&probe)
                    .status()
                    .unwrap()
                    .success()
            };
            assert!(sees(), "control: an unread handed fd is inherited");
            read_handed_fd(&fd.to_string()).unwrap();
            assert!(!sees(), "the fd outlived the read");
        }

        /// A stale env value naming some unrelated fd must not close it.
        #[test]
        fn a_non_pipe_fd_is_left_open() {
            let file = tempfile::tempfile().unwrap();
            let fd = file.as_raw_fd();
            let err = read_handed_fd(&fd.to_string()).unwrap_err();
            assert!(err.to_string().contains("not a pipe"), "{err:#}");
            // Still ours and still open.
            assert!(file.metadata().is_ok());
        }

        #[test]
        fn a_pipe_is_read_to_its_newline_and_trimmed() {
            let (r, mut w) = std::io::pipe().unwrap();
            w.write_all(b"AGE-SECRET-KEY-1TEST\n").unwrap();
            drop(w);
            let fd = r.into_raw_fd();
            assert_eq!(
                read_handed_fd(&fd.to_string()).unwrap(),
                "AGE-SECRET-KEY-1TEST"
            );
        }

        #[test]
        fn an_empty_pipe_falls_back() {
            let (r, w) = std::io::pipe().unwrap();
            drop(w);
            assert!(read_handed_fd(&r.into_raw_fd().to_string()).is_err());
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
