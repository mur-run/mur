use httpmock::prelude::*;
use mur_agent_runtime::llm::RequestIntent;
use mur_agent_runtime::llm::ollama::OllamaClient;
use mur_agent_runtime::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use mur_agent_runtime::telemetry_writer::Event;
use mur_common::a2a::{Message, MessagePart};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn runner_with_llm_generates_and_emits_telemetry() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/api/chat");
            then.status(200).json_body(json!({
                "model": "llama3.2",
                "message": {"role": "assistant", "content": "OK"},
                "prompt_eval_count": 5,
                "eval_count": 3,
                "done": true
            }));
        })
        .await;
    let client = Arc::new(OllamaClient::new(server.base_url(), "llama3.2".into()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let runner = TaskRunner::with_llm(client).with_telemetry(tx);
    let spec = TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "hi".into() }],
        },
        context_task_id: None,
        task_id: None,
        active_fleet: None,
        active_team: None,
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
    };
    let outcome = runner.run_sync(spec).await;
    let task = match outcome {
        TaskOutcome::Completed(t) => t,
        other => panic!("expected Completed, got {other:?}"),
    };
    assert_eq!(task.messages.len(), 2);
    let last = task.messages.last().unwrap();
    assert_eq!(last.role, "agent");
    let ev = rx.try_recv().expect("expected LlmCall telemetry on sink");
    assert!(
        matches!(ev, Event::LlmCall { .. }),
        "expected LlmCall, got {ev:?}"
    );
}
