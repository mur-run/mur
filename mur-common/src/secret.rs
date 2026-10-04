//! Typed reference to a secret value. The reference itself is safe to
//! commit / log / serialize; the resolved value (`SecretString`) is
//! zeroized on drop.
//!
//! Wire format is a single string with a colon-prefixed scheme:
//!   env:VAR_NAME
//!   keychain:service/account
//!   file:/absolute/or/~-path[.age]
//!   cmd:./script-or-binary args…

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretRef {
    Env(String),
    Keychain { service: String, account: String },
    File(PathBuf),
    Cmd(String),
}

#[derive(thiserror::Error, Debug)]
pub enum SecretError {
    #[error("env var {0} not set")]
    EnvNotSet(String),
    #[error("keychain item not found: {service}/{account}")]
    KeychainNotFound { service: String, account: String },
    #[error("keychain backend error: {0}")]
    KeychainBackend(String),
    #[error("read file {path}: {source}")]
    FileRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("file mode is not 0600: {0}")]
    FileMode(String),
    #[error("decrypt {0}")]
    AgeDecrypt(String),
    #[error("cmd {cmd} exited with {status}")]
    Cmd { cmd: String, status: i32 },
    #[error("invalid SecretRef syntax: {0}")]
    Parse(String),
}

/// Values resolved BEFORE a sandbox seals, for reading after it has.
///
/// The problem this exists for: an agent's provider secret is resolved when the
/// LLM client is built (`supervisor.rs:384`), which is AFTER
/// `sandbox::apply` (`:314`). A `file:` ref pointing inside `~/.mur/secrets/`
/// is therefore unreadable — that directory is a denied credential path — and
/// the caller silently falls back to a per-agent Keychain lookup, which is the
/// #866 failure. Both paths were broken at once on a real install, each hiding
/// the other.
///
/// The identity key already solves this by ordering: loaded at
/// `supervisor.rs:174`, before the seal. This gives secrets the same treatment.
/// The agent ends up holding the VALUE and never the path, so the `secrets/`
/// deny is not weakened at all — it stays a directory no agent may open.
///
/// Deliberately a value cache and not a path cache: nothing here lets a
/// post-seal caller learn where a secret came from, only what it was.
static PRESEAL_CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<(SecretRef, SecretString)>>> =
    std::sync::OnceLock::new();

fn preseal_cache() -> &'static std::sync::Mutex<Vec<(SecretRef, SecretString)>> {
    PRESEAL_CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Resolve `r` now and remember it, so a later `resolve_blocking` succeeds even
/// once the path is unreachable. Call before sealing. Errors are the caller's
/// to report — a secret that cannot be resolved pre-seal is not cached, and the
/// later lookup fails exactly as it would have.
pub fn cache_before_seal(r: &SecretRef) -> Result<(), SecretError> {
    let v = r.resolve_blocking()?;
    let mut c = preseal_cache().lock().unwrap_or_else(|e| e.into_inner());
    if !c.iter().any(|(k, _)| k == r) {
        c.push((r.clone(), v));
    }
    Ok(())
}

/// How many secrets are cached. For tests and diagnostics.
pub fn preseal_cached_count() -> usize {
    preseal_cache()
        .lock()
        .map(|c| c.len())
        .unwrap_or_else(|e| e.into_inner().len())
}

fn preseal_lookup(r: &SecretRef) -> Option<SecretString> {
    let c = preseal_cache().lock().unwrap_or_else(|e| e.into_inner());
    c.iter().find(|(k, _)| k == r).map(|(_, v)| v.clone())
}

/// A form of this reference that is safe to put in front of a user, a log, or
/// a model.
///
/// [`Display`](std::fmt::Display) prints the reference verbatim, which is right
/// where the reader is the operator looking at their own config. It is wrong
/// for `Cmd`: the whole command line is printed, and a command line is exactly
/// where an inline credential lives (`cmd:vault read --token=…`).
///
/// Redaction by pattern does not cover this — `redact_secrets` matches known
/// key shapes (`sk-`, `AKIA`, `ghp_`, JWT, PEM) and an arbitrary `--token=`
/// argument is none of them. So the arguments are dropped structurally rather
/// than filtered: the program name is what identifies the credential, and the
/// arguments are only where the danger is.
impl SecretRef {
    pub fn label(&self) -> String {
        match self {
            SecretRef::Cmd(c) => {
                let program = c.split_whitespace().next().unwrap_or("");
                if c.split_whitespace().nth(1).is_some() {
                    format!("cmd:{program} (arguments hidden)")
                } else {
                    format!("cmd:{program}")
                }
            }
            other => other.to_string(),
        }
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretRef::Env(v) => write!(f, "env:{v}"),
            SecretRef::Keychain { service, account } => {
                write!(f, "keychain:{service}/{account}")
            }
            SecretRef::File(p) => write!(f, "file:{}", p.display()),
            SecretRef::Cmd(c) => write!(f, "cmd:{c}"),
        }
    }
}

impl std::str::FromStr for SecretRef {
    type Err = SecretError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (scheme, rest) = s
            .split_once(':')
            .ok_or_else(|| SecretError::Parse(format!("missing scheme: {s}")))?;
        match scheme {
            "env" => Ok(SecretRef::Env(rest.to_string())),
            "keychain" => {
                let (service, account) = rest.split_once('/').ok_or_else(|| {
                    SecretError::Parse(format!("keychain ref needs service/account: {s}"))
                })?;
                Ok(SecretRef::Keychain {
                    service: service.to_string(),
                    account: account.to_string(),
                })
            }
            "file" => Ok(SecretRef::File(PathBuf::from(rest))),
            "cmd" => Ok(SecretRef::Cmd(rest.to_string())),
            other => Err(SecretError::Parse(format!("unknown scheme: {other}"))),
        }
    }
}

impl Serialize for SecretRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Force-block OS keychain access in this process: lookups behave as
/// "not found", writes are rejected. Exists so processes that must never
/// trigger a macOS keychain password prompt (test runs, CI) can opt out.
pub const ENV_KEYCHAIN_DISABLED: &str = "MUR_KEYCHAIN_DISABLED";
/// Overrides the automatic test-process block below. Set by tests that
/// install a keyring mock builder (those never reach the real keychain).
pub const ENV_KEYCHAIN_ALLOW: &str = "MUR_KEYCHAIN_ALLOW";

/// Cargo test binaries get a fresh hash suffix on every rebuild, so macOS
/// keychain "always allow" ACLs never stick and any test that resolves a
/// real `keychain:` ref (e.g. via the user's ~/.mur/config.yaml) rains
/// password prompts on every run. nextest sets `NEXTEST=1` in each test
/// process — treat that as "no real keychain" unless explicitly re-enabled.
fn keychain_blocked() -> bool {
    if std::env::var_os(ENV_KEYCHAIN_ALLOW).is_some() {
        return false;
    }
    std::env::var_os(ENV_KEYCHAIN_DISABLED).is_some() || std::env::var_os("NEXTEST").is_some()
}

impl SecretRef {
    pub async fn resolve(&self) -> Result<SecretString, SecretError> {
        match self {
            SecretRef::Env(var) => std::env::var(var)
                .map(SecretString::from)
                .map_err(|_| SecretError::EnvNotSet(var.clone())),
            SecretRef::Keychain { service, account } if keychain_blocked() => {
                Err(SecretError::KeychainNotFound {
                    service: service.clone(),
                    account: account.clone(),
                })
            }
            SecretRef::Keychain { service, account } => {
                let svc = service.clone();
                let acct = account.clone();
                let res = tokio::task::spawn_blocking(move || -> Result<String, SecretError> {
                    let entry = keyring::Entry::new(&svc, &acct)
                        .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
                    match entry.get_password() {
                        Ok(s) => Ok(s),
                        Err(keyring::Error::NoEntry) => Err(SecretError::KeychainNotFound {
                            service: svc.clone(),
                            account: acct.clone(),
                        }),
                        Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
                    }
                })
                .await
                .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?;
                res.map(SecretString::from)
            }
            SecretRef::File(path) => resolve_file(path).await,
            SecretRef::Cmd(spec) => resolve_cmd(spec).await,
        }
    }

    /// Probe whether the secret resolves successfully without surfacing the
    /// value. Used by GUI/CLI status indicators. Note: for `Cmd` refs this
    /// actually runs the command, which may have side effects or be slow.
    pub async fn check(&self) -> bool {
        self.resolve().await.is_ok()
    }

    /// Resolve and expose the secret as a plain `String` for callers that must
    /// hand the raw value to an external API (e.g. an `Authorization: Bearer`
    /// header). This is the deliberate materialization boundary — keep the
    /// returned value short-lived and never log or persist it. Returns `None`
    /// on any resolution failure (missing env var, keychain entry, etc.).
    pub async fn resolve_to_string(&self) -> Option<String> {
        use secrecy::ExposeSecret;
        self.resolve()
            .await
            .ok()
            .map(|s| s.expose_secret().to_string())
    }

    /// Synchronous resolve for callers outside an async context (CLI
    /// factories, config loaders). Inside a multi-thread tokio runtime it
    /// uses block_in_place; inside a current-thread runtime (where
    /// block_in_place panics) it hops to a fresh thread; otherwise it spins
    /// a current-thread runtime.
    /// The pre-seal cached value for this ref, if any — WITHOUT falling back
    /// to the backend.
    ///
    /// For callers that reach a backend directly rather than through
    /// `resolve_blocking`, so they can honour the cache without changing what
    /// they do when it misses.
    pub fn resolve_preseal_cached(&self) -> Option<SecretString> {
        preseal_lookup(self)
    }

    pub fn resolve_blocking(&self) -> Result<SecretString, SecretError> {
        // A value cached before the sandbox sealed wins. Without this, a `file:`
        // ref inside the denied credential store is unreadable post-seal and the
        // caller falls through to a per-agent Keychain lookup (#866). See
        // `cache_before_seal`.
        if let Some(v) = preseal_lookup(self) {
            return Ok(v);
        }
        fn fresh_runtime_resolve(r: &SecretRef) -> Result<SecretString, SecretError> {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| SecretError::KeychainBackend(format!("runtime: {e}")))?
                .block_on(r.resolve())
        }
        match tokio::runtime::Handle::try_current() {
            Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| h.block_on(self.resolve()))
            }
            // Current-thread runtime (e.g. #[tokio::test]): block_in_place
            // would panic — resolve on a fresh OS thread instead.
            Ok(_) => std::thread::scope(|s| {
                s.spawn(|| fresh_runtime_resolve(self))
                    .join()
                    .unwrap_or_else(|_| {
                        Err(SecretError::KeychainBackend(
                            "resolver thread panicked".into(),
                        ))
                    })
            }),
            Err(_) => fresh_runtime_resolve(self),
        }
    }

    /// Blocking analogue of `resolve_to_string` — same materialization
    /// caveats apply.
    pub fn resolve_to_string_blocking(&self) -> Option<String> {
        use secrecy::ExposeSecret;
        self.resolve_blocking()
            .ok()
            .map(|s| s.expose_secret().to_string())
    }
}

/// Read a secret from the OS keychain.
///
/// Returns `Ok(None)` when the entry doesn't exist (so callers can fall
/// through to the next precedence layer cleanly), and `Err(...)` only for
/// real backend failures (locked keychain, permission denied, malformed
/// service/account, transport error). Silently swallowing those errors would
/// mask configuration problems and let the next fallback layer take over
/// when the user actually expected the keychain entry to be honored.
///
/// Pairs with [`keychain_set`] / [`keychain_delete`].
pub async fn keychain_get(
    service: &str,
    account: &str,
) -> Result<Option<SecretString>, SecretError> {
    if keychain_blocked() {
        return Ok(None);
    }
    let svc = service.to_string();
    let acct = account.to_string();
    tokio::task::spawn_blocking(move || -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(&svc, &acct)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        match entry.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
        }
    })
    .await
    .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?
    .map(|opt| opt.map(SecretString::from))
}

/// `errSecItemNotFound` — the one `SecItemCopyMatching` status that means
/// "absent" rather than "failed".
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// Does a keychain item exist? Never reads the secret value.
///
/// For callers that only need presence (e.g. `mur doctor` counting what an
/// upgrade would lose). On macOS this is an attribute-only query: reading item
/// DATA is ACL-gated, and an ad-hoc binary whose hash changed on upgrade is no
/// longer on the ACL — `keychain_get` then blocks on an authorization prompt
/// nobody answers and fails as if absent. Attribute reads are not gated, so
/// this neither prompts nor stalls, and the answer is correct.
///
/// Other platforms' backends do not prompt, so they fall back to a value read.
pub fn keychain_item_exists(service: &str, account: &str) -> Result<bool, SecretError> {
    if keychain_blocked() {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
        let found = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(service)
            .account(account)
            .load_attributes(true)
            .load_data(false)
            .limit(Limit::Max(1))
            .search();
        match found {
            Ok(items) => Ok(!items.is_empty()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        match entry.get_password() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
        }
    }
}

/// Write a secret to the OS keychain. Used by `mur agent secret set` and the
/// GUI's `set_secret` command.
pub async fn keychain_set(service: &str, account: &str, value: &str) -> Result<(), SecretError> {
    if keychain_blocked() {
        return Err(SecretError::KeychainBackend(format!(
            "keychain access disabled in this process ({ENV_KEYCHAIN_DISABLED}/test); \
             set {ENV_KEYCHAIN_ALLOW}=1 to override"
        )));
    }
    let svc = service.to_string();
    let acct = account.to_string();
    let val = value.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&svc, &acct)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        entry
            .set_password(&val)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        Ok(())
    })
    .await
    .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?
}

/// Delete a secret from the OS keychain. Idempotent: missing entries are not
/// an error. Used by `mur agent secret delete`.
pub async fn keychain_delete(service: &str, account: &str) -> Result<(), SecretError> {
    if keychain_blocked() {
        return Ok(());
    }
    let svc = service.to_string();
    let acct = account.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&svc, &acct)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
        }
    })
    .await
    .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?
}

async fn resolve_cmd(spec: &str) -> Result<SecretString, SecretError> {
    let mut parts = shell_words::split(spec)
        .map_err(|e| SecretError::Parse(format!("split cmd {spec:?}: {e}")))?;
    if parts.is_empty() {
        return Err(SecretError::Parse("empty cmd".into()));
    }
    let program = parts.remove(0);
    let output = tokio::process::Command::new(&program)
        .args(&parts)
        .output()
        .await
        .map_err(|e| SecretError::Cmd {
            cmd: format!("{spec} ({e})"),
            status: -1,
        })?;
    if !output.status.success() {
        return Err(SecretError::Cmd {
            cmd: spec.to_string(),
            status: output.status.code().unwrap_or(-1),
        });
    }
    let s = String::from_utf8(output.stdout).map_err(|e| SecretError::Cmd {
        cmd: format!("{spec} (non-utf8 stdout: {e})"),
        status: -2,
    })?;
    Ok(SecretString::from(
        s.trim_end_matches(['\n', '\r']).to_string(),
    ))
}

async fn resolve_file(path: &std::path::Path) -> Result<SecretString, SecretError> {
    let expanded = shellexpand::full(&path.to_string_lossy())
        .map_err(|e| SecretError::Parse(format!("expand {path:?}: {e}")))?
        .to_string();
    let p = std::path::PathBuf::from(expanded);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(&p)
            .await
            .map_err(|e| SecretError::FileRead {
                path: p.display().to_string(),
                source: e,
            })?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(SecretError::FileMode(format!(
                "{}: mode {:o} grants group/world access",
                p.display(),
                mode
            )));
        }
    }

    let bytes = tokio::fs::read(&p)
        .await
        .map_err(|e| SecretError::FileRead {
            path: p.display().to_string(),
            source: e,
        })?;

    let plaintext = if p.extension().and_then(|s| s.to_str()) == Some("age") {
        decrypt_age(&bytes).await?
    } else {
        String::from_utf8(bytes).map_err(|e| SecretError::AgeDecrypt(e.to_string()))?
    };
    let trimmed = plaintext.trim_end_matches(['\n', '\r']).to_string();
    Ok(SecretString::from(trimmed))
}

async fn decrypt_age(bytes: &[u8]) -> Result<String, SecretError> {
    let id_path: std::path::PathBuf = match std::env::var("MUR_AGE_IDENTITY_PATH") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => crate::home::try_mur_home()
            .ok_or_else(|| {
                SecretError::AgeDecrypt(
                    "MUR_AGE_IDENTITY_PATH unset and MUR home not resolvable".into(),
                )
            })?
            .join("age/identity.txt"),
    };

    let id_str = tokio::fs::read_to_string(&id_path).await.map_err(|e| {
        SecretError::AgeDecrypt(format!("read identity {}: {}", id_path.display(), e))
    })?;
    let identity: age::x25519::Identity = id_str
        .trim()
        .parse()
        .map_err(|e: &str| SecretError::AgeDecrypt(format!("parse identity: {e}")))?;

    let decryptor =
        age::Decryptor::new(bytes).map_err(|e| SecretError::AgeDecrypt(e.to_string()))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| SecretError::AgeDecrypt(e.to_string()))?;
    let mut out = String::new();
    use std::io::Read;
    reader
        .read_to_string(&mut out)
        .map_err(|e| SecretError::AgeDecrypt(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod resolve_env_tests;

#[cfg(test)]
mod keychain_test_fixture;

#[cfg(test)]
mod resolve_keychain_tests;

#[cfg(all(test, unix))]
mod resolve_file_tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[tokio::test]
    async fn reads_plaintext_0600() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("k.txt");
        std::fs::write(&p, "abc\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = SecretRef::File(p);
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "abc"); // trailing newline stripped
    }

    #[tokio::test]
    async fn rejects_world_readable() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("k.txt");
        std::fs::write(&p, "abc").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let s = SecretRef::File(p);
        let err = s.resolve().await.unwrap_err();
        assert!(matches!(err, SecretError::FileMode(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn decrypts_age_recipient_file() {
        let dir = tempdir().unwrap();
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let payload = b"shh-from-age";

        let mut encrypted: Vec<u8> = Vec::new();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .unwrap();
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        std::io::Write::write_all(&mut writer, payload).unwrap();
        writer.finish().unwrap();

        let enc_path = dir.path().join("k.age");
        std::fs::write(&enc_path, &encrypted).unwrap();
        std::fs::set_permissions(&enc_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let id_path = dir.path().join("identity.txt");
        use secrecy::ExposeSecret as _;
        std::fs::write(&id_path, identity.to_string().expose_secret()).unwrap();
        std::fs::set_permissions(&id_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _env = crate::test_env::EnvGuard::set([("MUR_AGE_IDENTITY_PATH", &id_path)]);
        let s = SecretRef::File(enc_path);
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shh-from-age");
    }
}

#[cfg(all(test, unix))]
mod resolve_cmd_tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn echoes_stdout() {
        let s = SecretRef::Cmd("printf shh-from-cmd".into());
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shh-from-cmd");
    }

    #[tokio::test]
    async fn errors_on_non_zero_exit() {
        let s = SecretRef::Cmd("sh -c 'exit 7'".into());
        let err = s.resolve().await.unwrap_err();
        match err {
            SecretError::Cmd { status, .. } => assert_eq!(status, 7),
            other => panic!("unexpected: {other:?}"),
        }
    }
}

#[cfg(test)]
mod check_tests;

#[cfg(test)]
mod keychain_helpers_tests;

#[cfg(test)]
mod resolve_blocking_tests;
