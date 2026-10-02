use super::*;
use mur_common::model::{ModelEntry, ModelRegistry};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn seed_models_yaml(home: &TempDir, key: &str, provider: &str, model: &str) {
    let mut models = BTreeMap::new();
    models.insert(
        key.to_string(),
        ModelEntry {
            provider: provider.to_string(),
            model: model.to_string(),
            ..Default::default()
        },
    );
    let reg = ModelRegistry {
        schema_version: 1,
        models,
        roles: BTreeMap::new(),
    };
    reg.save_to(&home.path().join("models.yaml")).unwrap();
}

#[test]
fn create_with_bare_alias_sets_model_ref() {
    let home = TempDir::new().unwrap();
    seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");

    let mr = resolve_model_ref_for_create(home.path(), None, "claude_sonnet").unwrap();

    assert_eq!(mr, Some("claude_sonnet".to_string()));
}

#[test]
fn create_with_unknown_bare_model_leaves_model_ref_unset() {
    let home = TempDir::new().unwrap();
    seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");

    // Not a registry key, and provider is None -> caller defaults to
    // ollama before ever calling this function, so this case should not
    // be reached with a non-alias bare model; verify no false alias hit.
    let mr = resolve_model_ref_for_create(home.path(), None, "llama3.2:3b").unwrap();

    assert_eq!(mr, None);
}

#[test]
fn explicit_provider_still_matches_registry_entry() {
    let home = TempDir::new().unwrap();
    seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");

    let mr =
        resolve_model_ref_for_create(home.path(), Some("anthropic"), "claude-sonnet-5").unwrap();

    assert_eq!(mr, Some("claude_sonnet".to_string()));
}
