//! First-run model resolution (I/O side). Pairs with the pure `recommend()`
//! decision tree in `mur_common::model_resolve`. Shared by the CLI
//! (`mur agent install`) and the Hub GUI wizard (Plan 3). Spec §7.3–7.5.

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::model::ModelRegistry;
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
    // Update in place when the key already exists. A `ModelChoice` carries
    // four fields; a registry entry also holds pricing, the context window,
    // the vendor and the tier. Replacing it wholesale dropped those — the
    // agent kept running while its cost footer and `mur model prices show`
    // went blank, with nothing to point at as the cause.
    //
    // `None` on the choice means "unchanged", not "clear it": re-pointing an
    // agent at a model it already uses must not strip the endpoint or the
    // credential that made it reachable.
    let mut entry = reg.models.get(&key).cloned().unwrap_or_default();
    entry.provider = choice.provider.clone();
    entry.model = choice.model.clone();
    if choice.base_url.is_some() {
        entry.base_url = choice.base_url.clone();
    }
    if let Some(secret) = choice.secret.as_deref() {
        entry.secret = Some(secret.parse()?);
    }
    reg.models.insert(key.clone(), entry);
    reg.save_to(&reg_path)?;

    // 2. Point the agent at it.
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)
            .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.model_ref = Some(key.clone());
    // Through the shared saver so the legacy `model:` block is re-synced from
    // the registry entry we just wrote, instead of being left naming whatever
    // provider the agent was created with (#940).
    super::save_profile(&profile_path, &mut profile)?;

    Ok(key)
}

/// Fail-closed guard shared with the global `mur model default`/`fallback`
/// setters (`cmd/model.rs`): reject any model_ref that isn't already
/// registered in `<home>/models.yaml`.
fn ensure_ref_exists(home: &Path, r: &str) -> Result<()> {
    let reg = ModelRegistry::load_from(&home.join("models.yaml"))?;
    anyhow::ensure!(
        reg.models.contains_key(r),
        "model_ref {r:?} not in models.yaml"
    );
    Ok(())
}

/// Set an agent's per-agent fallback chain (`profile.fallback_chain`),
/// which takes priority over the global `models.fallback_chain` when
/// non-empty (see `mur_common::model::resolve_model_refs`). Each ref is
/// validated against `<home>/models.yaml` before the profile is written
/// (fail-closed — an unknown ref leaves the profile untouched).
pub fn cmd_agent_set_fallback(home: &Path, name: &str, refs: &[String]) -> Result<()> {
    for r in refs {
        ensure_ref_exists(home, r)?;
    }

    let profile_path = home.join("agents").join(name).join("profile.yaml");
    if !profile_path.exists() {
        bail!("agent '{name}' not installed at {}", profile_path.display());
    }
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)
            .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.fallback_chain = refs.to_vec();
    let yaml = serde_yaml_ng::to_string(&profile)?;
    std::fs::write(&profile_path, yaml)
        .with_context(|| format!("write {}", profile_path.display()))?;

    if refs.is_empty() {
        println!("agent '{name}' fallback chain cleared");
    } else {
        println!("agent '{name}' fallback chain = {}", refs.join(", "));
    }
    Ok(())
}

/// Set (or clear) this agent's Smart background-routing override.
/// `follow` removes the override so the agent inherits `models.smart`.
pub fn cmd_agent_set_smart(home: &Path, name: &str, state: &str) -> Result<()> {
    let profile_path = home.join("agents").join(name).join("profile.yaml");
    if !profile_path.exists() {
        bail!("agent '{name}' not installed at {}", profile_path.display());
    }
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)
            .with_context(|| format!("parse {}", profile_path.display()))?;
    // Preserve any other overridden field (e.g. a pinned cheap model): this
    // sets one field, it does not replace the block.
    let existing = profile.smart.take().unwrap_or_default();
    profile.smart = match state {
        "follow" => None,
        "on" => Some(mur_common::config::SmartOverride {
            enabled: Some(true),
            ..existing
        }),
        "off" => Some(mur_common::config::SmartOverride {
            enabled: Some(false),
            ..existing
        }),
        other => bail!("unknown state '{other}' (expected on, off, or follow)"),
    };
    std::fs::write(&profile_path, serde_yaml_ng::to_string(&profile)?)
        .with_context(|| format!("write {}", profile_path.display()))?;
    println!("agent '{name}' smart routing = {state}");
    Ok(())
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

    /// A registry entry is more than (provider, model): `mur model connect`
    /// fills in pricing, the context window and the vendor. Pointing an agent
    /// at an existing key must not cost the entry that metadata — the agent
    /// would keep working while the cost footer and `mur model prices show`
    /// silently went blank.
    #[test]
    fn applying_a_choice_over_an_existing_key_keeps_its_metadata() {
        use mur_common::model::{ModelEntry, ModelRegistry};

        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path().to_path_buf();
        let agent = mur_home.join("agents").join("coach");
        std::fs::create_dir_all(&agent).unwrap();
        let p = mur_common::AgentProfile::default_for_tests();
        std::fs::write(
            agent.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }

        // As `mur model connect deepseek --base-url …` would leave it.
        let reg_path = ModelRegistry::default_path().unwrap();
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "openai_deepseek_chat".into(),
            ModelEntry {
                provider: "openai".into(),
                vendor: Some("deepseek".into()),
                model: "deepseek-chat".into(),
                base_url: Some("https://api.deepseek.com/v1".into()),
                input_cost_per_1k: Some(0.00014),
                output_cost_per_1k: Some(0.00028),
                context_window: Some(1_000_000),
                ..Default::default()
            },
        );
        reg.save_to(&reg_path).unwrap();

        // What `apply_model_ref_override` reconstructs from that entry: four
        // fields, which is all a ModelChoice carries.
        let choice = ModelChoice {
            provider: "openai".into(),
            model: "deepseek-chat".into(),
            base_url: Some("https://api.deepseek.com/v1".into()),
            secret: None,
        };
        let key = apply_model_choice(&mur_home, "coach", &choice).unwrap();
        assert_eq!(key, "openai_deepseek_chat", "same key — it is an update");

        let after = ModelRegistry::load_from(&reg_path).unwrap();
        let e = &after.models[&key];
        assert_eq!(e.input_cost_per_1k, Some(0.00014), "pricing must survive");
        assert_eq!(e.output_cost_per_1k, Some(0.00028), "pricing must survive");
        assert_eq!(e.context_window, Some(1_000_000));
        assert_eq!(e.vendor.as_deref(), Some("deepseek"));
    }

    #[test]
    fn set_fallback_validates_refs_and_persists_to_profile() {
        use mur_common::model::ModelEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path().to_path_buf();
        let agent_home = mur_home.join("agents").join("coach");
        std::fs::create_dir_all(&agent_home).unwrap();
        let p = mur_common::AgentProfile::default_for_tests();
        std::fs::write(
            agent_home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "claude_sonnet".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-sonnet-5".into(),
                ..Default::default()
            },
        );
        reg.save_to(&mur_home.join("models.yaml")).unwrap();

        // Unknown ref → fail-closed, profile untouched.
        assert!(
            cmd_agent_set_fallback(&mur_home, "coach", &["does_not_exist".to_string()]).is_err()
        );

        cmd_agent_set_fallback(&mur_home, "coach", &["claude_sonnet".to_string()]).unwrap();
        let reloaded: mur_common::AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(agent_home.join("profile.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(reloaded.fallback_chain, vec!["claude_sonnet".to_string()]);
    }
}
