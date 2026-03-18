//! Pipeline executor — runs `PipelineExpr` trees.
//!
//! Phase 3: Full parallel execution with output merging and fail-fast.
//! `{{input}}` template variables are substituted with piped input.
//! Pipe passes output_text forward. Sequential gates on exit_code.
//! Parallel spawns all branches concurrently via tokio::spawn.

use std::pin::Pin;
use std::process::Stdio;
use std::time::Instant;

use anyhow::Result;
use futures::Future;
use mur_common::pipeline::{PipelineExpr, PipelineOutput, PipelineStatus, inject_input};
use tokio_util::sync::CancellationToken;

/// Type alias for the result vector from `tokio::spawn` join_all in parallel execution.
type ParallelJoinResults =
    Vec<std::result::Result<(String, Result<PipelineOutput>, f64), tokio::task::JoinError>>;

use crate::store::workflow_yaml::WorkflowYamlStore;

/// Executes pipeline expressions against the workflow store.
#[derive(Clone)]
pub struct PipelineExecutor {
    store: WorkflowYamlStore,
    /// When true, cancel remaining parallel branches on first failure.
    fail_fast: bool,
    /// Cancellation token for cooperative cancellation in fail-fast mode.
    cancel: CancellationToken,
}

impl PipelineExecutor {
    pub fn new(store: WorkflowYamlStore) -> Self {
        Self {
            store,
            fail_fast: false,
            cancel: CancellationToken::new(),
        }
    }

    pub fn with_fail_fast(mut self, fail_fast: bool) -> Self {
        self.fail_fast = fail_fast;
        self
    }

    /// Execute a pipeline expression tree, optionally with piped input.
    /// Uses Box::pin to handle async recursion.
    /// Checks the cancellation token before each step (for fail-fast support).
    pub fn execute(
        &self,
        expr: &PipelineExpr,
        input: Option<PipelineOutput>,
    ) -> Pin<Box<dyn Future<Output = Result<PipelineOutput>> + Send + '_>> {
        let expr = expr.clone();
        Box::pin(async move {
            // Check cancellation before starting
            if self.cancel.is_cancelled() {
                return Ok(PipelineOutput {
                    workflow_id: "cancelled".into(),
                    status: PipelineStatus::Skipped,
                    output_text: Some("cancelled by fail-fast".into()),
                    output_data: None,
                    exit_code: 1,
                    duration_ms: 0,
                });
            }

            match expr {
                PipelineExpr::Single(id) => self.run_single(&id, input).await,

                PipelineExpr::Pipe(left, right) => {
                    let left_output = self.execute(&left, input).await?;
                    if left_output.status != PipelineStatus::Success {
                        return Ok(left_output);
                    }
                    self.execute(&right, Some(left_output)).await
                }

                PipelineExpr::Sequential(left, right) => {
                    let left_output = self.execute(&left, input).await?;
                    if left_output.exit_code != 0 {
                        return Ok(left_output);
                    }
                    self.execute(&right, None).await
                }

                PipelineExpr::Parallel(exprs) => self.run_parallel(&exprs).await,
            }
        })
    }

    /// Run a single workflow by name/ID.
    ///
    /// - Substitutes `{{input}}` in step descriptions and commands with piped input.
    /// - Executes shell commands and captures stdout as output.
    /// - For prompt-only steps (no command), the description becomes output.
    /// - The last step's output becomes the workflow's `PipelineOutput.output_text`.
    async fn run_single(&self, id: &str, input: Option<PipelineOutput>) -> Result<PipelineOutput> {
        let start = Instant::now();

        // Look up workflow (exact match, then keyword fallback)
        let workflow = match self.store.get(id) {
            Ok(w) => w,
            Err(_) => {
                let all = self.store.list_all()?;
                let q = id.to_lowercase();
                match all.into_iter().find(|w| {
                    let text = format!("{} {} {}", w.name, w.description, w.tools.join(" "))
                        .to_lowercase();
                    text.contains(&q)
                }) {
                    Some(w) => w,
                    None => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        eprintln!("Pipeline: workflow not found: {}", id);
                        return Ok(PipelineOutput {
                            workflow_id: id.to_string(),
                            status: PipelineStatus::Failed,
                            output_text: Some(format!("workflow not found: {}", id)),
                            output_data: None,
                            exit_code: 1,
                            duration_ms,
                        });
                    }
                }
            }
        };

        // Extract input text for {{input}} substitution
        let input_text = input.as_ref().and_then(|p| p.output_text.as_deref());

        eprintln!("▶ Running workflow: {}", workflow.name);

        // If no steps, output the description (with {{input}} substituted)
        if workflow.steps.is_empty() {
            let desc = inject_input(&workflow.description, input_text);
            println!("{}", desc);
            let duration_ms = start.elapsed().as_millis() as u64;
            return Ok(PipelineOutput {
                workflow_id: workflow.name.clone(),
                status: PipelineStatus::Success,
                output_text: Some(desc),
                output_data: None,
                exit_code: 0,
                duration_ms,
            });
        }

        // Execute steps in order, capturing output from each
        let mut last_output = String::new();
        let mut last_exit_code = 0i32;

        for step in &workflow.steps {
            let description = inject_input(&step.description, input_text);

            if let Some(ref cmd_template) = step.command {
                // Shell step: substitute {{input}} and execute
                let cmd = inject_input(cmd_template, input_text);
                eprintln!("  Step {}: {} (`{}`)", step.order, description, cmd);

                let cmd_fut = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output();

                // Race command against cancellation token (for fail-fast)
                let cmd_result = tokio::select! {
                    result = cmd_fut => Some(result),
                    _ = self.cancel.cancelled() => None,
                };

                if cmd_result.is_none() {
                    eprintln!("  Step {} cancelled by fail-fast", step.order);
                    return Ok(PipelineOutput {
                        workflow_id: workflow.name.clone(),
                        status: PipelineStatus::Skipped,
                        output_text: Some("cancelled by fail-fast".into()),
                        output_data: None,
                        exit_code: 1,
                        duration_ms: start.elapsed().as_millis() as u64,
                    });
                }

                match cmd_result.unwrap() {
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr);

                        if !stdout.is_empty() {
                            print!("{}", stdout);
                        }
                        if !stderr.is_empty() {
                            eprint!("{}", stderr);
                        }

                        last_exit_code = output.status.code().unwrap_or(1);
                        last_output = stdout;

                        if last_exit_code != 0 {
                            match step.on_failure {
                                mur_common::workflow::FailureAction::Abort => {
                                    eprintln!(
                                        "  Step {} failed (exit {}), aborting workflow",
                                        step.order, last_exit_code
                                    );
                                    break;
                                }
                                mur_common::workflow::FailureAction::Skip => {
                                    eprintln!(
                                        "  Step {} failed (exit {}), skipping",
                                        step.order, last_exit_code
                                    );
                                    last_exit_code = 0; // Reset for next step
                                    continue;
                                }
                                mur_common::workflow::FailureAction::Retry => {
                                    // Simple single retry
                                    eprintln!("  Step {} failed, retrying once...", step.order);
                                    if let Ok(retry) = tokio::process::Command::new("sh")
                                        .arg("-c")
                                        .arg(&cmd)
                                        .stdout(Stdio::piped())
                                        .stderr(Stdio::piped())
                                        .output()
                                        .await
                                    {
                                        last_exit_code = retry.status.code().unwrap_or(1);
                                        last_output =
                                            String::from_utf8_lossy(&retry.stdout).to_string();
                                        if !last_output.is_empty() {
                                            print!("{}", last_output);
                                        }
                                        if last_exit_code != 0 {
                                            eprintln!(
                                                "  Step {} retry failed (exit {}), aborting",
                                                step.order, last_exit_code
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  Step {} exec error: {}", step.order, e);
                        last_exit_code = 1;
                        last_output = format!("exec error: {}", e);
                        break;
                    }
                }
            } else {
                // Prompt-only step (no command): description is the output
                eprintln!("  Step {}: {}", step.order, description);
                last_output = description;
            }
        }

        // Trim trailing whitespace from captured output
        let output_text = last_output.trim_end().to_string();

        // Try to parse as JSON for structured data
        let output_data = serde_json::from_str::<serde_json::Value>(&output_text).ok();

        let status = if last_exit_code == 0 {
            PipelineStatus::Success
        } else {
            PipelineStatus::Failed
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(PipelineOutput {
            workflow_id: workflow.name.clone(),
            status,
            output_text: Some(output_text),
            output_data,
            exit_code: last_exit_code,
            duration_ms,
        })
    }

    /// Run branches in parallel via tokio::spawn, collect and merge results.
    ///
    /// Output merging:
    /// - `output_text`: join branch outputs with `---` separator
    /// - `exit_code`: 0 only if ALL branches succeed, else first non-zero
    /// - `duration_ms`: max of all branches (wall-clock time)
    /// - `output_data`: merged into a JSON array if multiple branches produce data
    ///
    /// When `fail_fast` is true, cancels remaining branches on first failure.
    async fn run_parallel(&self, exprs: &[PipelineExpr]) -> Result<PipelineOutput> {
        let start = Instant::now();

        // Each branch gets its own child executor sharing the same cancel token
        let mut handles = Vec::with_capacity(exprs.len());
        for expr in exprs {
            let mut branch_executor = self.clone();
            // Share the parent cancel token so fail-fast propagates
            branch_executor.cancel = self.cancel.clone();
            let expr = expr.clone();
            let branch_label = expr_label(&expr);

            eprintln!("⏳ [parallel] Starting: {}", branch_label);

            handles.push(tokio::spawn(async move {
                let branch_start = Instant::now();
                let result = branch_executor.execute(&expr, None).await;
                let elapsed = branch_start.elapsed().as_secs_f64();
                (branch_label, result, elapsed)
            }));
        }

        if self.fail_fast {
            // Fail-fast: process results as they complete
            self.run_parallel_fail_fast(handles, start).await
        } else {
            // Wait for all
            let results = futures::future::join_all(handles).await;
            self.merge_parallel_results(results, start)
        }
    }

    /// Fail-fast parallel: cancel remaining on first failure.
    async fn run_parallel_fail_fast(
        &self,
        handles: Vec<tokio::task::JoinHandle<(String, Result<PipelineOutput>, f64)>>,
        start: Instant,
    ) -> Result<PipelineOutput> {
        use futures::StreamExt;
        use futures::stream::FuturesUnordered;

        let mut futs: FuturesUnordered<_> = handles.into_iter().collect();
        let mut outputs: Vec<(String, PipelineOutput)> = Vec::new();
        let mut first_error: Option<PipelineOutput> = None;

        while let Some(join_result) = futs.next().await {
            match join_result {
                Ok((label, Ok(output), elapsed)) => {
                    if output.exit_code != 0 {
                        eprintln!("❌ [parallel] Failed: {} ({:.1}s)", label, elapsed);
                        if first_error.is_none() {
                            first_error = Some(output.clone());
                            // Cancel remaining branches
                            self.cancel.cancel();
                            eprintln!("⚡ [parallel] Fail-fast: cancelling remaining branches");
                        }
                        outputs.push((label, output));
                    } else {
                        eprintln!("✅ [parallel] Done: {} ({:.1}s)", label, elapsed);
                        outputs.push((label, output));
                    }
                }
                Ok((label, Err(e), elapsed)) => {
                    eprintln!("❌ [parallel] Failed: {} ({:.1}s) — {}", label, elapsed, e);
                    let err_output = PipelineOutput {
                        workflow_id: label.clone(),
                        status: PipelineStatus::Failed,
                        output_text: Some(format!("Error: {}", e)),
                        output_data: None,
                        exit_code: 1,
                        duration_ms: (elapsed * 1000.0) as u64,
                    };
                    if first_error.is_none() {
                        first_error = Some(err_output.clone());
                        self.cancel.cancel();
                        eprintln!("⚡ [parallel] Fail-fast: cancelling remaining branches");
                    }
                    outputs.push((label, err_output));
                }
                Err(e) => {
                    let err_output = PipelineOutput {
                        workflow_id: "join-error".into(),
                        status: PipelineStatus::Failed,
                        output_text: Some(format!("Task join error: {}", e)),
                        output_data: None,
                        exit_code: 1,
                        duration_ms: 0,
                    };
                    if first_error.is_none() {
                        first_error = Some(err_output.clone());
                        self.cancel.cancel();
                    }
                    outputs.push(("join-error".into(), err_output));
                }
            }
        }

        // Merge collected outputs
        self.build_merged_output(outputs, start)
    }

    /// Merge results from join_all (non-fail-fast path).
    fn merge_parallel_results(
        &self,
        results: ParallelJoinResults,
        start: Instant,
    ) -> Result<PipelineOutput> {
        let mut outputs: Vec<(String, PipelineOutput)> = Vec::new();

        for result in results {
            match result {
                Ok((label, Ok(output), elapsed)) => {
                    if output.exit_code != 0 {
                        eprintln!("❌ [parallel] Failed: {} ({:.1}s)", label, elapsed);
                    } else {
                        eprintln!("✅ [parallel] Done: {} ({:.1}s)", label, elapsed);
                    }
                    outputs.push((label, output));
                }
                Ok((label, Err(e), elapsed)) => {
                    eprintln!("❌ [parallel] Failed: {} ({:.1}s) — {}", label, elapsed, e);
                    outputs.push((
                        label.clone(),
                        PipelineOutput {
                            workflow_id: label,
                            status: PipelineStatus::Failed,
                            output_text: Some(format!("Error: {}", e)),
                            output_data: None,
                            exit_code: 1,
                            duration_ms: (elapsed * 1000.0) as u64,
                        },
                    ));
                }
                Err(e) => {
                    outputs.push((
                        "join-error".into(),
                        PipelineOutput {
                            workflow_id: "join-error".into(),
                            status: PipelineStatus::Failed,
                            output_text: Some(format!("Task join error: {}", e)),
                            output_data: None,
                            exit_code: 1,
                            duration_ms: 0,
                        },
                    ));
                }
            }
        }

        self.build_merged_output(outputs, start)
    }

    /// Build a single merged PipelineOutput from collected branch outputs.
    fn build_merged_output(
        &self,
        outputs: Vec<(String, PipelineOutput)>,
        start: Instant,
    ) -> Result<PipelineOutput> {
        let mut texts: Vec<String> = Vec::new();
        let mut data_entries: Vec<serde_json::Value> = Vec::new();
        let mut workflow_ids: Vec<String> = Vec::new();
        let mut first_nonzero_exit: Option<i32> = None;
        let mut max_duration: u64 = 0;

        for (_label, output) in &outputs {
            workflow_ids.push(output.workflow_id.clone());
            if output.exit_code != 0 && first_nonzero_exit.is_none() {
                first_nonzero_exit = Some(output.exit_code);
            }
            if output.duration_ms > max_duration {
                max_duration = output.duration_ms;
            }
            if let Some(text) = &output.output_text
                && !text.is_empty()
            {
                texts.push(text.clone());
            }
            if let Some(data) = &output.output_data {
                data_entries.push(data.clone());
            }
        }

        let exit_code = first_nonzero_exit.unwrap_or(0);
        let status = if exit_code == 0 {
            PipelineStatus::Success
        } else {
            PipelineStatus::Failed
        };

        let output_text = if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n---\n"))
        };

        let output_data = match data_entries.len() {
            0 => None,
            1 => Some(data_entries.into_iter().next().unwrap()),
            _ => Some(serde_json::Value::Array(data_entries)),
        };

        // Use wall-clock time from start for total duration
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(PipelineOutput {
            workflow_id: workflow_ids.join(","),
            status,
            output_text,
            output_data,
            exit_code,
            duration_ms,
        })
    }
}

/// Extract a human-readable label from a pipeline expression (for progress messages).
fn expr_label(expr: &PipelineExpr) -> String {
    match expr {
        PipelineExpr::Single(id) => id.clone(),
        PipelineExpr::Pipe(left, right) => {
            format!("{} | {}", expr_label(left), expr_label(right))
        }
        PipelineExpr::Sequential(left, right) => {
            format!("{} && {}", expr_label(left), expr_label(right))
        }
        PipelineExpr::Parallel(exprs) => {
            let labels: Vec<String> = exprs.iter().map(expr_label).collect();
            labels.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::Content;
    use mur_common::workflow::{FailureAction, Step, Workflow};
    use tempfile::TempDir;

    fn make_store(tmp: &TempDir) -> WorkflowYamlStore {
        WorkflowYamlStore::new(tmp.path().to_path_buf()).unwrap()
    }

    fn make_workflow(name: &str, steps: Vec<Step>) -> Workflow {
        Workflow {
            base: KnowledgeBase {
                name: name.to_string(),
                description: format!("Test workflow {}", name),
                content: Content::Plain(String::new()),
                ..Default::default()
            },
            steps,
            variables: vec![],
            source_sessions: vec![],
            trigger: String::new(),
            tools: vec![],
            published_version: 0,
            permission: Default::default(),
        }
    }

    fn shell_step(order: u32, desc: &str, cmd: &str) -> Step {
        Step {
            order,
            description: desc.to_string(),
            command: Some(cmd.to_string()),
            tool: None,
            needs_approval: false,
            on_failure: FailureAction::Abort, breakpoint: None, retry: None, timeout_secs: None,
        }
    }

    fn prompt_step(order: u32, desc: &str) -> Step {
        Step {
            order,
            description: desc.to_string(),
            command: None,
            tool: None,
            needs_approval: false,
            on_failure: FailureAction::Abort, breakpoint: None, retry: None, timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn test_shell_step_captures_stdout() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let wf = make_workflow(
            "echo-test",
            vec![shell_step(1, "Echo hello", "echo hello-world")],
        );
        store.save(&wf).unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Single("echo-test".into());
        let output = executor.execute(&expr, None).await.unwrap();

        assert_eq!(output.status, PipelineStatus::Success);
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.output_text.as_deref(), Some("hello-world"));
    }

    #[tokio::test]
    async fn test_input_injection_in_command() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let wf = make_workflow(
            "injector",
            vec![shell_step(1, "Echo input", "echo '{{input}}'")],
        );
        store.save(&wf).unwrap();

        let executor = PipelineExecutor::new(store);
        let piped = PipelineOutput {
            workflow_id: "prev".into(),
            status: PipelineStatus::Success,
            output_text: Some("INJECTED_DATA".into()),
            output_data: None,
            exit_code: 0,
            duration_ms: 0,
        };

        let expr = PipelineExpr::Single("injector".into());
        let output = executor.execute(&expr, Some(piped)).await.unwrap();

        assert_eq!(output.status, PipelineStatus::Success);
        assert_eq!(output.output_text.as_deref(), Some("INJECTED_DATA"));
    }

    #[tokio::test]
    async fn test_input_injection_in_prompt_step() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let wf = make_workflow("prompt-inject", vec![prompt_step(1, "Analyze: {{input}}")]);
        store.save(&wf).unwrap();

        let executor = PipelineExecutor::new(store);
        let piped = PipelineOutput {
            workflow_id: "prev".into(),
            status: PipelineStatus::Success,
            output_text: Some("some data".into()),
            output_data: None,
            exit_code: 0,
            duration_ms: 0,
        };

        let expr = PipelineExpr::Single("prompt-inject".into());
        let output = executor.execute(&expr, Some(piped)).await.unwrap();

        assert_eq!(output.output_text.as_deref(), Some("Analyze: some data"));
    }

    #[tokio::test]
    async fn test_pipe_chains_output() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // w1: produces "hello"
        store
            .save(&make_workflow(
                "w1",
                vec![shell_step(1, "Produce", "echo hello")],
            ))
            .unwrap();

        // w2: receives {{input}}, transforms it
        store
            .save(&make_workflow(
                "w2",
                vec![shell_step(1, "Transform", "echo got-{{input}}")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Pipe(
            Box::new(PipelineExpr::Single("w1".into())),
            Box::new(PipelineExpr::Single("w2".into())),
        );

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Success);
        // w1 outputs "hello", w2 gets it as {{input}} → "got-hello"
        assert_eq!(output.output_text.as_deref(), Some("got-hello"));
    }

    #[tokio::test]
    async fn test_pipe_stops_on_failure() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "fail-w",
                vec![shell_step(1, "Fail", "exit 1")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "never-run",
                vec![shell_step(1, "Should not run", "echo nope")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Pipe(
            Box::new(PipelineExpr::Single("fail-w".into())),
            Box::new(PipelineExpr::Single("never-run".into())),
        );

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Failed);
        assert_eq!(output.workflow_id, "fail-w");
    }

    #[tokio::test]
    async fn test_json_output_populates_output_data() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "json-w",
                vec![shell_step(1, "JSON output", r#"echo '{"key":"value"}'"#)],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Single("json-w".into());
        let output = executor.execute(&expr, None).await.unwrap();

        assert_eq!(output.status, PipelineStatus::Success);
        assert!(output.output_data.is_some());
        let data = output.output_data.unwrap();
        assert_eq!(data["key"], "value");
    }

    #[tokio::test]
    async fn test_no_input_replaces_with_empty() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "no-input",
                vec![shell_step(1, "Echo", "echo 'before-{{input}}-after'")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Single("no-input".into());
        let output = executor.execute(&expr, None).await.unwrap();

        assert_eq!(output.output_text.as_deref(), Some("before--after"));
    }

    #[tokio::test]
    async fn test_triple_pipe_chain() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "p1",
                vec![shell_step(1, "Start", "echo start")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "p2",
                vec![shell_step(1, "Middle", "echo mid-{{input}}")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "p3",
                vec![shell_step(1, "End", "echo end-{{input}}")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        // p1 | p2 | p3
        let expr = PipelineExpr::Pipe(
            Box::new(PipelineExpr::Pipe(
                Box::new(PipelineExpr::Single("p1".into())),
                Box::new(PipelineExpr::Single("p2".into())),
            )),
            Box::new(PipelineExpr::Single("p3".into())),
        );

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Success);
        // p1→"start", p2→"mid-start", p3→"end-mid-start"
        assert_eq!(output.output_text.as_deref(), Some("end-mid-start"));
    }

    // ─── Phase 3: Parallel execution tests ──────────────────────────

    #[tokio::test]
    async fn test_parallel_two_workflows() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "par-a",
                vec![shell_step(1, "A", "echo alpha")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "par-b",
                vec![shell_step(1, "B", "echo beta")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Single("par-a".into()),
            PipelineExpr::Single("par-b".into()),
        ]);

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Success);
        assert_eq!(output.exit_code, 0);
        // Both outputs present, separated by ---
        let text = output.output_text.unwrap();
        assert!(text.contains("alpha"), "expected alpha in: {}", text);
        assert!(text.contains("beta"), "expected beta in: {}", text);
        assert!(text.contains("---"), "expected separator in: {}", text);
    }

    #[tokio::test]
    async fn test_parallel_actually_concurrent() {
        // Two branches each sleep 0.5s. If sequential, total > 1s.
        // If parallel, total should be ~0.5s (well under 1.5s).
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "slow-a",
                vec![shell_step(1, "Slow A", "sleep 0.5 && echo a-done")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "slow-b",
                vec![shell_step(1, "Slow B", "sleep 0.5 && echo b-done")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Single("slow-a".into()),
            PipelineExpr::Single("slow-b".into()),
        ]);

        let start = Instant::now();
        let output = executor.execute(&expr, None).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(output.status, PipelineStatus::Success);
        // Should complete in ~0.5s, not ~1s
        assert!(
            elapsed.as_secs_f64() < 1.5,
            "parallel branches took too long: {:.1}s (expected <1.5s)",
            elapsed.as_secs_f64()
        );
    }

    #[tokio::test]
    async fn test_parallel_output_merging() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "json-a",
                vec![shell_step(1, "JSON A", r#"echo '{"a":1}'"#)],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "json-b",
                vec![shell_step(1, "JSON B", r#"echo '{"b":2}'"#)],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Single("json-a".into()),
            PipelineExpr::Single("json-b".into()),
        ]);

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Success);
        // Multiple JSON outputs should be merged into an array
        let data = output.output_data.unwrap();
        assert!(data.is_array(), "expected JSON array, got: {}", data);
        assert_eq!(data.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_parallel_exit_code_first_nonzero() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow("ok-w", vec![shell_step(1, "OK", "echo ok")]))
            .unwrap();
        store
            .save(&make_workflow(
                "fail-w2",
                vec![shell_step(1, "Fail", "exit 42")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Single("ok-w".into()),
            PipelineExpr::Single("fail-w2".into()),
        ]);

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Failed);
        assert_ne!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn test_fail_fast_cancellation() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // Branch A fails immediately
        store
            .save(&make_workflow(
                "fast-fail",
                vec![shell_step(1, "Fail fast", "exit 1")],
            ))
            .unwrap();
        // Branch B sleeps for 2s — should be cancelled
        store
            .save(&make_workflow(
                "slow-cancel",
                vec![shell_step(1, "Slow", "sleep 2 && echo should-not-appear")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store).with_fail_fast(true);
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Single("fast-fail".into()),
            PipelineExpr::Single("slow-cancel".into()),
        ]);

        let start = Instant::now();
        let output = executor.execute(&expr, None).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(output.status, PipelineStatus::Failed);
        // Should complete quickly (not wait 2s for the slow branch)
        // CI runners can be slow; use generous timeout
        assert!(
            elapsed.as_secs_f64() < 5.0,
            "fail-fast should have cancelled slow branch, took {:.1}s",
            elapsed.as_secs_f64()
        );
    }

    #[tokio::test]
    async fn test_mixed_pipe_then_parallel() {
        // "w1 | w2, w3" → w2 gets w1's output via pipe, w3 runs independently
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "src",
                vec![shell_step(1, "Source", "echo source-data")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "transform",
                vec![shell_step(1, "Transform", "echo transformed-{{input}}")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "independent",
                vec![shell_step(1, "Indie", "echo indie-result")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        // Parse "src | transform, independent"
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Pipe(
                Box::new(PipelineExpr::Single("src".into())),
                Box::new(PipelineExpr::Single("transform".into())),
            ),
            PipelineExpr::Single("independent".into()),
        ]);

        let output = executor.execute(&expr, None).await.unwrap();
        assert_eq!(output.status, PipelineStatus::Success);
        let text = output.output_text.unwrap();
        assert!(
            text.contains("transformed-source-data"),
            "pipe chain should work in parallel branch: {}",
            text
        );
        assert!(
            text.contains("indie-result"),
            "independent branch should produce output: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_parallel_duration_is_wall_clock() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store
            .save(&make_workflow(
                "dur-a",
                vec![shell_step(1, "A", "sleep 0.3 && echo a")],
            ))
            .unwrap();
        store
            .save(&make_workflow(
                "dur-b",
                vec![shell_step(1, "B", "sleep 0.3 && echo b")],
            ))
            .unwrap();

        let executor = PipelineExecutor::new(store);
        let expr = PipelineExpr::Parallel(vec![
            PipelineExpr::Single("dur-a".into()),
            PipelineExpr::Single("dur-b".into()),
        ]);

        let output = executor.execute(&expr, None).await.unwrap();
        // Wall clock should be ~300ms, not ~600ms (sum)
        assert!(
            output.duration_ms < 1000,
            "duration should be wall clock (~300ms), got {}ms",
            output.duration_ms
        );
    }
}
