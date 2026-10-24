//! Git push broker daemon pieces that do not depend on HITL authority (#1764).
#![cfg(unix)]

use mur_daemon::git_push_broker::{
    EnrollmentProblem, Notifier, doctor_check_enrollment, parser_policy, sweeper_enabled,
};
use mur_git_broker::policy::RemotePolicy;
use std::path::{Path, PathBuf};

fn cfg_with(home: &Path, body: &str) {
    std::fs::write(home.join("config.yaml"), body).unwrap();
}

fn policy(base: &[&str], prefix: &str) -> RemotePolicy {
    RemotePolicy {
        remote_id: "o".into(),
        canonical_remote_endpoint: "https://h/r.git".into(),
        creation_base_refs: base.iter().map(|s| s.to_string()).collect(),
        ref_prefix: prefix.into(),
        credential: None,
        read_credential: None,
    }
}

#[test]
fn sweeper_is_off_unless_git_push_enabled() {
    let h = tempfile::tempdir().unwrap();
    assert!(!sweeper_enabled(h.path()), "no config at all");
    cfg_with(h.path(), "git_push:\n  enabled: false\n");
    assert!(!sweeper_enabled(h.path()));
    cfg_with(h.path(), "git_push:\n  enabled: true\n");
    assert!(sweeper_enabled(h.path()));
}

#[test]
fn parser_runs_without_network_and_with_write_access_only_to_its_out_dir() {
    let out = Path::new("/var/tmp/out-1");
    let p = parser_policy(out);
    assert_eq!(p.fs_write, vec![out.to_path_buf()]);
    assert!(!p.network_allowed, "parser has no network");
    assert!(
        p.env_allow
            .iter()
            .all(|k| !k.contains("TOKEN") && !k.contains("KEY") && !k.contains("SECRET")),
        "no credential env reaches the parser"
    );
}

#[test]
fn request_is_notified_once_per_request_id_and_aggregated_per_agent() {
    let mut n = Notifier::default();
    assert!(n.should_notify("bob", "r1"));
    assert!(!n.should_notify("bob", "r1"), "same id: never twice");
    assert!(n.should_notify("bob", "r2"));
    let batch = n.drain_batches();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].agent, "bob");
    assert_eq!(batch[0].request_ids, ["r1", "r2"]);
    assert!(n.drain_batches().is_empty(), "drained");
    assert!(
        !n.should_notify("bob", "r1"),
        "a drained id is still never re-notified"
    );
}

#[test]
fn doctor_refuses_creation_base_refs_that_are_not_full_heads() {
    let p = policy(&["main", "refs/heads/ok"], "refs/heads/agent/");
    let problems = doctor_check_enrollment(&p, &[]);
    assert_eq!(
        problems,
        vec![EnrollmentProblem::BaseRefNotFullHead("main".into())]
    );
}

#[test]
fn doctor_refuses_a_ref_prefix_outside_the_agent_namespace() {
    let problems = doctor_check_enrollment(&policy(&[], "refs/heads/"), &[]);
    assert_eq!(
        problems,
        vec![EnrollmentProblem::RefPrefixTooWide("refs/heads/".into())]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_refuses_enrollment_when_a_grant_covers_the_broker_dir_on_linux() {
    let _env = mur_common::test_env::EnvGuard::set([("MUR_HOME", "/home/u/.mur")]);
    let grants = vec![PathBuf::from("/home/u/.mur"), PathBuf::from("/home/u/src")];
    let problems = doctor_check_enrollment(&policy(&[], "refs/heads/agent/"), &grants);
    assert_eq!(
        problems,
        vec![EnrollmentProblem::GrantCoversBrokerDir(PathBuf::from(
            "/home/u/.mur"
        ))]
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn doctor_grant_check_is_linux_only_and_says_so() {
    let p = policy(&[], "refs/heads/agent/");
    assert!(
        doctor_check_enrollment(&p, &[PathBuf::from("/")])
            .iter()
            .all(|x| !matches!(x, EnrollmentProblem::GrantCoversBrokerDir(_))),
        "macOS seal covers this; not checked here"
    );
}
