//! Shared `MurHomeGuard` for integration tests that flip `MUR_HOME`.
//!
//! Cargo treats each integration test file as its own crate, so this
//! module is `mod common;`'d into every test file that needs it. The
//! `LazyLock<Mutex<()>>` serialises parallel test threads inside a
//! single integration-test binary; cross-binary serialisation isn't
//! needed because each `cargo test` per-test-file binary owns its own
//! process.

#![allow(dead_code)] // each test file uses a subset of helpers

use std::sync::{LazyLock, Mutex, MutexGuard};

static GUARD_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub struct MurHomeGuard {
    _lock: MutexGuard<'static, ()>,
}

impl MurHomeGuard {
    pub fn set(path: &std::path::Path) -> Self {
        let lock = GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: process-wide mutex held across the env mutation; cleared
        // again in `Drop`.
        unsafe {
            std::env::set_var("MUR_HOME", path);
        }
        Self { _lock: lock }
    }
}

impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        // SAFETY: still holding `_lock` until our fields drop.
        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }
}
