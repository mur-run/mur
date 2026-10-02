use super::*;
use crate::hooks::{HookCtx, ToolCall};
use mur_common::agent::{
    Entitlements, FilesystemEntitlement, InboundNetwork, LimitsEntitlement, NetworkEntitlement,
    NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, ResolveDnsConfig, SpawnEntitlement,
    SpawnMode, SyscallsEntitlement,
};
use tempfile::TempDir;

#[test]
fn side_effect_classifier_matches_expected_names() {
    for n in [
        "fs.delete",
        "delete_user",
        "process.spawn",
        "spawn_shell",
        "messaging.send",
        "send_email",
        "egress.http",
        "network.http_get",
        "fs.write",
        "feed.publish",
    ] {
        assert!(is_side_effect_tool(n), "{n} should be side-effect");
    }
    for n in ["fs.read", "search.query", "patterns.list", "config.get"] {
        assert!(!is_side_effect_tool(n), "{n} should be allowed");
    }
}

fn ent_with_spawn(mode: SpawnMode, allowed: Vec<String>) -> Entitlements {
    Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Restricted,
                allow_hosts: vec![],
                allow_ports: vec![],
                protocols: vec!["tcp".into()],
                resolve_dns: ResolveDnsConfig::default(),
            },
        },
        filesystem: FilesystemEntitlement::default(),
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement {
                mode,
                allowed,
                allowed_dirs: vec![],
            },
        },
        syscalls: SyscallsEntitlement::default(),
        limits: LimitsEntitlement::default(),
        llm: Default::default(),
        tools: vec![],
        fail_closed_on_sandbox_error: true,
    }
}

// ── Rule 5 coarse gate for `bash` (no argv array to check) ───────────
// `bash` never matched `SPAWN_TOOLS` before this slice, so it fell
// through to Allow unconditionally regardless of entitlements. These
// regression tests pin the fix: `bash` is now gated on `spawn.mode`
// at this layer, with per-binary enforcement left to the OS sandbox.

#[tokio::test]
async fn bash_denied_when_spawn_mode_none() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_spawn(SpawnMode::None, vec![]),
    );
    let call = ToolCall::test("bash", serde_json::json!({"command": "echo hi"}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(
        matches!(decision, Decision::Deny { .. }),
        "got {decision:?}"
    );
}

#[tokio::test]
async fn bash_allowed_when_spawn_mode_allowlist() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_spawn(SpawnMode::Allowlist, vec![]),
    );
    let call = ToolCall::test("bash", serde_json::json!({"command": "echo hi"}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(matches!(decision, Decision::Allow), "got {decision:?}");
}

#[tokio::test]
async fn bash_allowed_when_spawn_mode_any() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_spawn(SpawnMode::Any, vec![]),
    );
    let call = ToolCall::test("bash", serde_json::json!({"command": "echo hi"}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(matches!(decision, Decision::Allow), "got {decision:?}");
}
