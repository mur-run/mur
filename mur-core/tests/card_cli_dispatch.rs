//! M4.7.3: round-trip the `card export` → `import` → `list` → `accept`
//! pipeline through the library entry points the CLI dispatches to.
//!
//! Two scenarios:
//! 1. Signed export round-trips to inbox with `trust = "signed"`, and
//!    `accept` flips `profile.companion.onboarding.completed_at`.
//! 2. Unsigned export omits the `signature` block in the YAML.

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

use mur_common::identity::AgentIdentity;

/// Cargo runs integration tests in parallel threads; `MUR_HOME` is a
/// process-global env var, so we serialise tests that mutate it.
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

fn fixture_profile() -> String {
    std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .or_else(|_| std::fs::read_to_string("mur-common/tests/fixtures/profile_p0a_minimal.yaml"))
        .unwrap()
}

fn provision_agent(tmp: &TempDir, name: &str) -> std::path::PathBuf {
    let agent_dir = tmp.path().join("agents").join(name);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture_profile().replace("name: agent_test", &format!("name: {name}")),
    )
    .unwrap();
    AgentIdentity::generate().save(&agent_dir).unwrap();
    agent_dir
}

#[tokio::test]
async fn export_signed_round_trips_through_import_list_accept() {
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    let name = "card-cli-signed";
    let agent_dir = provision_agent(&tmp, name);

    // Signed export.
    let exported = mur_core::cmd::agent_companion::card::export::export_card(name, None, true)
        .await
        .unwrap();
    assert!(exported.exists());
    let yaml = std::fs::read_to_string(&exported).unwrap();
    assert!(
        yaml.contains("signature"),
        "signed export must embed a signature block"
    );

    // Import the just-exported file. trust must read back as "signed".
    let imported = mur_core::cmd::agent_companion::card::import::import_card(name, &exported)
        .await
        .unwrap();
    assert_eq!(imported.trust, "signed");

    // List sees exactly one entry.
    let entries = mur_core::cmd::agent_companion::card::list::list_inbox(name).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, imported.id);
    assert_eq!(entries[0].trust, "signed");

    // Accept promotes the inbox card → profile.companion.onboarding.
    mur_core::cmd::agent_companion::card::accept::accept_card(name, &entries[0].id)
        .await
        .unwrap();
    let profile_yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    let profile: serde_yaml_ng::Value = serde_yaml_ng::from_str(&profile_yaml).unwrap();
    assert!(
        profile["companion"]["onboarding"]["completed_at"].is_string(),
        "accept must stamp onboarding.completed_at",
    );
}

#[tokio::test]
async fn export_unsigned_omits_signature_block() {
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    let name = "card-cli-unsigned";
    let _agent_dir = provision_agent(&tmp, name);

    let exported = mur_core::cmd::agent_companion::card::export::export_card(name, None, false)
        .await
        .unwrap();
    let yaml = std::fs::read_to_string(&exported).unwrap();
    assert!(
        !yaml.contains("signature"),
        "unsigned export must not contain a signature block; got:\n{yaml}",
    );
}
