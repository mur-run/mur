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
}

pub fn cmd_doctor(
    names: &[String],
    checks: &[String],
    json: bool,
    strict: bool,
    fix: bool,
    apply: bool,
) -> Result<()> {
    if fix {
        eprintln!(
            "warning: --fix is accepted but not yet implemented (requires M5b). Showing findings only."
        );
    }
    if apply {
        eprintln!(
            "warning: --apply requires --fix and M5b's repair engine. Showing findings only."
        );
    }

    let home = resolve_mur_home()?;
    let now = Utc::now();
    let installed_names: HashSet<String> =
        list_installed(&home).unwrap_or_default().into_iter().collect();

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
                _ => {}
            }
        }
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
        match &req {
            Requirement { name, .. } => {
                if ctx.installed_skills.contains(name) {
                    // Dependency is installed — check version if constraint given
                    let constraint = &req.version;
                    if constraint != "*" {
                        // Check if installed version satisfies the constraint
                        if let Some(dep_manifest) = load_manifest(&ctx.home, name) {
                            let installed_version = &dep_manifest.version;
                            if let Ok(req_ver) = semver::VersionReq::parse(constraint) {
                                if let Ok(inst_ver) = semver::Version::parse(installed_version) {
                                    if !req_ver.matches(&inst_ver) {
                                        findings.push(Finding {
                                            check_id: "dependency-freshness".into(),
                                            category: "deps".into(),
                                            severity: Severity::Warn,
                                            skill_name: skill_name.to_string(),
                                            message: format!(
                                                "Requires {name} {constraint} but {installed_version} is installed."
                                            ),
                                            remediation: Some(format!(
                                                "mur skill update {name}"
                                            )),
                                            fixable: true,
                                        });
                                    }
                                }
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
        Severity::Ok => "\x1b[32m",    // green
        Severity::Warn => "\x1b[33m",   // yellow
        Severity::Fail => "\x1b[31m",   // red
        Severity::Unknown => "\x1b[36m", // cyan
    };
    let mut out = String::new();
    write!(out, "{code}{icon}\x1b[0m").ok();
    out
}

pub fn exit_code(findings: &[Finding], strict: bool) -> i32 {
    let any_fail = findings.iter().any(|f| f.severity == Severity::Fail);
    let any_warn = findings.iter().any(|f| f.severity == Severity::Warn);
    if any_fail {
        1
    } else if strict && any_warn {
        1
    } else {
        0
    }
}
