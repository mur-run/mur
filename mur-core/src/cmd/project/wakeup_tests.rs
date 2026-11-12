use std::path::{Path, PathBuf};

use mur_common::reindex_request::{REINDEX_REQUEST_DIR, WAKEUP_SOFT_CAP};

use super::*;

fn denied() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::PermissionDenied)
}

fn lock_err(home: &Path) -> anyhow::Error {
    let lock = home.join(CODEBASE_INDEX_SUBDIR).join("proj.lock");
    anyhow::Error::new(IndexStateIoError::new(
        "writing index lock",
        &lock,
        denied(),
    ))
}

fn wakeups(home: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(home.join(REINDEX_REQUEST_DIR)) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn denied_lock_writes_one_wakeup_and_one_line() {
    let home = tempfile::tempdir().unwrap();
    let line = quiet_denied_line(&lock_err(home.path()), "proj", home.path());
    assert_eq!(line, "mur: queued auto-index for 'proj' with the daemon");
    assert_eq!(wakeups(home.path()).len(), 1);
}

#[test]
fn context_wrapping_does_not_hide_the_index_error() {
    let home = tempfile::tempdir().unwrap();
    let e = lock_err(home.path()).context("acquiring lock");
    let line = quiet_denied_line(&e, "proj", home.path());
    assert!(line.starts_with("mur: queued"), "{line}");
}

#[test]
fn config_denial_is_not_turned_into_a_wakeup() {
    // A fresh sandbox with no config.yaml fails in `save_config_at`, whose
    // error is an untyped io::Error. It must keep the skip line.
    let home = tempfile::tempdir().unwrap();
    let e = anyhow::Error::new(denied()).context("creating ~/.mur/config.yaml");
    let line = quiet_denied_line(&e, "proj", home.path());
    assert!(
        line.starts_with("mur: skipped auto-index for 'proj'"),
        "{line}"
    );
    assert!(wakeups(home.path()).is_empty());
}

#[test]
fn typed_denial_outside_index_dir_is_not_a_wakeup() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = home.path().join("other").join("x.lock");
    let e = anyhow::Error::new(IndexStateIoError::new(
        "writing index lock",
        &elsewhere,
        denied(),
    ));
    let line = quiet_denied_line(&e, "proj", home.path());
    assert!(line.starts_with("mur: skipped"), "{line}");
    assert!(wakeups(home.path()).is_empty());
}

#[test]
fn soft_cap_falls_back_to_skip_line() {
    let home = tempfile::tempdir().unwrap();
    for _ in 0..WAKEUP_SOFT_CAP {
        quiet_denied_line(&lock_err(home.path()), "proj", home.path());
    }
    let line = quiet_denied_line(&lock_err(home.path()), "proj", home.path());
    assert!(line.starts_with("mur: skipped"), "{line}");
    assert_eq!(wakeups(home.path()).len(), WAKEUP_SOFT_CAP);
}

#[cfg(unix)]
#[test]
fn unwritable_inbox_prints_skip_line_without_bare_os_error() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o500)).unwrap();
    let line = quiet_denied_line(&lock_err(home.path()), "proj", home.path());
    std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o700)).unwrap();
    if wakeups(home.path()).is_empty() {
        // The denial applied (not root): skip line, prefixed and attributed.
        assert!(
            line.starts_with("mur: skipped auto-index for 'proj' — "),
            "{line}"
        );
    }
}

/// Pins the diagnosis against the real lock path: with the index dir made
/// read-only, `try_acquire_lock` fails with a typed denial on the lock file
/// (not on config, not on the dir), and that is what queues the wake-up.
#[cfg(unix)]
#[test]
fn real_lock_denial_is_the_failing_op() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let _env = mur_common::test_env::EnvGuard::set([("MUR_HOME", home.path())]);
    let index_dir = home.path().join(CODEBASE_INDEX_SUBDIR);
    std::fs::create_dir_all(&index_dir).unwrap();
    std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let idx = crate::codebase::CodebaseIndex::new("proj", &home.path().join("proj"));
    let res = idx.try_acquire_lock();
    std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let Err(e) = res else {
        eprintln!("skipping: mode bits not enforced (running as root?)");
        return;
    };
    let typed = e
        .chain()
        .find_map(|c| c.downcast_ref::<IndexStateIoError>())
        .expect("lock failure must carry IndexStateIoError");
    assert_eq!(typed.op, "writing index lock");
    assert_eq!(typed.path, index_dir.join("proj.lock"));
    let line = quiet_denied_line(&e, "proj", home.path());
    assert_eq!(line, "mur: queued auto-index for 'proj' with the daemon");
    assert_eq!(wakeups(home.path()).len(), 1);
}
