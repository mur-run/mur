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
            _ => Err(SecretError::Parse(
                "resolve not implemented for this variant yet".into(),
            )),
        }
    }
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
