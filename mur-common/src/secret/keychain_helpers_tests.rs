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
