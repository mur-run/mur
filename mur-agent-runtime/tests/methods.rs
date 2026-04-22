use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use mur_agent_runtime::protocol::methods::{
    message_send::MessageSendHandler,
    tasks::{TasksCancelHandler, TasksGetHandler, TasksListHandler},
};
use mur_agent_runtime::task_runner::TaskRunner;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn message_send_returns_completed_task() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = MessageSendHandler::new(runner);
    let result = h
        .handle(Some(json!({
            "message": {"role": "user", "parts": [{"kind": "text", "text": "hi"}]}
        })))
        .await
        .unwrap();
    assert_eq!(result["state"], "completed");
}

#[tokio::test]
async fn tasks_get_returns_not_found_for_unknown_id() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksGetHandler::new(runner);
    let err = h
        .handle(Some(json!({"id": "task-ghost"})))
        .await
        .unwrap_err();
    assert_eq!(err.code(), -32000);
}

#[tokio::test]
async fn tasks_cancel_on_unknown_returns_not_found() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksCancelHandler::new(runner);
    let err = h
        .handle(Some(json!({"id": "task-ghost"})))
        .await
        .unwrap_err();
    assert_eq!(err.code(), -32000);
}

#[tokio::test]
async fn tasks_list_returns_array() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksListHandler::new(runner);
    let result = h.handle(None).await.unwrap();
    assert!(result.is_array());
}
