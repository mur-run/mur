//! Rule 7: outbound message containing a credential is dropped.
//!
//! Field-name note: `MessagePatch` exposes a `drop: bool` flag plus a
//! `drop_reason: Option<String>` for telemetry. We assert against
//! `drop_reason.is_some()` (the plan's `patch.drop.is_some()` line was
//! pseudocode — the real field is `drop_reason`).

use mur_agent_runtime::hooks::{B0SafetyHook, Hook, HookCtx, OutboundView};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn outbound_with_openai_key_is_dropped() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let view = OutboundView {
        recipient: Some("peer".into()),
        body: "here is my OpenAI key: sk-abcd1234567890efghij1234".into(),
        locale: None,
    };
    let cancel = CancellationToken::new();
    let patch = hook.on_message_send(&ctx, &view, &cancel).await.unwrap();
    assert!(
        patch.drop,
        "expected drop=true on credential-containing body; got {patch:?}",
    );
    assert!(
        patch.drop_reason.is_some(),
        "expected drop_reason on credential-containing body; got {patch:?}",
    );
    let reason = patch.drop_reason.as_ref().unwrap();
    assert!(
        reason.contains("openai_key") || reason.contains("secret"),
        "got {reason}"
    );
}

#[tokio::test]
async fn clean_outbound_message_passes_through() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let view = OutboundView {
        recipient: Some("peer".into()),
        body: "hi friend, did you see today's weather?".into(),
        locale: None,
    };
    let cancel = CancellationToken::new();
    let patch = hook.on_message_send(&ctx, &view, &cancel).await.unwrap();
    assert!(!patch.drop, "clean message should pass; got {patch:?}");
    assert!(
        patch.drop_reason.is_none(),
        "clean message should pass; got {patch:?}"
    );
}
