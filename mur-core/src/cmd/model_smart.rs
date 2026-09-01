//! `mur model smart <on|off>` — the global Smart background-routing toggle.
//!
//! Its own module rather than another arm in `cmd/model.rs`, which is already
//! at the 800-line ceiling (CLAUDE.md rule 4), matching the `model_doctor` /
//! `model_connect` siblings.
use std::path::Path;

use crate::cmd::model::ensure_ref_exists;

/// Set `models.smart.enabled`, optionally pinning the model Smart downgrades
/// to. The ref is validated against `models.yaml` fail-closed, like every other
/// ref-taking setter.
pub fn cmd_model_smart(home: &Path, on: bool, cheap: Option<&str>) -> anyhow::Result<()> {
    if let Some(c) = cheap {
        ensure_ref_exists(home, c)?;
    }
    let mut cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    cfg.models.smart.enabled = on;
    if let Some(c) = cheap {
        cfg.models.smart.cheap = Some(c.to_string());
    }
    crate::store::config::save_config_at(&home.join("config.yaml"), &cfg)?;
    println!(
        "smart background routing = {} (cheap = {})",
        if on { "on" } else { "off" },
        cfg.models.smart.cheap.as_deref().unwrap_or("auto")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::{ModelEntry, ModelRegistry};

    #[test]
    fn toggles_and_validates_the_cheap_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "haiku".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                ..Default::default()
            },
        );
        reg.save_to(&home.join("models.yaml")).unwrap();

        cmd_model_smart(home, true, Some("haiku")).unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert!(cfg.models.smart.enabled);
        assert_eq!(cfg.models.smart.cheap.as_deref(), Some("haiku"));

        // Turning it off keeps the pinned ref — the user's choice survives.
        cmd_model_smart(home, false, None).unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert!(!cfg.models.smart.enabled);
        assert_eq!(cfg.models.smart.cheap.as_deref(), Some("haiku"));

        // Unknown ref is refused before anything is written.
        assert!(cmd_model_smart(home, true, Some("nope")).is_err());
    }
}
