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

/// A symlink grant and its canonical target are one unit: when the deny is
/// spelled through the target, the link must fall too, or the kernel would
/// follow it straight into the denied directory.
#[cfg(unix)]
#[test]
fn a_symlink_grant_falls_with_its_canonical_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let work = root.join("work");
    let secrets = work.join("secrets");
    let other = root.join("other");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let link = root.join("work-link");
    std::os::unix::fs::symlink(&work, &link).unwrap();

    let grants = super::super::with_resolved_aliases(vec![link.clone(), other.clone()]);
    let (kept, dropped) =
        partition_against_denies(grants, std::slice::from_ref(&secrets), &p("/m/a"));

    let dropped: Vec<_> = dropped.into_iter().map(|(g, _)| g).collect();
    assert!(
        dropped.contains(&link),
        "symlink alias must be dropped: {dropped:?}"
    );
    assert!(
        dropped.contains(&work),
        "canonical alias must be dropped: {dropped:?}"
    );
    assert_eq!(kept, vec![other], "negative control: unrelated grant kept");
}

/// The reporter must see the same alias overlap the filter does.
#[cfg(unix)]
#[test]
fn unenforceable_denies_sees_through_a_symlink_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let work = root.join("work");
    let secrets = work.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let link = root.join("work-link");
    std::os::unix::fs::symlink(&work, &link).unwrap();

    let mut policy = policy_with(&p("/m/agents/a"));
    policy.fs_write = vec![link.clone()];
    policy.fs_deny = vec![secrets.clone()];
    assert_eq!(policy.unenforceable_denies(), vec![(secrets, link)]);
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

/// #2160 review: a symlink grant whose canonical target contains the deny
/// must not survive `from_entitlements` on Linux.
#[cfg(target_os = "linux")]
#[test]
fn linux_drops_a_symlink_grant_whose_target_contains_the_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let agent_home = root.join("mur").join("agents").join("a");
    let work = root.join("work");
    let secrets = work.join("secrets");
    for d in [&agent_home, &secrets] {
        std::fs::create_dir_all(d).unwrap();
    }
    let link = root.join("work-link");
    std::os::unix::fs::symlink(&work, &link).unwrap();
    let mut ent = mur_common::agent::AgentProfile::default_for_tests().entitlements;
    let s = |q: &Path| q.to_string_lossy().into_owned();
    ent.filesystem.write = vec![s(&link)];
    ent.filesystem.deny = vec![s(&secrets)];

    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert!(
        !policy.fs_write.contains(&link),
        "symlink grant must be dropped"
    );
    assert!(
        !policy.fs_write.contains(&work),
        "canonical alias must be dropped"
    );
    assert!(
        policy
            .dropped
            .iter()
            .any(|d| d.path == s(&link) && d.verb == "write"),
        "the grant as declared is reported dropped: {:?}",
        policy.dropped
    );
}
