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
            fired_skills: vec![],
        })
        .await;
    writer.flush().await;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let file_path = tmp.path().join(format!("{today}.jsonl"));

    // macOS CI fs can be slow; retry the read a few times.
    let contents = {
        let mut attempts = 0;
        loop {
            match std::fs::read_to_string(&file_path) {
                Ok(c) if !c.is_empty() => break c,
                _ if attempts < 10 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                _ => panic!(
                    "telemetry file missing or empty after {attempts} retries: {}",
                    file_path.display()
                ),
            }
        }
    };
    assert!(
        contents.contains("\"gen_ai.request.model\":\"llama3.2\""),
        "expected gen_ai.request.model in telemetry file, got:\n{contents}"
    );
    assert!(
        contents.contains("\"mur.agent.name\":\"agent_a\""),
        "expected mur.agent.name in telemetry file, got:\n{contents}"
    );

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
