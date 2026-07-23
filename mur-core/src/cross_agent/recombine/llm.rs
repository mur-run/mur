//! LLM recombination strategy (M7b).
//!
//! Delegates the merge to `skill_llm::maintenance_call` with a fixed prompt.
//! Result is YAML, parsed via the canonical parser and validated via the
//! M6a schema validator before being returned.

use anyhow::Result;
use chrono::Duration;
use mur_common::model::ModelRegistry;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::parser::parse_canonical;
use mur_common::skill::validate::validate;
use std::path::Path;

use crate::skill_llm::{
    MaintenanceCtx, SkillLlmError, TokenBudget, maintenance_call, resolve_maintenance_model,
};

#[derive(Debug, thiserror::Error)]
pub enum LlmRecombineError {
    #[error("no model configured for skill maintenance — run `mur model add` to configure one")]
    NoModel,
    #[error("LLM returned no usable response (network or backend error)")]
    SoftFailed,
    #[error("LLM output failed schema validation: {0}")]
    Invalid(String),
    #[error("LLM call error: {0}")]
    Other(#[from] SkillLlmError),
}

pub async fn llm_recombine(
    home: &Path,
    a: &SkillManifest,
    b: &SkillManifest,
    output_name: &str,
) -> Result<SkillManifest, LlmRecombineError> {
    // Resolve model — from the caller's mur root, so tests and alternate
    // roots never leak in the user's real ~/.mur/models.yaml.
    let registry = ModelRegistry::load_from(&home.join("models.yaml")).unwrap_or_default();
    let model = resolve_maintenance_model(&registry, None).ok_or(LlmRecombineError::NoModel)?;

    // Build prompt
    let prompt = build_prompt(a, b, output_name)
        .map_err(|e| LlmRecombineError::Other(SkillLlmError::Other(e)))?;

    let ctx = MaintenanceCtx {
        budget_ledger: home.join("skill_llm_budget.json"),
        cache_ttl: Duration::days(30),
        daily_cap_usd: 1.00,
    };

    let response = maintenance_call(&prompt, &model, TokenBudget::DEFAULT, &ctx, &registry)
        .await
        .map_err(LlmRecombineError::Other)?;
    let yaml = response.ok_or(LlmRecombineError::SoftFailed)?;

    // Strip code fences if present
    let yaml = strip_code_fence(&yaml);

    // Parse + validate
    let manifest =
        parse_canonical(&yaml).map_err(|e| LlmRecombineError::Invalid(format!("parse: {e}")))?;
    validate(&manifest).map_err(|e| LlmRecombineError::Invalid(format!("validate: {e:?}")))?;

    Ok(manifest)
}

fn build_prompt(a: &SkillManifest, b: &SkillManifest, output_name: &str) -> Result<String> {
    let a_yaml = serde_yaml_ng::to_string(a)?;
    let b_yaml = serde_yaml_ng::to_string(b)?;
    Ok(format!(
        r#"You are recombining two YAML skill manifests into a single offspring manifest.

Parent A:
```yaml
{a_yaml}```

Parent B:
```yaml
{b_yaml}```

Rules:
- Output ONLY a single YAML document; no prose, no code fences.
- Set `name: {output_name}` and bump `version` to a fresh `0.1.0`.
- Preserve the same top-level shape as the parents (category, content.procedure, triggers, etc.).
- Combine triggers and requirements pragmatically; avoid duplicates.
- Steps: produce a coherent ordered sequence that achieves both parents' intents.
- Keep `mcp_requirements` minimal — only capabilities used by your output steps.
- Do not invent new tool names; reuse what either parent uses.

Output:
"#
    ))
}

fn strip_code_fence(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```yaml")
        && let Some(inner) = rest.trim_start_matches('\n').strip_suffix("```")
    {
        return inner.trim_end().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```")
        && let Some(inner) = rest.trim_start_matches('\n').strip_suffix("```")
    {
        return inner.trim_end().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_yaml_code_fence() {
        let s = "```yaml\nname: x\nversion: 1.0.0\n```";
        assert_eq!(strip_code_fence(s), "name: x\nversion: 1.0.0");
    }

    #[test]
    fn strips_bare_code_fence() {
        let s = "```\nname: x\n```";
        assert_eq!(strip_code_fence(s), "name: x");
    }

    #[test]
    fn passthrough_without_fence() {
        assert_eq!(strip_code_fence("name: x\n"), "name: x");
    }
}
