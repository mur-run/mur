//! First-run model resolution (I/O side). Pairs with the pure `recommend()`
//! decision tree in `mur_common::model_resolve`. Shared by the CLI
//! (`mur agent install`) and the Hub GUI wizard (Plan 3). Spec §7.3–7.5.

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::model_resolve::Hardware;
use serde::{Deserialize, Serialize};

/// A concrete resolution the user (or a flag) selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Secret ref string (e.g. "env:OPENAI_API_KEY"); recipient-supplied,
    /// never from the package.
    #[serde(default)]
    pub secret: Option<String>,
}

/// Detect host capabilities used by `recommend()` and the wizard UI.
pub fn detect_hardware() -> Hardware {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total_ram_gb = (sys.total_memory() / 1024 / 1024 / 1024) as u32;
    let apple_silicon = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let ollama_present = which_ollama();
    Hardware {
        total_ram_gb,
        apple_silicon,
        ollama_present,
    }
}

fn which_ollama() -> bool {
    std::process::Command::new("ollama")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Stable registry key for a choice, e.g. "ollama_llama3_2_3b".
pub fn choice_ref_name(choice: &ModelChoice) -> String {
    let raw = format!("{}_{}", choice.provider, choice.model);
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Write a `models.yaml` entry for `choice` and set the installed agent's
/// `model_ref` to it. Returns the registry key used.
pub fn apply_model_choice(mur_home: &Path, slug: &str, choice: &ModelChoice) -> Result<String> {
    let agent_home = mur_home.join("agents").join(slug);
    let profile_path = agent_home.join("profile.yaml");
    if !profile_path.exists() {
        bail!("agent '{slug}' not installed at {}", profile_path.display());
    }

    // 1. Upsert the registry entry.
    let reg_path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&reg_path)?;
    let key = choice_ref_name(choice);
    reg.models.insert(
        key.clone(),
        ModelEntry {
            provider: choice.provider.clone(),
            model: choice.model.clone(),
            base_url: choice.base_url.clone(),
            secret: choice.secret.as_deref().map(|s| s.parse()).transpose()?,
            capabilities: vec![],
            params: serde_json::Value::Null,
        },
    );
    reg.save_to(&reg_path)?;

    // 2. Point the agent at it.
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)
            .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.model_ref = Some(key.clone());
    let yaml = serde_yaml_ng::to_string(&profile)?;
    std::fs::write(&profile_path, yaml)
        .with_context(|| format!("write {}", profile_path.display()))?;

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_plausible_ram() {
        let hw = detect_hardware();
        assert!(hw.total_ram_gb > 0, "RAM detection should be non-zero");
    }

    #[test]
    fn choice_ref_name_is_sanitized() {
        let c = ModelChoice {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
        };
        assert_eq!(choice_ref_name(&c), "ollama_llama3_2_3b");
    }

    #[test]
    fn apply_writes_registry_and_sets_model_ref() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path().to_path_buf();
        let home = mur_home.join("agents").join("coach");
        std::fs::create_dir_all(&home).unwrap();
        let p = mur_common::AgentProfile::default_for_tests();
        std::fs::write(
            home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        // Isolate the registry path to the temp home.
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }
        let choice = ModelChoice {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
        };
        let key = apply_model_choice(&mur_home, "coach", &choice).unwrap();
        assert_eq!(key, "ollama_llama3_2_3b");

        let reloaded: mur_common::AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(home.join("profile.yaml")).unwrap())
                .unwrap();
        assert_eq!(reloaded.model_ref.as_deref(), Some("ollama_llama3_2_3b"));
    }
}
