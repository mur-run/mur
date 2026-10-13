//! #1813: a production-wired `TaskRunner` leaves a per-call usage line in the
//! agent's telemetry file, labelled with the provider it was built for.

use httpmock::prelude::*;
use mur_agent_runtime::llm::RequestIntent;
use mur_agent_runtime::llm::ollama::OllamaClient;
use mur_agent_runtime::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use mur_agent_runtime::telemetry_writer::TelemetryWriter;
use mur_common::a2a::{Message, MessagePart};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

fn spec() -> TaskSpec {
    TaskSpec {
        cwd: None,
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
        attended: true,
        deadline_secs: None,
    }
}

#[tokio::test]
async fn a_model_call_lands_in_the_telemetry_file_with_its_provider() {
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

    let tmp = TempDir::new().unwrap();
    let (writer, _notif_rx) =
        TelemetryWriter::new(tmp.path().to_path_buf(), "agent_a".into(), "uuid-x".into())
            .await
            .unwrap();

    // The same call `build_provider_runner` makes: the writer's own sender,
    // plus the provider id from the registry entry.
    let client = Arc::new(OllamaClient::new(server.base_url(), "llama3.2".into()));
    let runner = TaskRunner::with_llm(client).with_llm_telemetry(Some(writer.sender()), "my-gw");
    match runner.run_sync(spec()).await {
        TaskOutcome::Completed(_) => {}
        other => panic!("expected Completed, got {other:?}"),
    }
    writer.flush().await;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let path = tmp.path().join(format!("{today}.jsonl"));
    let mut line = None;
    for _ in 0..20 {
        if let Ok(c) = std::fs::read_to_string(&path) {
            line = c
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find(|v| v.to_string().contains("gen_ai.usage.input_tokens"));
            if line.is_some() {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let line = line.unwrap_or_else(|| panic!("no LlmCall line in {}", path.display()));
    let text = line.to_string();
    assert!(
        text.contains("\"gen_ai.provider.name\":\"my-gw\""),
        "{text}"
    );
    assert!(text.contains("\"gen_ai.usage.input_tokens\":5"), "{text}");
    assert!(text.contains("\"gen_ai.usage.output_tokens\":3"), "{text}");
    assert!(
        !text.contains("cost_usd"),
        "no price is known, so the key must be absent, not 0.0: {text}"
    );
}
