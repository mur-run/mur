//! M2.3.1 — `mur agent companion init --answers` accepts the two new
//! D2-onboarding fields (`agent_display_name`, `first_memory`) plus the
//! three-tier `proactive_tier` selector. Spec §4.2 / plan §M2.3.1.
//!
//! Calls the library entrypoint directly (`agent_companion::init::run`)
//! to avoid building the `mur` binary across the full workspace just to
//! exercise YAML deserialisation.

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

/// Load the shared P0a fixture and substitute `name: agent_test` →
/// `name: <name>` so the same baseline profile can host any agent.
fn fixture_with_name(name: &str) -> String {
    let yaml = std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .unwrap_or_else(|_| {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
            std::fs::read_to_string(p).expect("fixture must exist")
        });
    yaml.replace("name: agent_test", &format!("name: {name}"))
}

#[tokio::test]
async fn answers_yaml_supports_first_memory_and_display_name() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path();
    let _guard = MurHomeGuard::set(mur_home);

    // Pre-create a minimal agent home with a parseable AgentProfile.
    let agent_dir = mur_home.join("agents/test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("profile.yaml"), fixture_with_name("test")).unwrap();

    // Five-step answers payload: extends the 3-step YAML with
    // `agent_display_name`, `first_memory`, `proactive_tier`.
    let answers_path = tmp.path().join("answers.yaml");
    std::fs::write(
        &answers_path,
        r#"locale: en-US
name_for_user: David
agent_display_name: Mochi
relationship: friend
formality: casual
extra_instructions: ""
first_memory: "Sunday in Taipei"
proactive_tier: warm_only
"#,
    )
    .unwrap();

    mur_core::cmd::agent_companion::init::run("test", Some(answers_path), false)
        .await
        .expect("companion init must succeed");

    // profile.yaml: companion.onboarding now carries display name + first memory.
    let pf: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap())
            .unwrap();
    assert_eq!(
        pf["companion"]["onboarding"]["agent_display_name"].as_str(),
        Some("Mochi")
    );
    assert!(pf["companion"]["onboarding"]["first_memory"]["text"].is_string());
    assert_eq!(
        pf["companion"]["onboarding"]["first_memory"]["text"].as_str(),
        Some("Sunday in Taipei")
    );

    // relationship.json round-trips first_memory too.
    let rel: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(agent_dir.join("companion/relationship.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rel["first_memory"]["text"].as_str(),
        Some("Sunday in Taipei")
    );

    // Three-tier toggle: warm_only → companion.enabled = true,
    // rhythm.enabled = false, proactive.enabled = false.
    assert_eq!(pf["companion"]["enabled"], serde_yaml_ng::Value::Bool(true));
    assert_eq!(
        pf["companion"]["rhythm"]["enabled"],
        serde_yaml_ng::Value::Bool(false)
    );
    assert_eq!(
        pf["companion"]["proactive"]["enabled"],
        serde_yaml_ng::Value::Bool(false)
    );
}

/// Pre-D2 (3-step) `--answers` files lack the three new fields. They
/// must continue to deserialise cleanly thanks to `#[serde(default)]`.
#[tokio::test]
async fn legacy_3step_answers_still_parse() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path();
    let _guard = MurHomeGuard::set(mur_home);

    let agent_dir = mur_home.join("agents/legacy");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("profile.yaml"), fixture_with_name("legacy")).unwrap();

    let answers_path = tmp.path().join("answers.yaml");
    std::fs::write(
        &answers_path,
        "locale: en-US\nname_for_user: Alex\nrelationship: coach\nformality: neutral\nextra_instructions: \"\"\n",
    )
    .unwrap();

    mur_core::cmd::agent_companion::init::run("legacy", Some(answers_path), false)
        .await
        .expect("legacy 3-step answers must still parse");

    let pf: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap())
            .unwrap();
    // Display name + first_memory absent in answers → onboarding stays without them.
    assert!(pf["companion"]["onboarding"]["agent_display_name"].is_null());
    assert!(pf["companion"]["onboarding"]["first_memory"].is_null());
    // Default tier when absent is WarmOnly per M2.3.1 plan.
    assert_eq!(pf["companion"]["enabled"], serde_yaml_ng::Value::Bool(true));
    assert_eq!(
        pf["companion"]["rhythm"]["enabled"],
        serde_yaml_ng::Value::Bool(false)
    );
    assert_eq!(
        pf["companion"]["proactive"]["enabled"],
        serde_yaml_ng::Value::Bool(false)
    );
}
