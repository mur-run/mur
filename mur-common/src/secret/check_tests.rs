use super::*;

#[tokio::test]
async fn check_env_present() {
    let _env = crate::test_env::EnvGuard::set([("MUR_TEST_CHECK_ENV", "1")]);
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
