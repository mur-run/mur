//! Regression: `discover_blocking` and the dims-probe path are called
//! synchronously from `cmd_init`, which itself runs inside the
//! mur-main multi-threaded tokio runtime (see `main.rs::main`).
//!
//! Earlier M5 code constructed a NEW `tokio::runtime::Runtime` and
//! called `.block_on(...)` on it — that panics with "Cannot start a
//! runtime from within a runtime" because runtime construction inside
//! an existing runtime is forbidden. The fix uses
//! `tokio::task::block_in_place` + `Handle::current().block_on`.
//!
//! These tests reproduce the original panic conditions to make sure
//! the bug doesn't return.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_on_in_runtime_does_not_panic_inside_existing_runtime() {
    // The bug: building a new Runtime here would panic.
    // The fix: block_on_in_runtime uses Handle::current().block_on
    // via block_in_place, which is safe.
    let result = block_on_in_runtime(async { 42 });
    assert_eq!(result, 42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discover_blocking_does_not_panic_inside_existing_runtime() {
    // discover_blocking calls block_on_in_runtime; this is the exact
    // path cmd_init runs through. Either no local runtimes are detected
    // (returns empty Vec) or some are present (returns their models) —
    // either way it must not panic.
    let result = discover_blocking(false);
    assert!(
        result.is_ok(),
        "discover_blocking should not error inside a tokio runtime: {:?}",
        result.err()
    );
}
