//! Age-based sweep of `bash` job spools (issue #2333).
//!
//! Every `bash` call spools its output to `<agent home>/jobs/j-<uuid>.log`,
//! and the path goes back to the model so it can read the full output later.
//! Nothing else removes them. This sweep deletes spools older than
//! [`SPOOL_RETENTION`], never one whose job is still live, and never anything
//! that is not a regular `j-*.log` file directly inside the spool dir.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::bash_jobs::JobTable;

/// A spool modified within this window is kept: the model may still read it.
pub const SPOOL_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Delete stale spools in `dir`. Returns `(files removed, bytes freed)`.
/// `live` are job ids still in the job table; their spools are always kept.
pub fn sweep(dir: &Path, retention: Duration, live: &[String]) -> (usize, u64) {
    // `read_dir` follows a symlinked root, which would hand us the target's
    // entries as ordinary files. `symlink_metadata` does not follow it, so a
    // symlink here reads as "not a dir" and the sweep stops.
    // ponytail: check-then-open; a concurrent swap of `jobs/` for a symlink
    // between the two calls is not covered — openat-relative ops if it matters.
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return (0, 0);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let now = SystemTime::now();
    let (mut removed, mut freed) = (0, 0);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".log"))
            .filter(|id| id.starts_with("j-"))
        else {
            continue;
        };
        if live.iter().any(|l| l == id) {
            continue;
        }
        // `DirEntry::metadata` does not follow symlinks; `is_file` is false
        // for a symlink, so its target is never touched.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > retention);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            freed += meta.len();
        }
    }
    (removed, freed)
}

/// Run [`sweep`] on its own thread so it never blocks startup or a turn.
pub fn sweep_in_background(dir: PathBuf, jobs: Arc<JobTable>) {
    let spawned = std::thread::Builder::new()
        .name("bash-spool-sweep".into())
        .spawn(move || {
            let (removed, freed) = sweep(&dir, SPOOL_RETENTION, &jobs.running_ids());
            tracing::info!(
                dir = %dir.display(),
                removed,
                bytes_freed = freed,
                "swept stale bash job spools"
            );
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "bash spool sweep not started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn spool(dir: &Path, name: &str, age: Duration) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"output").unwrap();
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
        p
    }

    #[test]
    fn removes_only_the_old_spool() {
        let d = tempfile::tempdir().unwrap();
        let old = spool(d.path(), "j-old.log", 30 * DAY);
        let fresh = spool(d.path(), "j-fresh.log", DAY);
        let live = spool(d.path(), "j-live.log", 30 * DAY);
        let other = spool(d.path(), "notes.log", 30 * DAY);

        let (removed, freed) = sweep(d.path(), SPOOL_RETENTION, &["j-live".into()]);

        assert_eq!((removed, freed), (1, 6));
        assert!(!old.exists());
        assert!(fresh.exists() && live.exists() && other.exists());
    }

    #[test]
    fn window_covering_every_file_removes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let a = spool(d.path(), "j-a.log", 30 * DAY);
        let b = spool(d.path(), "j-b.log", DAY);

        assert_eq!(sweep(d.path(), 60 * DAY, &[]), (0, 0));
        assert!(a.exists() && b.exists());
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = spool(outside.path(), "keep.txt", 30 * DAY);
        std::os::unix::fs::symlink(&target, d.path().join("j-link.log")).unwrap();

        assert_eq!(sweep(d.path(), SPOOL_RETENTION, &[]), (0, 0));
        assert!(target.exists());
    }

    /// Review of #2334: `read_dir` follows a symlinked root, so `jobs/`
    /// pointing elsewhere must not let the sweep delete the target's files.
    #[cfg(unix)]
    #[test]
    fn never_follows_a_symlinked_spool_dir() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = spool(outside.path(), "j-old.log", 30 * DAY);
        let jobs = home.path().join("jobs");
        std::os::unix::fs::symlink(outside.path(), &jobs).unwrap();

        assert_eq!(sweep(&jobs, SPOOL_RETENTION, &[]), (0, 0));
        assert!(target.exists());
    }

    #[test]
    fn missing_dir_is_a_no_op() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(sweep(&d.path().join("jobs"), SPOOL_RETENTION, &[]), (0, 0));
    }
}
