use mur_agent_runtime::telemetry_writer::{Event, TelemetryWriter};
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn llm_call_event_appends_jsonl_and_emits_notification() {
    let tmp = TempDir::new().unwrap();
    let (writer, mut out_rx) =
        TelemetryWriter::new(tmp.path().to_path_buf(), "agent_a".into(), "uuid-x".into())
            .await
            .unwrap();
    writer
        .emit(Event::LlmCall {
            trace_id: "t1".into(),
            task_id: "task-1".into(),
            model: "llama3.2".into(),
            input_tokens: 100,
            output_tokens: 50,
            latency_ms: 100,
            cost_usd: 0.0,
            provider: "ollama".into(),
        })
        .await;
    writer.flush().await;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let file_path = tmp.path().join(format!("{today}.jsonl"));
    let contents = std::fs::read_to_string(&file_path).unwrap();
    assert!(contents.contains("\"gen_ai.request.model\":\"llama3.2\""));
    assert!(contents.contains("\"mur.agent.name\":\"agent_a\""));

    let notif = out_rx.recv().await.unwrap();
    assert_eq!(notif["method"], json!("telemetry/llm_call"));
    assert_eq!(notif["params"]["mur.task.id"], json!("task-1"));
}

#[tokio::test]
async fn error_event_has_kind_field() {
    let tmp = TempDir::new().unwrap();
    let (writer, mut rx) =
        TelemetryWriter::new(tmp.path().to_path_buf(), "agent_a".into(), "uuid-x".into())
            .await
            .unwrap();
    writer
        .emit(Event::Error {
            kind: "llm_rate_limit".into(),
            message: "429".into(),
            task_id: Some("task-1".into()),
            recoverable: true,
        })
        .await;
    let notif = rx.recv().await.unwrap();
    assert_eq!(notif["params"]["kind"], json!("llm_rate_limit"));
}
