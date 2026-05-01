//! M2.6.3 — first-launch detection ↔ skip integration test.
//!
//! Pins the contract App.tsx relies on: a freshly-seeded agent reports
//! `completed_at = None` (wizard renders), and after
//! `companion_onboarding_skip` it reports `completed_at = Some(_)`
//! (wizard hides on next launch).
//!
//! Cargo runs each integration test file as its own crate, so the
//! `MurHomeGuard` helper in `onboarding_commands.rs` isn't importable
//! from here — the env-var dance is inlined verbatim to keep the same
//! invariants (process-wide mutex around `MUR_HOME` mutation, cleanup
//! on Drop).

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

/// Process-global mutex around `MUR_HOME` mutation. The sister test
/// file uses an identically-shaped guard; if both files happen to run
/// in the same process they each get their own lock instance, which is
/// fine because Cargo schedules integration tests in separate binaries.
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

/// Resolve the shared P0a fixture across the same candidate paths
/// `onboarding_commands.rs` uses, then rename the agent to `default`
/// so it matches the App.tsx fallback identifier.
fn fixture_default() -> String {
    let candidates = [
        "../../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
        "../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
        "mur-common/tests/fixtures/profile_p0a_minimal.yaml",
    ];
    let yaml = candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
            std::fs::read_to_string(p).expect("fixture must exist")
        });
    yaml.replace("name: agent_test", "name: default")
}

#[tokio::test]
async fn first_launch_status_pending_then_completed_after_skip() {
    let tmp = TempDir::new().unwrap();
    let _guard = MurHomeGuard::set(tmp.path());
    let agent_dir = tmp.path().join("agents").join("default");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("profile.yaml"), fixture_default()).unwrap();

    // Pre-skip: App.tsx must see this and render the wizard.
    let s = mur_agent_gui_lib::commands::companion_onboarding_status_impl("default")
        .await
        .expect("status must succeed for a fresh agent");
    assert!(
        s.completed_at.is_none(),
        "fresh agent: pending — completed_at should be None, got {:?}",
        s.completed_at
    );

    // The user dismisses the wizard via the Skip path.
    mur_agent_gui_lib::commands::companion_onboarding_skip_impl("default")
        .await
        .expect("skip must succeed");

    // Post-skip: App.tsx must see this and hide the wizard on next launch.
    let s = mur_agent_gui_lib::commands::companion_onboarding_status_impl("default")
        .await
        .expect("status after skip must succeed");
    assert!(
        s.completed_at.is_some(),
        "after skip: complete — completed_at should be Some(_)"
    );
}
