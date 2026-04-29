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

impl SecretRef {
    pub async fn resolve(&self) -> Result<SecretString, SecretError> {
        match self {
            SecretRef::Env(var) => std::env::var(var)
                .map(SecretString::from)
                .map_err(|_| SecretError::EnvNotSet(var.clone())),
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
            _ => Err(SecretError::Parse(
                "resolve not implemented for this variant yet".into(),
            )),
        }
    }
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

    let bytes = tokio::fs::read(&p).await.map_err(|e| SecretError::FileRead {
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

    let decryptor = age::Decryptor::new(bytes)
        .map_err(|e| SecretError::AgeDecrypt(e.to_string()))?;
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
        assert_eq!(
            s,
            SecretRef::Cmd("op read op://vault/item/field".into())
        );
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
        unsafe { std::env::set_var("MUR_TEST_RESOLVE_ENV", "shhh"); }
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
}

#[cfg(test)]
mod resolve_keychain_tests {
    use super::*;
    use keyring::credential::{
        Credential, CredentialApi, CredentialBuilder, CredentialBuilderApi, CredentialPersistence,
    };
    use secrecy::ExposeSecret;
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

    // keyring v3's built-in `mock` backend uses CredentialPersistence::EntryOnly:
    // each `Entry::new` returns a fresh credential with its own storage, so a
    // password set in setup is invisible to a later `Entry::new` inside
    // resolve(). We need persistence across Entry instances, so we provide
    // our own builder backed by a shared HashMap.
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

    // Serialize tests: `set_default_credential_builder` mutates a process-global,
    // and we hold the lock across `.await` points so the builder doesn't get
    // swapped out from under our resolve() call. Use tokio's async-aware Mutex
    // so Clippy's await_holding_lock lint is satisfied.
    static MOCK_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

    async fn install_mock(initial: Option<(&str, &str, &str)>) -> AsyncMutexGuard<'static, ()> {
        let g = MOCK_LOCK.lock().await;
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
