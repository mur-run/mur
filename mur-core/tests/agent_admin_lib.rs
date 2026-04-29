//! Integration test for the public `mur_core::agent_admin` library
//! API — the surface that callers other than the `mur` CLI (e.g.
//! Tauri command handlers in `mur-agent-gui`) consume.
//!
//! Spins up a sandbox MUR_HOME with a fake agent home and exercises
//! each query function + the typed-error surface.

#![cfg(unix)]

use mur_core::agent_admin::{self, AgentAdminError};
use std::path::Path;
use std::process::Command;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

/// Each test mutates the global `MUR_HOME` env var. Cargo runs
/// integration tests across threads, so we serialise via this
/// process-wide mutex.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// RAII guard: locks `ENV_LOCK`, sets `MUR_HOME`, restores (removes)
/// it on drop — including panic unwinds. Collapses the
/// set/remove_var pair that every test would otherwise repeat.
struct MurHomeGuard {
    _lock: MutexGuard<'static, ()>,
}

impl MurHomeGuard {
    fn set(path: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap();
        // SAFETY: process-wide mutex held across the env mutation.
        unsafe { std::env::set_var("MUR_HOME", path) };
        MurHomeGuard { _lock: lock }
    }
}

impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        // SAFETY: we still hold the lock until this guard's fields
        // are dropped after this block.
        unsafe { std::env::remove_var("MUR_HOME") };
    }
}

fn mur_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

fn mur_create(mur_home: &Path, bin_dir: &Path, name: &str) {
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

    let _guard = MurHomeGuard::set(mur_home.path());
    let view = agent_admin::lifecycle::status("iview").expect("status returns Ok");
    assert_eq!(view.name, "iview");
    assert_eq!(view.kind, "stopped");
    assert!(view.pid.is_none());
    assert!(view.uptime_seconds.is_none());
    assert!(view.agent_home.starts_with(mur_home.path()));
}

#[test]
fn status_returns_agent_not_found_typed_error() {
    let mur_home = TempDir::new().unwrap();
    let _guard = MurHomeGuard::set(mur_home.path());

    let err = agent_admin::lifecycle::status("ghost").expect_err("ghost agent should miss");
    match err {
        AgentAdminError::AgentNotFound { name, path } => {
            assert_eq!(name, "ghost");
            assert!(path.starts_with(mur_home.path()));
        }
        other => panic!("expected AgentNotFound, got {other:?}"),
    }
}

#[test]
fn skill_show_returns_typed_not_found_with_kind_skill() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "skill-test");

    let _guard = MurHomeGuard::set(mur_home.path());
    let err = agent_admin::skill::show("skill-test", "no-such-skill").expect_err("missing skill");
    match err {
        AgentAdminError::NotFound { agent, kind, query } => {
            assert_eq!(agent, "skill-test");
            assert_eq!(kind, "skill");
            assert_eq!(query, "no-such-skill");
        }
        other => panic!("expected NotFound{{kind:\"skill\"}}, got {other:?}"),
    }
}

#[test]
fn perm_view_returns_default_entitlements_for_new_agent() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "perm-view");

    let _guard = MurHomeGuard::set(mur_home.path());
    let entitlements = agent_admin::perm::view("perm-view").expect("perm::view returns Ok");
    let _ = entitlements.network;
}

#[test]
fn mcp_list_starts_empty_for_new_agent() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "mcp-empty");

    let _guard = MurHomeGuard::set(mur_home.path());
    let servers = agent_admin::mcp::list("mcp-empty").expect("mcp::list returns Ok");
    assert!(
        servers.is_empty(),
        "expected zero MCP servers, got {}",
        servers.len()
    );
}
