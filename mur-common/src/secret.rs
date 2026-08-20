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
        Err(_) => dirs::home_dir()
            .ok_or_else(|| {
                SecretError::AgeDecrypt(
                    "MUR_AGE_IDENTITY_PATH unset and home dir not resolvable".into(),
                )
            })?
            .join(".mur/age/identity.txt"),
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
mod tests {
    use super::*;
    use serde_yaml_ng as yaml;

    #[test]
    fn parses_env_form() {
        let s: SecretRef = yaml::from_str("env:ANTHROPIC_API_KEY").unwrap();
        assert_eq!(s, SecretRef::Env("ANTHROPIC_API_KEY".into()));
    }

    #[test]
    fn parses_keychain_form() {
        let s: SecretRef = yaml::from_str("keychain:mur/anthropic-oauth").unwrap();
        assert_eq!(
            s,
            SecretRef::Keychain {
                service: "mur".into(),
                account: "anthropic-oauth".into()
            }
        );
    }

    #[test]
    fn parses_file_form() {
        let s: SecretRef = yaml::from_str("file:/tmp/foo.age").unwrap();
        assert_eq!(s, SecretRef::File(PathBuf::from("/tmp/foo.age")));
    }

    #[test]
    fn parses_cmd_form() {
        let s: SecretRef = yaml::from_str("cmd:op read op://vault/item/field").unwrap();
        assert_eq!(s, SecretRef::Cmd("op read op://vault/item/field".into()));
    }

    #[test]
    fn rejects_unknown_scheme() {
        let r: Result<SecretRef, _> = yaml::from_str("plain:supersecret");
        assert!(r.is_err());
    }

    #[test]
    fn round_trip_serde() {
        let cases = [
            "env:X",
            "keychain:svc/acct",
            "file:/p",
            "cmd:bin --flag arg",
        ];
        for s in cases {
            let parsed: SecretRef = yaml::from_str(s).unwrap();
            let back = yaml::to_string(&parsed).unwrap();
            // serde-yaml adds a trailing newline / quoting. Strip and compare.
            let normalized = back
                .trim()
                .trim_matches(|c: char| c == '"' || c == '\'')
                .to_string();
            let reparsed: SecretRef = yaml::from_str(&normalized).unwrap();
            assert_eq!(parsed, reparsed, "round-trip drift for {s}");
        }
    }
}

#[cfg(test)]
mod resolve_env_tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn resolves_env_when_set() {
        // SAFETY: uniquely named env var so concurrent tests don't collide.
        unsafe {
            std::env::set_var("MUR_TEST_RESOLVE_ENV", "shhh");
        }
        let s = SecretRef::Env("MUR_TEST_RESOLVE_ENV".into());
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shhh");
    }

    #[tokio::test]
    async fn errors_when_env_missing() {
        let s = SecretRef::Env("MUR_TEST_DEFINITELY_UNSET".into());
        let err = s.resolve().await.unwrap_err();
        assert!(matches!(err, SecretError::EnvNotSet(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn resolve_to_string_exposes_value_or_none() {
        // SAFETY: uniquely named env var so concurrent tests don't collide.
        unsafe {
            std::env::set_var("MUR_TEST_RESOLVE_TO_STRING", "kc-abc");
        }
        let set = SecretRef::Env("MUR_TEST_RESOLVE_TO_STRING".into());
        assert_eq!(set.resolve_to_string().await.as_deref(), Some("kc-abc"));

        let missing = SecretRef::Env("MUR_TEST_RESOLVE_TO_STRING_UNSET".into());
        assert_eq!(missing.resolve_to_string().await, None);
    }
}

#[cfg(test)]
mod keychain_test_fixture {
    //! Shared mock fixture used by every test module that touches the keyring.
    //!
    //! v3's stock `keyring::mock` advertises CredentialPersistence::EntryOnly
    //! and gives each Entry its own private storage — that breaks our tests
    //! because resolve() creates a fresh `Entry::new` after setup. The fixture
    //! below installs a SharedMockBuilder backed by an Arc<Mutex<HashMap>>
    //! so all Entry instances see the same data.
    //!
    //! Tests serialize on a tokio::sync::Mutex (held across await) because
    //! `set_default_credential_builder` mutates a process-global.

    use keyring::credential::{
        Credential, CredentialApi, CredentialBuilder, CredentialBuilderApi, CredentialPersistence,
    };
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

    type Store = Arc<Mutex<HashMap<(String, String), Vec<u8>>>>;

    struct SharedMockCredential {
        store: Store,
        key: (String, String),
    }

    impl CredentialApi for SharedMockCredential {
        fn set_secret(&self, password: &[u8]) -> keyring::Result<()> {
            self.store
                .lock()
                .unwrap()
                .insert(self.key.clone(), password.to_vec());
            Ok(())
        }
        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            self.store
                .lock()
                .unwrap()
                .get(&self.key)
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }
        fn delete_credential(&self) -> keyring::Result<()> {
            self.store
                .lock()
                .unwrap()
                .remove(&self.key)
                .map(|_| ())
                .ok_or(keyring::Error::NoEntry)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct SharedMockBuilder {
        store: Store,
    }

    impl CredentialBuilderApi for SharedMockBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(SharedMockCredential {
                store: self.store.clone(),
                key: (service.to_string(), user.to_string()),
            }))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn persistence(&self) -> CredentialPersistence {
            CredentialPersistence::ProcessOnly
        }
    }

    static MOCK_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

    /// Serialize env-var mutation with the mock installs above (both are
    /// process-global). Used by tests that exercise `keychain_blocked`.
    pub(super) async fn env_lock() -> AsyncMutexGuard<'static, ()> {
        MOCK_LOCK.lock().await
    }

    pub(super) async fn install_mock(
        initial: Option<(&str, &str, &str)>,
    ) -> AsyncMutexGuard<'static, ()> {
        let g = MOCK_LOCK.lock().await;
        // The mock never reaches the real OS keychain, so lift the automatic
        // test-process keychain block (`keychain_blocked`).
        // SAFETY: env mutation serialized by MOCK_LOCK; nextest runs one test
        // per process anyway.
        unsafe {
            std::env::set_var(super::ENV_KEYCHAIN_ALLOW, "1");
        }
        let store: Store = Arc::new(Mutex::new(HashMap::new()));
        if let Some((svc, user, pw)) = initial {
            store
                .lock()
                .unwrap()
                .insert((svc.to_string(), user.to_string()), pw.as_bytes().to_vec());
        }
        let builder: Box<CredentialBuilder> = Box::new(SharedMockBuilder { store });
        keyring::set_default_credential_builder(builder);
        g
    }
}

#[cfg(test)]
mod resolve_keychain_tests {
    use super::keychain_test_fixture::install_mock;
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn blocked_process_never_reaches_keychain() {
        let _g = super::keychain_test_fixture::env_lock().await;
        // SAFETY: env mutation serialized on the fixture lock; nextest is
        // process-per-test anyway.
        unsafe {
            std::env::remove_var(ENV_KEYCHAIN_ALLOW);
            std::env::set_var(ENV_KEYCHAIN_DISABLED, "1");
        }
        let s = SecretRef::Keychain {
            service: "mur-test".into(),
            account: "nope".into(),
        };
        assert!(matches!(
            s.resolve().await,
            Err(SecretError::KeychainNotFound { .. })
        ));
        assert!(keychain_get("mur-test", "nope").await.unwrap().is_none());
        assert!(keychain_set("mur-test", "nope", "v").await.is_err());
        assert!(keychain_delete("mur-test", "nope").await.is_ok());
        unsafe {
            std::env::remove_var(ENV_KEYCHAIN_DISABLED);
        }
    }

    #[tokio::test]
    async fn resolves_when_set() {
        let _g = install_mock(Some(("mur-test", "kc-acct", "kc-secret"))).await;
        let s = SecretRef::Keychain {
            service: "mur-test".into(),
            account: "kc-acct".into(),
        };
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "kc-secret");
    }

    #[tokio::test]
    async fn errors_when_missing() {
        let _g = install_mock(None).await;
        let s = SecretRef::Keychain {
            service: "mur-test".into(),
            account: "kc-acct".into(),
        };
        let err = s.resolve().await.unwrap_err();
        assert!(
            matches!(err, SecretError::KeychainNotFound { .. }),
            "got {err:?}"
        );
    }
}

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
        // SAFETY: setting an env var read by decrypt_age. Tests serialize on
        // the same env var, so concurrent writes would race; we serialize via
        // a Mutex-held guard.
        unsafe {
            std::env::set_var("MUR_AGE_IDENTITY_PATH", &id_path);
        }
        let s = SecretRef::File(enc_path);
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shh-from-age");
        unsafe {
            std::env::remove_var("MUR_AGE_IDENTITY_PATH");
        }
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
mod check_tests {
    use super::*;

    #[tokio::test]
    async fn check_env_present() {
        // SAFETY: uniquely named env var so concurrent tests don't collide.
        unsafe {
            std::env::set_var("MUR_TEST_CHECK_ENV", "1");
        }
        assert!(SecretRef::Env("MUR_TEST_CHECK_ENV".into()).check().await);
    }

    #[tokio::test]
    async fn check_env_absent() {
        assert!(
            !SecretRef::Env("MUR_TEST_CHECK_DEFINITELY_UNSET".into())
                .check()
                .await
        );
    }
}

#[cfg(test)]
mod keychain_helpers_tests {
    use super::keychain_test_fixture::install_mock;
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn set_then_resolve_round_trips() {
        let _g = install_mock(None).await;
        keychain_set("mur-test", "round-trip", "v1").await.unwrap();
        let v = SecretRef::Keychain {
            service: "mur-test".into(),
            account: "round-trip".into(),
        }
        .resolve()
        .await
        .unwrap();
        assert_eq!(v.expose_secret(), "v1");
    }

    #[tokio::test]
    async fn delete_works() {
        let _g = install_mock(None).await;
        keychain_set("mur-test", "to-delete", "v").await.unwrap();
        keychain_delete("mur-test", "to-delete").await.unwrap();
        let r = SecretRef::Keychain {
            service: "mur-test".into(),
            account: "to-delete".into(),
        }
        .resolve()
        .await;
        assert!(matches!(r, Err(SecretError::KeychainNotFound { .. })));
    }

    #[tokio::test]
    async fn delete_missing_is_idempotent() {
        let _g = install_mock(None).await;
        // No prior set — must still return Ok.
        keychain_delete("mur-test", "never-set").await.unwrap();
    }
}

#[cfg(test)]
mod resolve_blocking_tests {
    /// A secret cached before the seal resolves afterwards even when the path
    /// has become unreachable — which is what a sandboxed agent faces for a
    /// `file:` ref inside the denied credential store (#866).
    ///
    /// Simulated by deleting the file after caching: post-seal the path is gone
    /// as far as the process is concerned, exactly as a deny makes it.
    #[test]
    fn a_cached_secret_survives_its_path_becoming_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider.key");
        std::fs::write(&path, "sk-test-value").unwrap();
        // `file:` refs require 0600 — group/world access is refused outright.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let r = SecretRef::File(path.clone());

        cache_before_seal(&r).expect("resolves while the path is reachable");
        std::fs::remove_file(&path).unwrap();

        use secrecy::ExposeSecret;
        let got = r
            .resolve_blocking()
            .expect("cached value must still resolve");
        assert_eq!(got.expose_secret(), "sk-test-value");
    }

    /// Negative control for the above: WITHOUT caching, the same ref fails once
    /// the path is unreachable. That is the pre-fix behaviour, and it is what
    /// made the caller fall through to a Keychain lookup.
    #[test]
    fn an_uncached_secret_fails_when_its_path_is_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uncached.key");
        std::fs::write(&path, "sk-other-value").unwrap();
        let r = SecretRef::File(path.clone());
        std::fs::remove_file(&path).unwrap();

        assert!(
            r.resolve_blocking().is_err(),
            "an uncached ref must not resolve once its path is gone"
        );
    }

    /// Caching is idempotent and does not grow on repeat calls.
    #[test]
    fn caching_the_same_ref_twice_stores_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.key");
        std::fs::write(&path, "v").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let r = SecretRef::File(path);

        let before = preseal_cached_count();
        cache_before_seal(&r).unwrap();
        cache_before_seal(&r).unwrap();
        assert_eq!(preseal_cached_count(), before + 1);
    }

    use super::*;

    #[test]
    fn resolve_blocking_env_and_missing() {
        unsafe { std::env::set_var("MUR_TEST_SECRET_BLOCKING", "s3cret") };
        let r: SecretRef = "env:MUR_TEST_SECRET_BLOCKING".parse().unwrap();
        assert_eq!(r.resolve_to_string_blocking().as_deref(), Some("s3cret"));
        unsafe { std::env::remove_var("MUR_TEST_SECRET_BLOCKING") };
        assert!(r.resolve_blocking().is_err());
    }

    /// `#[tokio::test]` runs on a current-thread runtime, where
    /// `block_in_place` panics. `resolve_blocking` must detect the flavor and
    /// hop to a fresh thread instead (the crash behind the flaky rollup
    /// tests on machines whose config carries secret refs).
    #[tokio::test]
    async fn resolve_blocking_inside_current_thread_runtime_does_not_panic() {
        unsafe { std::env::set_var("MUR_TEST_SECRET_CT_RT", "s3cret") };
        let r: SecretRef = "env:MUR_TEST_SECRET_CT_RT".parse().unwrap();
        assert_eq!(r.resolve_to_string_blocking().as_deref(), Some("s3cret"));
        unsafe { std::env::remove_var("MUR_TEST_SECRET_CT_RT") };
    }
}
