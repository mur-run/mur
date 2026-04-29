//! Integration test for the public `mur_core::agent_admin` library
//! API — the surface that callers other than the `mur` CLI (e.g.
//! Tauri command handlers in `mur-agent-gui`) consume.
//!
//! Spins up a sandbox MUR_HOME with a fake agent home and exercises
//! each query function + the typed-error surface. Complements the
//! in-module unit tests (which cover individual helpers) by
//! validating the cross-function contract that external callers
//! actually use.
//!
//! Per PR #41 review § Minor #16.

#![cfg(unix)]

use mur_core::agent_admin::{self, AgentAdminError};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use tempfile::TempDir;

/// Each test mutates the global `MUR_HOME` env var. Cargo runs
/// integration tests across threads by default, so without
/// serialisation thread A's `set_var` would clobber thread B mid-
/// flight and `agent_admin::*` (which reads MUR_HOME via
/// `resolve_mur_home`) would observe the wrong sandbox. Hold this
/// process-wide mutex across each test's env mutations.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn mur_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

fn mur_create(mur_home: &std::path::Path, bin_dir: &std::path::Path, name: &str) {
    let out = Command::new(mur_bin())
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(["agent", "create", name, "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        out.status.success(),
        "mur agent create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn status_returns_typed_view_for_existing_agent() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "iview");

    // SAFETY: tests run sequentially per default (single-threaded); env
    // is restored at end via Drop semantics on the TempDirs.
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("MUR_HOME", mur_home.path());
    }
    let view = agent_admin::lifecycle::status("iview").expect("status returns Ok");
    assert_eq!(view.name, "iview");
    assert_eq!(view.kind, "stopped"); // freshly created, no running.lock
    assert!(view.pid.is_none());
    assert!(view.uptime_seconds.is_none());
    assert!(
        view.agent_home.starts_with(mur_home.path()),
        "agent_home should be under MUR_HOME, got {}",
        view.agent_home.display()
    );
    unsafe {
        std::env::remove_var("MUR_HOME");
    }
}

#[test]
fn status_returns_agent_not_found_typed_error() {
    let mur_home = TempDir::new().unwrap();
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("MUR_HOME", mur_home.path());
    }
    let err = agent_admin::lifecycle::status("ghost").expect_err("ghost agent should miss");
    match err {
        AgentAdminError::AgentNotFound { name, path } => {
            assert_eq!(name, "ghost");
            assert!(
                path.starts_with(mur_home.path()),
                "path should point under sandbox MUR_HOME, got {}",
                path.display()
            );
        }
        other => panic!("expected AgentNotFound, got {other:?}"),
    }
    unsafe {
        std::env::remove_var("MUR_HOME");
    }
}

#[test]
fn skill_show_returns_typed_not_found_with_kind_skill() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "skill-test");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("MUR_HOME", mur_home.path());
    }
    let err = agent_admin::skill::show("skill-test", "no-such-skill").expect_err("missing skill");
    match err {
        AgentAdminError::NotFound { agent, kind, query } => {
            assert_eq!(agent, "skill-test");
            assert_eq!(kind, "skill");
            assert_eq!(query, "no-such-skill");
        }
        other => panic!("expected NotFound{{kind:\"skill\"}}, got {other:?}"),
    }
    unsafe {
        std::env::remove_var("MUR_HOME");
    }
}

#[test]
fn perm_view_returns_default_entitlements_for_new_agent() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "perm-view");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("MUR_HOME", mur_home.path());
    }
    let entitlements = agent_admin::perm::view("perm-view").expect("perm::view returns Ok");
    // A freshly-created agent has whatever default mur agent create
    // emits. We just sanity-check that the round-trip parses + the
    // network section exists (full schema lives in mur-common).
    let _ = entitlements.network;
    unsafe {
        std::env::remove_var("MUR_HOME");
    }
}

#[test]
fn mcp_list_starts_empty_for_new_agent() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "mcp-empty");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("MUR_HOME", mur_home.path());
    }
    let servers = agent_admin::mcp::list("mcp-empty").expect("mcp::list returns Ok");
    assert!(
        servers.is_empty(),
        "freshly created agent should have no MCP servers, got {}",
        servers.len()
    );
    unsafe {
        std::env::remove_var("MUR_HOME");
    }
}
