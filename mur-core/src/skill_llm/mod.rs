//! LLM-augmented skill maintenance helpers (M6c).
//!
//! `maintenance_call` mediates between `mur-core` and the model registry for
//! three checks: api-drift, coverage-gap, and contradiction adjudication.
//! Every check degrades gracefully when no model is available — LLMs are an
//! upgrade, never a hard dependency.

pub mod budget;
pub mod cache;
pub mod pricing;
pub mod prompts;

use chrono::Duration;
use mur_common::config::BackendConfig;
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::secret::SecretRef;
use std::path::PathBuf;

// ── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct TokenBudget {
    pub max_input: u32,
    pub max_output: u32,
}

impl TokenBudget {
    pub const DEFAULT: TokenBudget = TokenBudget {
        max_input: 4000,
        max_output: 1500,
    };
}

#[derive(Debug, thiserror::Error)]
pub enum SkillLlmError {
    #[error("no model configured for maintenance role")]
    NoModel,
    #[error("daily budget exhausted (${spent_usd:.4} / ${cap_usd:.4})")]
    BudgetExhausted { spent_usd: f64, cap_usd: f64 },
    #[error("model returned invalid response: {0}")]
    #[allow(dead_code)]
    InvalidResponse(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Reference to a model in `~/.mur/models.yaml`.
#[derive(Debug, Clone)]
pub struct ModelRef {
    pub entry_key: String,
}

/// Context needed by every maintenance call.
pub struct MaintenanceCtx {
    /// Path to `~/.mur/skill_llm_budget.json`.
    pub budget_ledger: PathBuf,
    /// Cache TTL (e.g. `chrono::Duration::days(30)`).
    pub cache_ttl: Duration,
    /// Per-day USD cap.
    pub daily_cap_usd: f64,
}

// ── Role resolution ──────────────────────────────────────────────────

/// Pick the model for skill-maintenance calls.
///
/// Order: user's `skill_llm.model_ref` config override →
/// `roles.maintenance.primary` → user's default chat role →
/// first model with `capabilities: [chat]`.
pub fn resolve_maintenance_model(
    registry: &ModelRegistry,
    model_ref_override: Option<&str>,
) -> Option<ModelRef> {
    if let Some(key) = model_ref_override
        && registry.models.contains_key(key)
    {
        return Some(ModelRef {
            entry_key: key.to_string(),
        });
    }
    if let Some(role) = registry.resolve_role("maintenance") {
        return Some(ModelRef {
            entry_key: role.to_string(),
        });
    }
    if let Some(role) = registry.resolve_role("chat") {
        return Some(ModelRef {
            entry_key: role.to_string(),
        });
    }
    registry
        .models
        .iter()
        .find(|(_, m)| m.capabilities.iter().any(|c| c == "chat"))
        .map(|(k, _)| ModelRef {
            entry_key: k.clone(),
        })
}

// ── BackendConfig from ModelEntry ─────────────────────────────────────

fn backend_config_from_entry(entry: &ModelEntry) -> BackendConfig {
    let api_key_env = entry.secret.as_ref().and_then(|s| match s {
        SecretRef::Env(var) => Some(var.clone()),
        _ => None,
    });
    BackendConfig {
        provider: entry.provider.clone(),
        model: entry.model.clone(),
        endpoint: entry.base_url.clone(),
        api_key_env,
        // Keychain/file/cmd refs previously dropped silently — now carried.
        // SecretRef's Display impl round-trips through its FromStr parser
        // (env:VAR, keychain:service/account, file:path, cmd:spec).
        api_key_ref: entry.secret.as_ref().map(|s| s.to_string()),
        timeout_secs: None,
    }
}

// ── maintenance_call ──────────────────────────────────────────────────

/// Execute a cached, budget-controlled LLM call for skill maintenance.
///
/// Returns `Ok(Some(response))` on success, `Ok(None)` when the call
/// soft-fails (network/auth error) so the caller can degrade gracefully.
pub async fn maintenance_call(
    prompt: &str,
    model: &ModelRef,
    budget: TokenBudget,
    ctx: &MaintenanceCtx,
    registry: &ModelRegistry,
) -> Result<Option<String>, SkillLlmError> {
    // Cache lookup
    let cache_key = cache::key(&model.entry_key, prompt, budget);
    if let Ok(Some(hit)) = cache::load(&cache_key, ctx.cache_ttl) {
        tracing::debug!(target: "skill_llm", model = %model.entry_key, "cache hit");
        return Ok(Some(hit));
    }

    // Look up the model entry
    let entry = match registry.models.get(&model.entry_key) {
        Some(e) => e.clone(),
        None => return Err(SkillLlmError::NoModel),
    };

    // Budget check (pre-flight)
    let projected_cost = pricing::estimate_cost(
        &entry.provider,
        &entry.model,
        budget.max_input,
        budget.max_output,
    );
    if let Err(spent) =
        budget::check_and_reserve(&ctx.budget_ledger, projected_cost, ctx.daily_cap_usd)
    {
        return Err(SkillLlmError::BudgetExhausted {
            spent_usd: spent,
            cap_usd: ctx.daily_cap_usd,
        });
    }

    // Build backend + call
    let backend_cfg = backend_config_from_entry(&entry);
    let backend =
        match crate::conversations::backend::factory::build_for_stage(&backend_cfg, "skill_llm") {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(target: "skill_llm", error = %e, "backend init failed");
                return Ok(None);
            }
        };

    let req = crate::conversations::backend::ChatRequest {
        model: &backend_cfg.model,
        system: None,
        user: prompt,
        max_tokens: budget.max_output,
        temperature: None,
        stop: vec![],
        cache_system: false,
        cache_user_prefix: None,
    };

    let resp = match backend.generate(req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "skill_llm", error = %e, "maintenance_call soft-failed");
            return Ok(None);
        }
    };

    // Budget settle (actual cost)
    let actual_input_tokens = resp.usage.input_tokens as u32;
    let actual_output_tokens = resp.usage.output_tokens as u32;
    let actual_cost = pricing::estimate_cost(
        &entry.provider,
        &entry.model,
        actual_input_tokens,
        actual_output_tokens,
    );
    let _ = budget::settle(&ctx.budget_ledger, projected_cost, actual_cost);

    // Cache
    let _ = cache::save(&cache_key, &resp.text);

    Ok(Some(resp.text))
}
