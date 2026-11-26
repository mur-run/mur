//! #2587: a delegated turn that has an exec tool refused loses every exec
//! tool for the rest of the turn, not just the one name it called.
use super::*;
use mur_common::hitl::floor_waiver::FloorWaiver;

/// A non-bash tool that counts how often it actually ran.
struct Counting {
    name: &'static str,
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for Counting {
    fn name(&self) -> &str {
        self.name
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: self.name.into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok("ran".to_string().into())
    }
}

fn call(call_id: &str, tool: &str) -> crate::llm::LlmResponse {
    let mut r = tool_call_response(call_id, "rm -rf scratch");
    r.tool_calls[0].tool_name = tool.into();
    r
}

/// Runs a delegated turn: `bash` (refused: Ask, nobody to answer), then
/// `second`, which policy would otherwise `Allow`. Returns how often
/// `second` ran.
async fn second_ran_after_bash_refused(second: &'static str) -> u64 {
    second_ran(second, vec![call("c-1", "bash"), call("c-2", second)]).await
}

/// Same as `second_ran_after_bash_refused`, for a browser tool. Its `allow`
/// rule waives the built-in floor whole, so the ONLY thing that can stop it
/// is the exec-class withdrawal — without the waiver the floor would refuse
/// it on its own and the test would prove nothing.
async fn browser_ran_after_bash_refused(second: &'static str) -> u64 {
    let responses = vec![call("c-1", "bash"), call("c-2", second)];
    second_ran_with(second, responses, FloorWaiver::All).await
}

/// `browser_ran_after_bash_refused`, with both calls in one response.
async fn browser_ran_in_same_batch_as_refused_bash(second: &'static str) -> u64 {
    let mut batch = call("c-1", "bash");
    batch.tool_calls.extend(call("c-2", second).tool_calls);
    second_ran_with(second, vec![batch], FloorWaiver::All).await
}

/// Like `second_ran_after_bash_refused`, but `bash` and `second` arrive in
/// ONE response, so the withdrawal must land before the next call of the
/// same batch runs.
async fn second_ran_in_same_batch_as_refused_bash(second: &'static str) -> u64 {
    let mut batch = call("c-1", "bash");
    batch.tool_calls.extend(call("c-2", second).tool_calls);
    second_ran(second, vec![batch]).await
}

async fn second_ran(second: &'static str, responses: Vec<crate::llm::LlmResponse>) -> u64 {
    second_ran_with(second, responses, FloorWaiver::Off).await
}

async fn second_ran_with(
    second: &'static str,
    mut responses: Vec<crate::llm::LlmResponse>,
    waiver: FloorWaiver,
) -> u64 {
    use crate::llm::stub::SequenceLlm;
    responses.push(end_turn_response("done"));
    let bash_calls = Arc::new(AtomicU64::new(0));
    let second_calls = Arc::new(AtomicU64::new(0));
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
        .with_tools(vec![
            Arc::new(CountingBashTool {
                calls: bash_calls.clone(),
                seen_task: Default::default(),
            }),
            Arc::new(Counting {
                name: second,
                calls: second_calls.clone(),
            }),
        ])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: second.into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: waiver,
        }])
        .with_pending_approvals(empty_pending_approvals())
        .with_sandbox_enforcing(true)
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_hitl_timeout_secs(30)
        .with_iteration_ceiling(4);
    runner.mark_unattended("t-exec").await;
    runner.mark_delegated("t-exec").await;
    let mut spec = loop_spec("exec-withdrawal");
    spec.task_id = Some("t-exec".into());
    spec.attended = false;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Arc::new(runner).run_sync(spec),
    )
    .await
    .expect("a delegated turn must never wait on the HITL timeout");
    assert!(matches!(outcome, TaskOutcome::Completed(_)), "{outcome:?}");
    assert_eq!(bash_calls.load(Ordering::Relaxed), 0, "bash was refused");
    second_calls.load(Ordering::Relaxed)
}

#[tokio::test]
async fn a_refused_bash_also_withdraws_start_process() {
    assert_eq!(
        second_ran_after_bash_refused("mcp__dc__start_process").await,
        0,
        "a sibling exec tool must not run after bash was refused"
    );
}

#[tokio::test]
async fn a_refused_bash_leaves_non_exec_tools_alone() {
    assert_eq!(
        second_ran_after_bash_refused("mcp__dc__read_file").await,
        1,
        "only exec tools are withdrawn as a class"
    );
}

#[tokio::test]
async fn a_refused_bash_withdraws_start_process_in_the_same_response() {
    assert_eq!(
        second_ran_in_same_batch_as_refused_bash("mcp__dc__start_process").await,
        0,
        "a sibling exec tool in the same batch must not run after bash was refused"
    );
}

#[tokio::test]
async fn same_response_withdrawal_leaves_non_exec_tools_alone() {
    assert_eq!(
        second_ran_in_same_batch_as_refused_bash("mcp__dc__read_file").await,
        1,
        "only exec tools are withdrawn as a class"
    );
}

#[tokio::test]
async fn a_refused_bash_also_withdraws_browser_run_code_unsafe() {
    assert_eq!(
        browser_ran_after_bash_refused("mcp__browser__browser_run_code_unsafe").await,
        0,
        "browser_run_code_unsafe runs arbitrary code: it must not run after bash was refused"
    );
}

#[tokio::test]
async fn a_refused_bash_withdraws_browser_run_code_unsafe_in_the_same_response() {
    assert_eq!(
        browser_ran_in_same_batch_as_refused_bash("mcp__browser__browser_run_code_unsafe").await,
        0,
        "browser_run_code_unsafe in the same batch must not run after bash was refused"
    );
}

#[tokio::test]
async fn a_refused_bash_leaves_ordinary_browser_tools_alone() {
    assert_eq!(
        browser_ran_after_bash_refused("mcp__browser__browser_click").await,
        1,
        "a page action is not an exec tool"
    );
    assert_eq!(
        browser_ran_in_same_batch_as_refused_bash("mcp__browser__browser_click").await,
        1,
        "a page action in the same batch is not an exec tool"
    );
}
