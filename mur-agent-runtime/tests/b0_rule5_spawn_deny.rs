//! Rule 5: shell / eval / arbitrary spawn disabled by default; per-agent
//! toggle (entitlements.processes.spawn.mode = "any" or allowlist).

use mur_agent_runtime::hooks::{B0SafetyHook, Decision, Hook, HookCtx, ToolCall};
use mur_common::agent::{
    Entitlements, FilesystemEntitlement, InboundNetwork, LimitsEntitlement, NetworkEntitlement,
    NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, ResolveDnsConfig, SpawnEntitlement,
    SpawnMode, SyscallsEntitlement,
};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn ent_with_spawn(mode: SpawnMode, allowed: Vec<String>) -> Entitlements {
    Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Restricted,
                allow_hosts: vec![],
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

#[tokio::test]
async fn process_spawn_denied_when_allowlist_is_empty() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_spawn(SpawnMode::Allowlist, vec![]),
    );
    let call = ToolCall::test(
        "process.spawn",
        json!({"argv": ["/bin/sh", "-c", "echo hi"]}),
    );
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(
        matches!(decision, Decision::Deny { .. }),
        "got {decision:?}"
    );
}

#[tokio::test]
async fn process_spawn_allowed_when_program_in_allowlist() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_spawn(SpawnMode::Allowlist, vec!["/usr/bin/git".to_string()]),
    );
    let call = ToolCall::test("process.spawn", json!({"argv": ["/usr/bin/git", "status"]}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(matches!(decision, Decision::Allow), "got {decision:?}");
}

#[tokio::test]
async fn process_spawn_unrestricted_when_mode_any() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_spawn(SpawnMode::Any, vec![]),
    );
    let call = ToolCall::test("process.spawn", json!({"argv": ["/bin/sh"]}));
    let cancel = CancellationToken::new();
    let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
    assert!(matches!(decision, Decision::Allow), "got {decision:?}");
}
