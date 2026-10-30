//! LLM-backed checks for `mur skill doctor` (api-drift, coverage-gap) and the LLM status report.

use super::*;

pub(super) fn run_api_drift(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    if !ctx.llm_enabled {
        return vec![Finding {
            check_id: "api-drift".into(),
            category: "api-drift".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "api-drift requires --llm; enable to analyze.".into(),
            remediation: None,
            fixable: false,
        }];
    }

    let Some(ref llm_ctx) = ctx.llm_ctx else {
        return vec![Finding {
            check_id: "api-drift".into(),
            category: "api-drift".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "No model configured for skill_llm; api-drift unavailable.".into(),
            remediation: Some("mur model role set maintenance <model_key>".into()),
            fixable: false,
        }];
    };

    let Some(manifest) = load_manifest(&ctx.home, skill_name) else {
        return vec![Finding {
            check_id: "api-drift".into(),
            category: "api-drift".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "Cannot read manifest — unable to check API drift.".into(),
            remediation: None,
            fixable: false,
        }];
    };

    // Load recent traces for this skill
    let traces = match crate::skill_traces::cluster::load_recent_for(&ctx.home, skill_name, 20) {
        Ok(t) if !t.is_empty() => t,
        _ => return vec![], // no traces, nothing to compare
    };

    // Build prompt
    let procedure = manifest
        .content
        .procedure
        .as_ref()
        .map(|p| {
            p.steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i, s.description))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "(no procedure)".to_string());

    let trace_summary: String = traces
        .iter()
        .map(|t| {
            format!(
                "- {}: tools=[{}], outcome={:?}",
                t.timestamp.format("%Y-%m-%d %H:%M"),
                t.tools_used.join(", "),
                t.outcome
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = crate::skill_llm::prompts::API_DRIFT_V1
        .replace("{procedure}", &procedure)
        .replace("{trace_count}", &traces.len().to_string())
        .replace("{trace_summary}", &trace_summary);

    // Call LLM (sync wrapper for async)
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let budget = crate::skill_llm::TokenBudget::DEFAULT;
    let result = rt.block_on(crate::skill_llm::maintenance_call(
        &prompt,
        &llm_ctx.model_ref,
        budget,
        &llm_ctx.maint_ctx,
        &llm_ctx.registry,
    ));

    match result {
        Ok(Some(body)) => match parse_drift_verdict(&body) {
            Some(DriftVerdict::Aligned) => vec![],
            Some(DriftVerdict::Drifted { evidence, steps }) => vec![Finding {
                check_id: "api-drift".into(),
                category: "api-drift".into(),
                severity: Severity::Warn,
                skill_name: skill_name.to_string(),
                message: format!("api-drift: {evidence} (steps: {steps:?})"),
                remediation: Some("Review the skill procedure against recent tool usage.".into()),
                fixable: false,
            }],
            Some(DriftVerdict::Unknown) => vec![],
            None => vec![], // malformed JSON → silent skip
        },
        Ok(None) => vec![Finding {
            check_id: "api-drift".into(),
            category: "api-drift".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "LLM unavailable for api-drift check.".into(),
            remediation: Some("Check your model configuration and API key.".into()),
            fixable: false,
        }],
        Err(crate::skill_llm::SkillLlmError::BudgetExhausted { spent_usd, cap_usd }) => {
            vec![Finding {
                check_id: "api-drift".into(),
                category: "api-drift".into(),
                severity: Severity::Unknown,
                skill_name: skill_name.to_string(),
                message: format!(
                    "Skill LLM daily budget exhausted (${spent_usd:.4} / ${cap_usd:.4})."
                ),
                remediation: Some(
                    "Wait until tomorrow or increase per_day_usd_cap in config.".into(),
                ),
                fixable: false,
            }]
        }
        Err(_) => vec![],
    }
}

enum DriftVerdict {
    Aligned,
    Drifted { evidence: String, steps: Vec<usize> },
    Unknown,
}

fn parse_drift_verdict(json: &str) -> Option<DriftVerdict> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    match v.get("verdict")?.as_str()? {
        "aligned" => Some(DriftVerdict::Aligned),
        "unknown" => Some(DriftVerdict::Unknown),
        "drifted" => {
            let evidence = v
                .get("evidence")
                .and_then(|x| x.as_str())
                .unwrap_or("(no evidence)")
                .to_string();
            let steps = v
                .get("drifted_steps")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_u64())
                        .map(|n| n as usize)
                        .collect()
                })
                .unwrap_or_default();
            Some(DriftVerdict::Drifted { evidence, steps })
        }
        _ => None,
    }
}

pub(super) fn run_coverage_gap(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    if !ctx.llm_enabled {
        return vec![Finding {
            check_id: "coverage-gap".into(),
            category: "coverage-gap".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "coverage-gap requires --llm; enable to analyze.".into(),
            remediation: None,
            fixable: false,
        }];
    }

    let Some(ref llm_ctx) = ctx.llm_ctx else {
        return vec![Finding {
            check_id: "coverage-gap".into(),
            category: "coverage-gap".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "No model configured for skill_llm; coverage-gap unavailable.".into(),
            remediation: Some("mur model role set maintenance <model_key>".into()),
            fixable: false,
        }];
    };

    // Load failed traces for this skill in the last 30 days
    let traces = match crate::skill_traces::cluster::load_recent_for(&ctx.home, skill_name, 50) {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let failures: Vec<_> = traces
        .into_iter()
        .filter(|t| t.outcome == crate::skill_traces::TraceOutcome::Failure)
        .collect();

    if failures.len() < 3 {
        return vec![];
    }

    // Cluster by error signature
    let clusters = cluster_by_error(&failures);
    let mut findings = Vec::new();

    // Build skill inventory for the prompt
    let skill_inventory = build_skill_inventory(&ctx.home);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let budget = crate::skill_llm::TokenBudget::DEFAULT;

    for (sig, members) in &clusters {
        if members.len() < 3 {
            continue;
        }
        let sample_steps: String = members
            .iter()
            .take(5)
            .map(|t| {
                format!(
                    "- {}: tools=[{}] error={}",
                    t.timestamp.format("%Y-%m-%d %H:%M"),
                    t.tools_used.join(", "),
                    t.error.as_deref().unwrap_or("(none)")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = crate::skill_llm::prompts::COVERAGE_GAP_V1
            .replace("{count}", &members.len().to_string())
            .replace("{error_signature}", sig)
            .replace("{sample_steps}", &sample_steps)
            .replace("{skill_inventory}", &skill_inventory);

        let result = rt.block_on(crate::skill_llm::maintenance_call(
            &prompt,
            &llm_ctx.model_ref,
            budget,
            &llm_ctx.maint_ctx,
            &llm_ctx.registry,
        ));

        if let Ok(Some(body)) = result
            && let Some(rec) = parse_gap_recommendation(&body)
        {
            match rec {
                GapRec::Extend {
                    target,
                    step,
                    rationale,
                } => {
                    findings.push(Finding {
                        check_id: "coverage-gap".into(),
                        category: "coverage-gap".into(),
                        severity: Severity::Warn,
                        skill_name: skill_name.to_string(),
                        message: format!("coverage-gap: extend '{target}' — {step}. {rationale}"),
                        remediation: Some(format!("Add step to {target}: {step}")),
                        fixable: false,
                    });
                }
                GapRec::New { step, rationale } => {
                    findings.push(Finding {
                        check_id: "coverage-gap".into(),
                        category: "coverage-gap".into(),
                        severity: Severity::Warn,
                        skill_name: skill_name.to_string(),
                        message: format!("coverage-gap: new skill needed — {step}. {rationale}"),
                        remediation: Some(format!("Create a new skill: {step}")),
                        fixable: false,
                    });
                }
                GapRec::Ignore => {}
            }
        }
    }

    if findings.is_empty() && !clusters.is_empty() {
        // Had failures but LLM couldn't help
        findings.push(Finding {
            check_id: "coverage-gap".into(),
            category: "coverage-gap".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: format!(
                "{} failure cluster(s) found but LLM analysis unavailable",
                clusters.len()
            ),
            remediation: Some("Check LLM configuration and try again.".into()),
            fixable: false,
        });
    }

    findings
}

/// Cluster failures by a simple error-signature hash.
fn cluster_by_error(
    traces: &[crate::skill_traces::SkillTrace],
) -> Vec<(String, Vec<crate::skill_traces::SkillTrace>)> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<crate::skill_traces::SkillTrace>> = BTreeMap::new();
    for t in traces {
        let sig = t.error.as_deref().unwrap_or("unknown_error").to_string();
        // Truncate long errors to keep cluster keys stable
        let key: String = sig.chars().take(80).collect();
        groups.entry(key).or_default().push(t.clone());
    }
    let mut result: Vec<_> = groups.into_iter().collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
    result
}

fn build_skill_inventory(home: &Path) -> String {
    let installed = mur_common::skill::local::list_installed(home).unwrap_or_default();
    let mut lines = Vec::new();
    for name in installed.iter().take(30) {
        if let Some(manifest) = load_manifest(home, name) {
            let abstract_ = &manifest.content.r#abstract;
            lines.push(format!(
                "- {name}: {abstract_}",
                abstract_ = abstract_.chars().take(100).collect::<String>()
            ));
        }
    }
    lines.join("\n")
}

enum GapRec {
    Extend {
        target: String,
        step: String,
        rationale: String,
    },
    New {
        step: String,
        rationale: String,
    },
    Ignore,
}

fn parse_gap_recommendation(json: &str) -> Option<GapRec> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let rec = v.get("recommendation")?.as_str()?;
    let rationale = v
        .get("rationale")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    match rec {
        "extend" => {
            let target = v.get("target_skill")?.as_str()?.to_string();
            let step = v
                .get("suggested_step")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            Some(GapRec::Extend {
                target,
                step,
                rationale,
            })
        }
        "new" => {
            let step = v
                .get("suggested_step")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            Some(GapRec::New { step, rationale })
        }
        "ignore" => Some(GapRec::Ignore),
        _ => None,
    }
}

pub(super) fn print_llm_status(home: &Path) -> Result<()> {
    let config = crate::store::config::load_config().unwrap_or_default();
    let skill_llm_cfg = &config.skill_llm;
    let registry = mur_common::model::ModelRegistry::load_from(
        &mur_common::model::ModelRegistry::default_path().unwrap_or_default(),
    )
    .unwrap_or_default();

    let model_ref =
        crate::skill_llm::resolve_maintenance_model(&registry, skill_llm_cfg.model_ref.as_deref());

    let (spent, _reserved) =
        crate::skill_llm::budget::current_usage(&home.join("skill_llm_budget.json"));

    println!("LLM maintenance status:");
    match model_ref {
        Some(ref m) => {
            let entry = registry.models.get(&m.entry_key);
            println!(
                "  model:        {} ({})",
                m.entry_key,
                entry.map(|e| e.model.as_str()).unwrap_or("unknown")
            );
        }
        None => {
            println!("  model:        (none — set via `mur model role set maintenance <key>`)");
        }
    }
    println!(
        "  per-call cap: {} output tokens",
        skill_llm_cfg.per_call_token_cap
    );
    println!("  per-day cap:  ${:.2}", skill_llm_cfg.per_day_usd_cap);
    println!("  spent today:  ${:.4}", spent);
    println!(
        "  cache:        {} (TTL {}d)",
        home.join("skill_llm_cache").display(),
        skill_llm_cfg.cache_ttl_days
    );
    Ok(())
}
