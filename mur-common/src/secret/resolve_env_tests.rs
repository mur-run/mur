use super::*;
use secrecy::ExposeSecret;

#[tokio::test]
async fn resolves_env_when_set() {
    let _env = crate::test_env::EnvGuard::set([("MUR_TEST_RESOLVE_ENV", "shhh")]);
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
    let _env = crate::test_env::EnvGuard::set([("MUR_TEST_RESOLVE_TO_STRING", "kc-abc")]);
    let set = SecretRef::Env("MUR_TEST_RESOLVE_TO_STRING".into());
    assert_eq!(set.resolve_to_string().await.as_deref(), Some("kc-abc"));

    let missing = SecretRef::Env("MUR_TEST_RESOLVE_TO_STRING_UNSET".into());
    assert_eq!(missing.resolve_to_string().await, None);
}
