//! The dedicated key in a runtime-private ssh-agent, against real OpenSSH
//! (D spec §Tests "Integration", plan Task 16.8).
//!
//! The rig's `ssh-agent` plays the user's agent, with the dedicated key
//! loaded the way slice 6a used to load it. The runtime's own pre-seal start
//! then runs exactly as boot does. Afterwards:
//!
//! - the user's agent no longer lists the dedicated key (clean-up);
//! - the private agent's socket and its `run-<pid>/` directory are gone;
//! - a real `ssh` login offering only the dedicated key signs through the
//!   proxy, which routes it to the private agent by blob;
//! - the user's own key still signs through the user's agent.
//!
//! No network beyond 127.0.0.1. A missing OpenSSH binary skips locally and
//! fails under CI, like `ssh_proxy_openssh.rs`.
#![cfg(unix)]

#[path = "support/openssh_rig.rs"]
mod openssh_rig;
use openssh_rig::*;

use mur_ssh_request::keystore;

const AGENT: &str = "it-dedicated";

/// The user agent's public lines, `ssh-add -L` (exit 1 = no keys).
fn user_agent_lines(sock: &Path) -> Vec<String> {
    let out = Command::new("ssh-add")
        .env("SSH_AUTH_SOCK", sock)
        .arg("-L")
        .output()
        .expect("ssh-add -L");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `<type> <base64>` of a public line, comment dropped.
fn key_part(line: &str) -> String {
    line.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn rule(fp: String, host_fp: &str) -> SshKeyRule {
    SshKeyRule {
        fingerprint: fp,
        label: None,
        allow_hosts: vec![SshHostRule {
            host_key: host_fp.to_owned(),
            label: None,
            known_hosts: Vec::new(),
        }],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dedicated_key_signs_from_the_private_agent_and_leaves_the_users() {
    for bin in ["ssh", "ssh-agent", "ssh-add", "ssh-keygen"] {
        if on_path(bin).is_none() {
            assert!(!in_ci(), "{bin} not on PATH under CI");
            eprintln!("SKIP private_agent: {bin} not on PATH");
            return;
        }
    }
    let Some(sshd) = find_sshd() else {
        assert!(!in_ci(), "sshd not found under CI");
        eprintln!("SKIP private_agent: sshd not found");
        return;
    };
    let rig = Rig::start(&sshd);
    let host_fp = fp_of(&rig.host_key);

    // The dedicated key, generated where boot looks for it. The mur home is
    // short-named inside the rig dir: the private socket path must fit.
    let home = rig.dir.join("m");
    std::fs::create_dir(&home).unwrap();
    let key = keystore::generate(&home, AGENT).expect("generate dedicated key");
    let key_dir = keystore::key_dir(&home, AGENT).unwrap();
    let dedicated = key_part(&key.public_line);

    // The server accepts it; sshd rereads authorized_keys on every login.
    let authorized = rig.dir.join("authorized_keys");
    let mut lines = std::fs::read_to_string(&authorized).unwrap();
    lines.push_str(&format!("{}\n", key.public_line));
    std::fs::write(&authorized, lines).unwrap();

    // Slice 6a's leftover: the dedicated key sits in the user's agent.
    keystore::load_into(&key, &rig.upstream_sock).expect("ssh-add into user agent");
    assert!(
        user_agent_lines(&rig.upstream_sock)
            .iter()
            .any(|l| key_part(l) == dedicated),
        "setup: the user agent holds the dedicated key"
    );

    // Boot's pre-seal start, with the user's agent as the inherited one.
    // EnvGuard serializes the mutation and restores it when the test ends.
    let _env = mur_common::test_env::EnvGuard::set([("SSH_AUTH_SOCK", &rig.upstream_sock)]);
    let ent = SshEntitlement {
        keys: vec![
            rule(key.fingerprint.clone(), &host_fp),
            rule(fp_of(&rig.user_key), &host_fp),
        ],
    };
    let Some(private) =
        mur_agent_runtime::supervisor::start_private_agent_for_test(&home, AGENT, &ent).await
    else {
        panic!("the private ssh-agent did not start; see the warn! above");
    };

    // Clean-up: the user's agent kept its own key and lost the dedicated one.
    let left = user_agent_lines(&rig.upstream_sock);
    assert!(
        !left.iter().any(|l| key_part(l) == dedicated),
        "the dedicated key is still in the user agent: {left:?}"
    );
    assert!(
        left.iter()
            .any(|l| key_part(l) == key_part(&pub_line(&rig.user_key))),
        "clean-up removed the user's own key: {left:?}"
    );

    // No socket, no run directory: nothing can open a second connection.
    let runs: Vec<_> = std::fs::read_dir(&key_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("run-"))
        .collect();
    assert!(runs.is_empty(), "run directory left: {runs:?}");

    // A real login offering only the dedicated key signs through the proxy.
    // The user agent no longer holds it, so only the private one can.
    let (out, refusals) = login_with_upstreams(
        &rig,
        ent.clone(),
        Answering::new(GateOutcome::Allow { decision: None }),
        Method::Hostbound,
        "ded",
        Some(private.clone()),
        &key.private,
    )
    .await;
    assert!(
        out.status.success(),
        "dedicated-key login failed; refusals {:?}\n{}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>(),
        explain(&out, &rig)
    );
    assert_signed_with(&out, Method::Hostbound, &rig);

    // The user's own key still signs, through the user's agent.
    let (out, refusals) = login_with_upstreams(
        &rig,
        ent,
        Answering::new(GateOutcome::Allow { decision: None }),
        Method::Hostbound,
        "usr",
        Some(private),
        &rig.user_key,
    )
    .await;
    assert!(
        out.status.success(),
        "user-key login failed; refusals {:?}\n{}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>(),
        explain(&out, &rig)
    );
}
