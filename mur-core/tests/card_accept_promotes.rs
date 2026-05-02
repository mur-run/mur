//! M4.6.1: `card accept` promotes one inbox card → applied state:
//! profile.companion fields, companion/relationship.json mirror,
//! telemetry/inputs sidecar text + provenance ledger entry, and
//! atomic move of inbox files into `.applied/`.

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

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

fn fixture_v3_png() -> Vec<u8> {
    std::fs::read("../mur-core/tests/fixtures/cards/silly-v3.png")
        .or_else(|_| std::fs::read("mur-core/tests/fixtures/cards/silly-v3.png"))
        .unwrap()
}

#[tokio::test]
async fn accept_writes_profile_and_relationship() {
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    let agent_dir = tmp.path().join("agents/accept-test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture_profile().replace("name: agent_test", "name: accept-test"),
    )
    .unwrap();

    // Import a card to get an id.
    let png_path = tmp.path().join("input.png");
    std::fs::write(&png_path, fixture_v3_png()).unwrap();
    let r = mur_core::cmd::agent_companion::card::import::import_card("accept-test", &png_path)
        .await
        .unwrap();

    mur_core::cmd::agent_companion::card::accept::accept_card("accept-test", &r.id)
        .await
        .unwrap();

    // profile.yaml updated.
    let pf: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap())
            .unwrap();
    assert_eq!(pf["companion"]["enabled"], serde_yaml_ng::Value::Bool(true));
    assert!(pf["companion"]["onboarding"]["completed_at"].is_string());
    assert_eq!(
        pf["companion"]["onboarding"]["agent_display_name"].as_str(),
        Some("TestV3"),
    );

    // companion/relationship.json present.
    let rel = std::fs::read_to_string(agent_dir.join("companion/relationship.json")).unwrap();
    assert!(rel.contains("\"version\""));

    // Provenance entry written for the card text.
    let ledger =
        mur_common::multimodal::ProvenanceLedger::new(agent_dir.join("telemetry/inputs.jsonl"));
    let entries = ledger.read_turn(1).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, "card_import");

    // Sidecar text file present.
    let inputs_dir = agent_dir.join("telemetry/inputs");
    let txt_files: Vec<_> = std::fs::read_dir(&inputs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("txt"))
        .collect();
    assert_eq!(txt_files.len(), 1);

    // Inbox files moved to .applied/.
    let inbox = agent_dir.join("inbox/cards");
    let applied = inbox.join(".applied");
    assert!(applied.exists());
    assert!(!inbox.join(format!("{}.murcard.yaml", r.id)).exists());
    assert!(applied.join(format!("{}.murcard.yaml", r.id)).exists());
}

#[tokio::test]
async fn accept_unknown_id_errors() {
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    let agent_dir = tmp.path().join("agents/no-card");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture_profile().replace("name: agent_test", "name: no-card"),
    )
    .unwrap();
    let err = mur_core::cmd::agent_companion::card::accept::accept_card("no-card", "01HBOGUS")
        .await
        .expect_err("unknown id");
    assert!(
        err.to_string().contains("01HBOGUS")
            || err.to_string().to_lowercase().contains("not found")
    );
}
