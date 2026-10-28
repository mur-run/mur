//! Rule 1 (#2181): `write_file` / `edit_file` into the launch chain or an
//! `fs.deny` root is refused before the approval prompt. Every other write —
//! inside the home or outside it — is left to the tool's own
//! `check_write_entitlement`, so B0 never asks about a write the tool would
//! refuse anyway, and never adds a prompt for one it would allow.

use mur_agent_runtime::hooks::{B0SafetyHook, Decision, Hook, HookCtx, ToolCall};
use mur_common::agent::{Entitlements, FilesystemEntitlement};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// `<tmp>/agents/me` with a sibling `<tmp>/agents/other`.
fn layout() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("agents/me");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(root.join("agents/other")).unwrap();
    (tmp, home)
}

/// The test-default entitlements, with `fs` swapped in.
fn ent_with_fs(fs: FilesystemEntitlement) -> Entitlements {
    let mut ent = HookCtx::for_test_with_home(std::path::PathBuf::new(), 0)
        .entitlements()
        .clone();
    ent.filesystem = fs;
    ent
}

async fn decide(home: &std::path::Path, ent: Entitlements, tool: &str, path: &str) -> Decision {
    let ctx = HookCtx::for_test_with_entitlements(home.to_path_buf(), 1, ent);
    let call = ToolCall::test(tool, json!({ "path": path, "content": "x" }));
    B0SafetyHook::new()
        .pre_tool_use(&ctx, &call, &CancellationToken::new())
        .await
        .unwrap()
}

#[tokio::test]
async fn ordinary_writes_are_allowed_inside_and_outside_home() {
    let (_tmp, home) = layout();
    let elsewhere = TempDir::new().unwrap();
    for tool in ["write_file", "edit_file"] {
        for p in [
            home.join("notes.txt"),
            elsewhere.path().join("project/src/lib.rs"),
        ] {
            let d = decide(
                &home,
                ent_with_fs(FilesystemEntitlement::default()),
                tool,
                &p.display().to_string(),
            )
            .await;
            assert!(
                matches!(d, Decision::Allow),
                "{tool} {}: {d:?}",
                p.display()
            );
        }
    }
}

#[tokio::test]
async fn another_agents_profile_is_denied_as_launch_chain() {
    let (_tmp, home) = layout();
    let target = home.parent().unwrap().join("other/profile.yaml");
    for tool in ["write_file", "edit_file"] {
        match decide(
            &home,
            ent_with_fs(FilesystemEntitlement::default()),
            tool,
            &target.display().to_string(),
        )
        .await
        {
            Decision::Deny { reason } => {
                assert!(reason.starts_with("[launch_chain]"), "{tool}: {reason}")
            }
            other => panic!("{tool}: expected Deny, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn deny_root_is_denied_with_its_own_code() {
    let (_tmp, home) = layout();
    let vault = TempDir::new().unwrap();
    let vault_path = std::fs::canonicalize(vault.path()).unwrap();
    let ent = ent_with_fs(FilesystemEntitlement {
        write: vec![vault_path.display().to_string()],
        deny: vec![vault_path.display().to_string()],
        ..Default::default()
    });
    let target = vault_path.join("key.pem");
    match decide(&home, ent, "write_file", &target.display().to_string()).await {
        Decision::Deny { reason } => assert!(reason.starts_with("[fs_deny]"), "{reason}"),
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[tokio::test]
async fn legacy_fs_names_no_longer_gate() {
    // No tool registers `fs.write`; B0 no longer pretends to gate it.
    let (_tmp, home) = layout();
    let target = home.parent().unwrap().join("other/profile.yaml");
    let d = decide(
        &home,
        ent_with_fs(FilesystemEntitlement::default()),
        "fs.write",
        &target.display().to_string(),
    )
    .await;
    assert!(matches!(d, Decision::Allow), "{d:?}");
}
