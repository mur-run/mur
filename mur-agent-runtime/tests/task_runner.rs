use mur_agent_runtime::llm::RequestIntent;
use mur_agent_runtime::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use mur_common::a2a::{Message, MessagePart, TaskState};

#[tokio::test]
async fn sync_task_reaches_completed_state() {
    let runner = TaskRunner::new_stub_echo();
    let spec = TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: "ping".into(),
            }],
        },
        context_task_id: None,
        task_id: None,
        active_fleet: None,
        active_team: None,
        intent: RequestIntent::Interactive,
    };
    let outcome = runner.run_sync(spec).await;
    match outcome {
        TaskOutcome::Completed(task) => {
            assert_eq!(task.state, TaskState::Completed);
            assert!(task.messages.iter().any(|m| m.role == "agent"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn cancellation_transitions_to_cancelled() {
    let runner = TaskRunner::new_stub_slow();
    let spec = TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: "slow".into(),
            }],
        },
        context_task_id: None,
        task_id: None,
        active_fleet: None,
        active_team: None,
        intent: RequestIntent::Interactive,
    };
    let handle = runner.start_async(spec);
    let task_id = handle.task_id().to_string();
    runner.cancel(&task_id).await.unwrap();
    let outcome = handle.await_completion().await;
    assert!(matches!(outcome, TaskOutcome::Cancelled(_)));
}
