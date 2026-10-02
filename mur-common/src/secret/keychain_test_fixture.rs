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

/// Returns the lock guard AND the env guard. Both must outlive the test
/// body: `ENV_KEYCHAIN_ALLOW` is what lifts `keychain_blocked`, and under
/// nextest (`NEXTEST` is always set) the block is on by default, so a
/// guard dropped when this function returns leaves the mock unreachable
/// and every lookup fails with `KeychainNotFound`. The pre-`EnvGuard`
/// code set the var permanently, which is why the lifetime mattered only
/// once it became RAII.
pub(super) async fn install_mock(
    initial: Option<(&str, &str, &str)>,
) -> (AsyncMutexGuard<'static, ()>, crate::test_env::EnvGuard) {
    let g = MOCK_LOCK.lock().await;
    // The mock never reaches the real OS keychain, so lift the automatic
    // test-process keychain block (`keychain_blocked`).
    let env = crate::test_env::EnvGuard::set([(super::ENV_KEYCHAIN_ALLOW, "1")]);
    let store: Store = Arc::new(Mutex::new(HashMap::new()));
    if let Some((svc, user, pw)) = initial {
        store
            .lock()
            .unwrap()
            .insert((svc.to_string(), user.to_string()), pw.as_bytes().to_vec());
    }
    let builder: Box<CredentialBuilder> = Box::new(SharedMockBuilder { store });
    keyring::set_default_credential_builder(builder);
    (g, env)
}
