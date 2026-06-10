use mur_agent_runtime::protocol::a2a_server::{MethodHandler, RequestContext};
use mur_agent_runtime::protocol::methods::{
    message_send::MessageSendHandler,
    tasks::{TasksCancelHandler, TasksGetHandler, TasksListHandler},
};
use mur_agent_runtime::task_runner::TaskRunner;
use mur_agent_runtime::telemetry_writer::Event;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn message_send_returns_completed_task() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = MessageSendHandler::new(runner);
    let result = h
        .handle(
            Some(json!({
                "message": {"role": "user", "parts": [{"kind": "text", "text": "hi"}]}
            })),
            &RequestContext::none(),
        )
        .await
        .unwrap();
    assert_eq!(result["state"], "completed");
}

#[tokio::test]
async fn tasks_get_returns_not_found_for_unknown_id() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksGetHandler::new(runner);
    let err = h
        .handle(Some(json!({"id": "task-ghost"})), &RequestContext::none())
        .await
        .unwrap_err();
    assert_eq!(err.code(), -32000);
}

#[tokio::test]
async fn tasks_cancel_on_unknown_returns_not_found() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksCancelHandler::new(runner);
    let err = h
        .handle(Some(json!({"id": "task-ghost"})), &RequestContext::none())
        .await
        .unwrap_err();
    assert_eq!(err.code(), -32000);
}

#[tokio::test]
async fn tasks_list_returns_array() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksListHandler::new(runner);
    let result = h.handle(None, &RequestContext::none()).await.unwrap();
    assert!(result.is_array());
}

#[tokio::test]
async fn message_send_emits_task_progress_before_response() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let h = MessageSendHandler::with_progress(runner, tx);
    let result = h
        .handle(
            Some(json!({
                "message": {"role": "user", "parts": [{"kind": "text", "text": "hi"}]}
            })),
            &RequestContext::none(),
        )
        .await
        .unwrap();
    assert_eq!(result["state"], "completed");
    // At least one TaskProgress event must have been emitted.
    let ev = rx
        .try_recv()
        .expect("expected a TaskProgress event on the sink");
    assert!(
        matches!(ev, Event::TaskProgress { .. }),
        "first event should be TaskProgress, got {ev:?}"
    );
}
