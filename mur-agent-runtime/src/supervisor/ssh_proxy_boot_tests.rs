use super::*;
use mur_common::agent::{SshHostRule, SshKeyRule};

fn one_key() -> SshEntitlement {
    SshEntitlement {
        keys: vec![SshKeyRule {
            fingerprint: format!("SHA256:{}", "A".repeat(43)),
            label: None,
            allow_hosts: vec![SshHostRule {
                host_key: format!("SHA256:{}", "B".repeat(43)),
                label: None,
                known_hosts: Vec::new(),
            }],
        }],
    }
}

#[test]
fn no_keys_means_no_proxy_and_no_connect() {
    let dir = tempfile::tempdir().unwrap();
    // A path that would fail to connect; it must not even be tried.
    let up = dir.path().join("nope.sock");
    assert!(connect_before_seal(&SshEntitlement::default(), Some(&up)).is_none());
}

/// Pre-seal runs with no runtime (GHSA-j8mv-27gx-x7xj), so the connect and
/// the later adoption are separate steps: connect here, adopt inside tokio.
#[test]
fn unreachable_upstream_still_runs_the_proxy_disconnected() {
    let dir = tempfile::tempdir().unwrap();
    let up = dir.path().join("nope.sock");
    let missing = connect_before_seal(&one_key(), Some(&up)).unwrap();
    let unset = connect_before_seal(&one_key(), None).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        assert!(!missing.adopt().is_connected().await);
        assert!(!unset.adopt().is_connected().await);
    });
}

#[test]
fn a_reachable_upstream_is_connected_before_any_runtime_exists() {
    let dir = tempfile::tempdir().unwrap();
    let up = dir.path().join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&up).unwrap();
    let pre = connect_before_seal(&one_key(), Some(&up)).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { assert!(pre.adopt().is_connected().await) });
}

#[tokio::test]
async fn over_long_home_is_off_with_its_reason_and_no_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let mut home = tmp.path().to_path_buf();
    while listener::socket_path_fits(&listener::socket_path(&home)) {
        home = home.join("a".repeat(40));
    }
    std::fs::create_dir_all(&home).unwrap();
    let sock = start_after_seal(StartArgs {
        agent_home: &home,
        agents_dir: tmp.path().to_path_buf(),
        ent: &one_key(),
        pin_trust: PinTrust::Pinned,
        upstream: Arc::new(HeldUpstream::disconnected()),
        sink: RefusalSink::new(None),
        marks: Default::default(),
        approvals: Default::default(),
    })
    .await;
    assert!(sock.is_none(), "no SSH_AUTH_SOCK for the bash child");
    let s = ssh_proxy_status::read(&home).expect("status recorded");
    assert_eq!(s.label(), "off (socket_path_too_long)");
    assert!(s.explanation.contains("/tmp"), "{}", s.explanation);
    // And the bash child's SSH_AUTH_SOCK follows from `None`: unset.
    let child = sock.map(|sock| crate::tools::bash_env::SshChild { sock, client: None });
    assert!(crate::tools::bash_env::ssh_child_env(child.as_ref()).is_empty());
}

#[tokio::test]
async fn a_normal_home_listens_in_the_agent_home() {
    let home = tempfile::tempdir().unwrap();
    let sock = start_after_seal(StartArgs {
        agent_home: home.path(),
        agents_dir: home.path().to_path_buf(),
        ent: &one_key(),
        pin_trust: PinTrust::Pinned,
        upstream: Arc::new(HeldUpstream::disconnected()),
        sink: RefusalSink::new(None),
        marks: Default::default(),
        approvals: Default::default(),
    })
    .await
    .expect("proxy started");
    assert_eq!(sock, home.path().join(listener::SSH_PROXY_SOCKET_NAME));
    assert!(tokio::net::UnixStream::connect(&sock).await.is_ok());
}

const AGENT: &str = "demo";

/// A home with a real dedicated key, or `None` (printed) without ssh-keygen.
fn home_with_key() -> Option<(tempfile::TempDir, StoredKey)> {
    let home = tempfile::tempdir().unwrap();
    match keystore::generate(home.path(), AGENT) {
        Ok(key) => Some((home, key)),
        Err(KeystoreError::Spawn { tool, .. }) => {
            eprintln!("SKIPPED: {tool} is not on PATH");
            None
        }
        Err(e) => panic!("generate: {e}"),
    }
}

fn granting(fingerprint: &str) -> SshEntitlement {
    let mut ent = one_key();
    ent.keys[0].fingerprint = fingerprint.to_string();
    ent
}

/// A loader that counts its calls and returns `result`.
fn counting(
    result: Result<(), KeystoreError>,
) -> (
    Arc<std::sync::atomic::AtomicUsize>,
    impl FnOnce(&StoredKey) -> Result<(), KeystoreError> + Send + 'static,
) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    (calls, move |_: &StoredKey| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        result
    })
}

fn count(calls: &std::sync::atomic::AtomicUsize) -> usize {
    calls.load(std::sync::atomic::Ordering::SeqCst)
}

#[test]
fn no_grant_never_reads_the_key_store() {
    let Some((home, _)) = home_with_key() else {
        return;
    };
    let (calls, loader) = counting(Ok(()));
    let got = load_dedicated_with(home.path(), AGENT, &SshEntitlement::default(), loader);
    assert_eq!(got, DedicatedLoad::NoGrant);
    assert_eq!(count(&calls), 0);
}

#[test]
fn a_grant_with_no_stored_key_loads_nothing() {
    let home = tempfile::tempdir().unwrap();
    let (calls, loader) = counting(Ok(()));
    let got = load_dedicated_with(home.path(), AGENT, &one_key(), loader);
    assert_eq!(got, DedicatedLoad::NoKey);
    assert_eq!(count(&calls), 0);
}

#[test]
fn a_stored_key_the_profile_does_not_grant_is_not_loaded() {
    let Some((home, _)) = home_with_key() else {
        return;
    };
    let (calls, loader) = counting(Ok(()));
    // `one_key` grants some other fingerprint.
    let got = load_dedicated_with(home.path(), AGENT, &one_key(), loader);
    assert_eq!(got, DedicatedLoad::NotGranted);
    assert_eq!(count(&calls), 0);
}

#[test]
fn a_granted_stored_key_is_loaded_once() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    let (calls, loader) = counting(Ok(()));
    let got = load_dedicated_with(home.path(), AGENT, &granting(&key.fingerprint), loader);
    assert_eq!(got, DedicatedLoad::Loaded);
    assert_eq!(count(&calls), 1);
}

#[test]
fn a_loader_error_is_reported_and_does_not_panic() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    let (calls, loader) = counting(Err(KeystoreError::Timeout { tool: "ssh-add" }));
    let got = load_dedicated_with(home.path(), AGENT, &granting(&key.fingerprint), loader);
    assert_eq!(got, DedicatedLoad::Failed);
    assert_eq!(count(&calls), 1);
}

#[cfg(unix)]
#[test]
fn an_unusable_stored_key_is_not_loaded() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&key.private, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (calls, loader) = counting(Ok(()));
    let got = load_dedicated_with(home.path(), AGENT, &granting(&key.fingerprint), loader);
    assert_eq!(got, DedicatedLoad::Failed);
    assert_eq!(count(&calls), 0);
}
