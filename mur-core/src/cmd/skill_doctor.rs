//! `mur skill doctor` — read-only skill health checks (M5a).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use mur_common::skill::lifecycle::calculate_decay;
use mur_common::skill::local::list_installed;
use mur_common::skill::manifest::Requirement;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::agent::resolve_mur_home;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Ok,
    Warn,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub check_id: String,
    pub category: String,
    pub severity: Severity,
    pub skill_name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    pub fixable: bool,
}

pub enum DoctorFormat {
    Text,
    Json,
}

struct DoctorCtx {
    home: PathBuf,
    now: DateTime<Utc>,
    installed_skills: HashSet<String>,
    /// Available MCP tool names across all configured servers.
    /// `None` means "no agent context" — the check reports Unknown.
    mcp_tools: Option<Vec<String>>,
    /// Whether --llm was passed.
    llm_enabled: bool,
    /// LLM maintenance context (loaded when --llm is on).
    llm_ctx: Option<LlmDoctorCtx>,
}

/// LLM context for skill-maintenance doctor checks.
struct LlmDoctorCtx {
    model_ref: crate::skill_llm::ModelRef,
    maint_ctx: crate::skill_llm::MaintenanceCtx,
    registry: mur_common::model::ModelRegistry,
}

impl LlmDoctorCtx {
    fn load(home: &Path) -> Result<Self> {
        let config = crate::store::config::load_config().unwrap_or_default();
        let skill_llm_cfg = &config.skill_llm;
        let registry = mur_common::model::ModelRegistry::load_from(
            &mur_common::model::ModelRegistry::default_path().unwrap_or_default(),
        )
        .unwrap_or_default();

        let model_ref = crate::skill_llm::resolve_maintenance_model(
            &registry,
            skill_llm_cfg.model_ref.as_deref(),
        )
        .ok_or_else(|| anyhow::anyhow!("no model configured for skill_llm maintenance role"))?;

        let budget_ledger = home.join("skill_llm_budget.json");
        let cache_ttl = chrono::Duration::days(skill_llm_cfg.cache_ttl_days as i64);
        let daily_cap_usd = skill_llm_cfg.per_day_usd_cap;

        Ok(Self {
            model_ref,
            maint_ctx: crate::skill_llm::MaintenanceCtx {
                budget_ledger,
                cache_ttl,
                daily_cap_usd,
            },
            registry,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_doctor(
    names: &[String],
    checks: &[String],
    json: bool,
    strict: bool,
    fix: bool,
    apply: bool,
    llm: bool,
    llm_status: bool,
) -> Result<()> {
    let home = resolve_mur_home()?;
    let now = Utc::now();
    let installed_names: HashSet<String> = list_installed(&home)
        .unwrap_or_default()
        .into_iter()
        .collect();

    // ── --llm-status ──
    if llm_status {
        return print_llm_status(&home);
    }

    // Determine which skills to check
    let target_skills: Vec<String> = if names.is_empty() {
        let mut v: Vec<_> = installed_names.iter().cloned().collect();
        v.sort();
        v
    } else {
        // Support glob patterns in names
        names
            .iter()
            .flat_map(|n| {
                if n.contains('*') || n.contains('?') {
                    let pat = crate::skill_stats::reindex::glob_pattern(n);
                    installed_names
                        .iter()
                        .filter(|installed| pat.matches(installed))
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    vec![n.clone()]
                }
            })
            .collect()
    };

    // Determine which checks to run
    let all_checks = [
        "tool-availability",
        "dependency-freshness",
        "execution-recency",
        "failure-rate",
        "api-drift",
        "coverage-gap",
        "mcp-requirements-coverage",
        "mcp-capability-available",
        "intent-resolvable",
        "disclosure",
        "shadow-drift",
    ];
    let active_checks: Vec<&str> = if checks.is_empty() {
        all_checks.to_vec()
    } else {
        checks.iter().map(|c| c.as_str()).collect()
    };

    // LLM context (lazy — only load if --llm is on)
    let llm_ctx = if llm {
        Some(LlmDoctorCtx::load(&home)?)
    } else {
        None
    };

    let ctx = DoctorCtx {
        home,
        now,
        installed_skills: installed_names,
        mcp_tools: None, // wired to agent MCP registry in M6b
        llm_enabled: llm,
        llm_ctx,
    };

    let mut findings: Vec<Finding> = Vec::new();

    for skill_name in &target_skills {
        for &check_id in &active_checks {
            match check_id {
                "tool-availability" => {
                    findings.extend(run_tool_availability(&ctx, skill_name));
                }
                "dependency-freshness" => {
                    findings.extend(run_dependency_freshness(&ctx, skill_name));
                }
                "execution-recency" => {
                    findings.extend(run_execution_recency(&ctx, skill_name));
                }
                "failure-rate" => {
                    findings.extend(run_failure_rate(&ctx, skill_name));
                }
                "api-drift" => {
                    findings.extend(run_api_drift(&ctx, skill_name));
                }
                "coverage-gap" => {
                    findings.extend(run_coverage_gap(&ctx, skill_name));
                }
                "mcp-requirements-coverage" => {
                    findings.extend(run_mcp_requirements_coverage(&ctx, skill_name));
                }
                "mcp-capability-available" => {
                    findings.extend(run_mcp_capability_available(&ctx, skill_name));
                }
                "intent-resolvable" => {
                    findings.extend(run_intent_resolvable(&ctx, skill_name));
                }
                "disclosure" => {
                    findings.extend(run_disclosure(&ctx, skill_name));
                }
                _ => {}
            }
        }
    }

    if active_checks.contains(&"shadow-drift") {
        findings.extend(run_shadow_drift(&ctx));
    }

    // ── Repair (M5b) ──
    if fix {
        let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
            .unwrap_or_else(|_| crate::cmd::skill_registry::DEFAULT_REGISTRY.to_string());
        let repairs: Vec<Box<dyn crate::skill_repair::Repair>> = vec![
            Box::new(crate::skill_repair::tool_availability::ToolAvailabilityRepair),
            Box::new(crate::skill_repair::dep_freshness::DepFreshnessRepair),
            Box::new(crate::skill_repair::stats_sidecar::StatsSidecarRepair),
        ];
        let repair_ctx = crate::skill_repair::RepairCtx {
            home: &ctx.home,
            registry_url: &registry_url,
        };

        // Confirmation prompt for destructive --apply (interactive only)
        if apply {
            let fixable = findings.iter().filter(|f| f.fixable).count();
            if fixable > 0 {
                use std::io::{self, IsTerminal, Write};
                let is_tty = std::io::stdin().is_terminal();
                if is_tty {
                    print!(
                        "About to repair {} fixable finding(s). Continue? [y/N] ",
                        fixable
                    );
                    io::stdout().flush().ok();
                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();
                    if !input.trim().eq_ignore_ascii_case("y") {
                        println!("Aborted.");
                        return Ok(());
                    }
                }
            }
        }

        let report = crate::skill_repair::run_repairs(&findings, apply, &repair_ctx, &repairs);
        crate::skill_repair::print_repair_summary(&report, apply);
        return Ok(());
    }

    let fmt = if json {
        DoctorFormat::Json
    } else {
        DoctorFormat::Text
    };
    let color = supports_color::on_cached(supports_color::Stream::Stdout).is_some();

    format_findings(&findings, fmt, color)?;

    let code = exit_code(&findings, strict);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

// ── Checks ──

fn run_tool_availability(_ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    // Tool availability requires the agent's trust capability list, which
    // is not available from the CLI doctor. Report Unknown.
    vec![Finding {
        check_id: "tool-availability".into(),
        category: "tools".into(),
        severity: Severity::Unknown,
        skill_name: skill_name.to_string(),
        message: "Tool availability check requires agent context — run `mur skill doctor` from within an agent, or use `mur agent doctor`.".into(),
        remediation: None,
        fixable: false,
    }]
}

fn load_manifest(home: &Path, skill_name: &str) -> Option<mur_common::skill::SkillManifest> {
    let path = home.join("skills").join(skill_name).join("skill.yaml");
    let content = std::fs::read_to_string(&path).ok()?;
    mur_common::skill::parse_canonical(&content).ok()
}

fn run_dependency_freshness(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Some(manifest) = load_manifest(&ctx.home, skill_name) else {
        findings.push(Finding {
            check_id: "dependency-freshness".into(),
            category: "deps".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "Cannot read manifest — unable to check dependencies.".into(),
            remediation: None,
            fixable: false,
        });
        return findings;
    };

    for req in &manifest.requires {
        let Requirement { name, .. } = &req;
        if ctx.installed_skills.contains(name) {
            // Dependency is installed — check version if constraint given
            let constraint = &req.version;
            if constraint != "*" {
                // Check if installed version satisfies the constraint
                if let Some(dep_manifest) = load_manifest(&ctx.home, name) {
                    let installed_version = &dep_manifest.version;
                    if let Ok(req_ver) = semver::VersionReq::parse(constraint)
                        && let Ok(inst_ver) = semver::Version::parse(installed_version)
                        && !req_ver.matches(&inst_ver)
                    {
                        findings.push(Finding {
                            check_id: "dependency-freshness".into(),
                            category: "deps".into(),
                            severity: Severity::Warn,
                            skill_name: skill_name.to_string(),
                            message: format!(
                                "Requires {name} {constraint} but {installed_version} is installed."
                            ),
                            remediation: Some(format!("mur skill update {name}")),
                            fixable: true,
                        });
                    }
                }
            }
        } else {
            findings.push(Finding {
                check_id: "dependency-freshness".into(),
                category: "deps".into(),
                severity: Severity::Fail,
                skill_name: skill_name.to_string(),
                message: format!("Required skill '{name}' is not installed."),
                remediation: Some(format!("mur skill install {name}")),
                fixable: true,
            });
        }
    }

    if findings.is_empty() {
        findings.push(Finding {
            check_id: "dependency-freshness".into(),
            category: "deps".into(),
            severity: Severity::Ok,
            skill_name: skill_name.to_string(),
            message: "All required dependencies are installed.".into(),
            remediation: None,
            fixable: false,
        });
    }

    findings
}

fn run_execution_recency(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    let path = SkillStats::path(&ctx.home, skill_name);
    let stats = match SkillStats::load(&path) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return vec![Finding {
                check_id: "execution-recency".into(),
                category: "recency".into(),
                severity: Severity::Unknown,
                skill_name: skill_name.to_string(),
                message: "No stats sidecar — run `mur skill reindex-stats` to rebuild.".into(),
                remediation: Some(format!("mur skill reindex-stats {skill_name}")),
                fixable: true,
            }];
        }
        Err(_) => {
            return vec![Finding {
                check_id: "execution-recency".into(),
                category: "recency".into(),
                severity: Severity::Unknown,
                skill_name: skill_name.to_string(),
                message: "Stats sidecar unreadable.".into(),
                remediation: Some(format!("mur skill reindex-stats {skill_name}")),
                fixable: true,
            }];
        }
    };

    let state = stats.lifecycle_state;
    if state == LifecycleState::Archived {
        return vec![Finding {
            check_id: "execution-recency".into(),
            category: "recency".into(),
            severity: Severity::Warn,
            skill_name: skill_name.to_string(),
            message: "Skill is archived.".into(),
            remediation: None,
            fixable: false,
        }];
    }

    let decayed = calculate_decay(
        stats.anchor_confidence,
        stats.last_success_at,
        mur_common::skill::lifecycle::half_life_days(state),
        ctx.now,
    );

    let days_since_last = stats
        .last_success_at
        .map(|t| (ctx.now - t).num_days())
        .unwrap_or(i64::MAX);

    let severity = if days_since_last <= 30 {
        Severity::Ok
    } else if days_since_last <= 90 {
        Severity::Warn
    } else {
        Severity::Fail
    };

    let last_str = stats
        .last_success_at
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "never".to_string());

    vec![Finding {
        check_id: "execution-recency".into(),
        category: "recency".into(),
        severity,
        skill_name: skill_name.to_string(),
        message: format!(
            "Last success: {last_str} ({days_since_last}d ago), decayed confidence: {decayed:.3}"
        ),
        remediation: if severity == Severity::Fail {
            Some("Run the skill to restore confidence, or `mur skill pin` to preserve it.".into())
        } else {
            None
        },
        fixable: false,
    }]
}

fn run_failure_rate(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    // Scan today's trace JSONL for the last 10 executions
    let traces_dir = ctx.home.join("traces");

    let mut recent_outcomes: Vec<bool> = Vec::new(); // true = success

    // Scan today's file, then yesterday's if needed
    for day_offset in 0..7 {
        let day = ctx.now - Duration::days(day_offset);
        let path = traces_dir
            .join(day.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        if !path.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines().rev() {
            if recent_outcomes.len() >= 10 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || !trimmed.contains("mur.skill.executed") {
                continue;
            }
            let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
                continue;
            };
            let event_skill = val
                .get("mur.skill.name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if event_skill != skill_name {
                continue;
            }
            let outcome = val
                .get("mur.skill.outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("not_evaluated");
            recent_outcomes.push(outcome == "success");
        }
        if recent_outcomes.len() >= 10 {
            break;
        }
    }

    if recent_outcomes.is_empty() {
        return vec![Finding {
            check_id: "failure-rate".into(),
            category: "failure-rate".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "No recent execution traces found.".into(),
            remediation: Some("Use the skill at least once to establish a baseline.".into()),
            fixable: false,
        }];
    }

    let total = recent_outcomes.len();
    let successes = recent_outcomes.iter().filter(|&&s| s).count();
    let rate = successes as f64 / total as f64;

    let severity = if rate >= 0.9 {
        Severity::Ok
    } else if rate >= 0.7 {
        Severity::Warn
    } else {
        Severity::Fail
    };

    vec![Finding {
        check_id: "failure-rate".into(),
        category: "failure-rate".into(),
        severity,
        skill_name: skill_name.to_string(),
        message: format!(
            "Success rate: {successes}/{total} ({:.0}%) over last {total} executions",
            rate * 100.0
        ),
        remediation: if rate < 0.7 {
            Some("Review the skill's content and triggers for accuracy.".into())
        } else {
            None
        },
        fixable: false,
    }]
}

fn run_api_drift(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

fn run_mcp_requirements_coverage(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    let Some(manifest) = load_manifest(&ctx.home, skill_name) else {
        return vec![Finding {
            check_id: "mcp-requirements-coverage".into(),
            category: "mcp".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "Cannot read manifest — unable to check MCP requirements coverage.".into(),
            remediation: None,
            fixable: false,
        }];
    };

    // Only procedural skills can reference MCP tools in steps.
    if manifest.content.mode() != Some(mur_common::skill::types::ContentMode::Workflow) {
        return vec![];
    }
    // Already has explicit requirements — covered.
    if !manifest.mcp_requirements.is_empty() {
        return vec![];
    }
    let Some(proc) = &manifest.content.procedure else {
        return vec![];
    };

    let referenced: Vec<&str> = proc
        .steps
        .iter()
        .filter_map(|s| s.tool.as_deref())
        .filter(|t| t.contains('.') && !t.starts_with("./") && !t.starts_with("../"))
        .collect();

    if referenced.is_empty() {
        return vec![];
    }

    vec![Finding {
        check_id: "mcp-requirements-coverage".into(),
        category: "mcp".into(),
        severity: Severity::Warn,
        skill_name: skill_name.to_string(),
        message: format!(
            "procedural skill references {} dotted tool name(s) ({}) but declares no \
             mcp_requirements — add an mcp_requirements block to declare the needed MCP capability",
            referenced.len(),
            referenced
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        remediation: Some(
            "Add an mcp_requirements block mapping tool patterns to capabilities.".into(),
        ),
        fixable: false,
    }]
}

fn run_coverage_gap(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

fn print_llm_status(home: &Path) -> Result<()> {
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

// ── Output ──

fn format_findings(findings: &[Finding], fmt: DoctorFormat, color: bool) -> Result<()> {
    match fmt {
        DoctorFormat::Text => {
            if findings.is_empty() {
                println!("No skills to check. Install skills with `mur skill install <name>`.");
                return Ok(());
            }
            // Group by skill
            let mut by_skill: std::collections::BTreeMap<&str, Vec<&Finding>> =
                std::collections::BTreeMap::new();
            for f in findings {
                by_skill.entry(&f.skill_name).or_default().push(f);
            }
            for (skill_name, skill_findings) in &by_skill {
                println!("\n── {skill_name} ──");
                for f in skill_findings {
                    let icon = severity_icon(f.severity);
                    let icon = if color {
                        severity_color(f.severity, icon)
                    } else {
                        icon.to_string()
                    };
                    println!("  {icon} [{}] {}", f.check_id, f.message);
                    if let Some(ref rem) = f.remediation {
                        println!("      fix: {rem}");
                    }
                }
            }
            println!(); // trailing newline
        }
        DoctorFormat::Json => {
            let output = serde_json::to_string_pretty(&DoctorOutput {
                schema_version: 1,
                findings,
            })?;
            println!("{output}");
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct DoctorOutput<'a> {
    schema_version: u32,
    findings: &'a [Finding],
}

fn severity_icon(s: Severity) -> &'static str {
    match s {
        Severity::Ok => "[OK]",
        Severity::Warn => "[!]",
        Severity::Fail => "[X]",
        Severity::Unknown => "[?]",
    }
}

fn severity_color(s: Severity, icon: &str) -> String {
    use std::fmt::Write;
    let code = match s {
        Severity::Ok => "\x1b[32m",      // green
        Severity::Warn => "\x1b[33m",    // yellow
        Severity::Fail => "\x1b[31m",    // red
        Severity::Unknown => "\x1b[36m", // cyan
    };
    let mut out = String::new();
    write!(out, "{code}{icon}\x1b[0m").ok();
    out
}

fn run_mcp_capability_available(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    let Some(manifest) = load_manifest(&ctx.home, skill_name) else {
        return vec![Finding {
            check_id: "mcp-capability-available".into(),
            category: "mcp".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "Cannot read manifest — unable to check MCP capability availability.".into(),
            remediation: None,
            fixable: false,
        }];
    };

    if manifest.mcp_requirements.is_empty() {
        return vec![];
    }

    // Without agent context we can't enumerate available MCP tools.
    // Wire to the agent's MCP server registry in M6b.
    let Some(ref available) = ctx.mcp_tools else {
        return vec![Finding {
            check_id: "mcp-capability-available".into(),
            category: "mcp".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "MCP capability-availability check requires agent context — \
                     run from within an agent to check server availability."
                .into(),
            remediation: None,
            fixable: false,
        }];
    };

    let mut findings = Vec::new();
    for (i, req) in manifest.mcp_requirements.iter().enumerate() {
        // Skip requirements with a fallback.
        if !req.fallback.is_empty() {
            continue;
        }
        // Match glob pattern against available tools.
        let Ok(glob) = globset::Glob::new(&req.tool_pattern) else {
            continue;
        };
        let matcher = glob.compile_matcher();
        let has_match = available.iter().any(|t| matcher.is_match(t));
        if !has_match {
            findings.push(Finding {
                check_id: "mcp-capability-available".into(),
                category: "mcp".into(),
                severity: Severity::Warn,
                skill_name: skill_name.to_string(),
                message: format!(
                    "mcp_requirements[{i}]: no MCP server provides tool matching \
                     '{}' (capability {}, no fallback)",
                    req.tool_pattern, req.capability
                ),
                remediation: Some(
                    "Install an MCP server that provides this tool, or add a fallback.".into(),
                ),
                fixable: false,
            });
        }
    }
    findings
}

fn run_intent_resolvable(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    let Some(manifest) = load_manifest(&ctx.home, skill_name) else {
        return vec![Finding {
            check_id: "intent-resolvable".into(),
            category: "mcp".into(),
            severity: Severity::Unknown,
            skill_name: skill_name.to_string(),
            message: "Cannot read manifest — unable to check intent resolvability.".into(),
            remediation: None,
            fixable: false,
        }];
    };

    let Some(proc) = &manifest.content.procedure else {
        return vec![];
    };

    let inventory =
        mur_common::skill::McpInventory::from_tool_names(ctx.mcp_tools.clone().unwrap_or_default());
    let reqs = &manifest.mcp_requirements;

    let mut findings = Vec::new();
    for (idx, step) in proc.steps.iter().enumerate() {
        if step.intent.is_none() {
            continue;
        }
        if let mur_common::skill::Resolution::Unresolved { reason } =
            mur_common::skill::resolve_step(step, reqs, &inventory)
        {
            findings.push(Finding {
                check_id: "intent-resolvable".into(),
                category: "mcp".into(),
                severity: Severity::Warn,
                skill_name: skill_name.to_string(),
                message: format!(
                    "step[{idx}] intent '{}' unresolvable: {reason}",
                    step.intent.as_deref().unwrap_or("")
                ),
                remediation: Some(
                    "Install an MCP server providing the required tool, or add a fallback.".into(),
                ),
                fixable: false,
            });
        }
    }
    findings
}

fn run_disclosure(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    match load_manifest(&ctx.home, skill_name) {
        Some(m) => disclosure_findings(&m, skill_name),
        None => Vec::new(),
    }
}

/// Progressive-disclosure budgets (spec 2026-07-03 §4): description ≤ 120
/// chars, abstract ≤ 50 words. Warn-only — third-party skills stay valid.
fn disclosure_findings(
    m: &mur_common::skill::manifest::SkillManifest,
    skill_name: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let desc_chars = m.description.chars().count();
    if desc_chars > 120 {
        findings.push(Finding {
            check_id: "disclosure".into(),
            category: "disclosure".into(),
            severity: Severity::Warn,
            skill_name: skill_name.to_string(),
            message: format!(
                "description is {desc_chars} chars (budget 120) — the index line should say when to reach for the skill, not how"
            ),
            remediation: Some(
                "shorten description; move detail into content.abstract or the body".into(),
            ),
            fixable: false,
        });
    }
    let words = m.content.r#abstract.split_whitespace().count();
    if words > 50 {
        findings.push(Finding {
            check_id: "disclosure".into(),
            category: "disclosure".into(),
            severity: Severity::Warn,
            skill_name: skill_name.to_string(),
            message: format!(
                "abstract is {words} words (budget 50) — sink methodology into the body"
            ),
            remediation: Some("trim content.abstract to scope + one caveat + a load hint".into()),
            fixable: false,
        });
    }
    findings
}

fn exit_code(findings: &[Finding], strict: bool) -> i32 {
    let any_fail = findings.iter().any(|f| f.severity == Severity::Fail);
    let any_warn = findings.iter().any(|f| f.severity == Severity::Warn);
    i32::from(any_fail || (strict && any_warn))
}

/// Whole-store scan: detect agent-local skills that shadow a same-name skill
/// in the global store (builtin or registry). A vendored copy identical to
/// the global one is redundant (Ok — safe to de-pin); a diverged copy is a
/// shadow that silently pins a stale snapshot and never receives upstream
/// updates (Warn). Only names present in the global store can shadow, so
/// genuinely agent-specific skills are never flagged.
fn run_shadow_drift(ctx: &DoctorCtx) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Ok(agents) = std::fs::read_dir(ctx.home.join("agents")) else {
        return findings;
    };
    for agent in agents.flatten() {
        if !agent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let agent_name = agent.file_name().to_string_lossy().into_owned();
        let Ok(skills) = std::fs::read_dir(agent.path().join("skills")) else {
            continue;
        };
        for skill in skills.flatten() {
            let name = skill.file_name().to_string_lossy().into_owned();
            if !ctx.installed_skills.contains(&name) {
                continue; // no global twin → legitimately agent-specific
            }
            let Ok(local_raw) = std::fs::read_to_string(skill.path().join("skill.yaml")) else {
                continue;
            };
            let (Ok(local), Some(global)) = (
                mur_common::skill::parse_canonical(&local_raw),
                load_manifest(&ctx.home, &name),
            ) else {
                continue;
            };
            let (Ok(local_hash), Ok(global_hash)) = (
                mur_common::skill::content_hash_for_origin(&local),
                mur_common::skill::content_hash_for_origin(&global),
            ) else {
                continue;
            };
            let remediation = Some(format!("mur agent skill remove {agent_name} {name}"));
            if local_hash == global_hash {
                findings.push(Finding {
                    check_id: "shadow-drift".into(),
                    category: "shadow".into(),
                    severity: Severity::Ok,
                    skill_name: name.clone(),
                    message: format!(
                        "Agent '{agent_name}' vendors '{name}', identical to the global copy — redundant. De-pin so the global (builtin/registry) copy owns it."
                    ),
                    remediation,
                    fixable: true,
                });
            } else {
                findings.push(Finding {
                    check_id: "shadow-drift".into(),
                    category: "shadow".into(),
                    severity: Severity::Warn,
                    skill_name: name.clone(),
                    message: format!(
                        "Agent '{agent_name}' vendors '{name}', diverged from the global copy — this shadow pins a stale snapshot and never receives upstream MUR updates."
                    ),
                    remediation,
                    fixable: false,
                });
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_skill(dir: &TempDir, name: &str, yaml: &str) {
        let skill_dir = dir.path().join("skills").join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();
    }

    fn doctor_ctx(dir: &TempDir) -> DoctorCtx {
        DoctorCtx {
            home: dir.path().to_path_buf(),
            now: chrono::Utc::now(),
            installed_skills: std::collections::HashSet::new(),
            mcp_tools: None,
            llm_enabled: false,
            llm_ctx: None,
        }
    }

    fn doctor_ctx_with_tools(dir: &TempDir, tools: Vec<String>) -> DoctorCtx {
        DoctorCtx {
            home: dir.path().to_path_buf(),
            now: chrono::Utc::now(),
            installed_skills: std::collections::HashSet::new(),
            mcp_tools: Some(tools),
            llm_enabled: false,
            llm_ctx: None,
        }
    }

    fn write_shadow_skill(dir: &std::path::Path, name: &str, abstract_text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:test\ndescription: 'test skill {name}'\ncategory: context\ncontent:\n  abstract: '{abstract_text}'\n  context: 'body'\n"
        );
        std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
    }

    #[test]
    fn shadow_drift_flags_diverged_agent_copy_as_warn() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        write_shadow_skill(&home.join("skills/foo"), "foo", "global version");
        write_shadow_skill(
            &home.join("agents/a1/skills/foo"),
            "foo",
            "agent-diverged version",
        );

        let mut ctx = doctor_ctx(&tmp);
        ctx.installed_skills.insert("foo".to_string());

        let findings = run_shadow_drift(&ctx);
        let f = findings
            .iter()
            .find(|f| f.skill_name == "foo")
            .expect("expected a shadow finding for foo");
        assert_eq!(f.check_id, "shadow-drift");
        assert_eq!(f.severity, Severity::Warn);
        assert_eq!(
            f.remediation.as_deref().unwrap(),
            "mur agent skill remove a1 foo",
            "remediation must be the basename form"
        );
        assert!(!f.fixable, "diverged shadow must NOT be auto-fixable");
    }

    #[test]
    fn shadow_drift_flags_identical_agent_copy_as_ok() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        write_shadow_skill(&home.join("skills/foo"), "foo", "same version");
        write_shadow_skill(&home.join("agents/a1/skills/foo"), "foo", "same version");

        let mut ctx = doctor_ctx(&tmp);
        ctx.installed_skills.insert("foo".to_string());

        let findings = run_shadow_drift(&ctx);
        let f = findings
            .iter()
            .find(|f| f.skill_name == "foo")
            .expect("expected a shadow finding for foo");
        assert_eq!(f.severity, Severity::Ok);
        assert!(f.fixable, "identical shadow must be auto-fixable");
        assert_eq!(
            f.remediation.as_deref().unwrap(),
            "mur agent skill remove a1 foo",
            "remediation must be the basename form depin_skill resolves"
        );
    }

    #[test]
    fn shadow_drift_ignores_agent_only_skill() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        // agent-local skill with NO global twin — legitimate, must not be flagged
        write_shadow_skill(
            &home.join("agents/a1/skills/private"),
            "private",
            "agent only",
        );

        let ctx = doctor_ctx(&tmp); // installed_skills empty
        let findings = run_shadow_drift(&ctx);
        assert!(
            findings.is_empty(),
            "agent-only skills must not be flagged as shadows"
        );
    }

    #[test]
    fn coverage_workflow_with_dotted_tools_no_requirements() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: browser-skill
version: 1.0.0
publisher: human:test
description: Skill with tool refs but no mcp_requirements
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: navigate
        tool: browser.navigate
      - description: search
        tool: browser.search
"#;
        write_skill(&dir, "browser-skill", yaml);
        let ctx = doctor_ctx(&dir);
        let findings = run_mcp_requirements_coverage(&ctx, "browser-skill");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check_id, "mcp-requirements-coverage");
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].message.contains("browser.navigate"));
    }

    #[test]
    fn coverage_workflow_with_requirements_no_finding() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: covered-skill
version: 1.0.0
publisher: human:test
description: Skill with tool refs and mcp_requirements
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: navigate
        tool: browser.navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        write_skill(&dir, "covered-skill", yaml);
        let ctx = doctor_ctx(&dir);
        let findings = run_mcp_requirements_coverage(&ctx, "covered-skill");
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    #[test]
    fn coverage_context_mode_skipped() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: context-skill
version: 1.0.0
publisher: human:test
description: Context skill with no procedure
category: context
content:
  abstract: test
  context: some context
"#;
        write_skill(&dir, "context-skill", yaml);
        let ctx = doctor_ctx(&dir);
        let findings = run_mcp_requirements_coverage(&ctx, "context-skill");
        assert!(findings.is_empty());
    }

    #[test]
    fn coverage_no_dotted_tools_no_finding() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: no-tools
version: 1.0.0
publisher: human:test
description: Workflow with no dotted tool refs
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: do something
"#;
        write_skill(&dir, "no-tools", yaml);
        let ctx = doctor_ctx(&dir);
        let findings = run_mcp_requirements_coverage(&ctx, "no-tools");
        assert!(findings.is_empty());
    }

    // ── mcp-capability-available ──

    #[test]
    fn capability_check_unknown_without_agent_context() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: mcp-skill
version: 1.0.0
publisher: human:test
description: Skill with MCP requirements
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        write_skill(&dir, "mcp-skill", yaml);
        let ctx = doctor_ctx(&dir); // mcp_tools = None
        let findings = run_mcp_capability_available(&ctx, "mcp-skill");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check_id, "mcp-capability-available");
        assert_eq!(findings[0].severity, Severity::Unknown);
    }

    #[test]
    fn capability_check_warns_when_no_match() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: browser-skill
version: 1.0.0
publisher: human:test
description: Skill needing browser
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        write_skill(&dir, "browser-skill", yaml);
        let ctx =
            doctor_ctx_with_tools(&dir, vec!["filesystem.read".into(), "search.google".into()]);
        let findings = run_mcp_capability_available(&ctx, "browser-skill");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].message.contains("browser.*"));
    }

    #[test]
    fn capability_ok_when_glob_matches() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: browser-skill
version: 1.0.0
publisher: human:test
description: Skill needing browser
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        write_skill(&dir, "browser-skill", yaml);
        let ctx = doctor_ctx_with_tools(
            &dir,
            vec!["browser.navigate".into(), "browser.screenshot".into()],
        );
        let findings = run_mcp_capability_available(&ctx, "browser-skill");
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    #[test]
    fn capability_skips_fallback_requirement() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: fallback-skill
version: 1.0.0
publisher: human:test
description: Skill with fallback
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
    fallback: builtin-http
"#;
        write_skill(&dir, "fallback-skill", yaml);
        // No browser tools available — but fallback is set, so skip.
        let ctx = doctor_ctx_with_tools(&dir, vec!["filesystem.read".into()]);
        let findings = run_mcp_capability_available(&ctx, "fallback-skill");
        assert!(
            findings.is_empty(),
            "fallback requirements should be skipped, got {findings:?}"
        );
    }

    #[test]
    fn capability_empty_requirements_no_finding() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: simple-skill
version: 1.0.0
publisher: human:test
description: No MCP requirements
category: context
content:
  abstract: test
  context: body
"#;
        write_skill(&dir, "simple-skill", yaml);
        let ctx = doctor_ctx_with_tools(&dir, vec!["browser.navigate".into()]);
        let findings = run_mcp_capability_available(&ctx, "simple-skill");
        assert!(findings.is_empty());
    }

    // ── intent-resolvable ──

    #[test]
    fn intent_resolvable_matched_by_inventory() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: intent-skill
version: 1.0.0
publisher: human:test
description: Intent matched by inventory
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        write_skill(&dir, "intent-skill", yaml);
        let ctx = doctor_ctx_with_tools(
            &dir,
            vec!["browser.navigate".into(), "browser.click".into()],
        );
        let findings = run_intent_resolvable(&ctx, "intent-skill");
        assert!(
            findings.is_empty(),
            "expected no findings when intent matches, got {findings:?}"
        );
    }

    #[test]
    fn intent_resolvable_warns_when_no_match() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: unresolvable-skill
version: 1.0.0
publisher: human:test
description: Intent with no matching inventory
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        write_skill(&dir, "unresolvable-skill", yaml);
        let ctx = doctor_ctx_with_tools(&dir, vec!["filesystem.read".into()]);
        let findings = run_intent_resolvable(&ctx, "unresolvable-skill");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].message.contains("web_navigate"));
        assert!(findings[0].message.contains("unresolvable"));
    }

    #[test]
    fn intent_resolvable_fallback_in_inventory_no_warning() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: fallback-skill
version: 1.0.0
publisher: human:test
description: Intent resolved via fallback
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
    fallback: builtin-http
"#;
        write_skill(&dir, "fallback-skill", yaml);
        let ctx = doctor_ctx_with_tools(&dir, vec!["builtin-http".into()]);
        let findings = run_intent_resolvable(&ctx, "fallback-skill");
        assert!(
            findings.is_empty(),
            "fallback in inventory should resolve, got {findings:?}"
        );
    }

    #[test]
    fn intent_resolvable_skips_steps_without_intent() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
name: literal-skill
version: 1.0.0
publisher: human:test
description: Only literal tools, no intents
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        tool: browser.navigate
      - description: Search
"#;
        write_skill(&dir, "literal-skill", yaml);
        let ctx = doctor_ctx_with_tools(&dir, vec![]);
        let findings = run_intent_resolvable(&ctx, "literal-skill");
        assert!(
            findings.is_empty(),
            "steps without intent should be skipped, got {findings:?}"
        );
    }

    fn manifest(desc: &str, abstract_: &str) -> mur_common::skill::manifest::SkillManifest {
        let yaml = format!(
            r#"name: t
version: 0.1.0
publisher: human:t
description: "{desc}"
category: context
content:
  abstract: "{abstract_}"
  context: b
"#
        );
        mur_common::skill::parse_canonical(&yaml).unwrap()
    }

    #[test]
    fn disclosure_flags_fat_description_and_abstract() {
        let fat_desc = "d".repeat(121);
        let fat_abs = vec!["word"; 51].join(" ");
        let f = disclosure_findings(&manifest(&fat_desc, &fat_abs), "t");
        assert_eq!(f.len(), 2);
        assert!(
            f.iter()
                .all(|x| x.check_id == "disclosure" && x.severity == Severity::Warn)
        );

        let ok = disclosure_findings(&manifest("short", "brief abstract"), "t");
        assert!(ok.is_empty());
    }
}
