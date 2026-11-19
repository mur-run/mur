//! #2464: a model that calls an MCP tool by its bare name (`mur_job_status`)
//! got `unknown tool: mur_job_status` and nothing else. Runtime-authored tool
//! descriptions (`fleet_run`, `parallel_jobs`) say "Poll mur_job_status", so
//! the bare name is what the model has been taught — the error has to name
//! the tool that actually exists, or a small model gives up.

use super::*;

struct Named(&'static str);

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: self.0.into(),
            description: "stub".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        Ok("ran".to_string().into())
    }
}

fn runner(names: &[&'static str]) -> TaskRunner {
    TaskRunner::new_stub_echo().with_tools(
        names
            .iter()
            .map(|n| Arc::new(Named(n)) as Arc<dyn crate::tools::ToolExecutor>)
            .collect(),
    )
}

fn call(name: &str) -> crate::llm::ToolCallResult {
    crate::llm::ToolCallResult {
        call_id: "c-1".into(),
        tool_name: name.into(),
        input: serde_json::json!({}),
    }
}

#[tokio::test]
async fn a_bare_mcp_name_is_answered_with_the_real_name() {
    let r = runner(&["mcp__mur-tools__mur_job_status", "bash"]);
    let res = r
        .handle_tool_call("t", &call("mur_job_status"), None, None)
        .await
        .unwrap();
    assert!(res.is_error);
    assert!(
        res.content.contains("mcp__mur-tools__mur_job_status"),
        "the error must name the tool that exists: {}",
        res.content
    );
}

/// Not a resolver: the hint names the candidate, it never runs it. Running
/// it would let a bare name slip past policy rules written for the full one.
#[tokio::test]
async fn the_hint_does_not_run_the_tool() {
    let r = runner(&["mcp__mur-tools__mur_job_status"]);
    let res = r
        .handle_tool_call("t", &call("mur_job_status"), None, None)
        .await
        .unwrap();
    assert!(!res.content.contains("ran"), "{}", res.content);
}

/// Two servers exposing the same bare name: list both, pick neither.
#[tokio::test]
async fn an_ambiguous_bare_name_lists_every_candidate() {
    let r = runner(&["mcp__a__status", "mcp__b__status"]);
    let res = r
        .handle_tool_call("t", &call("status"), None, None)
        .await
        .unwrap();
    assert!(res.content.contains("mcp__a__status"), "{}", res.content);
    assert!(res.content.contains("mcp__b__status"), "{}", res.content);
}

/// A name with no suffix match keeps the plain message.
#[tokio::test]
async fn an_unrelated_unknown_name_gets_no_hint() {
    let r = runner(&["mcp__mur-tools__mur_job_status"]);
    let res = r
        .handle_tool_call("t", &call("nope"), None, None)
        .await
        .unwrap();
    assert_eq!(res.content, "unknown tool: nope");
}
