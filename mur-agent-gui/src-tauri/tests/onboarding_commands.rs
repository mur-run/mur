//! Integration tests for the D2 onboarding Tauri commands
//! (M2.4.1 / M2.4.2 / M2.4.3).
//!
//! These tests exercise the `_impl` helpers that back each Tauri
//! `#[tauri::command]` wrapper. The command wrappers themselves only
//! map `anyhow::Error -> String`; the core logic lives in the helpers
//! so they can be unit-tested without a webview.
//!
//! All three commands respect `MUR_HOME` via `mur_core::paths::mur_root`,
//! which is what the CLI side does too.

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

/// Cargo runs integration tests in parallel threads; `MUR_HOME` is a
/// process-global env var, so we serialise the tests that mutate it.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct MurHomeGuard {
    _lock: MutexGuard<'static, ()>,
}

impl MurHomeGuard {
    fn set(path: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: process-wide mutex held across the env mutation; cleared
        // again in `Drop`.
        unsafe { std::env::set_var("MUR_HOME", path) };
        MurHomeGuard { _lock: lock }
    }
}

impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        // SAFETY: still holding `_lock` until our fields drop.
        unsafe { std::env::remove_var("MUR_HOME") };
    }
}

/// Load the shared P0a fixture and substitute `name: agent_test` →
/// `name: <name>` so the same baseline profile can host any agent.
fn fixture_with_name(name: &str) -> String {
    let candidates = [
        "../../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
        "../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
        "mur-common/tests/fixtures/profile_p0a_minimal.yaml",
    ];
    let yaml = candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| {
            // Last-ditch resolve via CARGO_MANIFEST_DIR for direct
            // `cargo test` invocations.
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
            std::fs::read_to_string(p).expect("fixture must exist")
        });
    yaml.replace("name: agent_test", &format!("name: {name}"))
}

fn seed_agent(mur_home: &Path, name: &str) {
    let agent_dir = mur_home.join("agents").join(name);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("profile.yaml"), fixture_with_name(name)).unwrap();
}

// ─── M2.4.1 — `companion_onboarding_status` ────────────────────────────────

#[tokio::test]
async fn onboarding_status_returns_pending_for_new_agent() {
    let tmp = TempDir::new().unwrap();
    let _guard = MurHomeGuard::set(tmp.path());
    seed_agent(tmp.path(), "fresh");

    let s = mur_agent_gui_lib::commands::companion_onboarding_status_impl("fresh")
        .await
        .expect("status must succeed for a fresh agent");

    assert!(
        s.completed_at.is_none(),
        "fresh agent: not onboarded — completed_at should be None"
    );
    assert!(s.agent_display_name.is_none());
    assert!(s.first_memory_text.is_none());
    assert_eq!(
        s.proactive_tier, "off",
        "default config has companion.enabled=false → tier off"
    );
}

#[tokio::test]
async fn onboarding_status_missing_agent_errors() {
    let tmp = TempDir::new().unwrap();
    let _guard = MurHomeGuard::set(tmp.path());
    // No agent directory — should bubble up the file-not-found error.
    let res = mur_agent_gui_lib::commands::companion_onboarding_status_impl("nope").await;
    assert!(res.is_err(), "missing agent dir must error");
}
