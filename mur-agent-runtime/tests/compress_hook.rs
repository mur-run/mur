//! Surface 2: `CompressHook::post_tool_use` auto-compresses oversized tool
//! outputs into a retrieval envelope. Targets the hook directly, mirroring
//! `tests/b0_rule8_memory_redaction.rs`.

use mur_agent_runtime::hooks::{CompressHook, Hook, HookCtx, ToolCall, ToolResult};
use mur_compress::CompressConfig;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn cfg_with_min(min_tokens: usize) -> CompressConfig {
    let mut c = CompressConfig::default();
    c.auto.min_tokens = min_tokens;
    c
}

fn big_output() -> serde_json::Value {
    let results: Vec<_> = (0..3000)
        .map(|i| json!({"file": format!("f{i}.rs"), "score": 0.5}))
        .collect();
    json!({"results": results})
}

#[tokio::test]
async fn large_output_is_compressed_into_envelope() {
    let dir = TempDir::new().unwrap();
    let hook = CompressHook::new(dir.path().join("compress"), cfg_with_min(50));
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("project.search", json!({"query": "item"}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: big_output(),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    let replaced = patch.replace_output.expect("compression patch expected");
    // object output ⇒ dominant array field spliced with an envelope
    assert_eq!(replaced["results"]["compressed"], json!(true));
    assert!(replaced["results"]["hash"].as_str().is_some());
}

#[tokio::test]
async fn small_output_passes_through() {
    let dir = TempDir::new().unwrap();
    let hook = CompressHook::new(dir.path().join("compress"), cfg_with_min(1500));
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("project.search", json!({}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: json!({"results": ["a", "b"]}),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    assert!(
        patch.replace_output.is_none(),
        "small output must not be compressed"
    );
}

#[tokio::test]
async fn disabled_when_agent_runtime_flag_off() {
    let dir = TempDir::new().unwrap();
    let mut cfg = cfg_with_min(50);
    cfg.auto.agent_runtime = false;
    let hook = CompressHook::new(dir.path().join("compress"), cfg);
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("project.search", json!({}));
    let result = ToolResult {
        call_id: "c".into(),
        ok: true,
        output: big_output(),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook
        .post_tool_use(&ctx, &call, &result, &cancel)
        .await
        .unwrap();
    assert!(patch.replace_output.is_none());
}
