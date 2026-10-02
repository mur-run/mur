use super::*;

#[test]
fn set_default_validates_ref_exists() {
    use mur_common::model::{ModelEntry, ModelRegistry};
    // Temp home with a seeded models.yaml containing `claude_sonnet`.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "claude_sonnet".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-sonnet-5".into(),
            ..Default::default()
        },
    );
    reg.save_to(&home.join("models.yaml")).unwrap();

    // Unknown ref → error (fail-closed); known ref → persisted to config.yaml.
    assert!(cmd_model_default(&home, "does_not_exist").is_err());
    cmd_model_default(&home, "claude_sonnet").unwrap();
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    assert_eq!(cfg.models.default.as_deref(), Some("claude_sonnet"));
}

#[test]
fn set_fallback_validates_all_refs_and_clears_on_empty() {
    use mur_common::model::{ModelEntry, ModelRegistry};
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let mut reg = ModelRegistry::default();
    for key in ["claude_sonnet", "deepseek_v4_pro"] {
        reg.models.insert(
            key.into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: key.into(),
                ..Default::default()
            },
        );
    }
    reg.save_to(&home.join("models.yaml")).unwrap();

    // One unknown ref in the chain → whole call fails, nothing persisted.
    assert!(
        cmd_model_fallback(
            &home,
            &["claude_sonnet".to_string(), "does_not_exist".to_string()]
        )
        .is_err()
    );

    cmd_model_fallback(
        &home,
        &["claude_sonnet".to_string(), "deepseek_v4_pro".to_string()],
    )
    .unwrap();
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    assert_eq!(
        cfg.models.fallback_chain,
        vec!["claude_sonnet", "deepseek_v4_pro"]
    );

    // Empty slice clears the chain.
    cmd_model_fallback(&home, &[]).unwrap();
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    assert!(cfg.models.fallback_chain.is_empty());
}
