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
            }],
        }],
    }
}

#[tokio::test]
async fn no_keys_means_no_proxy_and_no_connect() {
    let dir = tempfile::tempdir().unwrap();
    // A path that would fail to connect; it must not even be tried.
    let up = dir.path().join("nope.sock");
    assert!(
        connect_before_seal(&SshEntitlement::default(), Some(&up))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn unreachable_upstream_still_runs_the_proxy_disconnected() {
    let dir = tempfile::tempdir().unwrap();
    let up = dir.path().join("nope.sock");
    let held = connect_before_seal(&one_key(), Some(&up)).await.unwrap();
    assert!(!held.is_connected().await);
    let held = connect_before_seal(&one_key(), None).await.unwrap();
    assert!(!held.is_connected().await);
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
    })
    .await;
    assert!(sock.is_none(), "no SSH_AUTH_SOCK for the bash child");
    let s = ssh_proxy_status::read(&home).expect("status recorded");
    assert_eq!(s.label(), "off (socket_path_too_long)");
    assert!(s.explanation.contains("/tmp"), "{}", s.explanation);
    // And the bash child's SSH_AUTH_SOCK follows from `None`: unset.
    assert_eq!(crate::tools::bash_env::ssh_agent_env(sock.as_deref()), None);
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
    })
    .await
    .expect("proxy started");
    assert_eq!(sock, home.path().join(listener::SSH_PROXY_SOCKET_NAME));
    assert!(tokio::net::UnixStream::connect(&sock).await.is_ok());
}
