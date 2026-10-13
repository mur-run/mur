//! Git push sweeper intake: verify, allowlist, consume-then-log, publish a
//! status. No agent is enrolled before sub-project 2 (spec §6), so a request
//! that passes every check still ends `rejected: not_enrolled`; it never
//! reaches the approval queue.
#![cfg(unix)]

use chrono::Utc;
use mur_common::git_push::{inbox_dir, status_dir};
use mur_common::identity::AgentIdentity;
use mur_daemon::git_push_broker::sweep_inboxes;
use mur_git_broker::request_file::{RequestFile, pack_path, write_request};
use std::path::{Path, PathBuf};

const AGENT: &str = "bob";

fn home_with(agents_allowed: &[&str]) -> (tempfile::TempDir, AgentIdentity) {
    let h = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    id.save(&h.path().join("agents").join(AGENT)).unwrap();
    let list = agents_allowed
        .iter()
        .map(|a| format!("\n    - {a}"))
        .collect::<String>();
    std::fs::write(
        h.path().join("config.yaml"),
        format!("git_push:\n  enabled: true\n  agents:{list}\n"),
    )
    .unwrap();
    (h, id)
}

fn request(signer: &AgentIdentity, request_id: &str, agent: &str) -> RequestFile {
    let mut r = RequestFile {
        agent: agent.into(),
        task_id: "t1".into(),
        request_id: request_id.into(),
        repo_id: "repo".into(),
        remote_id: "origin".into(),
        r#ref: "refs/heads/agent/x".into(),
        old_sha: "a".repeat(40),
        new_sha: "b".repeat(40),
        pack_sha256: "c".repeat(64),
        requested_at: Utc::now(),
        key_version: 0,
        sig: String::new(),
    };
    r.sig = signer.sign_multibase(&r.sign_input());
    r
}

fn inbox(home: &Path) -> PathBuf {
    inbox_dir(&home.join("agents").join(AGENT))
}

fn drop_request(home: &Path, r: &RequestFile) -> PathBuf {
    let p = write_request(&inbox(home), r).unwrap();
    std::fs::write(pack_path(&inbox(home), &r.request_id), b"PACK").unwrap();
    p
}

fn status(home: &Path, request_id: &str) -> Option<String> {
    std::fs::read_to_string(status_dir(home, AGENT).join(format!("{request_id}.yaml"))).ok()
}

#[test]
fn verified_request_is_consumed_and_rejected_as_not_enrolled() {
    let (h, id) = home_with(&[AGENT]);
    let p = drop_request(h.path(), &request(&id, "r1", AGENT));
    sweep_inboxes(h.path());
    assert!(!p.exists(), "request file consumed");
    assert!(
        !pack_path(&inbox(h.path()), "r1").exists(),
        "its pack consumed too"
    );
    assert_eq!(
        status(h.path(), "r1").as_deref(),
        Some("state: rejected\ncode: not_enrolled\n")
    );
}

#[test]
fn request_from_an_agent_not_in_the_allowlist_is_rejected_as_such() {
    let (h, id) = home_with(&["someone-else"]);
    let p = drop_request(h.path(), &request(&id, "r2", AGENT));
    sweep_inboxes(h.path());
    assert!(!p.exists());
    assert_eq!(
        status(h.path(), "r2").as_deref(),
        Some("state: rejected\ncode: agent_not_allowlisted\n")
    );
}

#[test]
fn request_signed_by_another_key_is_consumed_without_a_status() {
    let (h, _id) = home_with(&[AGENT]);
    let p = drop_request(h.path(), &request(&AgentIdentity::generate(), "r3", AGENT));
    sweep_inboxes(h.path());
    assert!(!p.exists(), "refused files are consumed, not retried");
    assert!(
        !pack_path(&inbox(h.path()), "r3").exists(),
        "and so is the pack beside them"
    );
    assert_eq!(
        status(h.path(), "r3"),
        None,
        "nothing unverified is published"
    );
}

#[test]
fn request_naming_another_agent_is_refused_even_if_that_agent_signed_it() {
    let (h, _bob) = home_with(&[AGENT, "eve"]);
    let eve = AgentIdentity::generate();
    eve.save(&h.path().join("agents").join("eve")).unwrap();
    // eve's genuine request, planted in bob's inbox: the inbox, not the payload, names the agent.
    let p = drop_request(h.path(), &request(&eve, "r4", "eve"));
    sweep_inboxes(h.path());
    assert!(!p.exists());
    assert_eq!(status(h.path(), "r4"), None);
    assert!(!status_dir(h.path(), "eve").join("r4.yaml").exists());
}

#[test]
fn disabled_broker_leaves_the_inbox_alone() {
    let (h, id) = home_with(&[AGENT]);
    std::fs::write(
        h.path().join("config.yaml"),
        "git_push:\n  enabled: false\n  agents: [bob]\n",
    )
    .unwrap();
    let p = drop_request(h.path(), &request(&id, "r5", AGENT));
    sweep_inboxes(h.path());
    assert!(p.exists(), "off means off: nothing consumed");
    assert_eq!(status(h.path(), "r5"), None);
}
