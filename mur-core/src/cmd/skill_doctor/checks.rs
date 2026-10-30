//! Static (non-LLM) health checks for `mur skill doctor`.

use super::*;

// ── Checks ──

pub(super) fn run_tool_availability(_ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn load_manifest(
    home: &Path,
    skill_name: &str,
) -> Option<mur_common::skill::SkillManifest> {
    let path = home.join("skills").join(skill_name).join("skill.yaml");
    let content = std::fs::read_to_string(&path).ok()?;
    mur_common::skill::parse_canonical(&content).ok()
}

pub(super) fn run_dependency_freshness(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn run_execution_recency(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn run_failure_rate(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn run_mcp_requirements_coverage(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn run_mcp_capability_available(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn run_intent_resolvable(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
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

pub(super) fn run_disclosure(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    match load_manifest(&ctx.home, skill_name) {
        Some(m) => disclosure_findings(&m, skill_name),
        None => Vec::new(),
    }
}

/// Progressive-disclosure budgets (spec 2026-07-03 §4): description ≤ 120
/// chars, abstract ≤ 50 words. Warn-only — third-party skills stay valid.
pub(super) fn disclosure_findings(
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

/// Whole-store scan: detect agent-local skills that shadow a same-name skill
/// in the global store (builtin or registry). A vendored copy identical to
/// the global one is redundant (Ok — safe to de-pin); a diverged copy is a
/// shadow that silently pins a stale snapshot and never receives upstream
/// updates (Warn). Only names present in the global store can shadow, so
/// genuinely agent-specific skills are never flagged.
pub(super) fn run_shadow_drift(ctx: &DoctorCtx) -> Vec<Finding> {
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
