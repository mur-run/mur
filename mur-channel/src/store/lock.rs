//! The channel's sidecar append lock (`events.lock`), reentrant per thread.
//!
//! A multi-step writer — `mur channel repair` — must hold the lock across
//! its whole transition (move the log aside, write the first event of the
//! fresh chain, reseal heads) while calling ordinary append paths that take
//! the same lock themselves. `flock` is per open file description, so a
//! second open + lock from the same process would deadlock on itself. A
//! thread that already holds the lock therefore gets a no-op guard instead.
//! Other threads and processes are excluded exactly as before.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Shared,
    Exclusive,
}

thread_local! {
    /// Lock files this thread holds, with mode and nesting depth.
    static HELD: RefCell<HashMap<PathBuf, (Mode, usize)>> = RefCell::new(HashMap::new());
}

/// Held for as long as the guard lives; dropping the outermost one unlocks.
#[must_use = "the lock is released when the guard is dropped"]
pub struct AppendLock {
    path: PathBuf,
    /// `None` for a nested (reentrant) guard: the outer one owns the file.
    file: Option<File>,
}

impl AppendLock {
    pub(super) fn exclusive(path: &Path) -> Result<Self> {
        Self::acquire(path, Mode::Exclusive)
    }

    pub(super) fn shared(path: &Path) -> Result<Self> {
        Self::acquire(path, Mode::Shared)
    }

    fn acquire(path: &Path, mode: Mode) -> Result<Self> {
        let nested = HELD.with(|h| -> Result<bool> {
            let mut h = h.borrow_mut();
            match h.get_mut(path) {
                // Exclusive covers both; shared covers only shared. Upgrading
                // in place would let another reader in between, so refuse.
                Some((held, depth)) if *held == Mode::Exclusive || mode == Mode::Shared => {
                    *depth += 1;
                    Ok(true)
                }
                Some(_) => anyhow::bail!(
                    "cannot take {} exclusively while this thread holds it shared",
                    path.display()
                ),
                None => Ok(false),
            }
        })?;
        if nested {
            return Ok(Self {
                path: path.to_path_buf(),
                file: None,
            });
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        match mode {
            Mode::Exclusive => file.lock_exclusive().context("lock channel events")?,
            Mode::Shared => FileExt::lock_shared(&file).context("lock channel events (shared)")?,
        }
        HELD.with(|h| h.borrow_mut().insert(path.to_path_buf(), (mode, 1)));
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
        })
    }
}

impl Drop for AppendLock {
    fn drop(&mut self) {
        HELD.with(|h| {
            let mut h = h.borrow_mut();
            if let Some((_, depth)) = h.get_mut(&self.path) {
                *depth -= 1;
                if *depth == 0 {
                    h.remove(&self.path);
                }
            }
        });
        if let Some(file) = &self.file {
            FileExt::unlock(file).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn locked_elsewhere(path: &Path) -> bool {
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            let f = OpenOptions::new().write(true).open(&p).unwrap();
            let busy = f.try_lock_exclusive().is_err();
            if !busy {
                FileExt::unlock(&f).unwrap();
            }
            busy
        })
        .join()
        .unwrap()
    }

    #[test]
    fn nesting_on_one_thread_does_not_deadlock_and_outer_guard_owns_it() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.lock");
        let outer = AppendLock::exclusive(&path).unwrap();
        {
            let _inner = AppendLock::exclusive(&path).unwrap();
            let _read = AppendLock::shared(&path).unwrap();
        }
        assert!(locked_elsewhere(&path), "inner drop must not release");
        drop(outer);
        assert!(!locked_elsewhere(&path));
    }

    #[test]
    fn upgrading_a_shared_hold_is_refused() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.lock");
        let _r = AppendLock::shared(&path).unwrap();
        assert!(AppendLock::exclusive(&path).is_err());
    }
}
