use super::*;
use crate::sandbox::launch_chain::LaunchChain;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn a_grant_containing_a_deny_is_dropped_whole_with_its_reason() {
    let (kept, dropped) = partition_against_denies(
        vec![p("/w"), p("/other")],
        &[p("/w/secrets")],
        &p("/m/agents/a"),
    );
    assert_eq!(dropped, vec![(p("/w"), p("/w/secrets"))]);
    // Negative control: an unrelated grant survives, so this is not a blanket prune.
    assert_eq!(kept, vec![p("/other")]);
}

#[test]
fn a_grant_inside_a_deny_is_dropped_too() {
    let (kept, dropped) =
        partition_against_denies(vec![p("/w/secrets/x")], &[p("/w/secrets")], &p("/m/a"));
    assert!(kept.is_empty());
    assert_eq!(dropped.len(), 1);
}

#[test]
fn overlap_is_component_wise_not_string_prefix() {
    // `/w/secrets-public` is not under `/w/secrets`.
    let (kept, dropped) =
        partition_against_denies(vec![p("/w/secrets-public")], &[p("/w/secrets")], &p("/m/a"));
    assert_eq!(kept, vec![p("/w/secrets-public")]);
    assert!(dropped.is_empty());
}

#[test]
fn own_home_grants_are_never_dropped() {
    let home = p("/m/agents/a");
    let (kept, dropped) =
        partition_against_denies(vec![home.clone()], &[home.join("frozen")], &home);
    assert_eq!(kept, vec![home]);
    assert!(dropped.is_empty());
}

#[test]
fn record_deny_drops_writes_a_reportable_row() {
    let mut out = Vec::new();
    record_deny_drops(&[(p("/w"), p("/w/secrets"))], "write", &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].path, "/w");
    assert_eq!(out[0].verb, "write");
    assert!(out[0].reason.contains("/w/secrets"), "{}", out[0].reason);
}

fn policy_with(home: &Path) -> SandboxPolicy {
    SandboxPolicy {
        launch_chain: LaunchChain::for_test(home, &p("/m/bin"), &p("/h")),
        ..SandboxPolicy::default()
    }
}

#[test]
fn a_deny_inside_a_surviving_grant_is_reported_unenforceable() {
    let home = p("/m/agents/a");
    let mut policy = policy_with(&home);
    policy.fs_write = vec![home.clone(), p("/tools")];
    policy.fs_deny = vec![home.join("frozen"), p("/elsewhere")];
    // Self-protected files are runtime denies, not user entries.
    policy
        .fs_deny
        .push(home.join(SELF_PROTECTED_AGENT_FILES[0]));

    assert_eq!(
        policy.unenforceable_denies(),
        vec![(home.join("frozen"), home.clone())],
        "only the user deny inside a grant counts; one outside every grant is \
         enforced by default-deny, and self-protected files are excluded"
    );

    policy.record_unenforceable_denies();
    assert_eq!(policy.dropped.len(), 1);
    assert_eq!(policy.dropped[0].verb, "deny");
    assert!(
        policy.dropped[0]
            .reason
            .contains("not enforced by the kernel")
    );
}

/// End-to-end through `from_entitlements` on the backend that cannot carve.
#[cfg(target_os = "linux")]
#[test]
fn linux_drops_user_grants_that_overlap_a_user_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join("mur");
    let agent_home = mur_home.join("agents").join("a");
    let work = tmp.path().join("work");
    let secrets = work.join("secrets");
    let notes = tmp.path().join("notes");
    for d in [&agent_home, &secrets, &notes] {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut ent = mur_common::agent::AgentProfile::default_for_tests().entitlements;
    let s = |q: &Path| q.to_string_lossy().into_owned();
    ent.filesystem.write = vec![s(&work), s(&notes)];
    ent.filesystem.read = vec![s(&work)];
    ent.filesystem.deny = vec![s(&secrets)];

    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert!(
        !policy.fs_write.contains(&work),
        "overlapping write grant must be dropped"
    );
    assert!(
        !policy.fs_read.contains(&work),
        "overlapping read grant must be dropped"
    );
    assert!(
        policy.fs_write.contains(&notes),
        "negative control: unrelated grant kept"
    );
    let rows: Vec<_> = policy
        .dropped
        .iter()
        .filter(|d| d.path == s(&work))
        .map(|d| d.verb.as_str())
        .collect();
    assert_eq!(rows, vec!["read", "write"]);
}
