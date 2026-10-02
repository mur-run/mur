/// Serialises the tests that touch [`PRESEAL_CACHE`].
///
/// That cache is a process-global. `cargo test` runs every test in this
/// module in ONE process on a thread pool, so two tests inserting
/// concurrently corrupt each other's view of the entry count — the delta
/// one test measures includes rows another test just added. `cargo
/// nextest` gives each test its own process, which hides the problem
/// rather than removing it, and nextest is this repo's canonical runner.
///
/// Poison is deliberately ignored: a panic in one guarded test must fail
/// that test alone, not cascade into every other one.
static PRESEAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A secret cached before the seal resolves afterwards even when the path
/// has become unreachable — which is what a sandboxed agent faces for a
/// `file:` ref inside the denied credential store (#866).
///
/// Simulated by deleting the file after caching: post-seal the path is gone
/// as far as the process is concerned, exactly as a deny makes it.
#[test]
fn a_cached_secret_survives_its_path_becoming_unreachable() {
    let _preseal_guard = PRESEAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// `resolve_preseal_cached` returns the cached value and NEVER reaches a
/// backend — the property the per-agent Keychain path depends on.
///
/// That path (`from_agent_credentials`) calls `keychain_get` directly
/// rather than going through `resolve_blocking`, so the first cut of the
/// pre-seal fix did not cover it at all: an agent whose model entry has no
/// `secret:` of its own still broke on every upgrade.
#[test]
fn preseal_cached_lookup_does_not_touch_the_backend() {
    let _preseal_guard = PRESEAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = SecretRef::Keychain {
        service: "mur-agent-test-nonexistent".into(),
        account: "no-such-agent/NO_SUCH_KEY".into(),
    };
    // Never cached, and the backend has no such item: a miss, not a hang
    // and not an error.
    assert!(r.resolve_preseal_cached().is_none());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.key");
    std::fs::write(&path, "sk-agent").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let f = SecretRef::File(path.clone());
    cache_before_seal(&f).unwrap();
    std::fs::remove_file(&path).unwrap();

    use secrecy::ExposeSecret;
    let got = f.resolve_preseal_cached().expect("cached hit");
    assert_eq!(got.expose_secret(), "sk-agent");
}

/// Caching is idempotent and does not grow on repeat calls.
#[test]
fn caching_the_same_ref_twice_stores_one_entry() {
    let _preseal_guard = PRESEAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let mut env = crate::test_env::EnvGuard::set([("MUR_TEST_SECRET_BLOCKING", "s3cret")]);
    let r: SecretRef = "env:MUR_TEST_SECRET_BLOCKING".parse().unwrap();
    assert_eq!(r.resolve_to_string_blocking().as_deref(), Some("s3cret"));
    // Load-bearing, not cleanup: the `_and_missing` half of this test is
    // that the same ref fails once the variable is gone.
    env.unset_var("MUR_TEST_SECRET_BLOCKING");
    assert!(r.resolve_blocking().is_err());
}

/// `#[tokio::test]` runs on a current-thread runtime, where
/// `block_in_place` panics. `resolve_blocking` must detect the flavor and
/// hop to a fresh thread instead (the crash behind the flaky rollup
/// tests on machines whose config carries secret refs).
#[tokio::test]
async fn resolve_blocking_inside_current_thread_runtime_does_not_panic() {
    let _env = crate::test_env::EnvGuard::set([("MUR_TEST_SECRET_CT_RT", "s3cret")]);
    let r: SecretRef = "env:MUR_TEST_SECRET_CT_RT".parse().unwrap();
    assert_eq!(r.resolve_to_string_blocking().as_deref(), Some("s3cret"));
}
