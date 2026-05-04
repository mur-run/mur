//! Rule 8 (M7.6): `B0SafetyHook::post_tool_use` redacts PII in
//! `memory.*` tool outputs before the value is persisted.
//!
//! These tests target the hook directly (not the chain) so they isolate
//! the redaction logic from chain-folding semantics. Wiring the chain's
//! `PostToolUsePatch` into the supervisor's persistence path is tracked
//! separately.

use mur_agent_runtime::hooks::{B0SafetyHook, Hook, HookCtx, ToolCall, ToolResult};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn memory_write_email_gets_redacted() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("memory.write", json!({"key": "user.email"}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: serde_json::Value::String("alex@example.com".into()),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    let redacted = patch
        .replace_output
        .as_ref()
        .expect("redaction patch expected");
    assert!(redacted.as_str().unwrap().contains("<REDACTED:email>"));
}

#[tokio::test]
async fn non_memory_tool_passes_through() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("fs.read", json!({"path": "/tmp/x"}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: serde_json::Value::String("alex@example.com".into()),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    assert!(
        patch.replace_output.is_none(),
        "non-memory tool should not be redacted; got {patch:?}",
    );
}

#[tokio::test]
async fn memory_read_clean_text_no_patch() {
    // memory.* tool output that's clean text should not produce a
    // patch — the supervisor should see `replace_output == None` and
    // leave the original output untouched.
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("memory.read", json!({"key": "project.name"}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: serde_json::Value::String("the project will ship next week".into()),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    assert!(
        patch.replace_output.is_none(),
        "clean memory.* output should pass through; got {patch:?}",
    );
}

#[tokio::test]
async fn memory_write_non_string_output_passes_through() {
    // Numeric / object outputs aren't redacted (no string body to scan).
    // We currently only redact `Value::String`; richer types are a
    // follow-up.
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("memory.write", json!({"key": "count"}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: json!({"count": 42}),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    assert!(
        patch.replace_output.is_none(),
        "non-string memory.* output should pass through; got {patch:?}",
    );
}
