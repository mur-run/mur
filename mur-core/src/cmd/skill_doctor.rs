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

mod checks;
mod llm_checks;

use super::agent::resolve_mur_home;
use checks::*;
use llm_checks::*;

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
            Box::new(crate::skill_repair::shadow_depin::ShadowDepinRepair),
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

fn exit_code(findings: &[Finding], strict: bool) -> i32 {
    let any_fail = findings.iter().any(|f| f.severity == Severity::Fail);
    let any_warn = findings.iter().any(|f| f.severity == Severity::Warn);
    i32::from(any_fail || (strict && any_warn))
}

#[cfg(test)]
mod tests;
