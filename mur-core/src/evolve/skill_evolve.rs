//! Skill evolution engine — FailureAnalyzer + SkillOptimizer + orchestration.
//!
//! Implements the closed loop: Create → Execute → Evaluate → Diagnose → Optimize → Repeat.

use anyhow::{Context, Result};
use mur_common::skill::scan::{ContentScanReport, scan_skill};
use mur_common::skill::{
    EvolutionEvent, SkillManifest, global_skill_dir, parse_canonical, read_from_dir, validate,
    write_to_dir,
};

use super::telemetry_reader::{SkillExecution, read_skill_executions};
use crate::conversations::backend::{ChatBackend, ChatRequest};

// ─── Failure Analyzer ────────────────────────────────────────────────

pub const FAILURE_ANALYZER_SYSTEM: &str = r#"
You are a Failure Analyzer for a skill system. Given a skill's content and its
execution telemetry, diagnose failures across 4 dimensions:

1. Knowledge — skill lacks domain information
2. Tool — wrong tool or wrong tool parameters
3. Clarification — ambiguous instructions / under-specified variables
4. Style — output format mismatch

INPUT: skill YAML + list of execution records (tool calls, errors, latencies).

OUTPUT: JSON array of diagnoses:
[{
  "dimension": "Knowledge|Tool|Clarification|Style",
  "severity": 0.0-1.0,
  "finding": "what is wrong",
  "suggested_fix": "specific change to make in the skill YAML",
  "evidence": ["telemetry excerpt 1", ...]
}]

Rules:
- Only report actionable findings with clear evidence.
- Severity 0.0 = cosmetic, 1.0 = blocking.
- If no failures occurred, return an empty array [].
"#;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Diagnosis {
    pub dimension: DiagnosisDimension,
    pub severity: f64,
    pub finding: String,
    pub suggested_fix: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DiagnosisDimension {
    Knowledge,
    Tool,
    Clarification,
    Style,
}

pub async fn diagnose_failures(
    skill: &SkillManifest,
    executions: &[SkillExecution],
    llm: &dyn ChatBackend,
) -> Result<Vec<Diagnosis>> {
    let failed: Vec<_> = executions.iter().filter(|e| !e.was_successful).collect();
    if failed.is_empty() {
        return Ok(vec![]);
    }

    let skill_yaml = serde_yaml_ng::to_string(skill).context("serialize skill for LLM")?;

    let prompt = format!(
        "Skill:\n{skill_yaml}\n\nExecutions ({} total, {} failures):\n{}",
        executions.len(),
        failed.len(),
        failed
            .iter()
            .map(|e| format!(
                "task={} tool_calls={:?} errors={:?} latency={}ms",
                e.task_id, e.tool_calls, e.errors, e.latency_ms
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let system = Some(FAILURE_ANALYZER_SYSTEM);
    let resp = llm
        .generate(ChatRequest {
            model: "",
            system,
            user: &prompt,
            max_tokens: 2048,
            temperature: Some(0.1f32),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await
        .context("failure analyzer LLM call")?;

    let raw = resp
        .text
        .trim()
        .trim_start_matches("```json")
        .trim_end_matches("```")
        .trim();
    let diagnoses: Vec<Diagnosis> =
        serde_json::from_str(raw).context("failure analyzer did not return valid JSON")?;
    Ok(diagnoses)
}

// ─── Skill Optimizer ──────────────────────────────────────────────────

pub const SKILL_OPTIMIZER_SYSTEM: &str = r#"
You are a Skill Optimizer. Given a skill YAML and a list of diagnosed failures,
rewrite the skill applying the minimal changes needed to fix each issue.

PRINCIPLE: Only change what's broken. Preserve verified behavior.
- If a procedure step has the wrong tool, fix the tool name — don't rewrite the step.
- If a variable is missing, add it — don't rename existing ones.
- If instructions are ambiguous, clarify — don't restructure.

INPUT: current skill YAML + JSON array of diagnoses.
OUTPUT: complete rewritten skill YAML (all fields present).
"#;

pub async fn optimize_skill(
    skill: &SkillManifest,
    diagnoses: &[Diagnosis],
    llm: &dyn ChatBackend,
) -> Result<SkillManifest> {
    let skill_yaml = serde_yaml_ng::to_string(skill).context("serialize skill for LLM")?;
    let diagnoses_json =
        serde_json::to_string_pretty(diagnoses).context("serialize diagnoses for LLM")?;

    let prompt = format!("Current skill YAML:\n{skill_yaml}\n\nDiagnoses:\n{diagnoses_json}");

    let system = Some(SKILL_OPTIMIZER_SYSTEM);
    let resp = llm
        .generate(ChatRequest {
            model: "",
            system,
            user: &prompt,
            max_tokens: 4096,
            temperature: Some(0.2f32),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await
        .context("skill optimizer LLM call")?;

    let yaml = resp
        .text
        .trim()
        .trim_start_matches("```yaml")
        .trim_end_matches("```")
        .trim();
    let evolved = parse_canonical(yaml).map_err(|e| anyhow::anyhow!("{e}"))?;
    validate(&evolved).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(evolved)
}

// ─── Helpers ──────────────────────────────────────────────────────────

fn bump_patch(version: &str) -> String {
    let parts: Vec<&str> = version.splitn(3, '.').collect();
    if parts.len() == 3
        && let Ok(patch) = parts[2].parse::<u32>()
    {
        return format!("{}.{}.{}", parts[0], parts[1], patch + 1);
    }
    format!("{version}.1")
}

fn score_executions(executions: &[SkillExecution]) -> f64 {
    if executions.is_empty() {
        return 0.0;
    }
    let ok = executions.iter().filter(|e| e.was_successful).count();
    ok as f64 / executions.len() as f64
}

// ─── Orchestrator ─────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct EvolutionResult {
    pub original_version: String,
    pub new_version: String,
    pub new_generation: u32,
    pub quality_score: f64,
    pub diagnoses: Vec<Diagnosis>,
    pub changes_summary: String,
    pub evolved_manifest: SkillManifest,
}

pub async fn evolve_skill(
    home: &std::path::Path,
    agent_name: &str,
    skill_name: &str,
    llm: &dyn ChatBackend,
    max_iterations: usize,
    dry_run: bool,
) -> Result<EvolutionResult> {
    let skill_dir = global_skill_dir(home, skill_name);
    let manifest =
        read_from_dir(&skill_dir).with_context(|| format!("skill '{skill_name}' not found"))?;

    let telemetry_dir = home.join("agents").join(agent_name).join("telemetry");
    let executions = read_skill_executions(&telemetry_dir, skill_name, 50)?;

    if executions.is_empty() {
        println!("No execution data for '{skill_name}' — nothing to evolve.");
        return Ok(EvolutionResult {
            original_version: manifest.version.clone(),
            new_version: manifest.version.clone(),
            new_generation: manifest
                .evolution_log
                .last()
                .map(|e| e.generation)
                .unwrap_or(0),
            quality_score: 0.0,
            diagnoses: vec![],
            changes_summary: String::new(),
            evolved_manifest: manifest,
        });
    }

    let original_version = manifest.version.clone();
    let mut current = manifest;

    for iteration in 1..=max_iterations {
        let diagnoses = diagnose_failures(&current, &executions, llm).await?;
        if diagnoses.is_empty() {
            println!("Iteration {iteration}: no failures diagnosed — skill is stable.");
            break;
        }

        eprintln!(
            "Iteration {iteration}: {} diagnosis(s) — {}",
            diagnoses.len(),
            diagnoses
                .iter()
                .map(|d| format!("{:?}({:.1})", d.dimension, d.severity))
                .collect::<Vec<_>>()
                .join(", "),
        );

        if dry_run {
            for d in &diagnoses {
                println!("  [{:?}] {} → {}", d.dimension, d.finding, d.suggested_fix);
            }
            break;
        }

        let mut evolved = optimize_skill(&current, &diagnoses, llm).await?;

        // Bump version: 0.1.0 → 0.1.1
        let new_version = bump_patch(&current.version);
        let generation = current
            .evolution_log
            .last()
            .map(|e| e.generation + 1)
            .unwrap_or(1);

        let quality_score = score_executions(&executions);
        let changes = diagnoses
            .iter()
            .map(|d| d.suggested_fix.clone())
            .collect::<Vec<_>>()
            .join("; ");

        evolved.version = new_version.clone();
        evolved.evolution_log = current.evolution_log.clone();
        evolved.evolution_log.push(EvolutionEvent::evolved(
            &new_version,
            generation,
            &changes,
            quality_score,
        ));

        // Write back.
        write_to_dir(&skill_dir, &evolved)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("write evolved skill")?;

        // Re-scan security.
        let report: ContentScanReport = scan_skill(&evolved).map_err(|e| anyhow::anyhow!("{e}"))?;
        if report.has_blocking_findings() {
            eprintln!("warning: evolved skill has new security findings — staying Sandboxed");
        }

        eprintln!("Evolved to v{new_version} (gen {generation}, score {quality_score:.2})");
        current = evolved;
    }

    let new_version = current.version.clone();
    let new_generation = current
        .evolution_log
        .last()
        .map(|e| e.generation)
        .unwrap_or(0);
    let quality_score = score_executions(&executions);

    Ok(EvolutionResult {
        original_version,
        new_version: new_version.clone(),
        new_generation,
        quality_score,
        diagnoses: vec![], // Last iteration's diagnoses already consumed
        changes_summary: current
            .evolution_log
            .last()
            .map(|e| e.changes.clone())
            .unwrap_or_default(),
        evolved_manifest: current,
    })
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_patch_increments() {
        assert_eq!(bump_patch("0.1.0"), "0.1.1");
        assert_eq!(bump_patch("2.0.0"), "2.0.1");
        assert_eq!(bump_patch("1.2.9"), "1.2.10");
    }

    #[test]
    fn bump_patch_handles_bad_input() {
        assert_eq!(bump_patch("0.1"), "0.1.1");
        assert_eq!(bump_patch("v1.2.3"), "v1.2.4");
    }

    #[test]
    fn score_executions_all_ok() {
        let execs = vec![
            SkillExecution {
                skill_name: "s".into(),
                task_id: "t1".into(),
                model: "m".into(),
                input_tokens: 100,
                latency_ms: 1000,
                tool_calls: vec![],
                errors: vec![],
                was_successful: true,
            },
            SkillExecution {
                skill_name: "s".into(),
                task_id: "t2".into(),
                model: "m".into(),
                input_tokens: 200,
                latency_ms: 1500,
                tool_calls: vec![],
                errors: vec![],
                was_successful: true,
            },
        ];
        assert_eq!(score_executions(&execs), 1.0);
    }

    #[test]
    fn score_executions_mixed() {
        let execs = vec![
            SkillExecution {
                skill_name: "s".into(),
                task_id: "t1".into(),
                model: "m".into(),
                input_tokens: 100,
                latency_ms: 1000,
                tool_calls: vec![],
                errors: vec![],
                was_successful: true,
            },
            SkillExecution {
                skill_name: "s".into(),
                task_id: "t2".into(),
                model: "m".into(),
                input_tokens: 200,
                latency_ms: 1500,
                tool_calls: vec![],
                errors: vec!["fail".into()],
                was_successful: false,
            },
        ];
        assert_eq!(score_executions(&execs), 0.5);
    }

    #[test]
    fn score_executions_empty() {
        assert_eq!(score_executions(&[]), 0.0);
    }
}
