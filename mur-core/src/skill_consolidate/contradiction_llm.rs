//! LLM contradiction adjudicator (M6c Task 5).
//!
//! Takes M5b's rule-based contradiction pairs and asks the LLM whether two
//! skills actually contradict, coexist, or are duplicates.

use super::SkillView;
use super::contradiction::{AdjudicationVerdict, ContradictionPair};
use crate::skill_llm::{MaintenanceCtx, TokenBudget, maintenance_call, resolve_maintenance_model};
use mur_common::model::ModelRegistry;
use std::path::Path;

/// Adjudicate contradiction pairs using the LLM.
///
/// Each pair gets a separate LLM call. Pairs that can't be adjudicated
/// (no model, budget exhausted, network error) are left unchanged.
pub async fn adjudicate(pairs: &mut [ContradictionPair], skills: &[SkillView], home: &Path) {
    if pairs.is_empty() {
        return;
    }

    let config = crate::store::config::load_config().unwrap_or_default();
    let skill_llm_cfg = &config.skill_llm;
    let registry = ModelRegistry::load_from(&ModelRegistry::default_path().unwrap_or_default())
        .unwrap_or_default();

    let Some(model_ref) = resolve_maintenance_model(&registry, skill_llm_cfg.model_ref.as_deref())
    else {
        tracing::info!("no maintenance model configured; skipping LLM adjudication");
        return;
    };

    let ctx = MaintenanceCtx {
        budget_ledger: home.join("skill_llm_budget.json"),
        cache_ttl: chrono::Duration::days(skill_llm_cfg.cache_ttl_days as i64),
        daily_cap_usd: skill_llm_cfg.per_day_usd_cap,
    };

    for pair in pairs.iter_mut() {
        let skill_a = skills.iter().find(|s| s.name == pair.a);
        let skill_b = skills.iter().find(|s| s.name == pair.b);

        let procedure_a = skill_a
            .map(|s| s.embed_text.as_str())
            .unwrap_or("(not found)");
        let procedure_b = skill_b
            .map(|s| s.embed_text.as_str())
            .unwrap_or("(not found)");

        let prompt = crate::skill_llm::prompts::CONTRADICTION_ADJUDICATE_V1
            .replace("{name_a}", &pair.a)
            .replace("{procedure_a}", procedure_a)
            .replace("{name_b}", &pair.b)
            .replace("{procedure_b}", procedure_b)
            .replace("{overlap_summary}", &pair.reason);

        match maintenance_call(&prompt, &model_ref, TokenBudget::DEFAULT, &ctx, &registry).await {
            Ok(Some(body)) => {
                if let Some(verdict) = parse_adjudication(&body) {
                    pair.adjudication = Some(verdict);
                }
            }
            Ok(None) | Err(_) => {
                // Pair stays unadjudicated
            }
        }
    }
}

fn parse_adjudication(json: &str) -> Option<AdjudicationVerdict> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    match v.get("verdict")?.as_str()? {
        "contradict" => Some(AdjudicationVerdict::Contradict),
        "coexist" => Some(AdjudicationVerdict::Coexist),
        "duplicate" => Some(AdjudicationVerdict::Duplicate),
        _ => None,
    }
}
