use super::keychain_test_fixture::install_mock;
use super::*;
use secrecy::ExposeSecret;

#[tokio::test]
async fn blocked_process_never_reaches_keychain() {
    let _g = super::keychain_test_fixture::env_lock().await;
    let mut _env = crate::test_env::EnvGuard::unset([ENV_KEYCHAIN_ALLOW]);
    _env.set_var(ENV_KEYCHAIN_DISABLED, "1");
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
