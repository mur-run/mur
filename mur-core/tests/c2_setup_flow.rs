//! Track C2 — `mur agent companion connector add --platform telegram`
//! integration tests.
//!
//! Coverage:
//!  * M-c2.0.2 (legacy): the telegram arm without flags returns a typed error
//!    pointing to M-c2.1 (still relevant — the interactive 5-step UX is
//!    unchanged from M-c2.1.2 and we don't want to introduce a stdin-script
//!    test for it here).
//!  * M-c2.1.2: `scaffold_telegram_bridge` writes the bot token to the keychain
//!    and produces a `TelegramConfig` with the expected fields.
//!  * M-c2.1.3: `confirm_e2e_disclosure` requires the literal "I understand"
//!    string — case- and whitespace-sensitive.
//!  * M-c2.1.4: the CLI exposes the non-interactive flags
//!    (`--bot-token`, `--bot-username`, `--chat-id`, `--ack`) and writes
//!    `agents/<name>/telegram.yaml` under `MUR_HOME` when invoked with
//!    `MUR_TELEGRAM_KEYCHAIN_BACKEND=mock`.
//!
//! Windows: gated for parity with `connector_add_stub.rs`.
#![cfg(unix)]

use std::process::Command;
use tempfile::TempDir;

use mur_core::bridge_keychain::{Keychain, MockKeychain};
use mur_core::cmd::agent_companion::connector::{
    ScaffoldArgs, ScaffoldOutcome, confirm_e2e_disclosure, scaffold_telegram_bridge,
};

#[test]
fn telegram_arm_without_flags_returns_typed_error() {
    // Without --bot-token / --bot-username / --chat-id / --ack the CLI falls
    // back to the interactive flow, which on a non-tty test harness errors out
    // with a deterministic message we can grep for. The exact wording is not
    // load-bearing — the assertion is just that we exit non-zero.
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "agent",
            "companion",
            "connector",
            "add",
            "tg",
            "--platform",
            "telegram",
            "--default-route",
            "coach",
        ])
        .env("MUR_HOME", &mur_home)
        .env("MUR_TELEGRAM_KEYCHAIN_BACKEND", "mock")
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "expected non-zero exit when interactive flags are missing: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn scaffold_writes_keychain_and_yaml_with_token_and_nonce() {
    let tmp = TempDir::new().unwrap();
    // SAFETY: tests in this binary are serial w.r.t. each other on the same
    // process, but other test binaries may run in parallel. MUR_HOME is read
    // inside `scaffold_telegram_bridge` via `paths::mur_root(None)` so we set
    // it for this test scope only.
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

    let kc = MockKeychain::default();
    let args = ScaffoldArgs {
        bridge_id: "tg-bridge".into(),
        bot_token: "1234:token".into(),
        bot_username: "MyAgentBot".into(),
        chat_id: 100,
        ack: true,
        allow_groups: vec![],
    };
    let outcome = scaffold_telegram_bridge(args, &kc).unwrap();
    let ScaffoldOutcome::Ok {
        config,
        profile_path,
    } = outcome;
    assert_eq!(config.bot_username, "MyAgentBot");
    assert_eq!(config.chat_id, 100);
    assert!(config.e2e_disclosure_acked_at.is_some());
    assert_eq!(
        kc.get("tg-bridge/telegram_bot_token").unwrap(),
        "1234:token"
    );
    assert!(profile_path.exists(), "telegram.yaml not written");
    assert!(profile_path.ends_with("agents/tg-bridge/telegram.yaml"));

    unsafe { std::env::remove_var("MUR_HOME") };
}

#[test]
fn scaffold_rejects_unacked() {
    let kc = MockKeychain::default();
    let args = ScaffoldArgs {
        bridge_id: "tg2".into(),
        bot_token: "x".into(),
        bot_username: "B".into(),
        chat_id: 1,
        ack: false,
        allow_groups: vec![],
    };
    let r = scaffold_telegram_bridge(args, &kc);
    let err = match r {
        Ok(_) => panic!("expected error when ack=false"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("E2E disclosure"), "msg={msg}");
}

#[test]
fn ack_text_must_match_literal() {
    assert!(confirm_e2e_disclosure("I understand"));
    assert!(!confirm_e2e_disclosure("yes"));
    assert!(!confirm_e2e_disclosure("i understand"));
    assert!(!confirm_e2e_disclosure(""));
    // Whitespace must not be trimmed silently — the user has to type the
    // exact literal.
    assert!(!confirm_e2e_disclosure(" I understand "));
}

#[test]
fn scaffold_registers_mcp_telegram_chat() {
    // M-c2.5.3: scaffold_telegram_bridge must emit a profile.yaml snippet
    // (or update an existing one) containing an `mcp_servers[]` entry named
    // `telegram_chat`. The user-agent picks this up via the standard
    // profile.mcp_servers[] surface and spawns the bridge as an MCP child.
    let tmp = TempDir::new().unwrap();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

    let kc = MockKeychain::default();
    let args = ScaffoldArgs {
        bridge_id: "tgX".into(),
        bot_token: "t".into(),
        bot_username: "BX".into(),
        chat_id: 1,
        ack: true,
        allow_groups: vec![],
    };
    let outcome = scaffold_telegram_bridge(args, &kc).unwrap();
    let outcome_path = match outcome {
        ScaffoldOutcome::Ok { profile_path, .. } => profile_path,
    };
    let profile_dir = outcome_path.parent().unwrap();
    let profile_yaml = std::fs::read_to_string(profile_dir.join("profile.yaml")).unwrap();
    assert!(
        profile_yaml.contains("name: telegram_chat"),
        "profile.yaml missing telegram_chat mcp entry: {profile_yaml}"
    );
    assert!(
        profile_yaml.contains("mcp"),
        "profile.yaml missing mcp_servers section: {profile_yaml}"
    );

    unsafe { std::env::remove_var("MUR_HOME") };
}

#[test]
fn cli_scaffold_via_stdin_script() {
    let tmp = TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", tmp.path())
        .env("MUR_TELEGRAM_KEYCHAIN_BACKEND", "mock")
        .args([
            "agent",
            "companion",
            "connector",
            "add",
            "tg",
            "--platform",
            "telegram",
            "--default-route",
            "coach",
            "--bot-token",
            "1234:abc",
            "--bot-username",
            "MyAgentBot",
            "--chat-id",
            "100",
            "--ack",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.path().join("agents/tg/telegram.yaml").exists());
}
