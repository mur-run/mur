//! M3.8.2: B0SafetyHook denies side-effect tools (delete / spawn /
//! send / egress / network / .write / .publish) on the same turn after
//! an untrusted input arrived.

use mur_agent_runtime::hooks::{B0SafetyHook, Decision, Hook, HookCtx, ToolCall};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn b0_denies_send_tool_after_untrusted_input() {
    let ctx = HookCtx::for_test_with_turn_flags(vec!["after_untrusted_input".into()]);
    let hook = B0SafetyHook::new();
    let call = ToolCall::test("messaging.send", serde_json::json!({"body": "hi"}));
    let tok = CancellationToken::new();
    match hook.pre_tool_use(&ctx, &call, &tok).await.unwrap() {
        Decision::AskUser { scope_key, .. } => {
            assert!(
                scope_key.tool_name.contains("after_untrusted_input"),
                "scope key carries rule tag: {}",
                scope_key.tool_name
            );
            assert!(
                scope_key.tool_name.contains("messaging.send"),
                "scope key carries tool name: {}",
                scope_key.tool_name
            );
        }
        other => panic!("expected AskUser, got {other:?}"),
    }
}

#[tokio::test]
async fn b0_denies_delete_tool() {
    let ctx = HookCtx::for_test_with_turn_flags(vec!["after_untrusted_input".into()]);
    let hook = B0SafetyHook::new();
    let call = ToolCall::test("fs.delete", serde_json::json!({}));
    let tok = CancellationToken::new();
    assert!(matches!(
        hook.pre_tool_use(&ctx, &call, &tok).await.unwrap(),
        Decision::AskUser { .. }
    ));
}

#[tokio::test]
async fn b0_allows_read_tool_after_untrusted_input() {
    let ctx = HookCtx::for_test_with_turn_flags(vec!["after_untrusted_input".into()]);
    let hook = B0SafetyHook::new();
    let call = ToolCall::test("fs.read", serde_json::json!({"path": "/tmp/x"}));
    let tok = CancellationToken::new();
    assert!(matches!(
        hook.pre_tool_use(&ctx, &call, &tok).await.unwrap(),
        Decision::Allow
    ));
}

#[tokio::test]
async fn b0_allows_send_when_no_untrusted_flag() {
    let ctx = HookCtx::for_test_with_turn_flags(vec![]); // no flag
    let hook = B0SafetyHook::new();
    let call = ToolCall::test("messaging.send", serde_json::json!({}));
    let tok = CancellationToken::new();
    assert!(matches!(
        hook.pre_tool_use(&ctx, &call, &tok).await.unwrap(),
        Decision::Allow
    ));
}
