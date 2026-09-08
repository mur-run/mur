//! `/model` — list registry models and hot-switch the agent's model.
//!
//! Listing is a pure read (provider groups, current model marked, per-1k
//! rates when priced). Switching dials A2A `model/set`; when the dial fails
//! (a runtime predating the method, or the agent not running) the pick is
//! still saved to `profile.yaml` with a restart hint, so the operator's
//! intent always lands and disk never trails what they asked for.

use std::path::Path;

use anyhow::{Context, Result};
use mur_common::agent::AgentProfile;
use mur_common::model::{ModelEntry, ModelRegistry};

/// Registry entries in display order: provider groups, aliases sorted inside.
/// The same ordering backs both list numbering and number resolution, so
/// `/model 2` picks exactly what the last `/model` printed.
pub(crate) fn ordered_models(reg: &ModelRegistry) -> Vec<(String, ModelEntry)> {
    let mut v: Vec<(String, ModelEntry)> = reg
        .models
        .iter()
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    v.sort_by(|a, b| {
        (a.1.provider.as_str(), a.0.as_str()).cmp(&(b.1.provider.as_str(), b.0.as_str()))
    });
    v
}

pub(crate) fn render_list(models: &[(String, ModelEntry)], current: Option<&str>) -> String {
    if models.is_empty() {
        return "no models in ~/.mur/models.yaml — add one with `mur model add`".to_string();
    }
    let mut out = String::new();
    let mut last_provider = "";
    for (i, (alias, entry)) in models.iter().enumerate() {
        if entry.provider != last_provider {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{}:\n", entry.provider));
            last_provider = &entry.provider;
        }
        let marker = if Some(alias.as_str()) == current {
            "●"
        } else {
            " "
        };
        let (input, output) = entry.effective_costs();
        let price = match (input, output) {
            (Some(i), Some(o)) => format!("  in ${i}/1k · out ${o}/1k"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "{marker} {:>2}. {alias}  {}{price}\n",
            i + 1,
            entry.model
        ));
    }
    out.push_str("\nswitch: /model <number|name>");
    out
}

/// Resolve `/model <arg>`: 1-based number from the printed list, exact alias,
/// or case-insensitive alias (same convention as agent names).
pub(crate) fn resolve_pick(models: &[(String, ModelEntry)], arg: &str) -> Option<String> {
    if let Ok(n) = arg.parse::<usize>() {
        return models.get(n.checked_sub(1)?).map(|(a, _)| a.clone());
    }
    if models.iter().any(|(a, _)| a == arg) {
        return Some(arg.to_string());
    }
    models
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(arg))
        .map(|(a, _)| a.clone())
}

fn profile_path(home: &Path, agent: &str) -> std::path::PathBuf {
    home.join("agents").join(agent).join("profile.yaml")
}

/// Current `model_ref` from the agent's profile (best-effort).
pub(crate) fn current_model_ref(home: &Path, agent: &str) -> Option<String> {
    let yaml = std::fs::read_to_string(profile_path(home, agent)).ok()?;
    serde_yaml_ng::from_str::<AgentProfile>(&yaml)
        .ok()?
        .model_ref
}

/// The raw vendor model id behind an alias, e.g. `fast` → `deepseek-v4`.
///
/// Split from `current_model_id` so the registry lookup is testable without a
/// registry file on disk.
pub(crate) fn resolve_model_id(reg: &ModelRegistry, model_ref: Option<&str>) -> Option<String> {
    reg.models.get(model_ref?).map(|e| e.model.clone())
}

/// The raw model id this agent is configured with.
///
/// Keyed on `ModelEntry.model`, never on `ModelEntry.provider` — that field
/// records the wire protocol, so DeepSeek, Qwen and every other
/// OpenAI-compatible third party all read `openai`. Anything asking "what can
/// this model do" must go through the raw id. Best-effort: `None` when the
/// profile, the registry, or the alias is missing.
pub(crate) fn current_model_id(home: &Path, agent: &str) -> Option<String> {
    let model_ref = current_model_ref(home, agent)?;
    let reg = ModelRegistry::default_path()
        .and_then(|p| ModelRegistry::load_from(&p))
        .ok()?;
    resolve_model_id(&reg, Some(&model_ref))
}

/// The effort stored on the agent's profile, if any.
///
/// Read from the same file `current_model_ref` reads, so `/effort` reports the
/// value the runtime will actually load at its next start rather than a copy
/// held somewhere else.
pub(crate) fn current_effort(home: &Path, agent: &str) -> Option<mur_common::llm::Effort> {
    let yaml = std::fs::read_to_string(profile_path(home, agent)).ok()?;
    serde_yaml_ng::from_str::<AgentProfile>(&yaml).ok()?.effort
}

/// Fallback persistence when the dial can't reach a `model/set`-capable
/// runtime: typed round-trip + temp/rename, mirroring the runtime handler.
pub(crate) fn write_model_ref(home: &Path, agent: &str, model_ref: &str) -> Result<()> {
    let path = profile_path(home, agent);
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut p: AgentProfile = serde_yaml_ng::from_str(&yaml).context("parse profile.yaml")?;
    p.model_ref = Some(model_ref.to_string());
    p.updated_at = chrono::Utc::now().to_rfc3339();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_yaml_ng::to_string(&p)?.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(entries: &[(&str, &str, &str)]) -> ModelRegistry {
        let mut reg = ModelRegistry::default();
        for (alias, provider, model) in entries {
            reg.models.insert(
                alias.to_string(),
                ModelEntry {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    ..Default::default()
                },
            );
        }
        reg
    }

    #[test]
    fn ordering_groups_by_provider_then_alias() {
        let reg = reg(&[
            ("zeta", "openai", "z"),
            ("claude_opus", "anthropic", "claude-opus-5"),
            ("alpha", "openai", "a"),
        ]);
        let names: Vec<String> = ordered_models(&reg).into_iter().map(|(a, _)| a).collect();
        assert_eq!(names, vec!["claude_opus", "alpha", "zeta"]);
    }

    #[test]
    fn render_marks_current_and_numbers_globally() {
        let reg = reg(&[
            ("claude_opus", "anthropic", "claude-opus-5"),
            ("dsv4", "openai", "deepseek-v4"),
        ]);
        let out = render_list(&ordered_models(&reg), Some("dsv4"));
        assert!(out.contains("anthropic:"), "{out}");
        assert!(out.contains("openai:"), "{out}");
        assert!(out.contains("  1. claude_opus"), "{out}");
        assert!(out.contains("●  2. dsv4"), "{out}");
        assert!(out.contains("switch: /model"), "{out}");
    }

    #[test]
    fn resolve_by_number_name_and_case() {
        let reg = reg(&[
            ("claude_opus", "anthropic", "claude-opus-5"),
            ("dsv4", "openai", "deepseek-v4"),
        ]);
        let models = ordered_models(&reg);
        assert_eq!(resolve_pick(&models, "2").as_deref(), Some("dsv4"));
        assert_eq!(
            resolve_pick(&models, "claude_opus").as_deref(),
            Some("claude_opus")
        );
        assert_eq!(
            resolve_pick(&models, "Claude_Opus").as_deref(),
            Some("claude_opus")
        );
        assert_eq!(resolve_pick(&models, "0"), None);
        assert_eq!(resolve_pick(&models, "3"), None);
        assert_eq!(resolve_pick(&models, "ghost"), None);
    }

    #[test]
    fn write_and_read_model_ref_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let adir = home.join("agents").join("t");
        std::fs::create_dir_all(&adir).unwrap();
        const MINIMAL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mur-common/tests/fixtures/profile_p0a_minimal.yaml"
        ));
        std::fs::write(adir.join("profile.yaml"), MINIMAL).unwrap();

        assert_eq!(current_model_ref(home, "t"), None);
        write_model_ref(home, "t", "claude_opus").unwrap();
        assert_eq!(current_model_ref(home, "t").as_deref(), Some("claude_opus"));
        // Legacy `model:` block survives the rewrite (live read path).
        let text = std::fs::read_to_string(adir.join("profile.yaml")).unwrap();
        assert!(text.contains("provider: ollama"), "{text}");
    }

    /// The id the effort table keys on is `ModelEntry.model` — the raw vendor
    /// id — never the alias and never `provider:`, which is the wire protocol.
    #[test]
    fn current_model_id_returns_the_raw_vendor_id() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("a");
        std::fs::create_dir_all(&dir).unwrap();
        // The same fixture the round-trip test uses: a hand-written profile
        // does not deserialize into `AgentProfile`, and `current_model_ref`
        // fails soft, so the test would read `None` and prove nothing.
        const MINIMAL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mur-common/tests/fixtures/profile_p0a_minimal.yaml"
        ));
        std::fs::write(dir.join("profile.yaml"), MINIMAL).unwrap();
        write_model_ref(home.path(), "a", "fast").unwrap();
        // `reg` is the existing helper in this module: (alias, provider, model).
        // The provider is `openai` on purpose — DeepSeek speaks that protocol,
        // and a lookup keyed on it would resolve the wrong effort table.
        let reg = reg(&[("fast", "openai", "deepseek-v4")]);
        assert_eq!(
            resolve_model_id(&reg, current_model_ref(home.path(), "a").as_deref()),
            Some("deepseek-v4".to_string())
        );
    }
}
