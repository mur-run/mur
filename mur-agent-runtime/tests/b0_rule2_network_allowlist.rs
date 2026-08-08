//! Rule 2: outbound network allowlist + GrantStore consumption.
//! New host → AskUser; existing grant → silent allow; existing deny → bail.

use mur_agent_runtime::hooks::{AskDefault, B0SafetyHook, Decision, Hook, HookCtx, ToolCall};
use mur_common::agent::{
    Entitlements, FilesystemEntitlement, InboundNetwork, LimitsEntitlement, NetworkEntitlement,
    NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, ResolveDnsConfig, SpawnEntitlement,
    SpawnMode, SyscallsEntitlement,
};
use mur_common::permissions::{Grant, GrantDecision, GrantSource, GrantStore, ScopeKey};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// `for_test_with_entitlements` fills `agent_uuid` with this constant; the
/// scope_key written into a pre-populated GrantStore must match it for the
/// lookup to land.
const TEST_AGENT_UUID: &str = "00000000-0000-0000-0000-000000000000";

fn ent_with_outbound(mode: NetworkOutboundMode, allow: Vec<String>) -> Entitlements {
    Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode,
                allow_hosts: allow,
                protocols: vec!["tcp".to_string()],
                resolve_dns: ResolveDnsConfig::default(),
            },
        },
        filesystem: FilesystemEntitlement::default(),
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement {
                mode: SpawnMode::Allowlist,
                allowed: vec![],
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

const NET_TOOL: &str = "network.http_get";

fn net_call(host: &str) -> ToolCall {
    ToolCall::test(
        NET_TOOL,
        json!({"url": format!("https://{host}/v1/models")}),
    )
}

#[tokio::test]
async fn host_in_allowlist_is_allowed_silently() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_outbound(
            NetworkOutboundMode::Restricted,
            vec!["api.openai.com".into()],
        ),
    );
    let cancel = CancellationToken::new();
    let decision = hook
        .pre_tool_use(&ctx, &net_call("api.openai.com"), &cancel)
        .await
        .unwrap();
    assert!(matches!(decision, Decision::Allow), "got {decision:?}");
}

#[tokio::test]
async fn new_host_triggers_ask_user() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_outbound(
            NetworkOutboundMode::Restricted,
            vec!["api.openai.com".into()],
        ),
    );
    let cancel = CancellationToken::new();
    let decision = hook
        .pre_tool_use(&ctx, &net_call("evil.example.com"), &cancel)
        .await
        .unwrap();
    match decision {
        Decision::AskUser {
            default, prompt, ..
        } => {
            assert!(matches!(default, AskDefault::Deny));
            assert!(prompt.contains("evil.example.com"));
        }
        other => panic!("expected AskUser for new host, got {other:?}"),
    }
}

#[tokio::test]
async fn existing_grant_skips_prompt() {
    let dir = TempDir::new().unwrap();
    // Pre-populate a grant for the new host.
    let mut store = GrantStore::new(dir.path());
    let scope = ScopeKey {
        agent_id: TEST_AGENT_UUID.into(),
        tool_name: "network_outbound::evil.example.com".into(),
        input_schema_hash: String::new(),
    };
    store
        .insert(Grant {
            scope_key: scope,
            decision: GrantDecision::Allow,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            last_used_at: None,
            source: GrantSource::Ui,
            source_audit_id: None,
        })
        .unwrap();

    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_outbound(
            NetworkOutboundMode::Restricted,
            vec!["api.openai.com".into()],
        ),
    );
    let cancel = CancellationToken::new();
    let decision = hook
        .pre_tool_use(&ctx, &net_call("evil.example.com"), &cancel)
        .await
        .unwrap();
    assert!(
        matches!(decision, Decision::Allow),
        "stored Allow grant should silently allow; got {decision:?}",
    );
}

#[tokio::test]
async fn existing_deny_grant_bails() {
    let dir = TempDir::new().unwrap();
    let mut store = GrantStore::new(dir.path());
    let scope = ScopeKey {
        agent_id: TEST_AGENT_UUID.into(),
        tool_name: "network_outbound::evil.example.com".into(),
        input_schema_hash: String::new(),
    };
    store
        .insert(Grant {
            scope_key: scope,
            decision: GrantDecision::Deny,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            last_used_at: None,
            source: GrantSource::Ui,
            source_audit_id: None,
        })
        .unwrap();

    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_outbound(NetworkOutboundMode::Restricted, vec![]),
    );
    let cancel = CancellationToken::new();
    let decision = hook
        .pre_tool_use(&ctx, &net_call("evil.example.com"), &cancel)
        .await
        .unwrap();
    assert!(
        matches!(decision, Decision::Deny { .. }),
        "got {decision:?}"
    );
}

#[tokio::test]
async fn unrestricted_mode_allows_everything() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_outbound(NetworkOutboundMode::Unrestricted, vec![]),
    );
    let cancel = CancellationToken::new();
    let decision = hook
        .pre_tool_use(&ctx, &net_call("anywhere.com"), &cancel)
        .await
        .unwrap();
    assert!(matches!(decision, Decision::Allow), "got {decision:?}");
}

#[tokio::test]
async fn off_mode_denies_everything() {
    let dir = TempDir::new().unwrap();
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_entitlements(
        dir.path().to_path_buf(),
        1,
        ent_with_outbound(NetworkOutboundMode::Off, vec!["api.openai.com".into()]),
    );
    let cancel = CancellationToken::new();
    let decision = hook
        .pre_tool_use(&ctx, &net_call("api.openai.com"), &cancel)
        .await
        .unwrap();
    assert!(
        matches!(decision, Decision::Deny { .. }),
        "got {decision:?}"
    );
}
