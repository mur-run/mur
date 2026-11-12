//! Payload-free codebase reindex wake-ups.
//!
//! A sandboxed agent cannot write `<mur_home>/indexes/codebase/`, so when its
//! post-commit hook is denied the index lock it drops an empty file here
//! instead, and `mur-daemon` rebuilds stale indexes outside the sandbox.
//!
//! The file carries no content on purpose: the daemon never opens it, it only
//! counts and unlinks entries and then decides on its own which projects are
//! stale. That is what lets an attacker-writable inbox stay harmless.
//!
//! Lives in `mur-common` because the runtime (which creates the directory at
//! seal time) must not depend on `mur-core`.

use std::path::{Path, PathBuf};

/// File-drop directory, relative to the MUR home.
pub const REINDEX_REQUEST_DIR: &str = "inbox/reindex-requests";

/// Above this many pending wake-ups a new one is not written. A wake-up is
/// only a hint — one pending entry already makes the daemon look — so the cap
/// just keeps a commit loop from filling the inbox.
pub const WAKEUP_SOFT_CAP: usize = 8;

/// Prefix of the temp name used for the write-then-rename. Entries starting
/// with it are in flight and are not counted toward the cap.
const TMP_PREFIX: &str = ".tmp-";

/// What [`drop_wakeup`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeupOutcome {
    /// A new wake-up was written at this path.
    Queued(PathBuf),
    /// The inbox already holds at least [`WAKEUP_SOFT_CAP`] entries.
    AtCap,
}

/// Write one empty wake-up into `<mur_home>/`[`REINDEX_REQUEST_DIR`]
/// (temp file + rename, so the daemon never sees a half-made entry).
///
/// The directory is normally created at seal time so the Landlock rule
/// attaches; the `create_dir_all` here is only a fallback.
pub fn drop_wakeup(mur_home: &Path) -> std::io::Result<WakeupOutcome> {
    let dir = mur_home.join(REINDEX_REQUEST_DIR);
    std::fs::create_dir_all(&dir)?;
    if pending_count(&dir)? >= WAKEUP_SOFT_CAP {
        return Ok(WakeupOutcome::AtCap);
    }
    let name = uuid::Uuid::new_v4().to_string();
    let tmp = dir.join(format!("{TMP_PREFIX}{name}"));
    let dest = dir.join(&name);
    std::fs::write(&tmp, b"")?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(WakeupOutcome::Queued(dest))
}

/// Count settled entries, stopping as soon as the cap is reached so a flooded
/// directory costs at most `WAKEUP_SOFT_CAP` reads.
fn pending_count(dir: &Path) -> std::io::Result<usize> {
    let mut n = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            continue;
        }
        n += 1;
        if n >= WAKEUP_SOFT_CAP {
            break;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(home: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(home.join(REINDEX_REQUEST_DIR))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    #[test]
    fn writes_one_empty_entry_and_creates_dir() {
        let home = tempfile::tempdir().unwrap();
        let out = drop_wakeup(home.path()).unwrap();
        let WakeupOutcome::Queued(p) = out else {
            panic!("expected Queued, got {out:?}")
        };
        assert_eq!(entries(home.path()), vec![p.clone()]);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0);
        assert!(
            !p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(TMP_PREFIX)
        );
    }

    #[test]
    fn soft_cap_is_respected() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..WAKEUP_SOFT_CAP {
            assert!(matches!(
                drop_wakeup(home.path()).unwrap(),
                WakeupOutcome::Queued(_)
            ));
        }
        assert_eq!(drop_wakeup(home.path()).unwrap(), WakeupOutcome::AtCap);
        assert_eq!(entries(home.path()).len(), WAKEUP_SOFT_CAP);
    }

    #[test]
    fn in_flight_temp_files_do_not_count_toward_cap() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(REINDEX_REQUEST_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..WAKEUP_SOFT_CAP {
            std::fs::write(dir.join(format!("{TMP_PREFIX}{i}")), b"").unwrap();
        }
        assert!(matches!(
            drop_wakeup(home.path()).unwrap(),
            WakeupOutcome::Queued(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_inbox_is_an_error_not_a_panic() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(REINDEX_REQUEST_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let res = drop_wakeup(home.path());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Root ignores mode bits; only assert when the denial actually applied.
        if let Err(e) = res {
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        }
    }
}
