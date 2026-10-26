//! Turn-start MCP tool refresh after a hot reload (spec 2026-10-25 §5, §8):
//! re-list only the edited server, re-apply `ToolRule`s, announce tool-name
//! changes once per generation, and log (not announce) the rest.

use mur_agent_runtime::mcp::pool::McpPool;
use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::tools::ToolExecutor;
use mur_agent_runtime::tools::refresh::{McpRefresh, ReloadView};
use mur_agent_runtime::tools::registry::{McpBackend, build_tools};
use mur_common::agent::{McpServerEntry, ToolRule};
use std::sync::{Arc, Mutex};

fn mock(args: &[&str]) -> McpServerEntry {
    McpServerEntry {
        name: "mock".into(),
        command: env!("CARGO_BIN_EXE_mock_mcp").into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

fn quiet() -> ReloadView {
    ReloadView {
        refusals: Default::default(),
        cold: vec![],
    }
}

fn names(tools: &[Arc<dyn ToolExecutor>]) -> Vec<String> {
    let mut v: Vec<String> = tools
        .iter()
        .map(|t| t.name().to_string())
        .filter(|n| n.starts_with("mcp__"))
        .collect();
    v.sort();
    v
}

struct Rig {
    pool: Arc<McpPool>,
    served: Arc<Mutex<McpServerEntry>>,
    refresh: McpRefresh,
    tools: Vec<Arc<dyn ToolExecutor>>,
}

async fn rig(rules: &[ToolRule]) -> Rig {
    let boot = mock(&[]);
    let pool = McpPool::new(vec![boot.clone()], SandboxPolicy::default(), None);
    let served = Arc::new(Mutex::new(boot.clone()));
    let source = served.clone();
    pool.attach_live(Box::new(move |name| {
        let e = source.lock().unwrap().clone();
        (e.name == name).then_some(e)
    }));
    let (_, map) = build_tools(
        None,
        None,
        None,
        None,
        &[boot],
        rules,
        McpBackend::unrouted(pool.clone()),
    )
    .await;
    let tools: Vec<_> = map.into_values().collect();
    let refresh = McpRefresh::new(pool.clone(), None, "bob".into(), &tools);
    Rig {
        pool,
        served,
        refresh,
        tools,
    }
}

#[tokio::test]
async fn an_unchanged_entry_is_not_relisted_and_says_nothing() {
    let r = rig(&[]).await;
    let out = r.refresh.refresh(&r.tools, &[], quiet).await;
    assert!(out.tools.is_none());
    assert!(out.notices.is_empty(), "{:?}", out.notices);
    r.pool.shutdown().await;
}

#[tokio::test]
async fn a_new_tool_name_is_offered_and_announced_once() {
    let mut r = rig(&[]).await;
    assert_eq!(names(&r.tools), ["mcp__mock__connect", "mcp__mock__echo"]);

    *r.served.lock().unwrap() = mock(&["--extra-tool", "snap"]);
    let out = r.refresh.refresh(&r.tools, &[], quiet).await;
    let tools = out.tools.expect("re-listed");
    assert_eq!(
        names(&tools),
        ["mcp__mock__connect", "mcp__mock__echo", "mcp__mock__snap"]
    );
    assert!(
        tools.iter().any(|t| t.name() == "suggest_replies"),
        "other tools are kept"
    );
    assert_eq!(out.notices, ["MCP `mock` reloaded: added `snap`"]);
    r.tools = tools;

    let again = r.refresh.refresh(&r.tools, &[], quiet).await;
    assert!(again.tools.is_none() && again.notices.is_empty());

    *r.served.lock().unwrap() = mock(&[]);
    let back = r.refresh.refresh(&r.tools, &[], quiet).await;
    assert_eq!(back.notices, ["MCP `mock` reloaded: removed `snap`"]);
    r.pool.shutdown().await;
}

#[tokio::test]
async fn an_args_edit_with_the_same_tools_relists_without_a_notice() {
    let r = rig(&[]).await;
    *r.served.lock().unwrap() = mock(&["--harmless"]);
    let out = r.refresh.refresh(&r.tools, &[], quiet).await;
    assert_eq!(names(&out.tools.expect("re-listed")), names(&r.tools));
    assert!(out.notices.is_empty(), "{:?}", out.notices);
    r.pool.shutdown().await;
}

#[tokio::test]
async fn a_relisted_server_is_filtered_by_the_same_tool_rules() {
    let deny: Vec<ToolRule> = serde_json::from_value(serde_json::json!([
        {"pattern": "mcp__mock__snap", "policy": "deny"}
    ]))
    .unwrap();
    let r = rig(&deny).await;
    *r.served.lock().unwrap() = mock(&["--extra-tool", "snap"]);
    let out = r.refresh.refresh(&r.tools, &deny, quiet).await;
    assert_eq!(
        names(&out.tools.expect("re-listed")),
        ["mcp__mock__connect", "mcp__mock__echo"]
    );
    assert!(out.notices.is_empty(), "a denied tool is not news");
    r.pool.shutdown().await;
}

#[tokio::test]
async fn a_new_config_that_fails_to_start_is_said_once() {
    let r = rig(&[]).await;
    let mut broken = mock(&[]);
    broken.command = "/definitely/not/a/real/mcp-binary".into();
    *r.served.lock().unwrap() = broken;
    let out = r.refresh.refresh(&r.tools, &[], quiet).await;
    assert_eq!(out.notices.len(), 1, "{:?}", out.notices);
    assert!(
        out.notices[0].contains("`mock` new config failed to start"),
        "{}",
        out.notices[0]
    );
    let again = r.refresh.refresh(&r.tools, &[], quiet).await;
    assert!(again.notices.is_empty(), "{:?}", again.notices);
    r.pool.shutdown().await;
}

#[tokio::test]
async fn a_refusal_is_said_once_while_it_stands_and_again_if_it_returns() {
    let r = rig(&[]).await;
    let refused = || ReloadView {
        refusals: [("mock".to_string(), "pin mismatch".to_string())].into(),
        cold: vec![],
    };
    let first = r.refresh.refresh(&r.tools, &[], refused).await;
    assert_eq!(first.notices.len(), 1);
    assert!(first.notices[0].contains("reload refused: pin mismatch"));
    assert!(
        r.refresh
            .refresh(&r.tools, &[], refused)
            .await
            .notices
            .is_empty()
    );
    assert!(
        r.refresh
            .refresh(&r.tools, &[], quiet)
            .await
            .notices
            .is_empty()
    );
    assert_eq!(
        r.refresh
            .refresh(&r.tools, &[], refused)
            .await
            .notices
            .len(),
        1
    );
    r.pool.shutdown().await;
}

/// Records the tool names each request offered.
struct Offered {
    inner: mur_agent_runtime::llm::stub::SequenceLlm,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl mur_agent_runtime::llm::LlmClient for Offered {
    async fn generate(
        &self,
        req: mur_agent_runtime::llm::LlmRequest,
    ) -> Result<mur_agent_runtime::llm::LlmResponse, mur_agent_runtime::llm::LlmError> {
        self.seen
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|d| d.name.clone()).collect());
        self.inner.generate(req).await
    }
    fn model_name(&self) -> &str {
        "offered"
    }
}

fn done(text: &str) -> mur_agent_runtime::llm::LlmResponse {
    mur_agent_runtime::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: text.into(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: mur_agent_runtime::llm::StopReason::EndTurn,
        registry_key: None,
    }
}

fn turn(text: &str) -> mur_agent_runtime::task_runner::TaskSpec {
    use mur_common::a2a::{Message, MessagePart};
    mur_agent_runtime::task_runner::TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: text.into() }],
        },
        context_task_id: None,
        task_id: None,
        active_fleet: None,
        active_team: None,
        intent: mur_agent_runtime::llm::RequestIntent::Interactive,
        output_artifact_path: None,
        cwd: None,
        attended: true,
        deadline_secs: None,
    }
}

fn reply(out: mur_agent_runtime::task_runner::TaskOutcome) -> String {
    use mur_agent_runtime::task_runner::TaskOutcome;
    let TaskOutcome::Completed(task) = out else {
        panic!("expected Completed, got {out:?}");
    };
    task.messages
        .last()
        .into_iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match p {
            mur_common::a2a::MessagePart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The whole path a running agent takes: the same runner, an edited entry,
/// the next turn offers the new tool and its reply says so; the turn after
/// says nothing.
#[tokio::test]
async fn the_next_turn_offers_the_reloaded_tools_and_its_reply_says_so() {
    let r = rig(&[]).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let approvals: mur_agent_runtime::hitl::HitlApprovals =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let runner = mur_agent_runtime::task_runner::TaskRunner::with_llm(Arc::new(Offered {
        inner: mur_agent_runtime::llm::stub::SequenceLlm::new(vec![
            done("one"),
            done("two"),
            done("three"),
        ]),
        seen: seen.clone(),
    }))
    .with_tools(r.tools.clone())
    .with_mcp_refresh(Arc::new(r.refresh))
    .with_pending_approvals(approvals);

    let first = reply(runner.run_sync(turn("one")).await);
    assert!(!first.contains("reloaded"), "{first}");

    *r.served.lock().unwrap() = mock(&["--extra-tool", "snap"]);
    let second = reply(runner.run_sync(turn("two")).await);
    assert!(
        second.contains("MCP `mock` reloaded: added `snap`"),
        "{second}"
    );
    let third = reply(runner.run_sync(turn("three")).await);
    assert!(!third.contains("reloaded"), "{third}");

    let seen = seen.lock().unwrap().clone();
    let snap = "mcp__mock__snap".to_string();
    assert!(!seen[0].contains(&snap), "{:?}", seen[0]);
    assert!(seen[1].contains(&snap), "{:?}", seen[1]);
    assert!(seen[2].contains(&snap), "{:?}", seen[2]);
    r.pool.shutdown().await;
}

/// Spec §8: an edit only a restart applies carries the restart chip, once
/// while it stands; a refusal never does (a restart reads the same profile).
#[tokio::test]
async fn a_restart_notice_asks_for_the_chip_once_and_a_refusal_never_does() {
    let r = rig(&[]).await;
    let cold = || ReloadView {
        refusals: Default::default(),
        cold: vec!["mcp_servers.mock.network.mode".into()],
    };
    let first = r.refresh.refresh(&r.tools, &[], cold).await;
    assert!(first.restart, "{:?}", first.notices);
    assert!(!r.refresh.refresh(&r.tools, &[], cold).await.restart);

    let refused = || ReloadView {
        refusals: [("mock".to_string(), "pin mismatch".to_string())].into(),
        cold: vec![],
    };
    let out = r.refresh.refresh(&r.tools, &[], refused).await;
    assert_eq!(out.notices.len(), 1, "{:?}", out.notices);
    assert!(!out.restart);
    r.pool.shutdown().await;
}
