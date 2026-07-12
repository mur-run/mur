//! Tauri commands to manage the global model-switching config (config.yaml
//! `models:` block) from the Hub. Wraps Phase-1 mur_core config load/save +
//! the same fail-closed ref validation the CLI uses.
use std::path::Path;

use mur_common::config::{Config, ModelSwitchConfig};
use mur_common::model::ModelRegistry;

/// Every model_ref referenced by the config must exist in `<home>/models.yaml`.
fn validate_refs(home: &Path, cfg: &ModelSwitchConfig) -> Result<(), String> {
    let reg = ModelRegistry::load_from(&home.join("models.yaml"))
        .map_err(|e| format!("load models.yaml: {e}"))?;
    let mut refs: Vec<&String> = Vec::new();
    if let Some(d) = &cfg.default {
        refs.push(d);
    }
    refs.extend(cfg.fallback_chain.iter());
    if let Some(c) = &cfg.routing.cheap {
        refs.push(c);
    }
    if let Some(f) = &cfg.routing.frontier {
        refs.push(f);
    }
    for r in refs {
        if !reg.models.contains_key(r) {
            return Err(format!("model_ref {r:?} not in models.yaml"));
        }
    }
    Ok(())
}

pub(crate) fn model_switch_get_impl(home: &Path) -> Result<ModelSwitchConfig, String> {
    Ok(Config::load_or_default(&home.join("config.yaml")).models)
}

pub(crate) fn model_switch_set_impl(
    home: &Path,
    next: ModelSwitchConfig,
) -> Result<ModelSwitchConfig, String> {
    validate_refs(home, &next)?;
    let mut cfg = Config::load_or_default(&home.join("config.yaml"));
    cfg.models = next;
    mur_core::store::config::save_config_at(&home.join("config.yaml"), &cfg)
        .map_err(|e| format!("save config: {e}"))?;
    Ok(cfg.models)
}

#[tauri::command]
pub fn model_switch_get() -> Result<ModelSwitchConfig, String> {
    model_switch_get_impl(&crate::mur_home_path())
}

#[tauri::command]
pub fn model_switch_set(next: ModelSwitchConfig) -> Result<ModelSwitchConfig, String> {
    model_switch_set_impl(&crate::mur_home_path(), next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::config::{ModelSwitchConfig, RoutingConfig};
    use mur_common::model::{ModelEntry, ModelRegistry};
    use tempfile::TempDir;

    fn seed_home() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let mut reg = ModelRegistry::default();
        for k in ["claude_sonnet", "deepseek_v4_pro"] {
            reg.models.insert(
                k.into(),
                ModelEntry {
                    provider: "anthropic".into(),
                    model: k.into(),
                    ..Default::default()
                },
            );
        }
        reg.save_to(&home.join("models.yaml")).unwrap();
        (dir, home)
    }

    #[test]
    fn get_returns_defaults_then_set_roundtrips() {
        let (_d, home) = seed_home();
        // Fresh home → default (empty) model-switch config.
        let got = model_switch_get_impl(&home).unwrap();
        assert_eq!(got.default, None);
        assert!(got.fallback_chain.is_empty());

        // Set a valid config → persisted + returned.
        let mut next = ModelSwitchConfig::default();
        next.default = Some("claude_sonnet".into());
        next.fallback_chain = vec!["claude_sonnet".into(), "deepseek_v4_pro".into()];
        let saved = model_switch_set_impl(&home, next.clone()).unwrap();
        assert_eq!(saved.default.as_deref(), Some("claude_sonnet"));
        // Reloading from disk reflects it.
        let reloaded = model_switch_get_impl(&home).unwrap();
        assert_eq!(
            reloaded.fallback_chain,
            vec!["claude_sonnet", "deepseek_v4_pro"]
        );
    }

    #[test]
    fn set_rejects_unknown_ref_and_persists_nothing() {
        let (_d, home) = seed_home();
        let mut bad = ModelSwitchConfig::default();
        bad.default = Some("does_not_exist".into());
        assert!(model_switch_set_impl(&home, bad).is_err());
        // Nothing persisted.
        assert_eq!(model_switch_get_impl(&home).unwrap().default, None);
    }

    #[test]
    fn set_validates_routing_and_chain_refs() {
        let (_d, home) = seed_home();
        let mut cfg = ModelSwitchConfig::default();
        cfg.routing = RoutingConfig {
            enabled: true,
            cheap: Some("claude_sonnet".into()),
            frontier: Some("nope".into()), // unknown → reject
            threshold_input_tokens: Some(1000),
        };
        assert!(model_switch_set_impl(&home, cfg).is_err());
    }
}
