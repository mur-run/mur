//! Drain `<mur_home>/inbox/reindex-requests/` without trusting its contents.
//!
//! The inbox is agent-writable. Entries are classified from `read_dir`'s
//! file type and an `lstat` only — nothing is ever opened, so a FIFO cannot
//! block the sweep and a symlink cannot point the daemon anywhere. Every
//! entry looked at is unlinked, valid or not, so the directory cannot fill up.
//!
//! A planted directory is taken apart under the same per-sweep bound as
//! everything else (see [`remove_dir_bounded`]), so no single sweep does more
//! unlinks than `max_intake`, however much an agent put inside it.

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, SystemTime};

use mur_common::reindex_request::TMP_PREFIX;

/// An in-flight `.tmp-*` file older than this was left by a writer that died
/// between write and rename; it is removed. Younger ones belong to a writer
/// that is mid-rename and are left alone.
pub(super) const TMP_STALE_AFTER: Duration = Duration::from_secs(60);

/// Name prefix of a nested directory moved up into the inbox so a later
/// sweep takes it apart as an entry of its own.
#[cfg(unix)]
const MOVED_UP_PREFIX: &str = ".nested-";

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Swept {
    /// Entries looked at this sweep, the contents of planted directories
    /// included (never more than the intake bound).
    pub examined: usize,
    /// Regular, empty, non-temp files: the only shape a wake-up has.
    pub wakeups: usize,
}

/// Look at no more than `max_intake` entries of `dir`, counting what is
/// inside any directory found there.
pub(super) fn sweep(dir: &Path, max_intake: usize, now: SystemTime) -> Swept {
    let mut out = Swept::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd {
        if out.examined >= max_intake {
            break;
        }
        out.examined += 1;
        let Ok(entry) = entry else { continue };
        // `DirEntry::file_type` / `metadata` do not follow symlinks.
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        let is_tmp = entry.file_name().to_string_lossy().starts_with(TMP_PREFIX);
        if is_tmp {
            let fresh = ft.is_file()
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < TMP_STALE_AFTER);
            if fresh {
                continue;
            }
        } else if ft.is_file() && entry.metadata().is_ok_and(|m| m.len() == 0) {
            out.wakeups += 1;
        }
        let removed = if ft.is_dir() {
            let (spent, gone) =
                remove_dir_bounded(dir, &entry.file_name(), max_intake - out.examined);
            out.examined += spent;
            // `Ok(false)`: out of budget, the rest goes on later sweeps.
            gone.map(drop)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = removed {
            tracing::warn!(entry = %path.display(), error = %e, "could not remove reindex inbox entry");
        }
    }
    out
}

/// Take the directory `name` in `inbox` apart with at most `budget` unlinks
/// or renames, and remove it once it is empty. Returns how much of the
/// budget was spent and whether the directory is gone.
///
/// Nothing is recursed into: a file is unlinked, and a subdirectory is moved
/// up into `inbox` (one rename, whatever it holds) to be handled as an entry
/// of its own. Every step is relative to an open directory handle and never
/// follows a symlink, so swapping a directory for a link mid-sweep cannot
/// point the daemon outside the inbox.
#[cfg(unix)]
fn remove_dir_bounded(inbox: &Path, name: &OsStr, budget: usize) -> (usize, std::io::Result<bool>) {
    use rustix::fs::{AtFlags, Dir, Mode, OFlags};
    use std::sync::atomic::{AtomicU64, Ordering};
    static MOVED_UP: AtomicU64 = AtomicU64::new(0);

    let mut spent = 0;
    let mut work = || -> rustix::io::Result<bool> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let inbox_fd = rustix::fs::open(inbox, flags, Mode::empty())?;
        let fd = rustix::fs::openat(&inbox_fd, name, flags | OFlags::NOFOLLOW, Mode::empty())?;
        for child in Dir::read_from(&fd)? {
            let child = child?;
            let child = child.file_name();
            if matches!(child.to_bytes(), b"." | b"..") {
                continue;
            }
            if spent == budget {
                return Ok(false);
            }
            spent += 1;
            // No type check to race with: an unlink that fails is a directory.
            if rustix::fs::unlinkat(&fd, child, AtFlags::empty()).is_err() {
                let n = MOVED_UP.fetch_add(1, Ordering::Relaxed);
                let up = format!("{MOVED_UP_PREFIX}{}-{n}", std::process::id());
                rustix::fs::renameat(&fd, child, &inbox_fd, up)?;
            }
        }
        match rustix::fs::unlinkat(&inbox_fd, name, AtFlags::REMOVEDIR) {
            Ok(()) => Ok(true),
            // Something was added while it was being emptied; next sweep.
            Err(rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => Ok(false),
            Err(e) => Err(e),
        }
    };
    let gone = work().map_err(std::io::Error::from);
    (spent, gone)
}

/// No sandboxed agents write this inbox off unix; removal is not bounded.
#[cfg(not(unix))]
fn remove_dir_bounded(
    inbox: &Path,
    name: &OsStr,
    _budget: usize,
) -> (usize, std::io::Result<bool>) {
    (0, std::fs::remove_dir_all(inbox.join(name)).map(|()| true))
}
