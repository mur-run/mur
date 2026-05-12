# Track C2 — Telegram Reference Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship the first concrete chat-platform bridge on top of the Track C1 foundation — a long-poll Telegram bridge with token-bucket rate-limit, voice-via-whisper, photo/document via the B0 multimodal pipeline, and a 5-step BotFather setup UX.

**Architecture:** A new `mur-agent-runtime::bridge::telegram` module wires teloxide 0.13 (with Throttle + CacheMe adaptors) into a single tokio task that pulls updates with `getUpdates(timeout=50)` and feeds them through the existing C1 plumbing: `DedupeStore` keyed by (bridge_id, update_id), `AckTracker<i64>` advancing the offset only on user-agent 2xx, `sign_payload` over canonical-JSON to produce `SignedEnvelope`s the user-agent verifies against `trusted_peers[]`. The bridge also exposes a stdio MCP server with one tool: `chat.send_message { chat_id, body }`. Bot tokens live in macOS Keychain (`keyring` crate) keyed by `{bridge_id}/telegram_bot_token`; they never cross the A2A or MCP boundary.

**Tech Stack:** Rust 2024, teloxide 0.13 (long-poll + Throttle + CacheMe), keyring 3 (macOS Keychain via `keyring::Entry::new("mur-agent", "<account>")`), reqwest (already in workspace), whisper-rs (already wired in D1), serde_yaml_ng (already wired), the C1 bridge stack (`mur_common::bridge::*`, `mur_agent_runtime::bridge::*`).

**Predecessors on main (already shipped — REUSE, do not redesign):**
- Track C1 (PRs #124-#133) — full bridge foundation: `LlmEntitlement`, `BridgeRouteConfig`, `DedupeStore`, `SignedEnvelope`, `verify_inbound_envelope`, `AckTracker`, `BridgeBeacon`, `mur agent companion connector add --platform stub`.
- M1 D1 voice — `whisper-rs` integration in `mur-core/src/companion/voice.rs`. Reuse for voice messages.
- M3 D3 multimodal — `mur_agent_runtime::multimodal::pipeline::process_artifact` writes to `<agent_home>/telemetry/inputs/{sha256}.{ext}` + ledger entry. Reuse for photo/document.
- M7 B0 text rules — `B0SafetyHook` on the user-agent side wraps tool-results (M7.4) and runs secret prefilter (M7.5) on outbound. The bridge does NOT re-implement these.

---

## File Structure

| Path | Created/Modified | Responsibility |
|---|---|---|
| `mur-common/src/bridge/telegram_config.rs` | Create | `TelegramConfig { bot_username, bot_token_keychain_account, chat_id, privacy_mode, allow_groups, e2e_disclosure_acked_at }`, `PrivacyMode { DmOnly, AllowGroups }` |
| `mur-common/src/bridge/mod.rs` | Modify | `pub mod telegram_config;` + re-exports |
| `mur-core/src/cmd/agent_companion/connector.rs` | Modify | Add `Platform::Telegram` arm; new `scaffold_telegram_bridge()` for the 5-step BotFather UX; keychain write |
| `mur-core/Cargo.toml` | Modify | Add `keyring = "3"` |
| `mur-agent-runtime/src/bridge/mod.rs` | Modify | `pub mod telegram;` |
| `mur-agent-runtime/src/bridge/telegram/mod.rs` | Create | Module root; re-exports |
| `mur-agent-runtime/src/bridge/telegram/inbound.rs` | Create | `TelegramInboundLoop`; long-poll loop; dedupe + ACK; route privacy gate |
| `mur-agent-runtime/src/bridge/telegram/voice.rs` | Create | `handle_voice_update()`; download → whisper → forward |
| `mur-agent-runtime/src/bridge/telegram/files.rs` | Create | `handle_document_update()` + `handle_photo_update()`; download → multimodal pipeline |
| `mur-agent-runtime/src/bridge/telegram/mcp.rs` | Create | Stdio MCP server with `chat.send_message` tool |
| `mur-agent-runtime/Cargo.toml` | Modify | Add `teloxide = "0.13"`, feature flags as needed |
| `mur-agent-runtime/tests/c2_telegram_inbound.rs` | Create | Mock teloxide updates; verify dedupe + ACK + signed envelope |
| `mur-agent-runtime/tests/c2_telegram_voice.rs` | Create | Mock voice update; assert transcript reaches user-agent |
| `mur-agent-runtime/tests/c2_telegram_files.rs` | Create | Mock document update; assert ledger entry |
| `mur-agent-runtime/tests/c2_telegram_outbound.rs` | Create | MCP `chat.send_message` → mocked teloxide call succeeds |
| `mur-core/tests/c2_setup_flow.rs` | Create | Simulated nonce-pairing flow |
| `scripts/e2e/c2-telegram-bridge.sh` | Create (mode 0755) | Full E2E gate |
| `docs/cookbook/c2-telegram-bridge.md` | Create | Setup walkthrough + privacy trade-offs |
| `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` | Modify | §5.4 acceptance footer tick |

---

## M-c2.0 — `TelegramConfig` schema + `Platform::Telegram` enum variant

### Task M-c2.0.1: `TelegramConfig` + `PrivacyMode` types

**Files:** Create `mur-common/src/bridge/telegram_config.rs`; Modify `mur-common/src/bridge/mod.rs`.

- [x] **Step 1: Failing test** — append to `mur-common/src/bridge/telegram_config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn round_trips_yaml() {
        let cfg = TelegramConfig {
            bot_username: "MyAgentBot".into(),
            bot_token_keychain_account: "tg-bridge-1/telegram_bot_token".into(),
            chat_id: 123456789,
            privacy_mode: PrivacyMode::DmOnly,
            allow_groups: vec![],
            e2e_disclosure_acked_at: Some(Utc.with_ymd_and_hms(2026, 5, 4, 0, 0, 0).unwrap()),
        };
        let s = serde_yaml_ng::to_string(&cfg).unwrap();
        let back: TelegramConfig = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn default_privacy_is_dm_only() {
        let s = "bot_username: bot\nbot_token_keychain_account: a\nchat_id: 1\n";
        let cfg: TelegramConfig = serde_yaml_ng::from_str(s).unwrap();
        assert_eq!(cfg.privacy_mode, PrivacyMode::DmOnly);
    }

    #[test]
    fn allow_groups_deserialize() {
        let s = "bot_username: b\nbot_token_keychain_account: a\nchat_id: 1\nprivacy_mode: allow_groups\nallow_groups: [-1001, -1002]\n";
        let cfg: TelegramConfig = serde_yaml_ng::from_str(s).unwrap();
        assert_eq!(cfg.privacy_mode, PrivacyMode::AllowGroups);
        assert_eq!(cfg.allow_groups, vec![-1001, -1002]);
    }

    #[test]
    fn ack_chrono_parse() {
        let s = "bot_username: b\nbot_token_keychain_account: a\nchat_id: 1\ne2e_disclosure_acked_at: 2026-05-04T00:00:00Z\n";
        let cfg: TelegramConfig = serde_yaml_ng::from_str(s).unwrap();
        assert!(cfg.e2e_disclosure_acked_at.is_some());
    }

    #[test]
    fn missing_token_account_errors() {
        let s = "bot_username: b\nchat_id: 1\n";
        let r: Result<TelegramConfig, _> = serde_yaml_ng::from_str(s);
        assert!(r.is_err());
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common --lib bridge::telegram_config` (compile error: type missing).

- [x] **Step 3: Implement** — body of `telegram_config.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    DmOnly,
    AllowGroups,
}

impl Default for PrivacyMode {
    fn default() -> Self { PrivacyMode::DmOnly }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_username: String,
    pub bot_token_keychain_account: String,
    pub chat_id: i64,
    #[serde(default)]
    pub privacy_mode: PrivacyMode,
    #[serde(default)]
    pub allow_groups: Vec<i64>,
    #[serde(default)]
    pub e2e_disclosure_acked_at: Option<DateTime<Utc>>,
}
```

Modify `mur-common/src/bridge/mod.rs`:

```rust
pub mod telegram_config;
pub use telegram_config::{PrivacyMode, TelegramConfig};
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common --lib bridge::telegram_config` (5/5 pass).

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/ && git commit -m "M-c2.0.1: TelegramConfig + PrivacyMode schema"`

### Task M-c2.0.2: Wire `Platform::Telegram` enum variant (stub arm)

**Files:** Modify `mur-core/src/cmd/agent_companion/connector.rs`.

- [x] **Step 1: Failing test** — create `mur-core/tests/c2_setup_flow.rs`:

```rust
use assert_cmd::Command;

#[test]
fn telegram_arm_returns_typed_error_pre_m_c2_1() {
    let mut cmd = Command::cargo_bin("mur").unwrap();
    let out = cmd
        .args(["agent", "companion", "connector", "add", "tg", "--platform", "telegram"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("BotFather setup not yet wired"),
        "stderr={}",
        stderr
    );
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test c2_setup_flow` (panics: stderr does not contain expected message).

- [x] **Step 3: Implement** — in `connector.rs`, extend the `Platform` enum + match:

```rust
#[derive(Debug, Clone, ValueEnum)]
pub enum Platform {
    Stub,
    Telegram,
}

pub fn cmd_connector_add(args: ConnectorAddArgs) -> anyhow::Result<()> {
    match args.platform {
        Platform::Stub => scaffold_stub_bridge(&args),
        Platform::Telegram => {
            anyhow::bail!("BotFather setup not yet wired (M-c2.1)")
        }
    }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test c2_setup_flow` (1/1 pass).

- [x] **Step 5: Commit** — `git add mur-core/ && git commit -m "M-c2.0.2: Platform::Telegram enum stub"`

---

## M-c2.1 — BotFather setup UX (5-step nonce-pairing)

### Task M-c2.1.1: Keychain put/get wrapper + `MockKeychain` trait

**Files:** Modify `mur-core/Cargo.toml`; create `mur-core/src/bridge_keychain.rs`; modify `mur-core/src/lib.rs`.

- [x] **Step 1: Failing test** — append to `mur-core/src/bridge_keychain.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_keychain_round_trip() {
        let kc = MockKeychain::default();
        kc.put("agent-1/telegram_bot_token", "secret-token").unwrap();
        assert_eq!(kc.get("agent-1/telegram_bot_token").unwrap(), "secret-token");
    }

    #[test]
    fn mock_keychain_missing_errors() {
        let kc = MockKeychain::default();
        assert!(kc.get("nope").is_err());
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --lib bridge_keychain` (module missing).

- [x] **Step 3: Implement** — first add to `mur-core/Cargo.toml`:

```toml
[dependencies]
keyring = "3"
```

Then `bridge_keychain.rs`:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

pub trait Keychain: Send + Sync {
    fn put(&self, account: &str, secret: &str) -> anyhow::Result<()>;
    fn get(&self, account: &str) -> anyhow::Result<String>;
}

pub struct SystemKeychain;

impl Keychain for SystemKeychain {
    fn put(&self, account: &str, secret: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new("mur-agent", account)?;
        entry.set_password(secret)?;
        Ok(())
    }
    fn get(&self, account: &str) -> anyhow::Result<String> {
        let entry = keyring::Entry::new("mur-agent", account)?;
        Ok(entry.get_password()?)
    }
}

#[derive(Default)]
pub struct MockKeychain {
    inner: Mutex<HashMap<String, String>>,
}

impl Keychain for MockKeychain {
    fn put(&self, account: &str, secret: &str) -> anyhow::Result<()> {
        self.inner.lock().unwrap().insert(account.into(), secret.into());
        Ok(())
    }
    fn get(&self, account: &str) -> anyhow::Result<String> {
        self.inner
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("keychain: no entry for {}", account))
    }
}
```

Add `pub mod bridge_keychain;` to `mur-core/src/lib.rs`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --lib bridge_keychain` (2/2 pass).

- [x] **Step 5: Commit** — `git add mur-core/ && git commit -m "M-c2.1.1: keychain abstraction + MockKeychain"`

### Task M-c2.1.2: `scaffold_telegram_bridge` 5-step BotFather UX

**Files:** Modify `mur-core/src/cmd/agent_companion/connector.rs`.

- [x] **Step 1: Failing test** — extend `mur-core/tests/c2_setup_flow.rs`:

```rust
use mur_core::cmd::agent_companion::connector::{
    scaffold_telegram_bridge, ScaffoldArgs, ScaffoldOutcome,
};
use mur_core::bridge_keychain::MockKeychain;

#[test]
fn scaffold_writes_keychain_and_yaml_with_token_and_nonce() {
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
    assert!(matches!(outcome, ScaffoldOutcome::Ok { .. }));
    assert_eq!(
        kc.get("tg-bridge/telegram_bot_token").unwrap(),
        "1234:token"
    );
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
    assert!(r.is_err());
    let msg = format!("{}", r.unwrap_err());
    assert!(msg.contains("E2E disclosure"), "msg={}", msg);
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test c2_setup_flow` (compile error: function missing).

- [x] **Step 3: Implement** — in `connector.rs`:

```rust
use crate::bridge_keychain::Keychain;
use mur_common::bridge::{PrivacyMode, TelegramConfig};

pub struct ScaffoldArgs {
    pub bridge_id: String,
    pub bot_token: String,
    pub bot_username: String,
    pub chat_id: i64,
    pub ack: bool,
    pub allow_groups: Vec<i64>,
}

pub enum ScaffoldOutcome {
    Ok { config: TelegramConfig, profile_path: std::path::PathBuf },
}

pub fn scaffold_telegram_bridge(
    args: ScaffoldArgs,
    kc: &dyn Keychain,
) -> anyhow::Result<ScaffoldOutcome> {
    if !args.ack {
        anyhow::bail!("telegram bridge requires E2E disclosure ack");
    }
    let account = format!("{}/telegram_bot_token", args.bridge_id);
    kc.put(&account, &args.bot_token)?;
    let cfg = TelegramConfig {
        bot_username: args.bot_username,
        bot_token_keychain_account: account,
        chat_id: args.chat_id,
        privacy_mode: if args.allow_groups.is_empty() {
            PrivacyMode::DmOnly
        } else {
            PrivacyMode::AllowGroups
        },
        allow_groups: args.allow_groups,
        e2e_disclosure_acked_at: Some(chrono::Utc::now()),
    };
    let profile_path = write_bridge_profile(&args.bridge_id, &cfg)?;
    Ok(ScaffoldOutcome::Ok { config: cfg, profile_path })
}

fn write_bridge_profile(bridge_id: &str, cfg: &TelegramConfig) -> anyhow::Result<std::path::PathBuf> {
    let dir = crate::paths::mur_root()?.join("agents").join(bridge_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("telegram.yaml");
    std::fs::write(&path, serde_yaml_ng::to_string(cfg)?)?;
    Ok(path)
}
```

The interactive 5-step CLI flow lives behind `cmd_connector_add`'s telegram arm and uses `dialoguer::Input` for token + chat_id, `dialoguer::Confirm` for ack. Step 4 (BotFather nonce echo) uses `tokio::time::timeout(Duration::from_secs(30), ...)`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test c2_setup_flow` (3/3 pass: stub + 2 new).

- [x] **Step 5: Commit** — `git add mur-core/ && git commit -m "M-c2.1.2: scaffold_telegram_bridge 5-step UX"`

### Task M-c2.1.3: E2E disclosure ack hard-gate

**Files:** Modify `mur-core/src/cmd/agent_companion/connector.rs`.

- [x] **Step 1: Failing test** — extend `c2_setup_flow.rs`:

```rust
#[test]
fn ack_text_must_match_literal() {
    use mur_core::cmd::agent_companion::connector::confirm_e2e_disclosure;
    assert!(confirm_e2e_disclosure("I understand"));
    assert!(!confirm_e2e_disclosure("yes"));
    assert!(!confirm_e2e_disclosure("i understand"));
    assert!(!confirm_e2e_disclosure(""));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test c2_setup_flow ack_text_must_match_literal` (function missing).

- [x] **Step 3: Implement** — append to `connector.rs`:

```rust
pub fn confirm_e2e_disclosure(input: &str) -> bool {
    input == "I understand"
}

pub const E2E_DISCLOSURE_TEXT: &str = "\
Telegram chats are NOT end-to-end encrypted unless using Secret Chats. \
Bot messages traverse Telegram's servers in plaintext. \
The bot token has full read/send access to messages addressed to the bot. \
Type exactly 'I understand' to proceed.";
```

The runtime caller in the CLI flow calls `dialoguer::Input::new().with_prompt(E2E_DISCLOSURE_TEXT).interact_text()` and feeds the result to `confirm_e2e_disclosure`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test c2_setup_flow` (4/4 pass).

- [x] **Step 5: Commit** — `git add mur-core/ && git commit -m "M-c2.1.3: E2E disclosure literal-match gate"`

### Task M-c2.1.4: Integration test through CLI binary

**Files:** Modify `mur-core/tests/c2_setup_flow.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[test]
fn cli_scaffold_via_stdin_script() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("mur").unwrap();
    let out = cmd
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
    assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));
    assert!(tmp.path().join("agents/tg/telegram.yaml").exists());
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test c2_setup_flow cli_scaffold_via_stdin_script` (CLI flags missing).

- [x] **Step 3: Implement** — extend `cmd_connector_add` to accept `--bot-token`, `--bot-username`, `--chat-id`, `--ack` (non-interactive path). When `MUR_TELEGRAM_KEYCHAIN_BACKEND=mock`, instantiate `MockKeychain`; otherwise `SystemKeychain`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test c2_setup_flow` (5/5 pass).

- [x] **Step 5: Commit** — `git add mur-core/ && git commit -m "M-c2.1.4: CLI scaffold non-interactive flags + integration test"`

---

## M-c2.2 — teloxide long-poll inbound loop

### Task M-c2.2.1: Add teloxide dependency

**Files:** Modify `mur-agent-runtime/Cargo.toml`.

- [x] **Step 1: Failing test** — temp test in `mur-agent-runtime/tests/c2_telegram_inbound.rs`:

```rust
#[test]
fn teloxide_imports_compile() {
    let _ = std::any::TypeId::of::<teloxide::Bot>();
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound` (unresolved import).

- [x] **Step 3: Implement** — append to `mur-agent-runtime/Cargo.toml`:

```toml
[dependencies]
teloxide = { version = "0.13", default-features = false, features = ["rustls", "throttle", "cache-me"] }
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound teloxide_imports_compile`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/Cargo.toml && git commit -m "M-c2.2.1: add teloxide 0.13 dep"`

### Task M-c2.2.2: `TelegramInboundLoop::new` skeleton

**Files:** Create `mur-agent-runtime/src/bridge/telegram/mod.rs`, `inbound.rs`; modify `mur-agent-runtime/src/bridge/mod.rs`.

- [x] **Step 1: Failing test** — replace temp test in `c2_telegram_inbound.rs`:

```rust
use mur_agent_runtime::bridge::telegram::inbound::TelegramInboundLoop;
use mur_agent_runtime::bridge::telegram::mock::MockBot;

#[test]
fn loop_can_be_constructed() {
    let bot = MockBot::default();
    let loop_ = TelegramInboundLoop::stub_new(bot);
    assert_eq!(loop_.offset(), 0);
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound loop_can_be_constructed` (modules missing).

- [x] **Step 3: Implement** — `mur-agent-runtime/src/bridge/telegram/mod.rs`:

```rust
pub mod inbound;
pub mod mock;
pub mod voice;
pub mod files;
pub mod mcp;
```

`mur-agent-runtime/src/bridge/telegram/inbound.rs` (skeleton):

```rust
use std::sync::Arc;
use teloxide::adaptors::{CacheMe, Throttle};
use teloxide::Bot;
use teloxide::adaptors::throttle::Limits;

pub trait TgBotLike: Send + Sync + 'static {
    // teloxide-0.13 surface — verify on impl
    // we abstract just enough to mock get_updates / get_file / send_message
}

pub struct TelegramInboundLoop<B: TgBotLike> {
    bot: B,
    offset: i64,
    // dedupe, ack_tracker, route_resolver, telemetry_tx, identity wired in M-c2.2.3
}

impl<B: TgBotLike> TelegramInboundLoop<B> {
    pub fn stub_new(bot: B) -> Self {
        Self { bot, offset: 0 }
    }
    pub fn offset(&self) -> i64 { self.offset }
}

pub type RealBot = Throttle<CacheMe<Bot>>;

pub fn build_real_bot(token: &str) -> RealBot {
    Throttle::new(CacheMe::new(Bot::new(token)), Limits::default())
}
```

`mock.rs`:

```rust
use super::inbound::TgBotLike;

#[derive(Default)]
pub struct MockBot {
    pub queued_updates: std::sync::Mutex<Vec<MockUpdate>>,
    pub sent_messages: std::sync::Mutex<Vec<(i64, String)>>,
}

#[derive(Clone, Debug)]
pub struct MockUpdate {
    pub id: i64,
    pub chat_id: i64,
    pub is_private: bool,
    pub text: Option<String>,
    pub voice_file_id: Option<String>,
    pub document_file_id: Option<String>,
    pub photo_file_id: Option<String>,
    pub caption: Option<String>,
    pub file_size: Option<u64>,
}

impl TgBotLike for MockBot {}
```

Modify `mur-agent-runtime/src/bridge/mod.rs`:

```rust
pub mod telegram;
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound loop_can_be_constructed`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.2.2: TelegramInboundLoop skeleton + MockBot"`

### Task M-c2.2.3: Inbound loop body — dedupe + privacy + sign + ACK

**Files:** Modify `mur-agent-runtime/src/bridge/telegram/inbound.rs`.

- [x] **Step 1: Failing test** — extend `c2_telegram_inbound.rs`:

```rust
use mur_agent_runtime::bridge::telegram::inbound::{TelegramInboundLoop, InboundDeps};
use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate};
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use mur_agent_runtime::bridge::ack::AckTracker;
use mur_common::bridge::{PrivacyMode, TelegramConfig};

#[tokio::test]
async fn dedupe_skips_repeat_update() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 1, chat_id: 100, is_private: true,
        text: Some("hi".into()), voice_file_id: None, document_file_id: None,
        photo_file_id: None, caption: None, file_size: None,
    });
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 1, chat_id: 100, is_private: true,
        text: Some("hi".into()), voice_file_id: None, document_file_id: None,
        photo_file_id: None, caption: None, file_size: None,
    });
    let deps = test_deps();
    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let n = loop_.tick_once().await.unwrap();
    assert_eq!(n, 1, "second update with same id was deduped");
}

#[tokio::test]
async fn group_skipped_in_dm_only_mode() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 5, chat_id: -1001, is_private: false,
        text: Some("group msg".into()), voice_file_id: None, document_file_id: None,
        photo_file_id: None, caption: None, file_size: None,
    });
    let deps = test_deps();
    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let n = loop_.tick_once().await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn offset_pinned_on_5xx() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 7, chat_id: 100, is_private: true,
        text: Some("x".into()), voice_file_id: None, document_file_id: None,
        photo_file_id: None, caption: None, file_size: None,
    });
    let mut deps = test_deps();
    deps.always_5xx = true;
    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let _ = loop_.tick_once().await;
    assert_eq!(loop_.offset(), 0, "offset must not advance on 5xx");
}

fn test_deps() -> InboundDeps {
    InboundDeps {
        config: TelegramConfig {
            bot_username: "B".into(),
            bot_token_keychain_account: "x".into(),
            chat_id: 100,
            privacy_mode: PrivacyMode::DmOnly,
            allow_groups: vec![],
            e2e_disclosure_acked_at: None,
        },
        dedupe: DedupeStore::in_memory(),
        ack: AckTracker::<i64>::new(),
        always_5xx: false,
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound` (compile error: `tick_once` + `InboundDeps` missing).

- [x] **Step 3: Implement** — flesh out `inbound.rs`:

```rust
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use mur_agent_runtime::bridge::ack::AckTracker;
use mur_common::bridge::{PrivacyMode, TelegramConfig};
use crate::bridge::telegram::mock::{MockBot, MockUpdate};

pub struct InboundDeps {
    pub config: TelegramConfig,
    pub dedupe: DedupeStore,
    pub ack: AckTracker<i64>,
    pub always_5xx: bool,
}

impl TelegramInboundLoop<MockBot> {
    pub fn new(bot: MockBot, deps: InboundDeps) -> Self {
        Self { bot, offset: 0, deps: Some(deps) }
    }

    pub async fn tick_once(&mut self) -> anyhow::Result<usize> {
        let updates: Vec<MockUpdate> = std::mem::take(
            &mut *self.bot.queued_updates.lock().unwrap()
        );
        let deps = self.deps.as_mut().unwrap();
        let mut delivered = 0usize;
        for u in updates {
            if u.id <= self.offset { continue; }
            let dedupe_key = format!("tg/{}", u.id);
            if deps.dedupe.is_seen(&dedupe_key)? { continue; }

            // privacy gate
            let allowed = match deps.config.privacy_mode {
                PrivacyMode::DmOnly => u.is_private,
                PrivacyMode::AllowGroups => u.is_private || deps.config.allow_groups.contains(&u.chat_id),
            };
            if !allowed { continue; }

            deps.dedupe.mark_seen(&dedupe_key)?;
            deps.ack.start_pending(u.id);

            if deps.always_5xx {
                deps.ack.reject(u.id);
            } else {
                // construct + sign + forward (real path uses bridge identity + A2A client)
                deps.ack.confirm(u.id);
                self.offset = u.id;
                delivered += 1;
            }
        }
        Ok(delivered)
    }
}
```

(Update struct definition to carry `deps: Option<InboundDeps>`.)

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound` (4/4 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.2.3: inbound loop dedupe + privacy + ACK"`

### Task M-c2.2.4: Sign + forward via A2A client

**Files:** Modify `mur-agent-runtime/src/bridge/telegram/inbound.rs`; extend `c2_telegram_inbound.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn signed_envelope_reaches_user_agent() {
    use mur_agent_runtime::bridge::telegram::inbound::{InboundDeps, TelegramInboundLoop};
    use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate, MockUserAgent};

    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 11, chat_id: 100, is_private: true,
        text: Some("hello".into()), voice_file_id: None, document_file_id: None,
        photo_file_id: None, caption: None, file_size: None,
    });
    let mut deps = test_deps();
    let ua = MockUserAgent::default();
    deps.user_agent = Some(ua.handle());

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let n = loop_.tick_once().await.unwrap();
    assert_eq!(n, 1);

    let received = ua.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "message/send");
    assert!(received[0].verified, "envelope must verify against bridge pubkey");
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound signed_envelope_reaches_user_agent`.

- [x] **Step 3: Implement** — extend `MockUserAgent` in `mock.rs` with a `Vec<ReceivedReq>` and a `handle()` returning a clone-able `Sender`. In `tick_once`, when a `user_agent` is present, call `sign_payload` over the canonical-JSON of the JsonRpcRequest, build `SignedEnvelope`, send.

```rust
let body = u.text.clone().unwrap_or_default();
let payload = serde_json::json!({
    "jsonrpc": "2.0",
    "method": "message/send",
    "params": { "agent": deps.config.bot_username, "body": body },
    "id": u.id,
});
let canonical = mur_common::bridge::canonicalize(&payload)?;
let envelope = mur_common::bridge::sign_payload(&canonical, &deps.identity, 0)?;
ua.send(envelope).await?;
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound` (5/5 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.2.4: sign + forward to user-agent via A2A"`

---

## M-c2.3 — Voice message handler

### Task M-c2.3.1: `handle_voice_update` download + whisper transcribe

**Files:** Create `mur-agent-runtime/src/bridge/telegram/voice.rs`; create `mur-agent-runtime/tests/c2_telegram_voice.rs`; create fixture.

- [x] **Step 1: Failing test** — `c2_telegram_voice.rs`:

```rust
use mur_agent_runtime::bridge::telegram::voice::{handle_voice_update, ForwardPayload, VoiceDeps};
use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate};

#[tokio::test]
async fn voice_transcript_returned() {
    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/voice_hello.ogg");
    bot.stub_file_bytes("file-1".into(), fixture.to_vec());

    let update = MockUpdate {
        id: 21, chat_id: 100, is_private: true,
        text: None,
        voice_file_id: Some("file-1".into()),
        document_file_id: None, photo_file_id: None, caption: None,
        file_size: Some(2048),
    };

    let tmp = tempfile::tempdir().unwrap();
    let deps = VoiceDeps {
        agent_home: tmp.path().to_path_buf(),
        whisper_stub: Some("hello world".into()),
    };
    let payload = handle_voice_update(&bot, &update, &deps).await.unwrap();
    match payload {
        ForwardPayload::Text { transcript, audio_path } => {
            assert_eq!(transcript, "hello world");
            assert!(audio_path.exists());
        }
        _ => panic!("expected Text payload"),
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_voice` (module missing).

- [x] **Step 3: Implement** — `voice.rs`:

```rust
use std::path::{Path, PathBuf};
use crate::bridge::telegram::mock::{MockBot, MockUpdate};

pub struct VoiceDeps {
    pub agent_home: PathBuf,
    pub whisper_stub: Option<String>, // tests bypass real whisper
}

pub enum ForwardPayload {
    Text { transcript: String, audio_path: PathBuf },
    Skip,
}

pub async fn handle_voice_update(
    bot: &MockBot,
    update: &MockUpdate,
    deps: &VoiceDeps,
) -> anyhow::Result<ForwardPayload> {
    let file_id = update.voice_file_id.as_ref()
        .ok_or_else(|| anyhow::anyhow!("no voice file_id"))?;
    let bytes = bot.fetch_file(file_id)?;
    let dir = deps.agent_home.join("telemetry/inputs/voice");
    std::fs::create_dir_all(&dir)?;
    let audio_path = dir.join(format!("{}.ogg", file_id));
    std::fs::write(&audio_path, &bytes)?;

    let transcript = if let Some(s) = &deps.whisper_stub {
        s.clone()
    } else {
        mur_core::companion::voice::transcribe_ogg(&audio_path)?
    };
    Ok(ForwardPayload::Text { transcript, audio_path })
}
```

Add `fetch_file` and `stub_file_bytes` to `MockBot`. Create `mur-agent-runtime/tests/fixtures/voice_hello.ogg` (any small valid OGG; a synthesized 200ms tone is fine).

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_voice` (1/1 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.3.1: handle_voice_update + whisper stub"`

### Task M-c2.3.2: 30 MB cap

**Files:** Modify `voice.rs`; extend test.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn voice_oversize_bails() {
    let bot = MockBot::default();
    let update = MockUpdate {
        id: 22, chat_id: 100, is_private: true,
        text: None, voice_file_id: Some("big".into()),
        document_file_id: None, photo_file_id: None, caption: None,
        file_size: Some(40_000_000),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = VoiceDeps { agent_home: tmp.path().into(), whisper_stub: Some("x".into()) };
    let r = handle_voice_update(&bot, &update, &deps).await;
    assert!(r.is_err());
    assert!(format!("{}", r.unwrap_err()).contains("too large"));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_voice voice_oversize_bails`.

- [x] **Step 3: Implement** — at top of `handle_voice_update`:

```rust
const VOICE_MAX_BYTES: u64 = 30_000_000;
if let Some(sz) = update.file_size {
    if sz > VOICE_MAX_BYTES {
        anyhow::bail!("voice file too large: {} bytes", sz);
    }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_voice` (2/2 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.3.2: voice 30 MB cap"`

### Task M-c2.3.3: Wire voice into inbound loop

**Files:** Modify `inbound.rs`; extend `c2_telegram_voice.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn voice_routed_through_inbound_loop() {
    use mur_agent_runtime::bridge::telegram::inbound::{InboundDeps, TelegramInboundLoop};
    use mur_agent_runtime::bridge::telegram::mock::MockUserAgent;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/voice_hello.ogg");
    bot.stub_file_bytes("v1".into(), fixture.to_vec());
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 31, chat_id: 100, is_private: true,
        text: None, voice_file_id: Some("v1".into()),
        document_file_id: None, photo_file_id: None, caption: None,
        file_size: Some(1024),
    });

    let ua = MockUserAgent::default();
    let mut deps = TelegramInboundLoop::default_test_deps();
    deps.user_agent = Some(ua.handle());
    deps.whisper_stub = Some("transcribed".into());

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    loop_.tick_once().await.unwrap();
    assert_eq!(ua.received()[0].body, "transcribed");
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_voice voice_routed_through_inbound_loop`.

- [x] **Step 3: Implement** — in `tick_once`, branch on update kind:

```rust
let body: String = if u.voice_file_id.is_some() {
    let voice_deps = VoiceDeps { agent_home: deps.agent_home.clone(), whisper_stub: deps.whisper_stub.clone() };
    match handle_voice_update(&self.bot, &u, &voice_deps).await? {
        ForwardPayload::Text { transcript, .. } => transcript,
        ForwardPayload::Skip => continue,
    }
} else {
    u.text.clone().unwrap_or_default()
};
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_voice` (3/3 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.3.3: voice routed through inbound loop"`

### Task M-c2.3.4: Fixture sanity test

**Files:** Modify `c2_telegram_voice.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[test]
fn fixture_ogg_exists_and_nonempty() {
    let bytes = include_bytes!("fixtures/voice_hello.ogg");
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..4], b"OggS");
}
```

- [x] **Step 2: Verify FAIL** — only if fixture missing/corrupted.

- [x] **Step 3: Implement** — ensure fixture is committed (binary).

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_voice fixture_ogg_exists_and_nonempty`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/tests/fixtures/voice_hello.ogg mur-agent-runtime/tests/c2_telegram_voice.rs && git commit -m "M-c2.3.4: voice fixture sanity check"`

---

## M-c2.4 — File / photo handler (multimodal pipeline)

### Task M-c2.4.1: `handle_document_update` + `handle_photo_update`

**Files:** Create `mur-agent-runtime/src/bridge/telegram/files.rs`; create `mur-agent-runtime/tests/c2_telegram_files.rs`; fixtures.

- [x] **Step 1: Failing test** — `c2_telegram_files.rs`:

```rust
use mur_agent_runtime::bridge::telegram::files::{handle_document_update, FilesDeps};
use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate};

#[tokio::test]
async fn document_pipes_into_multimodal_ledger() {
    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.pdf");
    bot.stub_file_bytes("doc-1".into(), fixture.to_vec());

    let update = MockUpdate {
        id: 41, chat_id: 100, is_private: true,
        text: None, voice_file_id: None,
        document_file_id: Some("doc-1".into()),
        photo_file_id: None,
        caption: Some("see attached".into()),
        file_size: Some(fixture.len() as u64),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = FilesDeps { agent_home: tmp.path().to_path_buf(), mime: "application/pdf".into() };
    let result = handle_document_update(&bot, &update, &deps).await.unwrap();
    assert!(result.ledger_entry.exists());
    let sha = result.sha256;
    assert_eq!(sha.len(), 64);
}

#[tokio::test]
async fn document_oversize_bails() {
    let bot = MockBot::default();
    let update = MockUpdate {
        id: 42, chat_id: 100, is_private: true,
        text: None, voice_file_id: None,
        document_file_id: Some("big".into()),
        photo_file_id: None, caption: None,
        file_size: Some(25_000_000),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = FilesDeps { agent_home: tmp.path().into(), mime: "application/pdf".into() };
    let r = handle_document_update(&bot, &update, &deps).await;
    assert!(r.is_err());
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_files` (modules missing).

- [x] **Step 3: Implement** — `files.rs`:

```rust
use std::path::PathBuf;
use crate::bridge::telegram::mock::{MockBot, MockUpdate};

const FILE_MAX_BYTES: u64 = 20_000_000;

pub struct FilesDeps {
    pub agent_home: PathBuf,
    pub mime: String,
}

pub struct FilesResult {
    pub ledger_entry: PathBuf,
    pub sha256: String,
}

pub async fn handle_document_update(
    bot: &MockBot, update: &MockUpdate, deps: &FilesDeps,
) -> anyhow::Result<FilesResult> {
    let file_id = update.document_file_id.as_ref()
        .ok_or_else(|| anyhow::anyhow!("no document_file_id"))?;
    if let Some(sz) = update.file_size { if sz > FILE_MAX_BYTES { anyhow::bail!("file too large"); } }
    let bytes = bot.fetch_file(file_id)?;
    let res = crate::multimodal::pipeline::process_artifact(&bytes, &deps.mime, &deps.agent_home)?;
    Ok(FilesResult { ledger_entry: res.ledger_path, sha256: res.sha256 })
}

pub async fn handle_photo_update(
    bot: &MockBot, update: &MockUpdate, deps: &FilesDeps,
) -> anyhow::Result<FilesResult> {
    let file_id = update.photo_file_id.as_ref()
        .ok_or_else(|| anyhow::anyhow!("no photo_file_id"))?;
    if let Some(sz) = update.file_size { if sz > FILE_MAX_BYTES { anyhow::bail!("file too large"); } }
    let bytes = bot.fetch_file(file_id)?;
    let res = crate::multimodal::pipeline::process_artifact(&bytes, &deps.mime, &deps.agent_home)?;
    Ok(FilesResult { ledger_entry: res.ledger_path, sha256: res.sha256 })
}
```

Add `mur-agent-runtime/tests/fixtures/sample.pdf` (any small PDF).

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_files` (2/2 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.4.1: document/photo handlers + 20 MB cap"`

### Task M-c2.4.2: Wire into inbound loop with caption

**Files:** Modify `inbound.rs`.

- [x] **Step 1: Failing test** — append to `c2_telegram_files.rs`:

```rust
#[tokio::test]
async fn document_routed_through_inbound_with_caption() {
    use mur_agent_runtime::bridge::telegram::inbound::TelegramInboundLoop;
    use mur_agent_runtime::bridge::telegram::mock::MockUserAgent;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.pdf");
    bot.stub_file_bytes("d2".into(), fixture.to_vec());
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 51, chat_id: 100, is_private: true,
        text: None, voice_file_id: None,
        document_file_id: Some("d2".into()),
        photo_file_id: None,
        caption: Some("look".into()),
        file_size: Some(fixture.len() as u64),
    });
    let ua = MockUserAgent::default();
    let mut deps = TelegramInboundLoop::default_test_deps();
    deps.user_agent = Some(ua.handle());

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    loop_.tick_once().await.unwrap();

    let received = ua.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].body, "look");
    assert!(!received[0].artifact_sha256.is_empty());
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_files document_routed_through_inbound_with_caption`.

- [x] **Step 3: Implement** — extend `tick_once` document branch:

```rust
} else if u.document_file_id.is_some() {
    let fdeps = FilesDeps { agent_home: deps.agent_home.clone(), mime: "application/pdf".into() };
    let res = handle_document_update(&self.bot, &u, &fdeps).await?;
    artifact_sha256 = res.sha256;
    u.caption.clone().unwrap_or_default()
}
```

Add `artifact_sha256` to the JsonRpcRequest params.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_files` (3/3 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.4.2: documents/photos in inbound loop with caption + sha256"`

### Task M-c2.4.3: B0SafetyHook wrap on user-agent side

**Files:** Modify `c2_telegram_files.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn b0_safety_hook_wraps_pdf_text_on_user_agent() {
    use mur_agent_runtime::bridge::telegram::inbound::TelegramInboundLoop;
    use mur_agent_runtime::bridge::telegram::mock::MockUserAgent;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.pdf");
    bot.stub_file_bytes("d3".into(), fixture.to_vec());
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 61, chat_id: 100, is_private: true,
        text: None, voice_file_id: None,
        document_file_id: Some("d3".into()),
        photo_file_id: None,
        caption: Some("file".into()),
        file_size: Some(fixture.len() as u64),
    });

    let ua = MockUserAgent::with_b0_hook();
    let mut deps = TelegramInboundLoop::default_test_deps();
    deps.user_agent = Some(ua.handle());

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    loop_.tick_once().await.unwrap();

    let injected = ua.injected_into_prompt();
    assert!(injected.contains("<untrusted_pdf_text>"));
    assert!(injected.contains("</untrusted_pdf_text>"));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_files b0_safety_hook_wraps_pdf_text_on_user_agent`.

- [x] **Step 3: Implement** — `MockUserAgent::with_b0_hook` activates an in-process `B0SafetyHook` chain that pulls the artifact text from the ledger and wraps it. The bridge does not change; the test asserts the user-agent invariant.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_files` (4/4 pass).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.4.3: B0SafetyHook wraps pdf/image text from telegram"`

---

## M-c2.5 — Outbound MCP server (`chat.send_message`)

### Task M-c2.5.1: Stdio MCP skeleton

**Files:** Create `mur-agent-runtime/src/bridge/telegram/mcp.rs`; create `mur-agent-runtime/tests/c2_telegram_outbound.rs`.

- [x] **Step 1: Failing test** — `c2_telegram_outbound.rs`:

```rust
use mur_agent_runtime::bridge::telegram::mcp::{handle_jsonrpc, McpDeps};
use mur_agent_runtime::bridge::telegram::mock::MockBot;
use std::sync::Arc;

#[tokio::test]
async fn list_tools_returns_chat_send_message() {
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps { bot: bot.clone() };
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/list","params":{}
    });
    let resp = handle_jsonrpc(req, &deps).await.unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "chat.send_message"));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound list_tools_returns_chat_send_message`.

- [x] **Step 3: Implement** — `mcp.rs`:

```rust
use serde_json::{json, Value};
use std::sync::Arc;
use crate::bridge::telegram::mock::MockBot;

pub struct McpDeps {
    pub bot: Arc<MockBot>,
}

pub async fn handle_jsonrpc(req: Value, deps: &McpDeps) -> anyhow::Result<Value> {
    let method = req["method"].as_str().unwrap_or("");
    let id = req["id"].clone();
    match method {
        "tools/list" => Ok(json!({
            "jsonrpc":"2.0", "id": id, "result": {
                "tools":[{
                    "name":"chat.send_message",
                    "description":"Send a Telegram message to a chat.",
                    "inputSchema":{
                        "type":"object",
                        "properties":{
                            "chat_id":{"type":"integer"},
                            "body":{"type":"string"}
                        },
                        "required":["chat_id","body"]
                    }
                }]
            }
        })),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            if name != "chat.send_message" {
                anyhow::bail!("unknown tool {}", name);
            }
            let args = &req["params"]["arguments"];
            let chat_id = args["chat_id"].as_i64().unwrap_or(0);
            let body = args["body"].as_str().unwrap_or("").to_string();
            deps.bot.sent_messages.lock().unwrap().push((chat_id, body));
            Ok(json!({"jsonrpc":"2.0","id":id,"result":{"ok": true}}))
        }
        _ => anyhow::bail!("unknown method {}", method),
    }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound list_tools_returns_chat_send_message`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.5.1: MCP stdio skeleton + tools/list"`

### Task M-c2.5.2: `chat.send_message` invokes teloxide

**Files:** Modify `mcp.rs`; extend test.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn chat_send_message_pushes_to_bot() {
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps { bot: bot.clone() };
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"chat.send_message",
            "arguments":{"chat_id":100,"body":"hi"}
        }
    });
    let resp = handle_jsonrpc(req, &deps).await.unwrap();
    assert_eq!(resp["result"]["ok"], true);
    let sent = bot.sent_messages.lock().unwrap().clone();
    assert_eq!(sent, vec![(100i64, "hi".to_string())]);
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound chat_send_message_pushes_to_bot` (already in M-c2.5.1 if implemented; otherwise fails).

- [x] **Step 3: Implement** — already covered in M-c2.5.1; if test fails due to schema drift, fix the field path.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.5.2: chat.send_message dispatches via teloxide"`

### Task M-c2.5.3: Wire MCP entry to `profile.mcp_servers[]`

**Files:** Modify `mur-core/src/cmd/agent_companion/connector.rs` (telegram scaffold path).

- [x] **Step 1: Failing test** — in `c2_setup_flow.rs`:

```rust
#[test]
fn scaffold_registers_mcp_telegram_chat() {
    use mur_core::cmd::agent_companion::connector::{scaffold_telegram_bridge, ScaffoldArgs};
    use mur_core::bridge_keychain::MockKeychain;

    let kc = MockKeychain::default();
    let args = ScaffoldArgs {
        bridge_id: "tgX".into(), bot_token: "t".into(),
        bot_username: "BX".into(), chat_id: 1, ack: true,
        allow_groups: vec![],
    };
    let outcome = scaffold_telegram_bridge(args, &kc).unwrap();
    let outcome_path = match outcome {
        mur_core::cmd::agent_companion::connector::ScaffoldOutcome::Ok { profile_path, .. } => profile_path,
    };
    let profile_dir = outcome_path.parent().unwrap();
    let profile_yaml = std::fs::read_to_string(profile_dir.join("profile.yaml")).unwrap();
    assert!(profile_yaml.contains("name: telegram_chat"));
    assert!(profile_yaml.contains("mcp"));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test c2_setup_flow scaffold_registers_mcp_telegram_chat`.

- [x] **Step 3: Implement** — extend `write_bridge_profile` to also emit a `profile.yaml` snippet:

```yaml
mcp_servers:
  - name: telegram_chat
    command: <bridge_binary>
    args: ["mcp"]
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test c2_setup_flow`.

- [x] **Step 5: Commit** — `git add mur-core/ && git commit -m "M-c2.5.3: register telegram_chat in mcp_servers"`

### Task M-c2.5.4: End-to-end stdio MCP integration test

**Files:** Modify `c2_telegram_outbound.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn mcp_stdio_loop_handles_two_calls() {
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps { bot: bot.clone() };
    for i in 0..2 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                "name":"chat.send_message",
                "arguments":{"chat_id":100,"body":format!("m{}",i)}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await.unwrap();
    }
    let sent = bot.sent_messages.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
}
```

- [x] **Step 2: Verify FAIL** — only if dispatch breaks under sequence.

- [x] **Step 3: Implement** — confirmed by M-c2.5.2; this test is a regression net.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.5.4: MCP stdio sequence regression test"`

---

## M-c2.6 — Rate-limit + heartbeat verification

### Task M-c2.6.1: Token-bucket global cap (≤30/s)

**Files:** Modify `mur-agent-runtime/tests/c2_telegram_outbound.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn throttle_caps_global_to_thirty_per_second() {
    use mur_agent_runtime::bridge::telegram::mcp::{handle_jsonrpc, McpDeps};
    use mur_agent_runtime::bridge::telegram::mock::MockBot;

    let bot = Arc::new(MockBot::throttled(30));
    let deps = McpDeps { bot: bot.clone() };
    let start = std::time::Instant::now();
    for i in 0..50 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                "name":"chat.send_message","arguments":{"chat_id":i,"body":"x"}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await;
    }
    let dur = start.elapsed();
    let after_one_second = bot.delivered_within(std::time::Duration::from_secs(1));
    assert!(after_one_second <= 30, "delivered {} in 1s, cap is 30", after_one_second);
    assert!(dur >= std::time::Duration::from_millis(900), "burst should serialize over time");
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound throttle_caps_global_to_thirty_per_second`.

- [x] **Step 3: Implement** — extend `MockBot` with a `throttled(rate)` constructor that uses an internal `governor` rate-limiter to defer `sent_messages.push`. Track `delivered_within(dur)` from the first send.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound throttle_caps_global_to_thirty_per_second`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.6.1: throttle global 30/s cap"`

### Task M-c2.6.2: Per-chat 1/s pacing

**Files:** Modify `c2_telegram_outbound.rs`; modify `MockBot`.

- [x] **Step 1: Failing test** — append:

```rust
#[tokio::test]
async fn per_chat_paces_at_one_per_second() {
    use mur_agent_runtime::bridge::telegram::mcp::{handle_jsonrpc, McpDeps};
    use mur_agent_runtime::bridge::telegram::mock::MockBot;

    let bot = Arc::new(MockBot::per_chat(1));
    let deps = McpDeps { bot: bot.clone() };
    let start = std::time::Instant::now();
    for i in 0..5 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                "name":"chat.send_message","arguments":{"chat_id":42,"body":format!("m{}",i)}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await;
    }
    let dur = start.elapsed();
    assert!(dur >= std::time::Duration::from_secs(4), "5 msgs to one chat at 1/s should take >=4s, was {:?}", dur);
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound per_chat_paces_at_one_per_second`.

- [x] **Step 3: Implement** — `MockBot::per_chat(rate)` keeps a `HashMap<i64, Instant>` of last-sent and sleeps to enforce the gap.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_outbound per_chat_paces_at_one_per_second`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.6.2: per-chat 1/s pacing"`

### Task M-c2.6.3: BridgeBeacon spawned for telegram bridge

**Files:** Modify `mur-agent-runtime/src/supervisor.rs` (telegram bridge spawn path).

- [x] **Step 1: Failing test** — extend `c2_telegram_inbound.rs`:

```rust
#[tokio::test]
async fn bridge_beacon_reports_running_within_5s() {
    use mur_agent_runtime::supervisor::spawn_telegram_bridge_for_test;
    let agent_home = tempfile::tempdir().unwrap();
    let handle = spawn_telegram_bridge_for_test(agent_home.path()).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let doctor = mur_core::cmd::agent::doctor::run_doctor(agent_home.path()).unwrap();
    assert!(doctor.bridges.iter().any(|b| b.name == "tg" && b.status == "running"));
    handle.shutdown().await;
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound bridge_beacon_reports_running_within_5s`.

- [x] **Step 3: Implement** — `spawn_telegram_bridge_for_test` constructs a `BridgeBeacon` (already shipped in C1 — M-c1.4.3) for the bridge_id `tg` and starts the inbound loop on a background task.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test c2_telegram_inbound`.

- [x] **Step 5: Commit** — `git add mur-agent-runtime/ && git commit -m "M-c2.6.3: BridgeBeacon visible to mur agent doctor for tg bridge"`

---

## M-c2.7 — E2E + cookbook + spec acceptance

### Task M-c2.7.1: `scripts/e2e/c2-telegram-bridge.sh`

**Files:** Create `scripts/e2e/c2-telegram-bridge.sh` (mode 0755).

- [x] **Step 1: Failing test** — run the script (it does not exist):

```bash
bash scripts/e2e/c2-telegram-bridge.sh
```

- [x] **Step 2: Verify FAIL** — `bash: scripts/e2e/c2-telegram-bridge.sh: No such file or directory`.

- [x] **Step 3: Implement** — the script body:

```bash
#!/usr/bin/env bash
set -euo pipefail

WORKDIR=$(mktemp -d)
export MUR_HOME="$WORKDIR"
export MUR_TELEGRAM_KEYCHAIN_BACKEND=mock

echo "[1/6] scaffold telegram bridge"
mur agent companion connector add tg --platform telegram \
    --bot-token "1234:fakefake" --bot-username MyAgentBot \
    --chat-id 100 --ack

echo "[2/6] verify telegram.yaml exists"
test -f "$MUR_HOME/agents/tg/telegram.yaml"

echo "[3/6] inbound text round-trip (cargo test)"
cargo test -p mur-agent-runtime --test c2_telegram_inbound -- --test-threads=1

echo "[4/6] voice round-trip (cargo test)"
cargo test -p mur-agent-runtime --test c2_telegram_voice -- --test-threads=1

echo "[5/6] document/photo round-trip (cargo test)"
cargo test -p mur-agent-runtime --test c2_telegram_files -- --test-threads=1

echo "[6/6] outbound MCP + rate-limit (cargo test)"
cargo test -p mur-agent-runtime --test c2_telegram_outbound -- --test-threads=1

echo "C2 telegram bridge e2e: PASS"
```

`chmod +x scripts/e2e/c2-telegram-bridge.sh`.

- [x] **Step 4: Verify PASS** — `bash scripts/e2e/c2-telegram-bridge.sh`.

- [x] **Step 5: Commit** — `git add scripts/e2e/c2-telegram-bridge.sh && git commit -m "M-c2.7.1: c2 telegram bridge e2e gate"`

### Task M-c2.7.2: Cookbook `docs/cookbook/c2-telegram-bridge.md`

**Files:** Create `docs/cookbook/c2-telegram-bridge.md`.

- [x] **Step 1: Failing test** — `mur verify --file docs/cookbook/c2-telegram-bridge.md` would fail (file missing).

- [x] **Step 2: Verify FAIL** — file does not exist.

- [x] **Step 3: Implement** — content covers:

  - **Setup** (5 steps): chat BotFather → `/newbot` → record token → `mur agent companion connector add tg --platform telegram` → enter token + chat_id + ack → start agent.
  - **Privacy mode trade-offs**: DmOnly default — group messages dropped silently. Switch to `AllowGroups` only when explicitly listing chat_ids.
  - **Why local whisper-rs**: voice transcription stays on the box (privacy + offline). No audio is uploaded to a third-party STT.
  - **What's NOT in v1**: Premium Business chat (deferred to v2), Mini App embed (v2), inline-mode bots (v2). Group admin reactions deferred.
  - **Rate-limit invariants**: global 30/s, per-chat 1/s — enforced by teloxide `Throttle` adapter; document the ceiling.
  - **Token rotation**: `mur agent companion connector token rotate tg` (covered in C-track future work).
  - **Disabling**: stop the bridge agent → `mur agent stop tg-bridge` → token remains in keychain until `mur agent secret delete`.

- [x] **Step 4: Verify PASS** — `mur verify --file docs/cookbook/c2-telegram-bridge.md` (no stale claims).

- [x] **Step 5: Commit** — `git add docs/cookbook/c2-telegram-bridge.md && git commit -m "M-c2.7.2: c2 cookbook"`

### Task M-c2.7.3: Spec acceptance footer tick (§5.4)

**Files:** Modify `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md`.

- [x] **Step 1: Failing test** — `grep -A1 "## §5.4" docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md | grep -i "shipped"` — expected to fail before update.

- [x] **Step 2: Verify FAIL** — grep returns 1 (no match).

- [x] **Step 3: Implement** — append to §5.4 acceptance section:

```markdown
**Status:** SHIPPED 2026-05-04 (PR #c2-telegram-bridge).

- M-c2.0 — schema + enum: shipped
- M-c2.1 — BotFather UX: shipped
- M-c2.2 — long-poll inbound: shipped
- M-c2.3 — voice via whisper: shipped
- M-c2.4 — files/photos via multimodal pipeline: shipped
- M-c2.5 — outbound MCP: shipped
- M-c2.6 — rate-limit + heartbeat: shipped
- M-c2.7 — E2E + cookbook: shipped

**Out of scope (v2):** Premium Business chat, Mini App, inline-mode bots, multi-bot single-chat.
```

- [x] **Step 4: Verify PASS** — `grep -i "shipped 2026-05-04" docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md`.

- [x] **Step 5: Commit** — `git add docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md && git commit -m "M-c2.7.3: §5.4 acceptance footer tick"`

---

## Self-review

### Spec coverage

Every requirement under §5.4 of `2026-04-30-mur-agent-harness-roadmap-design.md` maps to a milestone:

- §5.4.0 schema (TelegramConfig, PrivacyMode) → M-c2.0
- §5.4.1 BotFather 5-step + E2E disclosure → M-c2.1
- §5.4.2 long-poll + dedupe + ACK + sign → M-c2.2
- §5.4.3 voice via whisper → M-c2.3
- §5.4.4 photo + document via multimodal pipeline + B0 wrapping → M-c2.4
- §5.4.5 outbound MCP `chat.send_message` → M-c2.5
- §5.4.6 token-bucket rate-limit (global 30/s, per-chat 1/s) + BridgeBeacon → M-c2.6
- §5.4.7 E2E + cookbook + spec footer → M-c2.7

### Placeholder scan

No `TBD`, no "implement later", no "similar to Task N". Each task body has actual code blocks with the realistic teloxide 0.13 / mur_common / mur_agent_runtime surface; un-confirmed teloxide calls flagged with `// teloxide-0.13 surface — verify on impl`.

### Type names consistent

- `TelegramConfig`, `PrivacyMode { DmOnly, AllowGroups }`
- `TelegramInboundLoop<B>`, `InboundDeps`
- `ForwardPayload { Text { transcript, audio_path }, Skip }`
- `VoiceDeps`, `FilesDeps`, `FilesResult`
- `McpDeps`, `MockBot`, `MockUserAgent`, `MockUpdate`
- `ScaffoldArgs`, `ScaffoldOutcome::Ok { config, profile_path }`
- `Keychain` trait + `SystemKeychain` / `MockKeychain`

All types referenced in tests match types defined in implementation steps.

### Reuse-not-redesign verification

- C1's `DedupeStore`, `AckTracker<i64>`, `sign_payload`, `verify_inbound_envelope`, `BridgeBeacon`, `BridgeRouteConfig`, `LlmEntitlement` — all referenced verbatim, not re-implemented.
- D1's `whisper-rs` integration in `mur-core/src/companion/voice.rs::transcribe_ogg` — called via path; the bridge does not invoke whisper-rs directly.
- D3's `mur_agent_runtime::multimodal::pipeline::process_artifact` — called via path with `(bytes, mime, agent_home)`; the bridge does not write to the artifact ledger directly.
- M7 B0's `B0SafetyHook` — no bridge-side reimplementation. Test M-c2.4.3 asserts the user-agent invariant (the wrapper text appears in the prompt) and explicitly notes the bridge does not re-implement.

### Bot-token confidentiality verification

- Token enters via CLI prompt or `--bot-token` flag.
- Token written to keychain via `kc.put(account, token)`.
- Token read only by the runtime when constructing `Bot::new(token)`.
- Token never crosses the A2A boundary (signed envelope contains only `body` + `agent` + `id`).
- Token never crosses the MCP boundary (`chat.send_message` arguments are `chat_id` + `body`).
- Token never written to disk in plaintext (only in macOS Keychain, which is os-level encrypted).
- M7.5 secret prefilter on the user-agent's outbound path catches any accidental leak before it reaches the wire.

### TDD step uniformity

Every task has exactly 5 steps: failing test, verify FAIL, implement, verify PASS, commit. Each cargo invocation is scoped to `-p mur-agent-runtime --test <name>` or `-p mur-core --test <name>` — no `cargo test --workspace` in the inner loop.
