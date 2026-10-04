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

#[tokio::test]
async fn keychain_item_exists_is_false_when_keychain_disabled() {
    // The blocked path must short-circuit before touching any backend, so
    // doctor under MUR_KEYCHAIN_DISABLED (and every test run) stays instant.
    let _l = super::keychain_test_fixture::env_lock().await;
    let mut env = crate::test_env::EnvGuard::hold();
    env.set_var(ENV_KEYCHAIN_DISABLED, "1")
        .unset_var(ENV_KEYCHAIN_ALLOW);
    let r = keychain_item_exists("mur-agent", "nobody/ANTHROPIC_API_KEY");
    assert!(matches!(r, Ok(false)), "{r:?}");
}
