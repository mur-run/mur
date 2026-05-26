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
}

pub fn cmd_doctor(
    names: &[String],
    checks: &[String],
    json: bool,
    strict: bool,
    fix: bool,
    apply: bool,
) -> Result<()> {
    let home = resolve_mur_home()?;
    let now = Utc::now();
    let installed_names: HashSet<String> = list_installed(&home)
        .unwrap_or_default()
        .into_iter()
        .collect();

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
        "mcp-requirements-coverage",
        "mcp-capability-available",
        "intent-resolvable",
    ];
    let active_checks: Vec<&str> = if checks.is_empty() {
        all_checks.to_vec()
    } else {
        checks.iter().map(|c| c.as_str()).collect()
    };

    let ctx = DoctorCtx {
        home,
        now,
        installed_skills: installed_names,
        mcp_tools: None, // wired to agent MCP registry in M6b
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
                "mcp-requirements-coverage" => {
                    findings.extend(run_mcp_requirements_coverage(&ctx, skill_name));
                }
                "mcp-capability-available" => {
                    findings.extend(run_mcp_capability_available(&ctx, skill_name));
                }
                "intent-resolvable" => {
                    findings.extend(run_intent_resolvable(&ctx, skill_name));
                }
                _ => {}
            }
        }
    }

    // ── Repair (M5b) ──
    if fix {
        let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
            .unwrap_or_else(|_| crate::cmd::skill_registry::DEFAULT_REGISTRY.to_string());
        let repairs: Vec<Box<dyn crate::skill_repair::Repair>> = vec![
            Box::new(crate::skill_repair::tool_availability::ToolAvailabilityRepair),
            Box::new(crate::skill_repair::dep_freshness::DepFreshnessRepair),
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

fn run_api_drift(_ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    vec![Finding {
        check_id: "api-drift".into(),
        category: "api-drift".into(),
        severity: Severity::Unknown,
        skill_name: skill_name.to_string(),
        message: "API drift detection deferred to M6 (LLM-driven analysis).".into(),
        remediation: None,
        fixable: false,
    }]
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

    let inventory = mur_common::skill::McpInventory::from_tool_names(
        ctx.mcp_tools.clone().unwrap_or_default(),
    );
    let reqs = &manifest.mcp_requirements;

    let mut findings = Vec::new();
    for (idx, step) in proc.steps.iter().enumerate() {
        if step.intent.is_none() {
            continue;
        }
        match mur_common::skill::resolve_step(step, reqs, &inventory) {
            mur_common::skill::Resolution::Unresolved { reason } => {
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
                        "Install an MCP server providing the required tool, or add a fallback."
                            .into(),
                    ),
                    fixable: false,
                });
            }
            _ => {}
        }
    }
    findings
}

fn exit_code(findings: &[Finding], strict: bool) -> i32 {
    let any_fail = findings.iter().any(|f| f.severity == Severity::Fail);
    let any_warn = findings.iter().any(|f| f.severity == Severity::Warn);
    i32::from(any_fail || (strict && any_warn))
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
        }
    }

    fn doctor_ctx_with_tools(dir: &TempDir, tools: Vec<String>) -> DoctorCtx {
        DoctorCtx {
            home: dir.path().to_path_buf(),
            now: chrono::Utc::now(),
            installed_skills: std::collections::HashSet::new(),
            mcp_tools: Some(tools),
        }
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
        let ctx =
            doctor_ctx_with_tools(&dir, vec!["browser.navigate".into(), "browser.click".into()]);
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
}
