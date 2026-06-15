//! Unified DAG executor for `category: Workflow` skills (workflow-engine v2 P3).
//!
//! Loads a `Procedure` (from a skill's `ProcedureStep` list), topo-sorts by
//! `depends_on`, groups steps by topological rank, and executes each rank
//! concurrently via `tokio::spawn`. Command-mode steps run via `sh -c`;
//! intent-mode steps print instructions and mark `skipped_intent` in the
//! ledger. Every step writes a run-ledger record via `record_run`.
//!
//! The existing `PipelineExecutor` (`pipeline.rs`) handles legacy flat
//! `Workflow` objects and `|`/`&&`/`,` pipeline composition — this module
//! is for skill-based DAG workflows only.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::Result;
use mur_common::pipeline::{PipelineOutput, PipelineStatus, inject_input};
use mur_common::skill::event_log::{RunRecord, record_run};
use mur_common::skill::manifest::{FailureAction, Procedure, ProcedureStep};
use tokio::time::{Duration, sleep, timeout as tokio_timeout};

/// Options for a single DAG execution.
pub struct DagExecOptions<'a> {
    /// Piped input from a previous pipeline stage (for `{{input}}` substitution).
    pub input: Option<PipelineOutput>,
    /// `--yes` flag: auto-approve all `needs_approval` steps.
    pub yes: bool,
    /// Explicit override of the env classification (P4-ready).
    pub env_class_override: Option<&'a str>,
    /// Variable substitutions: `(name, value)` pairs for `{{name}}` in commands.
    pub variables: Vec<(String, String)>,
    /// Device identifier for the run ledger.
    pub device_id: String,
    /// Human-readable trigger source: "manual" | "schedule" | "agent".
    pub trigger: &'a str,
    /// Channel the executor runs OVER — events are appended to
    /// `~/.mur/channels/<id>/` as the workflow proceeds (v3a).
    pub channel_id: Option<String>,
}

impl<'a> Default for DagExecOptions<'a> {
    fn default() -> Self {
        Self {
            input: None,
            yes: false,
            env_class_override: None,
            variables: vec![],
            device_id: "cli".to_string(),
            trigger: "manual",
            channel_id: None,
        }
    }
}

// ── Graph types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct DagNode {
    step: ProcedureStep,
    /// Topological rank (0 = root, N = max ancestors to a root).
    rank: usize,
}

#[derive(Debug)]
#[allow(dead_code)]
struct DagGraph {
    nodes: Vec<DagNode>,
    /// Mapping from step id → index in `nodes`.
    id_to_idx: HashMap<String, usize>,
}

/// Build the DAG: assign ids, validate depends_on, detect cycles, compute ranks.
fn build_dag(steps: &[ProcedureStep]) -> Result<DagGraph> {
    // Assign default ids to steps without one.
    let steps: Vec<ProcedureStep> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| ProcedureStep {
            id: s.id.clone().or_else(|| Some(format!("s{i}"))),
            ..s.clone()
        })
        .collect();

    // Build id→index map.
    let id_to_idx: HashMap<String, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone().unwrap(), i))
        .collect();

    // Validate depends_on — every referenced id must exist.
    for s in &steps {
        let sid = s.id.as_deref().unwrap_or("");
        for dep in &s.depends_on {
            if !id_to_idx.contains_key(dep) {
                anyhow::bail!(
                    "step `{sid}` depends_on unknown step `{dep}` — available ids: {:?}",
                    id_to_idx.keys().collect::<Vec<_>>()
                );
            }
        }
    }

    // Topo-sort via Kahn: compute in-degree.
    let n = steps.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    for s in steps.iter() {
        let i = id_to_idx[&s.id.clone().unwrap()];
        for dep in &s.depends_on {
            let d = id_to_idx[dep];
            adj[d].push(i);
            in_degree[i] += 1;
        }
    }

    // Kahn: start with all in-degree=0.
    let mut queue: Vec<usize> = (0..n).filter(|i| in_degree[*i] == 0).collect();
    let mut topo = Vec::with_capacity(n);
    while let Some(i) = queue.pop() {
        topo.push(i);
        for &next in &adj[i] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push(next);
            }
        }
    }

    if topo.len() != n {
        // Some steps weren't reachable → cycle.
        let unreachable: Vec<String> = (0..n)
            .filter(|i| !topo.contains(i))
            .map(|i| steps[i].id.clone().unwrap())
            .collect();
        anyhow::bail!(
            "cycle detected in workflow DAG — unreachable steps: {:?}",
            unreachable
        );
    }

    // Assign ranks: rank[i] = 0 + max(rank[dep] + 1) over depends_on.
    let mut rank = vec![0usize; n];
    for &i in &topo {
        for dep in &steps[i].depends_on {
            let d = id_to_idx[dep];
            rank[i] = rank[i].max(rank[d] + 1);
        }
    }

    let nodes: Vec<DagNode> = steps
        .into_iter()
        .enumerate()
        .map(|(i, step)| DagNode {
            step,
            rank: rank[i],
        })
        .collect();

    Ok(DagGraph { nodes, id_to_idx })
}

// ── Step execution ──────────────────────────────────────────────────────────

struct StepResult {
    exit_code: i32,
    output_text: String,
    duration_ms: u64,
    failed_step: Option<String>,
    success: bool,
}

/// Run a single step. This is the per-step async block spawned per rank.
async fn execute_step(
    step: &ProcedureStep,
    opts: &DagExecOptions<'_>,
    step_index: usize,
) -> StepResult {
    let start = std::time::Instant::now();

    // ── needs_approval ──
    if step.needs_approval {
        let approved = if opts.yes {
            true
        } else if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            dialoguer::Confirm::new()
                .with_prompt(format!(
                    "Step {}: «{}» — run?",
                    step.id.as_deref().unwrap_or(&step_index.to_string()),
                    step.description
                ))
                .default(true)
                .interact()
                .unwrap_or(false)
        } else {
            false
        };
        if !approved {
            eprintln!(
                "  Step {}: needs_approval, skipped (yields true) — use `--yes` to auto-approve",
                step.id.as_deref().unwrap_or(&step_index.to_string())
            );
            return StepResult {
                exit_code: 0,
                output_text: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: None,
                success: true,
            };
        }
    }

    // ── Command-mode ──
    if let Some(cmd_template) = &step.command {
        // Variable substitution: {{var_name}} → value
        let mut cmd_string = cmd_template.clone();
        for (name, value) in &opts.variables {
            cmd_string = cmd_string.replace(&format!("{{{{{name}}}}}"), value);
        }
        // Piped input: {{input}}
        let input_text = opts
            .input
            .as_ref()
            .and_then(|o| o.output_text.as_deref())
            .filter(|t| !t.is_empty());
        cmd_string = inject_input(&cmd_string, input_text);

        // Dedent and normalize for display.
        let display_cmd = cmd_string.trim();
        eprintln!(
            "  Step {}: {} (`{}`)",
            step.id.as_deref().unwrap_or(&step_index.to_string()),
            step.description,
            display_cmd
        );

        // Build command with optional timeout.
        let cmd_result = {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c")
                .arg(&cmd_string)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match step.timeout_secs {
                Some(secs) => match tokio_timeout(Duration::from_secs(secs), cmd.output()).await {
                    Ok(Ok(out)) => Some(out),
                    Ok(Err(e)) => {
                        eprintln!(
                            "  Step {} exec error: {}",
                            step.id.as_deref().unwrap_or(&step_index.to_string()),
                            e
                        );
                        return StepResult {
                            exit_code: 1,
                            output_text: format!("exec error: {e}"),
                            duration_ms: start.elapsed().as_millis() as u64,
                            failed_step: Some(step.description.clone()),
                            success: false,
                        };
                    }
                    Err(_) => {
                        eprintln!(
                            "  Step {} timed out after {secs}s",
                            step.id.as_deref().unwrap_or(&step_index.to_string())
                        );
                        return StepResult {
                            exit_code: -1,
                            output_text: "timeout".to_string(),
                            duration_ms: start.elapsed().as_millis() as u64,
                            failed_step: Some(step.description.clone()),
                            success: false,
                        };
                    }
                },
                None => match cmd.output().await {
                    Ok(out) => Some(out),
                    Err(e) => {
                        eprintln!(
                            "  Step {} exec error: {}",
                            step.id.as_deref().unwrap_or(&step_index.to_string()),
                            e
                        );
                        return StepResult {
                            exit_code: 1,
                            output_text: format!("exec error: {e}"),
                            duration_ms: start.elapsed().as_millis() as u64,
                            failed_step: Some(step.description.clone()),
                            success: false,
                        };
                    }
                },
            }
        };

        let output = cmd_result.unwrap();

        let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
        if !stderr_str.is_empty() {
            eprint!("{}", stderr_str);
        }
        if !stdout_str.is_empty() {
            print!("{}", stdout_str);
        }

        let exit_code = output.status.code().unwrap_or(1);

        // Handle on_failure if exit_code != 0 (caller decides Abort/Skip/Retry).
        StepResult {
            exit_code,
            output_text: stdout_str,
            duration_ms: start.elapsed().as_millis() as u64,
            failed_step: if exit_code != 0 {
                Some(step.description.clone())
            } else {
                None
            },
            success: exit_code == 0,
        }
    } else {
        // ── Intent-mode (no command) ──
        eprintln!(
            "  Step {}: {} {}",
            step.id.as_deref().unwrap_or(&step_index.to_string()),
            step.description,
            step.tool
                .as_deref()
                .map(|t| format!("(tool: {t})"))
                .unwrap_or_default()
        );
        StepResult {
            exit_code: 0,
            output_text: step.description.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
            failed_step: None,
            success: true,
        }
    }
}

// ── Core DAG executor ───────────────────────────────────────────────────────

/// Execute a Procedure (skill workflow) DAG. Uses the `--yes` and `variables`
/// from `DagExecOptions`. Records every step outcome via the run-ledger.
///
/// Returns a `PipelineOutput` for composability with the existing pipeline
/// infrastructure (e.g. piping output to the next CLI command).
pub async fn execute_dag(
    mur_home: &Path,
    skill_name: &str,
    procedure: &Procedure,
    opts: &DagExecOptions<'_>,
) -> Result<PipelineOutput> {
    let start = std::time::Instant::now();

    let graph = build_dag(&procedure.steps)?;

    // Fail-closed: needs_approval requires a HITL round-trip (v3c). Do not silently skip
    // approval when running headless over a channel — that would be a policy bypass.
    if opts.channel_id.is_some() && procedure.steps.iter().any(|s| s.needs_approval) {
        anyhow::bail!(
            "needs_approval requires interactive HITL (v3c); cannot run over a channel headlessly. \
             Remove needs_approval from the workflow or run without --channel."
        );
    }

    if graph.nodes.is_empty() {
        return Ok(PipelineOutput {
            workflow_id: skill_name.to_string(),
            status: PipelineStatus::Success,
            output_text: Some(String::new()),
            output_data: None,
            exit_code: 0,
            duration_ms: 0,
        });
    }

    // Group by rank.
    let max_rank = graph.nodes.iter().map(|n| n.rank).max().unwrap_or(0);
    let mut overall_exit_code = 0i32;
    let mut overall_output = String::new();

    for rank in 0..=max_rank {
        let indices: Vec<usize> = (0..graph.nodes.len())
            .filter(|i| graph.nodes[*i].rank == rank)
            .collect();

        if indices.is_empty() {
            continue;
        }

        // Concurrent: spawn each step in this rank.
        // Extract owned values from opts for the spawned tasks.
        let opt_yes = opts.yes;
        let opt_input = opts.input.clone();
        let opt_env_override = opts.env_class_override.map(|s| s.to_string());
        let opt_vars = opts.variables.clone();
        let opt_dev_id = opts.device_id.clone();
        let opt_trigger = opts.trigger.to_string();
        let mut handles = Vec::new();
        for &i in &indices {
            let step = graph.nodes[i].step.clone();
            let env_override = opt_env_override.clone();
            let dev_id = opt_dev_id.clone();
            let inp = opt_input.clone();
            let vars = opt_vars.clone();
            let tr = opt_trigger.clone();
            handles.push(tokio::task::spawn(async move {
                let opts_clone = DagExecOptions {
                    yes: opt_yes,
                    input: inp,
                    env_class_override: env_override.as_deref(),
                    variables: vars,
                    device_id: dev_id,
                    trigger: &tr,
                    channel_id: None,
                };
                execute_step(&step, &opts_clone, i).await
            }));
        }

        let mut results = Vec::with_capacity(indices.len());
        for h in handles {
            match h.await {
                Ok(r) => results.push(r),
                Err(e) => {
                    eprintln!("  ⚠ Task join error: {}", e);
                    results.push(StepResult {
                        exit_code: 1,
                        output_text: format!("task join error: {e}"),
                        duration_ms: 0,
                        failed_step: Some("(task)".to_string()),
                        success: false,
                    });
                }
            }
        }

        // Collect results and record to ledger.
        for ri in 0..results.len() {
            let result = &results[ri];
            let step = &graph.nodes[indices[ri]].step;

            // Write run-ledger record.
            let stderr_for_ledger = if !result.success && result.output_text.is_empty() {
                Some(result.output_text.as_str())
            } else {
                None
            };
            record_run(
                mur_home,
                skill_name,
                &opts.device_id,
                &RunRecord {
                    success: result.success,
                    duration_ms: Some(result.duration_ms),
                    exit_code: Some(result.exit_code),
                    stderr: stderr_for_ledger,
                    failed_step: result.failed_step.clone(),
                    trigger: opts.trigger,
                    env_class_override: opts.env_class_override,
                },
            )
            .ok();

            if !result.output_text.is_empty() {
                if !overall_output.is_empty() {
                    overall_output.push('\n');
                }
                overall_output.push_str(&result.output_text);
            }

            if result.exit_code != 0 {
                overall_exit_code = result.exit_code;

                // Handle on_failure strategy.
                let sid = step.id.as_deref().unwrap_or("?");
                match step.on_failure {
                    FailureAction::Abort => {
                        eprintln!(
                            "  Step {sid} failed (exit {}), aborting workflow",
                            result.exit_code
                        );
                        return Ok(PipelineOutput {
                            workflow_id: skill_name.to_string(),
                            status: PipelineStatus::Failed,
                            output_text: Some(overall_output),
                            output_data: None,
                            exit_code: result.exit_code,
                            duration_ms: start.elapsed().as_millis() as u64,
                        });
                    }
                    FailureAction::Skip => {
                        eprintln!("  Step {sid} failed (exit {}), skipping", result.exit_code);
                        overall_exit_code = 0;
                    }
                    FailureAction::Retry => {
                        let max_retries = step.retry.as_ref().map(|r| r.max_retries).unwrap_or(1);
                        let backoff = step
                            .retry
                            .as_ref()
                            .and_then(|r| r.backoff_secs)
                            .unwrap_or(0);
                        for attempt in 0..max_retries {
                            if backoff > 0 {
                                sleep(Duration::from_secs(backoff as u64)).await;
                            }
                            eprintln!(
                                "  Step {sid} failed, retry {}/{}...",
                                attempt + 1,
                                max_retries
                            );
                            let retry_result = execute_step(step, opts, indices[ri]).await;
                            if retry_result.success {
                                results[ri] = retry_result;
                                overall_exit_code = 0;
                                break;
                            } else if attempt + 1 == max_retries {
                                let _retry_code = retry_result.exit_code;
                                eprintln!("  Step {sid} retry exhausted, aborting workflow");
                                return Ok(PipelineOutput {
                                    workflow_id: skill_name.to_string(),
                                    status: PipelineStatus::Failed,
                                    output_text: Some(overall_output),
                                    output_data: None,
                                    exit_code: retry_result.exit_code,
                                    duration_ms: start.elapsed().as_millis() as u64,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let status = if overall_exit_code == 0 {
        PipelineStatus::Success
    } else {
        PipelineStatus::Failed
    };

    Ok(PipelineOutput {
        workflow_id: skill_name.to_string(),
        status,
        output_text: Some(overall_output),
        output_data: None,
        exit_code: overall_exit_code,
        duration_ms,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::manifest::{FailureAction, ProcedureStep, RetryConfig};

    fn step(id: &str, deps: &[&str], cmd: Option<&str>) -> ProcedureStep {
        ProcedureStep {
            id: Some(id.to_string()),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            command: cmd.map(|s| s.to_string()),
            description: format!("step {id}"),
            ..Default::default()
        }
    }

    #[test]
    fn linear_chain_toposorts() {
        let steps = vec![
            step("s0", &[], Some("echo zero")),
            step("s1", &["s0"], Some("echo one")),
            step("s2", &["s1"], Some("echo two")),
        ];
        let graph = build_dag(&steps).unwrap();
        assert_eq!(graph.nodes.len(), 3);
        // s0 rank 0, s1 rank 1, s2 rank 2
        for n in &graph.nodes {
            let id = n.step.id.as_deref().unwrap();
            let expected = match id {
                "s0" => 0,
                "s1" => 1,
                "s2" => 2,
                _ => unreachable!(),
            };
            assert_eq!(n.rank, expected, "step {id} expected rank {expected}");
        }
    }

    #[test]
    fn concurrent_roots() {
        let steps = vec![
            step("s0", &[], None), // root
            step("s1", &[], None), // root
            step("s2", &["s0", "s1"], None),
        ];
        let graph = build_dag(&steps).unwrap();
        for n in &graph.nodes {
            match n.step.id.as_deref().unwrap() {
                "s0" | "s1" => assert_eq!(n.rank, 0),
                "s2" => assert_eq!(n.rank, 1),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn cycle_detected() {
        let steps = vec![step("s0", &["s1"], None), step("s1", &["s0"], None)];
        let err = build_dag(&steps).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cycle"), "expected cycle error, got: {msg}");
    }

    #[test]
    fn unknown_dep_detected() {
        let steps = vec![step("s0", &["nonexistent"], None)];
        let err = build_dag(&steps).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nonexistent"),
            "expected nonexistent dep error, got: {msg}"
        );
    }

    #[test]
    fn missing_ids_autogenerated() {
        let steps = vec![ProcedureStep {
            id: None,
            depends_on: vec![],
            command: Some("echo hi".to_string()),
            ..Default::default()
        }];
        let graph = build_dag(&steps).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].step.id.as_deref(), Some("s0"));
    }

    #[test]
    fn empty_steps_returns_ok() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proc = Procedure {
            variables: vec![],
            steps: vec![],
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt
            .block_on(execute_dag(
                tmp.path(),
                "empty-test",
                &proc,
                &DagExecOptions::default(),
            ))
            .unwrap();
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn channel_run_refuses_needs_approval() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proc = Procedure {
            variables: vec![],
            steps: vec![ProcedureStep {
                id: Some("s0".to_string()),
                command: Some("echo ok".to_string()),
                description: "needs approval".to_string(),
                needs_approval: true,
                ..Default::default()
            }],
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(execute_dag(
                tmp.path(),
                "test",
                &proc,
                &DagExecOptions {
                    channel_id: Some("ch-1".to_string()),
                    ..DagExecOptions::default()
                },
            ))
            .unwrap_err();
        assert!(
            err.to_string().contains("needs_approval"),
            "expected needs_approval error, got: {err}"
        );
    }
}
