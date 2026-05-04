//! Rule 1: pre_tool_use issues AskUser for fs.write outside agent_home.

use mur_agent_runtime::hooks::{AskDefault, B0SafetyHook, Decision, Hook, HookCtx, ToolCall};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn fs_write_inside_agent_home_is_allowed() {
    let agent_home = TempDir::new().unwrap();
    let target = agent_home.path().join("notes.txt");
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(agent_home.path().to_path_buf(), 1);
    let call = ToolCall::test("fs.write", json!({"path": target.display().to_string()}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(matches!(decision, Decision::Allow), "got {:?}", decision);
}

#[tokio::test]
async fn fs_write_outside_agent_home_asks_user() {
    let agent_home = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let target = other.path().join("foreign.txt");
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(agent_home.path().to_path_buf(), 1);
    let call = ToolCall::test("fs.write", json!({"path": target.display().to_string()}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    match decision {
        Decision::AskUser {
            default, prompt, ..
        } => {
            assert!(matches!(default, AskDefault::Deny));
            assert!(prompt.to_lowercase().contains("outside") || prompt.contains("foreign"));
        }
        other => panic!("expected AskUser for outside-home write, got {other:?}"),
    }
}
