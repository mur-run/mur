use super::*;

/// Test LLM that records how many times `generate` was called and always
/// emits a tool_use response (so the agentic loop never ends naturally).
struct CountingToolLlm {
    calls: Arc<AtomicU64>,
    input_tokens_per_call: u64,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for CountingToolLlm {
    async fn generate(
        &self,
        _req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(crate::llm::LlmResponse {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            text: String::new(),
            input_tokens: self.input_tokens_per_call,
            output_tokens: 1,
            model: "counting".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: format!("id-{n}"),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": "echo loop"}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        })
    }
    fn model_name(&self) -> &str {
        "counting"
    }
}

/// The PRODUCTION wiring function `build_runner` applies the profile's
/// limits: an unattended turn with a zero deadline stops before its first
/// tool call — one generate() for the graceful summary, none for work.
#[tokio::test]
async fn build_runner_applies_profile_limits() {
    let calls = Arc::new(AtomicU64::new(0));
    let client: Arc<dyn crate::llm::LlmClient> = Arc::new(CountingToolLlm {
        calls: calls.clone(),
        input_tokens_per_call: 1,
    });
    let (notif_tx, _rx) = tokio::sync::mpsc::channel(64);
    let runner = crate::supervisor_runner::build_runner(
        TaskRunner::with_llm(client),
        None,
        Arc::new(RuntimeSkills::build(vec![])),
        SkillsConfig::default(),
        Default::default(),
        None,
        None,
        None,
        Some(empty_pending_approvals()),
        Some(notif_tx),
        1,
        mur_common::hitl::Autonomy::default(),
        vec![],
        vec![],
        (
            mur_common::limits::Limits::default(),
            Some(mur_common::limits::Limits {
                deadline: Some("0s".into()),
                stuck: None,
                cost_usd: None,
            }),
        ),
        None,
        None,
        None,
        String::new(),
        None,
        None,
        None,
        None,
        None,
        true,
    );
    let mut spec = loop_spec("loop");
    spec.attended = false;
    let out = runner.run_sync(spec).await;
    let usage = task_usage(&out);
    assert_eq!(usage["stop_reason"], "deadline", "usage={usage}");
    assert!(
        calls.load(Ordering::Relaxed) <= 1,
        "no work turn may run past an expired deadline"
    );
    // The same profile, attended: the deadline is ignored (§3.2) and the
    // counting LLM runs until the test ceiling.
    let (runner2, spec2) = runner_with_scripted_tool_calls(8, true, "off");
    let usage = task_usage(&runner2.run_sync(spec2).await);
    assert!(
        usage.get("stop_reason").is_none(),
        "attended must end naturally: {usage}"
    );
}

/// A runner whose stub emits `n` distinct `bash` calls then ends the turn.
/// No `bash` tool is registered — every call resolves to the same
/// "unknown tool" result, which is fine: the progress rule keys on the
/// call, and distinct args are distinct calls.
fn runner_with_scripted_tool_calls(
    n: usize,
    attended: bool,
    stuck: &str,
) -> (Arc<TaskRunner>, TaskSpec) {
    use crate::llm::stub::SequenceLlm;
    let mut responses: Vec<crate::llm::LlmResponse> = (0..n)
        .map(|i| tool_call_response(&format!("id-{i}"), &format!("echo step-{i}")))
        .collect();
    responses.push(end_turn_response("DONE"));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(200)
            .with_limits(
                mur_common::limits::Limits {
                    deadline: None,
                    stuck: Some(stuck.into()),
                    cost_usd: None,
                },
                None,
            ),
    );
    let mut spec = loop_spec("scripted");
    spec.attended = attended;
    (runner, spec)
}

/// A tool that takes a moment and answers differently every call, under
/// whatever name the test gives it. Varying output keeps the doom-loop
/// guard (which keys on the result too) out of the way, so what ends the
/// turn is the stuck clock — or nothing.
struct SlowVaryingTool {
    name: String,
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for SlowVaryingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: self.name.clone(),
            description: "slow varying test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(format!("output #{n}").into())
    }
}

/// Like the above, but every turn is the SAME call to `tool` with the same
/// input — the "retrying the same thing" shape — against `SlowVaryingTool`,
/// so ~350 ms passes per iteration and a 1 s stuck window is reachable.
fn runner_with_scripted_tool_calls_repeating(
    tool: &str,
    n: usize,
    attended: bool,
    stuck: &str,
) -> (Arc<TaskRunner>, TaskSpec) {
    use crate::llm::stub::SequenceLlm;
    let mut responses: Vec<crate::llm::LlmResponse> = (0..n)
        .map(|i| crate::llm::LlmResponse {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: format!("same-{i}"),
                tool_name: tool.into(),
                input: serde_json::json!({"path": "x"}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        })
        .collect();
    responses.push(end_turn_response("DONE"));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(SlowVaryingTool {
                name: tool.into(),
                calls: Arc::new(AtomicU64::new(0)),
            })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: tool.into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(200)
            .with_limits(
                mur_common::limits::Limits {
                    deadline: None,
                    stuck: Some(stuck.into()),
                    cost_usd: None,
                },
                None,
            ),
    );
    let mut spec = loop_spec("repeating");
    spec.attended = attended;
    (runner, spec)
}

fn task_usage(out: &TaskOutcome) -> serde_json::Value {
    match out {
        TaskOutcome::Completed(task) => task.usage.clone().unwrap_or_default(),
        other => panic!("expected Completed, got {other:?}"),
    }
}

fn last_agent_text(out: &TaskOutcome) -> String {
    match out {
        TaskOutcome::Completed(task) => task.messages.last().map(text_of).unwrap_or_default(),
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// §7: an attended run passes the old 25-iteration mark without stopping.
#[tokio::test]
async fn attended_run_passes_the_old_iteration_cap() {
    let (runner, spec) = runner_with_scripted_tool_calls(30, true, "off");
    let out = runner.run_sync(spec).await;
    let usage = task_usage(&out);
    assert!(
        usage.get("stop_reason").is_none(),
        "must end naturally: {usage}"
    );
    assert!(last_agent_text(&out).contains("DONE"));
}

/// §7: unattended with a 1 s stuck window and ~350 ms per iteration, the
/// same `bash` call repeated stops with `stuck` (identical calls are not
/// progress) naming the last calls and the remedy; the same shape through
/// `write_file` never stops — a file write is always progress.
#[tokio::test]
async fn unattended_stuck_stops_on_no_progress_and_not_after_a_write() {
    let (runner, spec) = runner_with_scripted_tool_calls_repeating("bash", 6, false, "1s");
    let out = runner.run_sync(spec).await;
    let usage = task_usage(&out);
    assert_eq!(usage["stop_reason"], "stuck", "{usage}");
    let card = last_agent_text(&out);
    assert!(
        card.contains("last calls: bash"),
        "last calls named: {card}"
    );
    assert!(
        card.contains("mur limits") && card.contains("--stuck"),
        "remedy: {card}"
    );

    let (runner, spec) = runner_with_scripted_tool_calls_repeating("write_file", 6, false, "1s");
    let usage = task_usage(&runner.run_sync(spec).await);
    assert!(
        usage.get("stop_reason").is_none(),
        "a write every turn is progress: {usage}"
    );
}

/// §3.2 + §3.7: an unattended turn past its deadline stops with `deadline`
/// and the remedy names the limits command.
#[tokio::test]
async fn unattended_deadline_stops_with_reason_and_remedy() {
    let (runner, mut spec) = runner_with_scripted_tool_calls(50, false, "off");
    spec.deadline_secs = Some(0);
    let out = runner.run_sync(spec).await;
    let usage = task_usage(&out);
    assert_eq!(usage["stop_reason"], "deadline", "{usage}");
    assert!(
        last_agent_text(&out).contains("mur limits"),
        "{}",
        last_agent_text(&out)
    );
}

/// Test 15 — D11 policy aliasing.
#[test]
fn bash_control_tools_inherit_bashs_rule_unless_named() {
    use mur_common::agent::{ToolPolicy, ToolRule};
    let rule = |p: &str, policy| ToolRule {
        pattern: p.into(),
        policy,
        risk: None,
    };
    let allow = vec![rule("bash", ToolPolicy::Allow)];
    assert_eq!(
        effective_tool_policy(&allow, "bash_wait"),
        ToolPolicy::Allow
    );
    assert_eq!(
        effective_tool_policy(&allow, "bash_kill"),
        ToolPolicy::Allow
    );
    let ask = vec![rule("bash", ToolPolicy::Ask)];
    assert_eq!(effective_tool_policy(&ask, "bash_wait"), ToolPolicy::Ask);
    let mixed = vec![
        rule("bash", ToolPolicy::Allow),
        rule("bash_kill", ToolPolicy::Deny),
    ];
    assert_eq!(
        effective_tool_policy(&mixed, "bash_wait"),
        ToolPolicy::Allow
    );
    assert_eq!(effective_tool_policy(&mixed, "bash_kill"), ToolPolicy::Deny);
    assert_eq!(
        effective_tool_policy(&[], "bash_wait"),
        ToolPolicy::default()
    );
}

fn bash_call(id: &str, command: &str, timeout_secs: u64) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: id.into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": command, "timeout_secs": timeout_secs}),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    }
}

fn runner_with_real_bash(
    responses: Vec<crate::llm::LlmResponse>,
    deadline: Option<&str>,
) -> (Arc<TaskRunner>, Arc<crate::tools::bash_jobs::JobTable>) {
    use crate::llm::stub::SequenceLlm;
    let base = std::env::temp_dir();
    let jobs = crate::tools::bash_jobs::JobTable::new();
    let bash: Arc<dyn crate::tools::ToolExecutor> = Arc::new(
        crate::tools::bash::BashTool::new(
            base.clone(),
            crate::tools::fs_policy::SessionCwd::new(base),
        )
        .with_jobs(jobs.clone()),
    );
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_tools(vec![bash])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "bash".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_bash_jobs(jobs.clone())
            .with_iteration_ceiling(50)
            .with_limits(
                mur_common::limits::Limits {
                    deadline: deadline.map(str::to_string),
                    stuck: Some("off".into()),
                    cost_usd: None,
                },
                None,
            ),
    );
    (runner, jobs)
}

/// Test 12 — an unattended deadline stop ends the task's jobs; an
/// attended turn that ends normally leaves them running.
#[cfg(unix)]
#[tokio::test]
async fn unattended_deadline_kills_the_tasks_jobs_and_attended_does_not() {
    // Unattended: job spawned at once, a 2 s call carries the loop past
    // the 1 s deadline, the next iteration stops and kills the job.
    let (runner, jobs) = runner_with_real_bash(
        vec![
            bash_call("c0", "sleep 30", 0),
            bash_call("c1", "sleep 2", 5),
            end_turn_response("DONE"),
        ],
        Some("1s"),
    );
    let mut spec = loop_spec("deadline");
    spec.attended = false;
    spec.deadline_secs = Some(1);
    let out = runner.run_sync(spec).await;
    assert_eq!(task_usage(&out)["stop_reason"], "deadline");
    assert!(jobs.running_ids().is_empty(), "{:?}", jobs.running_ids());

    // Attended: same script, no deadline applies, the job outlives the turn.
    let (runner, jobs) = runner_with_real_bash(
        vec![bash_call("c0", "sleep 30", 0), end_turn_response("DONE")],
        Some("1s"),
    );
    let mut spec = loop_spec("attended");
    spec.attended = true;
    runner.run_sync(spec).await;
    assert_eq!(
        jobs.running_ids().len(),
        1,
        "an attended turn must not kill its jobs"
    );
    jobs.kill_all().await;
}

/// Test 13 — `tasks/cancel` ends the task's jobs even when the
/// generation is no longer cancellable.
#[cfg(unix)]
#[tokio::test]
async fn cancel_kills_the_tasks_jobs() {
    let (runner, jobs) = runner_with_real_bash(vec![], None);
    let base = std::env::temp_dir();
    let id = crate::tools::bash_jobs::CURRENT_TASK_ID
        .scope("task-c".to_string(), async {
            jobs.spawn(crate::tools::bash_jobs::SpawnSpec {
                command: "sleep 30",
                cwd: &base,
                env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
                spool_dir: &base,
                vault: None,
            })
        })
        .await
        .unwrap();
    let pid = jobs.pid(&id).unwrap();
    let r = runner.cancel("task-c").await;
    assert!(r.is_err(), "nothing registered a cancel signal: {r:?}");
    assert!(
        !crate::tools::bash_jobs::pid_alive(pid),
        "cancel left the job running"
    );
}

/// Test 14 — D4: the stuck fingerprint differs when bytes arrived and
/// repeats when nothing did.
#[test]
fn running_fingerprint_folds_bytes_seen() {
    let input = serde_json::json!({"job_id": "j-1"});
    let fp = |bytes_seen: u64| {
        fingerprint_args(&input) ^ fingerprint_str(&format!("bytes_seen:{bytes_seen}"))
    };
    assert_ne!(fp(10), fp(20));
    assert_eq!(fp(20), fp(20));
    assert_ne!(
        fp(10),
        fingerprint_args(&input),
        "a yield is not the bare call"
    );
}

/// §6: the ceiling is a diagnostic, not a setting — absurd on purpose.
#[test]
fn iteration_ceiling_is_absurd_on_purpose() {
    assert_eq!(ITERATION_CEILING, 10_000);
}
