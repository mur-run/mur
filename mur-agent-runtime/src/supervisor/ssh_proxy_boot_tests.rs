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
        dedicated: None,
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
        dedicated: None,
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

type Calls = Arc<std::sync::atomic::AtomicUsize>;

fn count(calls: &std::sync::atomic::AtomicUsize) -> usize {
    calls.load(std::sync::atomic::Ordering::SeqCst)
}

/// An unloader that counts its calls.
fn unloader(calls: &Calls) -> impl FnOnce(&StoredKey) -> Result<bool, KeystoreError> {
    let seen = calls.clone();
    move |_: &StoredKey| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(true)
    }
}

/// A starter that counts its calls and fails with `NoIdentities`, or
/// succeeds with a handle that has no agent behind it.
fn starter(
    calls: &Calls,
    ok: bool,
) -> impl FnOnce(&Path, &StoredKey) -> Result<PreSealDedicated, StartError> {
    let seen = calls.clone();
    move |_, _| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if ok {
            Ok(PreSealDedicated::detached())
        } else {
            Err(StartError::NoIdentities)
        }
    }
}

fn counters() -> (Calls, Calls) {
    (Calls::default(), Calls::default())
}

#[test]
fn no_grant_never_reads_the_key_store_or_spawns() {
    let Some((home, _)) = home_with_key() else {
        return;
    };
    let (u, s) = counters();
    let ent = SshEntitlement::default();
    let got = start_private_with(home.path(), AGENT, &ent, unloader(&u), starter(&s, true));
    assert_eq!(got.outcome, DedicatedLoad::NoGrant);
    assert!(got.pre.is_none());
    assert_eq!((count(&u), count(&s)), (0, 0));
}

#[test]
fn a_grant_with_no_stored_key_starts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let (u, s) = counters();
    let got = start_private_with(
        home.path(),
        AGENT,
        &one_key(),
        unloader(&u),
        starter(&s, true),
    );
    assert_eq!(got.outcome, DedicatedLoad::NoKey);
    assert_eq!((count(&u), count(&s)), (0, 0));
}

#[test]
fn an_ungranted_stored_key_is_cleaned_up_but_not_started() {
    let Some((home, _)) = home_with_key() else {
        return;
    };
    let (u, s) = counters();
    // `one_key` grants some other fingerprint.
    let got = start_private_with(
        home.path(),
        AGENT,
        &one_key(),
        unloader(&u),
        starter(&s, true),
    );
    assert_eq!(got.outcome, DedicatedLoad::NotGranted);
    assert!(got.pre.is_none());
    assert_eq!((count(&u), count(&s)), (1, 0));
}

#[test]
fn a_granted_stored_key_is_cleaned_up_once_then_started() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    let (u, s) = counters();
    let ent = granting(&key.fingerprint);
    let got = start_private_with(home.path(), AGENT, &ent, unloader(&u), starter(&s, true));
    assert_eq!(got.outcome, DedicatedLoad::Started);
    assert!(got.pre.is_some());
    assert_eq!((count(&u), count(&s)), (1, 1));
}

#[test]
fn a_start_failure_is_reported_and_boot_continues() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    let (u, s) = counters();
    let ent = granting(&key.fingerprint);
    let got = start_private_with(home.path(), AGENT, &ent, unloader(&u), starter(&s, false));
    assert_eq!(got.outcome, DedicatedLoad::Failed);
    assert!(got.pre.is_none());
    assert_eq!(count(&s), 1);
}

#[test]
fn a_failed_clean_up_does_not_stop_the_start() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    let s = Calls::default();
    let ent = granting(&key.fingerprint);
    let unload = |_: &StoredKey| Err(KeystoreError::Timeout { tool: "ssh-add" });
    let got = start_private_with(home.path(), AGENT, &ent, unload, starter(&s, true));
    assert_eq!(got.outcome, DedicatedLoad::Started);
}

#[cfg(unix)]
#[test]
fn an_unusable_stored_key_is_not_started() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&key.private, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (u, s) = counters();
    let ent = granting(&key.fingerprint);
    let got = start_private_with(home.path(), AGENT, &ent, unloader(&u), starter(&s, true));
    assert_eq!(got.outcome, DedicatedLoad::Failed);
    assert_eq!((count(&u), count(&s)), (0, 0));
}

/// The real thing: a granted key ends up in a private agent, the socket and
/// run directory are gone, and the held connection lists the key.
#[cfg(unix)]
#[test]
fn a_real_start_leaves_no_socket_and_holds_the_key() {
    let Some((home, key)) = home_with_key() else {
        return;
    };
    let ent = granting(&key.fingerprint);
    let got = start_private_with(
        home.path(),
        AGENT,
        &ent,
        |_: &StoredKey| Ok(false),
        super::super::private_agent::start,
    );
    if got.outcome == DedicatedLoad::Failed {
        eprintln!("SKIPPED: ssh-agent could not be started here");
        return;
    }
    assert_eq!(got.outcome, DedicatedLoad::Started);
    let d = got.pre.unwrap();
    assert_eq!(d.blobs().len(), 1);
    let dir = keystore::key_dir(home.path(), AGENT).unwrap();
    let runs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("run-"))
        .collect();
    assert!(runs.is_empty(), "run directory removed");
}
