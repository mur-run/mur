//! Serialized, panic-safe access to process-global environment variables.
//!
//! Test-only in purpose, compiled always: `mur-agent-runtime`'s tests need it
//! and a `#[cfg(test)]` item in this crate is invisible to them. It is ~60
//! lines of dead code in a release binary.
//!
//! # Why one lock for every variable
//!
//! `setenv(3)` may reallocate the `environ` array, so a concurrent `getenv`
//! anywhere in the process — including inside libc or a dependency, for a
//! variable this test has never heard of — can read freed memory. That is why
//! Rust 2024 made `std::env::set_var` `unsafe`. The hazard is the array, not
//! the name, so a per-variable lock (which this replaces) gives false comfort:
//! it orders writers of `MUR_HOME` against each other and does nothing about
//! the reader of `PATH` two threads over.
//!
//! # Why a guard rather than a save/restore pair
//!
//! The pattern this replaces saved the prior value, set the variable, ran the
//! test, then restored — with the restore *after* the assertions. A failing
//! assertion panics past it, so the variable outlives the `TempDir` it points
//! at, and a `std::sync::Mutex` held across that panic is poisoned for every
//! test after it. One failed assertion became a file of failures that named
//! the lock instead of the bug. Restoring in `Drop` is what makes the restore
//! actually run; tolerating poison is what keeps the cascade from starting.

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Holds the process's environment lock and restores every variable it
/// touched when dropped — including back to *absent*.
pub struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(OsString, Option<OsString>)>,
}

impl EnvGuard {
    /// Take the lock without changing anything — for a test that only needs
    /// to be alone with the environment, or that will `set_var` later.
    pub fn hold() -> Self {
        Self {
            // Poison means some earlier test panicked while holding this. That
            // is a fact about that test, not about this one, and the values it
            // set were restored by its own `Drop` before the poison was set.
            _lock: ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
            saved: Vec::new(),
        }
    }

    /// Take the lock and set these variables.
    pub fn set<K, V>(vars: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut g = Self::hold();
        for (k, v) in vars {
            g.set_var(k, v);
        }
        g
    }

    /// Take the lock and remove these variables.
    pub fn unset<K: AsRef<OsStr>>(vars: impl IntoIterator<Item = K>) -> Self {
        let mut g = Self::hold();
        for k in vars {
            g.unset_var(k);
        }
        g
    }

    /// Set one variable inside this guard's critical section.
    pub fn set_var<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, value: V) -> &mut Self {
        self.remember(key.as_ref());
        // SAFETY: `ENV_LOCK` is held, and it is the only lock any environment
        // mutation in this workspace takes, so no other test thread is in
        // `setenv`/`getenv` on our behalf. Restored in `Drop`.
        unsafe { std::env::set_var(key.as_ref(), value.as_ref()) };
        self
    }

    /// Remove one variable inside this guard's critical section.
    pub fn unset_var<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.remember(key.as_ref());
        // SAFETY: as `set_var` above.
        unsafe { std::env::remove_var(key.as_ref()) };
        self
    }

    /// Record the value to restore — the value from BEFORE this guard, so a
    /// variable set twice still ends up where it started.
    fn remember(&mut self, key: &OsStr) {
        if self.saved.iter().any(|(k, _)| k == key) {
            return;
        }
        self.saved.push((key.to_os_string(), std::env::var_os(key)));
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, prior) in self.saved.drain(..) {
            // SAFETY: the lock is still held — it is dropped after this.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: &str = "MUR_TEST_ENV_GUARD";

    #[test]
    fn a_variable_absent_before_is_absent_after() {
        {
            let _g = EnvGuard::set([(K, "x")]);
            assert_eq!(std::env::var(K).as_deref(), Ok("x"));
        }
        assert!(
            std::env::var_os(K).is_none(),
            "absent must restore to absent"
        );
    }

    #[test]
    fn setting_twice_restores_to_the_original_not_the_middle() {
        let k = "MUR_TEST_ENV_GUARD_TWICE";
        {
            let mut g = EnvGuard::set([(k, "first")]);
            g.set_var(k, "second");
            assert_eq!(std::env::var(k).as_deref(), Ok("second"));
        }
        assert!(std::env::var_os(k).is_none());
    }

    #[test]
    fn a_panic_still_restores() {
        // The defect this type exists for. The pattern it replaces restored
        // AFTER the assertions, so a failing one skipped the restore and left
        // the variable pointing at a TempDir that was about to be deleted.
        let k = "MUR_TEST_ENV_GUARD_PANIC";
        // `r.is_err()` alone is not enough: if the guard itself panicked on
        // acquisition the variable was never set, and the assertion below
        // would pass without Drop doing anything. This flag says we reached
        // the panic with the variable actually set — the first draft of this
        // test passed under exactly the break it exists to catch.
        static REACHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let r = std::panic::catch_unwind(|| {
            let _g = EnvGuard::set([(k, "leaked?")]);
            assert_eq!(std::env::var(k).as_deref(), Ok("leaked?"));
            REACHED.store(true, std::sync::atomic::Ordering::SeqCst);
            panic!("a failing assertion");
        });
        assert!(r.is_err(), "the panic must actually have happened");
        assert!(
            REACHED.load(std::sync::atomic::Ordering::SeqCst),
            "the variable must have been set before the panic, or this proves nothing"
        );
        assert!(
            std::env::var_os(k).is_none(),
            "Drop runs on unwind; a save/restore pair does not"
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_cascade() {
        // `.lock().unwrap()` on a Mutex poisoned by any earlier panicking test
        // fails every test after it, naming the lock instead of the bug.
        let k = "MUR_TEST_ENV_GUARD_POISON";
        let _ = std::panic::catch_unwind(|| {
            let _g = EnvGuard::hold();
            panic!("poison the lock");
        });
        let _g = EnvGuard::set([(k, "still works")]);
        assert_eq!(std::env::var(k).as_deref(), Ok("still works"));
    }

    #[test]
    fn unset_restores_a_value_that_was_there() {
        let k = "MUR_TEST_ENV_GUARD_UNSET";
        // SAFETY: single-threaded setup for this test's own fixture, and the
        // guard below takes the lock before anything else touches it.
        unsafe { std::env::set_var(k, "original") };
        {
            let _g = EnvGuard::unset([k]);
            assert!(std::env::var_os(k).is_none());
        }
        assert_eq!(std::env::var(k).as_deref(), Ok("original"));
        unsafe { std::env::remove_var(k) };
    }
}

/// Every environment mutation in the converted crates goes through [`EnvGuard`].
///
/// A ratchet, not a clean bill of health. `mur-common` and `mur-agent-runtime`
/// are converted and are guarded here; the rest of the workspace is not, and
/// the counts below say by how much. Convert a crate, add it to `GUARDED`, and
/// it can never regress. Until then this test says nothing about it.
///
/// It lives in one place rather than one copy per crate because a
/// `#[cfg(test)]` item here is NOT compiled into a crate that depends on
/// `mur-common` — per-crate copies would each silently cover only themselves.
///
/// Exemptions are named individually with a reason of one kind: "runs before
/// any thread exists". Not "uniquely named variable" (the hazard is the
/// `environ` array, not the name) and not "nextest isolates tests" (true of
/// CI, false of the `cargo test` that CLAUDE.md documents) — those were the
/// two justifications this replaced.
#[cfg(test)]
#[test]
fn converted_crates_never_mutate_the_environment_directly() {
    const GUARDED: &[&str] = &["mur-common", "mur-agent-runtime"];
    /// Mutation that happens before any thread could observe it.
    const ALLOWED: &[(&str, &str)] = &[
        (
            "mur-common/src/test_env.rs",
            "the guard's own implementation, which holds the lock",
        ),
        (
            "mur-agent-runtime/src/supervisor.rs",
            "argv0 name stash at startup, before tokio spawns",
        ),
    ];
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf();
    let mut offenders = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = GUARDED
        .iter()
        .map(|c| workspace.join(c).join("src"))
        .collect();
    assert_eq!(stack.len(), GUARDED.len(), "every guarded crate must exist");
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&workspace)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if ALLOWED.iter().any(|(f, _)| rel == *f) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (i, line) in body.lines().enumerate() {
                if line.contains("env::set_var") || line.contains("env::remove_var") {
                    offenders.push(format!("{rel}:{}", i + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use mur_common::test_env::EnvGuard — it serializes the mutation and \
         restores it on unwind, which a set/restore pair does not: {offenders:?}"
    );
}
