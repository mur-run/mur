//! Confirm legacy inline `model:` and new `model_ref:` both resolve.
//!
//! Tests mutate `MUR_HOME` (the cross-platform escape hatch — on Windows
//! `dirs::home_dir()` calls SHGetKnownFolderPath and ignores HOME) so they
//! serialize on a static Mutex; `std::env::set_var` is process-global and
//! parallel cargo-test threads would otherwise race the registry path lookup.

use mur_agent_runtime::supervisor::resolve_model_entry;
use mur_common::agent::AgentProfile;
use mur_common::model::{ModelEntry, ModelRegistry};
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn legacy_profile_resolves_inline() {
    // No HOME mutation — does not need the lock.
    let yaml = include_str!("fixtures/profile_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    let entry = resolve_model_entry(&p).unwrap();
    assert_eq!(entry.provider, "ollama");
    assert_eq!(entry.model, "m");
    assert!(entry.secret.is_none());
}

#[test]
fn model_ref_resolves_from_registry() {
    let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mur_home = dir.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // SAFETY: MUR_HOME mutation guarded by HOME_LOCK above.
    unsafe {
        std::env::set_var("MUR_HOME", &mur_home);
    }

    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "test_model".into(),
        ModelEntry {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: Some("http://127.0.0.1:11434".into()),
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
            ..Default::default()
        },
    );
    reg.save_to(&dir.path().join(".mur/models.yaml")).unwrap();

    let yaml_with_ref = include_str!("fixtures/profile_with_model_ref.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml_with_ref).unwrap();
    let entry = resolve_model_entry(&p).unwrap();
    assert_eq!(entry.model, "llama3.2:3b");
    assert_eq!(entry.base_url.as_deref(), Some("http://127.0.0.1:11434"));
}

#[test]
fn missing_model_ref_errors_loudly() {
    let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mur_home = dir.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // SAFETY: MUR_HOME mutation guarded by HOME_LOCK above.
    unsafe {
        std::env::set_var("MUR_HOME", &mur_home);
    }
    // Empty registry on disk.
    ModelRegistry::default()
        .save_to(&dir.path().join(".mur/models.yaml"))
        .unwrap();

    let yaml_with_ref = include_str!("fixtures/profile_with_model_ref.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml_with_ref).unwrap();
    let err = resolve_model_entry(&p).unwrap_err();
    assert!(err.to_string().contains("test_model"), "got {err}");
}
