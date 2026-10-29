use super::*;
use chrono::Utc;
use mur_common::startup_override::claim::{consumed_path, withdrawn_path};
use mur_common::startup_override::{
    ConfirmedBy, IssuedBy, OVERRIDE_SCHEMA_VERSION, is_well_formed_nonce,
};

const AGENT: &str = "alpha";

fn file(nonce: &str) -> OverrideFile {
    OverrideFile {
        version: OVERRIDE_SCHEMA_VERSION,
        nonce: nonce.to_owned(),
        created_at: Utc::now(),
        issued_by: IssuedBy::Cli,
        confirmed_by: ConfirmedBy::FlagConfirm,
        no_hitl: true,
        net_unrestricted: false,
        full_perms: false,
    }
}

fn nonce() -> String {
    new_nonce().unwrap()
}

fn tmp_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(TMP_SUFFIX))
        .collect()
}

#[test]
fn nonce_is_well_formed_and_fresh() {
    let a = nonce();
    let b = nonce();
    assert!(is_well_formed_nonce(&a), "{a}");
    assert!(is_well_formed_nonce(&b), "{b}");
    assert_ne!(a, b);
}

#[test]
fn second_lock_is_busy_until_the_first_drops() {
    let d = tempfile::tempdir().unwrap();
    let first = lock(d.path(), AGENT).unwrap();
    assert!(matches!(lock(d.path(), AGENT), Err(LockError::Busy)));
    // Another agent's lock is independent.
    let _other = lock(d.path(), "beta").unwrap();
    drop(first);
    let _again = lock(d.path(), AGENT).unwrap();
}

#[test]
fn lock_rejects_a_bad_agent_name() {
    let d = tempfile::tempdir().unwrap();
    assert!(matches!(lock(d.path(), "../x"), Err(LockError::Io(_))));
}

#[test]
fn publish_writes_the_live_file_and_leaves_no_tmp() {
    let d = tempfile::tempdir().unwrap();
    let f = file(&nonce());
    publish(d.path(), AGENT, &f).unwrap();
    let live: OverrideFile =
        serde_json::from_slice(&std::fs::read(live_path(d.path(), AGENT)).unwrap()).unwrap();
    assert_eq!(live, f);
    assert!(tmp_names(d.path()).is_empty());
}

#[test]
fn publish_never_replaces_a_live_file() {
    let d = tempfile::tempdir().unwrap();
    let first = file(&nonce());
    publish(d.path(), AGENT, &first).unwrap();
    let second = file(&nonce());
    assert!(matches!(
        publish(d.path(), AGENT, &second),
        Err(PublishError::Exists)
    ));
    let live: OverrideFile =
        serde_json::from_slice(&std::fs::read(live_path(d.path(), AGENT)).unwrap()).unwrap();
    assert_eq!(live.nonce, first.nonce);
    assert!(tmp_names(d.path()).is_empty(), "failed publish left a tmp");
}

#[cfg(unix)]
#[test]
fn published_file_and_dir_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("secrets/startup-overrides");
    publish(&dir, AGENT, &file(&nonce())).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&live_path(&dir, AGENT)), 0o600);
}

#[test]
fn sweep_on_an_empty_dir_is_clear() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(sweep(d.path(), AGENT).unwrap(), Swept::Clear);
}

#[test]
fn sweep_discards_an_orphan_and_reports_its_nonce() {
    let d = tempfile::tempdir().unwrap();
    let n = nonce();
    publish(d.path(), AGENT, &file(&n)).unwrap();
    assert_eq!(
        sweep(d.path(), AGENT).unwrap(),
        Swept::Discarded { nonce: Some(n) }
    );
    assert!(!live_path(d.path(), AGENT).exists());
    assert!(!withdrawn_path(d.path(), AGENT).exists());
    // Nothing is ever put back: a publish now succeeds.
    publish(d.path(), AGENT, &file(&nonce())).unwrap();
}

#[test]
fn sweep_discards_an_unreadable_orphan() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(live_path(d.path(), AGENT), b"not json").unwrap();
    assert_eq!(
        sweep(d.path(), AGENT).unwrap(),
        Swept::Discarded { nonce: None }
    );
    assert!(!live_path(d.path(), AGENT).exists());
}

#[test]
fn sweep_never_touches_the_consumed_copy() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(consumed_path(d.path(), AGENT), b"{}").unwrap();
    assert_eq!(sweep(d.path(), AGENT).unwrap(), Swept::Clear);
    assert!(consumed_path(d.path(), AGENT).exists());
}

/// A read-only directory makes the claim's `rename` fail with `EACCES`,
/// which is neither success nor `ENOENT`.
#[cfg(unix)]
fn read_only(dir: &Path, f: impl FnOnce()) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    f();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn sweep_that_cannot_withdraw_leaves_the_orphan_live() {
    let d = tempfile::tempdir().unwrap();
    publish(d.path(), AGENT, &file(&nonce())).unwrap();
    read_only(d.path(), || {
        assert!(sweep(d.path(), AGENT).is_err());
    });
    assert!(live_path(d.path(), AGENT).exists());
}

#[test]
fn withdraw_takes_our_own_file_back() {
    let d = tempfile::tempdir().unwrap();
    let n = nonce();
    publish(d.path(), AGENT, &file(&n)).unwrap();
    assert!(matches!(withdraw(d.path(), AGENT, &n), Withdrawn::Ours));
    assert!(!live_path(d.path(), AGENT).exists());
    assert!(!withdrawn_path(d.path(), AGENT).exists());
}

#[test]
fn withdraw_leaves_a_foreign_file_and_names_its_nonce() {
    let d = tempfile::tempdir().unwrap();
    let theirs = nonce();
    publish(d.path(), AGENT, &file(&theirs)).unwrap();
    match withdraw(d.path(), AGENT, &nonce()) {
        Withdrawn::Foreign { found } => assert_eq!(found.as_deref(), Some(theirs.as_str())),
        other => panic!("{other:?}"),
    }
    assert!(withdrawn_path(d.path(), AGENT).exists());
}

#[test]
fn withdraw_after_a_consume_reports_consumed() {
    let d = tempfile::tempdir().unwrap();
    let n = nonce();
    publish(d.path(), AGENT, &file(&n)).unwrap();
    assert!(matches!(
        claim::claim(d.path(), AGENT, ClaimAs::Consume),
        Claimed::Took(_)
    ));
    assert!(matches!(withdraw(d.path(), AGENT, &n), Withdrawn::Consumed));
    assert!(
        consumed_path(d.path(), AGENT).exists(),
        "consumed copy must stay"
    );
}

#[cfg(unix)]
#[test]
fn withdraw_that_fails_leaves_the_file_live() {
    let d = tempfile::tempdir().unwrap();
    let n = nonce();
    publish(d.path(), AGENT, &file(&n)).unwrap();
    read_only(d.path(), || {
        assert!(matches!(
            withdraw(d.path(), AGENT, &n),
            Withdrawn::Failed(_)
        ));
    });
    assert!(live_path(d.path(), AGENT).exists());
}
