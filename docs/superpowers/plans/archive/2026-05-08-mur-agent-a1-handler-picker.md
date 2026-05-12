# mur Agent A1 — Config-Driven Handler Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Add a `hooks:` block to `profile.yaml` that controls which built-in handlers join the `HookChain`, and formally wire the two currently-disconnected handlers (`CompanionVoiceHook`, `VoiceInputHook`) based on feature flags.

**Architecture:** Four stacked PRs off `main`. A1.1 adds `HooksConfig` to `mur-common`. A1.2 adds `build_chain()` to `mur-agent-runtime` and wires the optional handlers. A1.3 replaces the hardcoded chain assembly in `supervisor.rs`. A1.4 adds `mur agent hooks show [--json]` to `mur-core`. Cascade-merge bottom-up (squash + retarget to main) as in D3.

**Tech Stack:** Rust 2024, `serde`, `clap`, existing `CompanionVoiceHook` / `VoiceInputHook` / `LedgerHook` / `B0SafetyHook` / `TelemetryHook` in `mur-agent-runtime`.

**Commit prefix:** `A1.<milestone>.<task>: <subject>`

**Branch policy:**
- `feat/mur-agent-a1-handler-picker-a1.1-schema`
- `feat/mur-agent-a1-handler-picker-a1.2-builder`
- `feat/mur-agent-a1-handler-picker-a1.3-supervisor`
- `feat/mur-agent-a1-handler-picker-a1.4-cli`

Each stacks on the previous. Merge order: A1.1 → A1.2 → A1.3 → A1.4.

**Key invariant:** `B0SafetyHook` and `TelemetryHook` are **always** in the chain (positions 0 and 1). They cannot be suppressed by any `hooks:` config. `LedgerHook` is default-on but can be disabled. `CompanionVoiceHook` / `VoiceInputHook` auto-wire from `companion.enabled` / `voice.enabled` and can be overridden.

**Test command for mur-agent-gui (workspace-excluded):**
```bash
PROTOC=/opt/homebrew/bin/protoc cargo test --manifest-path mur-agent-gui/src-tauri/Cargo.toml
```

**cargo path:** `/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo` (not on PATH; use full path or `PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`).

---

## File Map

```
mur-common/src/
  hooks_config.rs                      CREATE — HooksConfig struct
  lib.rs                               MODIFY — pub mod hooks_config; pub use hooks_config::HooksConfig;
  agent.rs                             MODIFY — add `pub hooks: HooksConfig` to AgentProfile

mur-common/tests/
  hooks_config.rs                      CREATE — 4 unit tests

mur-agent-runtime/src/hooks/
  builder.rs                           CREATE — build_chain(profile, agent_home, mur_home)
  mod.rs                               MODIFY — pub mod builder;

mur-agent-runtime/src/
  supervisor.rs                        MODIFY — replace inline HookChain::new([...]) with build_chain()

mur-agent-runtime/tests/
  hook_builder.rs                      CREATE — 6 unit tests

mur-core/src/cmd/
  agent_hooks.rs                       CREATE — cmd_hooks_show(name, json) impl
  agent.rs                             MODIFY — (no change needed; hooks show wires through main.rs)

mur-core/src/
  main.rs                              MODIFY — add Hooks { action } arm + AgentHooksAction enum + dispatch
```

---

## Milestone A1.1 — `HooksConfig` schema in mur-common

**Branch:** `feat/mur-agent-a1-handler-picker-a1.1-schema` off `main`

### Task A1.1.1: Add `HooksConfig` + `AgentProfile.hooks` field

**Files:**
- Create: `mur-common/src/hooks_config.rs`
- Modify: `mur-common/src/lib.rs`
- Modify: `mur-common/src/agent.rs`
- Create: `mur-common/tests/hooks_config.rs`

- [x] **Step 1: Write the failing tests**

```rust
// mur-common/tests/hooks_config.rs
use mur_common::HooksConfig;

#[test]
fn hooks_config_defaults() {
    let cfg = HooksConfig::default();
    assert!(cfg.ledger, "ledger default is true");
    assert_eq!(cfg.companion_voice, None, "companion_voice default is None (auto)");
    assert_eq!(cfg.voice_input, None, "voice_input default is None (auto)");
}

#[test]
fn hooks_config_roundtrip_empty_yaml() {
    // A profile.yaml with no hooks: block at all should deserialize to defaults.
    let yaml = "ledger: true\n";
    let cfg: HooksConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(cfg.ledger);
    assert_eq!(cfg.companion_voice, None);
}

#[test]
fn hooks_config_partial_override_ledger_false() {
    let yaml = "ledger: false\n";
    let cfg: HooksConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(!cfg.ledger);
    assert_eq!(cfg.companion_voice, None);
    assert_eq!(cfg.voice_input, None);
}

#[test]
fn hooks_config_explicit_companion_voice_true() {
    let yaml = "companion_voice: true\n";
    let cfg: HooksConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(cfg.ledger, "ledger still defaults to true");
    assert_eq!(cfg.companion_voice, Some(true));
    assert_eq!(cfg.voice_input, None);
}
```

- [x] **Step 2: Run, confirm fail**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-common --test hooks_config 2>&1 | tail -5
```
Expected: `error[E0432]: unresolved import 'mur_common::HooksConfig'`

- [x] **Step 3: Create `mur-common/src/hooks_config.rs`**

```rust
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Whether the outbox `LedgerHook` runs. Default: true.
    #[serde(default = "default_true")]
    pub ledger: bool,

    /// Whether `CompanionVoiceHook` runs.
    /// `None` = auto-wire from `profile.companion.enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_voice: Option<bool>,

    /// Whether `VoiceInputHook` runs.
    /// `None` = auto-wire from `profile.voice.enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_input: Option<bool>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            ledger: true,
            companion_voice: None,
            voice_input: None,
        }
    }
}
```

- [x] **Step 4: Wire into `mur-common/src/lib.rs`**

Find the block of `pub mod` declarations and append:

```rust
pub mod hooks_config;
pub use hooks_config::HooksConfig;
```

- [x] **Step 5: Add `hooks` field to `AgentProfile` in `mur-common/src/agent.rs`**

Find `pub voice: VoiceConfig,` (around line 49) and add directly after it:

```rust
    /// A1: config-driven handler picker. Absent block = all defaults.
    #[serde(default)]
    pub hooks: HooksConfig,
```

- [x] **Step 6: Run tests, confirm pass**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-common --test hooks_config 2>&1 | tail -10
```
Expected: `4 passed`.

- [x] **Step 7: Run full mur-common suite + lint**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-common && \
  cargo clippy -p mur-common -- -D warnings && \
  cargo fmt --check
```
Expected: all pass.

- [x] **Step 8: Commit**

```bash
git add mur-common/src/hooks_config.rs mur-common/src/lib.rs \
        mur-common/src/agent.rs mur-common/tests/hooks_config.rs
git commit -m "A1.1.1: HooksConfig schema + AgentProfile.hooks field"
```

---

## Milestone A1.2 — `build_chain()` builder in mur-agent-runtime

**Branch:** `feat/mur-agent-a1-handler-picker-a1.2-builder` off `a1.1-schema`

### Task A1.2.1: `hooks::builder` module with unit tests

**Files:**
- Create: `mur-agent-runtime/src/hooks/builder.rs`
- Modify: `mur-agent-runtime/src/hooks/mod.rs`
- Create: `mur-agent-runtime/tests/hook_builder.rs`

- [x] **Step 1: Write the failing tests**

```rust
// mur-agent-runtime/tests/hook_builder.rs
use mur_agent_runtime::hooks::builder::build_chain;
use mur_common::agent::{AgentProfile, CompanionConfig, VoiceConfig};
use mur_common::HooksConfig;
use std::path::Path;

/// Helper: a minimal AgentProfile with sane defaults, no companion, no voice.
fn base_profile() -> AgentProfile {
    let yaml = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bare_profile.yaml")
    ).unwrap();
    serde_yaml::from_str(&yaml).unwrap()
}

fn tmp_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    (tempfile::TempDir::new().unwrap(), tempfile::TempDir::new().unwrap())
}

#[test]
fn mandatory_handlers_always_present() {
    let profile = base_profile();
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    let names = chain.names();
    assert_eq!(names[0], "TelemetryHook", "TelemetryHook must be first");
    assert_eq!(names[1], "B0SafetyHook", "B0SafetyHook must be second");
}

#[test]
fn ledger_on_by_default() {
    let profile = base_profile();
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(chain.names().contains(&"LedgerHook"), "LedgerHook on by default");
}

#[test]
fn ledger_disabled_via_hooks_config() {
    let mut profile = base_profile();
    profile.hooks.ledger = false;
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(!chain.names().contains(&"LedgerHook"), "LedgerHook must be absent");
}

#[test]
fn companion_voice_auto_wires_when_companion_enabled() {
    let mut profile = base_profile();
    profile.companion.enabled = true;
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(chain.names().contains(&"CompanionVoiceHook"),
            "CompanionVoiceHook must auto-wire when companion.enabled = true");
}

#[test]
fn companion_voice_explicit_override_when_companion_disabled() {
    let mut profile = base_profile();
    profile.companion.enabled = false;
    profile.hooks.companion_voice = Some(true);
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(chain.names().contains(&"CompanionVoiceHook"),
            "hooks.companion_voice=true must override companion.enabled=false");
}

#[test]
fn voice_input_suppressed_when_model_absent() {
    let mut profile = base_profile();
    profile.voice.enabled = true;
    // mur_tmp has no voices/ dir → model not found → handler skipped
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    // No panic; VoiceInputHook simply absent with a tracing::warn
    assert!(!chain.names().contains(&"VoiceInputHook"),
            "VoiceInputHook skipped gracefully when model absent");
}
```

- [x] **Step 2: Create `mur-agent-runtime/tests/fixtures/bare_profile.yaml`**

```yaml
schema: 1
id: "01900000-0000-7000-8000-000000000001"
name: "test-agent"
display_name: "Test Agent"
version: "0.1.0"
persona:
  category: general
  traits:
    tone: neutral
    style: concise
    personality: helpful
sys_prompt_file: "sys_prompt.md"
model:
  provider: ollama
  model: qwen3:14b
transport:
  stdio: { enabled: true }
communication:
  max_response_length: 4096
  response_format: text
capabilities: []
entitlements:
  llm:
    mode: allowed
  network:
    outbound: { allow_hosts: [] }
    inbound: {}
  filesystem:
    read: []
    write: []
  process:
    spawn: []
  resource_limits: {}
notifications:
  on_task_complete: false
  on_error: false
retry:
  max_attempts: 3
  backoff_ms: 500
lifecycle:
  execution: on_demand
  schedule: []
  idle_triggers: []
identity: {}
file_transfer: {}
deployment: {}
companion:
  enabled: false
  locale: "en-US"
voice:
  enabled: false
created_at: "2026-05-08T00:00:00Z"
updated_at: "2026-05-08T00:00:00Z"
```

- [x] **Step 3: Run tests, confirm fail**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-agent-runtime --test hook_builder 2>&1 | tail -5
```
Expected: `error[E0432]: unresolved import 'mur_agent_runtime::hooks::builder'`

- [x] **Step 4: Create `mur-agent-runtime/src/hooks/builder.rs`**

```rust
//! A1 — config-driven chain builder.
//!
//! Mandatory tier (always present, fixed order):
//!   TelemetryHook (pos 0) → B0SafetyHook (pos 1)
//!
//! Optional tier (auto-wire from feature flags, overridable via `profile.hooks`):
//!   LedgerHook → CompanionVoiceHook → VoiceInputHook

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mur_common::AgentProfile;
use mur_common::companion::Formality;

use super::{Hook, HookChain, b0::B0SafetyHook, companion_voice::CompanionVoiceHook,
            ledger::LedgerHook, telemetry::TelemetryHook, voice_input::VoiceInputHook};
use crate::companion::voice::{VoiceInput, compose_with_overrides};
use crate::voice::stt::{VadGate, WhisperStt};

/// Build the `HookChain` for `profile`.
///
/// * `agent_home` — `~/.mur/agents/<name>/`
/// * `mur_home`   — `~/.mur/`
pub fn build_chain(
    profile: &AgentProfile,
    agent_home: &Path,
    mur_home: &Path,
) -> HookChain {
    let cfg = &profile.hooks;

    let mut chain: Vec<Arc<dyn Hook>> = vec![
        Arc::new(TelemetryHook::new()) as Arc<dyn Hook>,
        Arc::new(B0SafetyHook::new()),
    ];

    if cfg.ledger {
        chain.push(Arc::new(LedgerHook::new()));
    }

    let want_companion_voice = cfg.companion_voice
        .unwrap_or(profile.companion.enabled);
    if want_companion_voice {
        let formality_str = match profile.companion.voice_overrides.formality {
            Some(Formality::Formal) => "formal",
            _ => "casual",
        };
        let extra = profile.companion.voice_overrides.extra_instructions
            .as_deref()
            .unwrap_or("");
        let first_memory = profile.companion.onboarding.first_memory
            .as_ref()
            .map(|m| m.text.as_str());
        let rendered = compose_with_overrides(
            Some(agent_home),
            Some(mur_home),
            VoiceInput {
                relationship: profile.companion.relationship,
                locale: &profile.companion.locale,
                name_for_user: &profile.display_name,
                first_memory,
                formality: formality_str,
                extra_instructions: extra,
            },
        );
        chain.push(Arc::new(CompanionVoiceHook::new(Arc::new(rendered))));
    }

    let want_voice_input = cfg.voice_input
        .unwrap_or(profile.voice.enabled);
    if want_voice_input {
        let model_path = mur_home.join("voices/whisper-large-v3-turbo-q5_1.bin");
        match WhisperStt::new(&model_path) {
            Ok(stt) => chain.push(Arc::new(VoiceInputHook::new(
                stt,
                VadGate::default(),
                profile.voice.input_device.clone(),
                Duration::from_secs(30),
            ))),
            Err(e) => tracing::warn!(
                model = %model_path.display(),
                error = %e,
                "VoiceInputHook skipped: whisper model not found; \
                 run `mur voice install` to download"
            ),
        }
    }

    HookChain::new(chain)
}
```

- [x] **Step 5: Add `pub mod builder;` to `mur-agent-runtime/src/hooks/mod.rs`**

At the end of the existing `pub mod` block (after `pub mod voice_input;`), add:

```rust
pub mod builder;
```

- [x] **Step 6: Run tests, confirm pass**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-agent-runtime --test hook_builder 2>&1 | tail -15
```
Expected: `6 passed`.

- [x] **Step 7: Run full runtime suite + lint**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-agent-runtime && \
  cargo clippy -p mur-agent-runtime -- -D warnings && \
  cargo fmt --check
```

- [x] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/hooks/builder.rs \
        mur-agent-runtime/src/hooks/mod.rs \
        mur-agent-runtime/tests/hook_builder.rs \
        mur-agent-runtime/tests/fixtures/bare_profile.yaml
git commit -m "A1.2.1: build_chain() — config-driven HookChain assembly"
```

---

## Milestone A1.3 — Wire supervisor.rs to use `build_chain()`

**Branch:** `feat/mur-agent-a1-handler-picker-a1.3-supervisor` off `a1.2-builder`

### Task A1.3.1: Replace inline chain assembly in supervisor.rs

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`

- [x] **Step 1: Read the existing chain assembly block**

In `supervisor.rs` around line 177–189, you will find:

```rust
    // 4a. Build the A0 hook chain. M0 ships TelemetryHook + B0SafetyHook
    //     (no-op stub) + LedgerHook (no-op stub). CompanionVoiceHook is
    //     registered when the companion subsystem renders its voice (out
    //     of M0 scope; companion phase 1.1's reactive path remains
    //     unchanged). The on_startup observe-hooks fire before transports
    //     bind so telemetry includes the create_agent span event.
    let telemetry_emitter: Arc<dyn TelemetryEmitter> =
        Arc::new(WriterTelemetryEmitter::new(writer.sender()));
    let hook_chain = HookChain::new(vec![
        Arc::new(TelemetryHook::new()) as Arc<dyn Hook>,
        Arc::new(B0SafetyHook::new()),
        Arc::new(LedgerHook::new()),
    ]);
```

- [x] **Step 2: Locate `mur_home` in supervisor.rs**

The supervisor computes `mur_home` around line 90:
```rust
let mur_home = std::env::var_os("MUR_HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
```
This variable is in scope when chain assembly happens (line ~185).

- [x] **Step 3: Replace the chain assembly block**

Replace the `hook_chain` assignment (lines ~184–189) with:

```rust
    // 4a. Build the A1 config-driven hook chain.
    //     Mandatory: TelemetryHook → B0SafetyHook (always).
    //     Optional: LedgerHook / CompanionVoiceHook / VoiceInputHook
    //     controlled by profile.hooks + auto-wire from feature flags.
    let telemetry_emitter: Arc<dyn TelemetryEmitter> =
        Arc::new(WriterTelemetryEmitter::new(writer.sender()));
    let hook_chain = crate::hooks::builder::build_chain(
        &profile.inner,
        &agent_home,
        &mur_home,
    );
```

- [x] **Step 4: Remove now-unused imports from supervisor.rs**

Remove `LedgerHook` from the existing hooks import at the top of supervisor.rs:

```rust
// Before:
use crate::hooks::{
    Hook, HookChain, HookCtx, ShutdownReason, TelemetryEmitter, b0::B0SafetyHook,
    ledger::LedgerHook, telemetry::TelemetryHook,
};
// After:
use crate::hooks::{
    Hook, HookChain, HookCtx, ShutdownReason, TelemetryEmitter,
};
```

(builder.rs owns all handler imports now.)

- [x] **Step 5: Build the workspace to confirm no type errors**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo build -p mur-agent-runtime 2>&1 | grep -E "^error" | head -10
```
Expected: zero errors.

- [x] **Step 6: Run full runtime suite + lint**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-agent-runtime && \
  cargo clippy -p mur-agent-runtime -- -D warnings && \
  cargo fmt --check
```

- [x] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs
git commit -m "A1.3.1: supervisor uses build_chain() — replaces inline HookChain::new"
```

---

## Milestone A1.4 — `mur agent hooks show [--json]` CLI

**Branch:** `feat/mur-agent-a1-handler-picker-a1.4-cli` off `a1.3-supervisor`

### Task A1.4.1: `agent_hooks.rs` command impl + main.rs wiring

**Files:**
- Create: `mur-core/src/cmd/agent_hooks.rs`
- Modify: `mur-core/src/main.rs`

**Reference pattern:** `mur-core/src/cmd/agent_schedule.rs` (reads profile, prints table).

- [x] **Step 1: Write the failing CLI test**

```rust
// mur-core/tests/cmd_hooks_show.rs
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use std::fs;

fn make_agent_dir(tmp: &TempDir, profile_yaml: &str) -> String {
    let agent_name = "hooks-test-agent";
    let mur_home = tmp.path().join(".mur");
    let agent_home = mur_home.join("agents").join(agent_name);
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), profile_yaml).unwrap();
    agent_name.to_string()
}

const BARE_PROFILE: &str = r#"
schema: 1
id: "01900000-0000-7000-8000-000000000002"
name: "hooks-test-agent"
display_name: "Hooks Test"
version: "0.1.0"
persona: { category: general, traits: { tone: neutral, style: concise, personality: helpful } }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, model: qwen3:14b }
transport: { stdio: { enabled: true } }
communication: { max_response_length: 4096, response_format: text }
capabilities: []
entitlements:
  llm: { mode: allowed }
  network: { outbound: { allow_hosts: [] }, inbound: {} }
  filesystem: { read: [], write: [] }
  process: { spawn: [] }
  resource_limits: {}
notifications: { on_task_complete: false, on_error: false }
retry: { max_attempts: 3, backoff_ms: 500 }
lifecycle: { execution: on_demand, schedule: [], idle_triggers: [] }
identity: {}
file_transfer: {}
deployment: {}
companion: { enabled: false, locale: "en-US" }
voice: { enabled: false }
created_at: "2026-05-08T00:00:00Z"
updated_at: "2026-05-08T00:00:00Z"
"#;

#[test]
fn hooks_show_table_mandatory_always_listed() {
    let tmp = TempDir::new().unwrap();
    let agent_name = make_agent_dir(&tmp, BARE_PROFILE);
    let mur_home = tmp.path().join(".mur");

    Command::cargo_bin("mur").unwrap()
        .env("MUR_HOME", &mur_home)
        .args(["agent", "hooks", "show", &agent_name])
        .assert()
        .success()
        .stdout(predicate::str::contains("TelemetryHook"))
        .stdout(predicate::str::contains("B0SafetyHook"))
        .stdout(predicate::str::contains("mandatory"));
}

#[test]
fn hooks_show_json_parses() {
    let tmp = TempDir::new().unwrap();
    let agent_name = make_agent_dir(&tmp, BARE_PROFILE);
    let mur_home = tmp.path().join(".mur");

    let output = Command::cargo_bin("mur").unwrap()
        .env("MUR_HOME", &mur_home)
        .args(["agent", "hooks", "show", &agent_name, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let arr = v.as_array().expect("output must be JSON array");
    assert!(arr.len() >= 2, "at least 2 entries (TelemetryHook + B0SafetyHook)");
    assert_eq!(arr[0]["name"], "TelemetryHook");
    assert_eq!(arr[0]["tier"], "mandatory");
    assert_eq!(arr[0]["enabled"], true);
}
```

- [x] **Step 2: Run, confirm fail**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-core --test cmd_hooks_show 2>&1 | tail -5
```
Expected: compile error (no `hooks show` subcommand yet).

- [x] **Step 3: Create `mur-core/src/cmd/agent_hooks.rs`**

```rust
//! A1 — `mur agent hooks show [--json]`

use anyhow::{Context, Result};
use serde::Serialize;

use mur_common::HooksConfig;
use mur_common::agent::AgentProfile;

#[derive(Serialize)]
struct HookEntry {
    name: &'static str,
    tier: &'static str,
    enabled: bool,
    source: String,
}

fn chain_entries(profile: &AgentProfile) -> Vec<HookEntry> {
    let cfg = &profile.hooks;
    let mut out = vec![
        HookEntry {
            name: "TelemetryHook",
            tier: "mandatory",
            enabled: true,
            source: "hardcoded".into(),
        },
        HookEntry {
            name: "B0SafetyHook",
            tier: "mandatory",
            enabled: true,
            source: "hardcoded".into(),
        },
        HookEntry {
            name: "LedgerHook",
            tier: "optional",
            enabled: cfg.ledger,
            source: "hooks.ledger".into(),
        },
    ];

    let companion_voice_on = cfg.companion_voice.unwrap_or(profile.companion.enabled);
    out.push(HookEntry {
        name: "CompanionVoiceHook",
        tier: "optional",
        enabled: companion_voice_on,
        source: match cfg.companion_voice {
            Some(v) => format!("hooks.companion_voice = {v}"),
            None => format!("auto ← companion.enabled = {}", profile.companion.enabled),
        },
    });

    let voice_input_on = cfg.voice_input.unwrap_or(profile.voice.enabled);
    out.push(HookEntry {
        name: "VoiceInputHook",
        tier: "optional",
        enabled: voice_input_on,
        source: match cfg.voice_input {
            Some(v) => format!("hooks.voice_input = {v}"),
            None => format!("auto ← voice.enabled = {}", profile.voice.enabled),
        },
    });

    out
}

pub fn cmd_hooks_show(name: &str, json: bool) -> Result<()> {
    let mur_home = crate::paths::mur_root();
    let agent_home = mur_home.join("agents").join(name);
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: AgentProfile = serde_yaml::from_str(&yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    let entries = chain_entries(&profile);

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!("── Hook chain for agent \"{name}\" ────────────────────────────────");
    for e in &entries {
        let state = if e.enabled { "on " } else { "off" };
        println!("  [{:9}]  {:20}  {}  ({})", e.tier, e.name, state, e.source);
    }
    println!("──────────────────────────────────────────────────────────────────");
    Ok(())
}
```

- [x] **Step 4: Check `crate::paths::mur_root` exists in mur-core**

```bash
grep -rn "pub fn mur_root\|fn mur_root" /Volumes/Firecuda4tb/Projects/mur/mur-core/src/ | head -5
```

If `mur_root()` doesn't exist, find how the agent command reads `mur_home` and use the same pattern. Typical pattern is:

```rust
fn mur_root() -> std::path::PathBuf {
    std::env::var_os("MUR_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"))
}
```

Add this as a private helper at the top of `agent_hooks.rs` if `paths::mur_root` doesn't exist:

```rust
fn mur_root() -> std::path::PathBuf {
    std::env::var_os("MUR_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"))
}
```

And replace `crate::paths::mur_root()` with `mur_root()` in `cmd_hooks_show`.

- [x] **Step 5: Wire into `mur-core/src/main.rs`**

Find the `Agent` subcommand dispatch (search for `AgentCmd` or the `agent` match arm). Add alongside the existing `Schedule` and `Companion` arms:

**5a.** Add to the `pub mod cmd {` or the imports section at the top of main.rs:
```rust
// (in the cmd module or near other agent_* imports)
mod agent_hooks;
```

**5b.** Find the `AgentCmd` enum (search for `/// Manage lifecycle cron schedule entries`) and add a new variant after `Schedule`:

```rust
    /// Show the active hook chain for an agent (A1)
    Hooks {
        #[command(subcommand)]
        action: AgentHooksAction,
    },
```

**5c.** Add the `AgentHooksAction` enum near `AgentScheduleAction`:

```rust
#[derive(clap::Subcommand, Debug)]
enum AgentHooksAction {
    /// Print the active hook chain (table or JSON)
    Show {
        /// Agent name
        name: String,
        /// Emit machine-readable JSON array
        #[arg(long)]
        json: bool,
    },
}
```

**5d.** Add the dispatch arm in the agent match block (near the `AgentCmd::Schedule` arm):

```rust
            AgentCmd::Hooks { action } => match action {
                AgentHooksAction::Show { name, json } => {
                    cmd::agent_hooks::cmd_hooks_show(&name, json)?
                }
            },
```

- [x] **Step 6: Run tests, confirm pass**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-core --test cmd_hooks_show 2>&1 | tail -15
```
Expected: `2 passed`.

- [x] **Step 7: Smoke-test manually**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo run -p mur-core -- agent hooks show <your-agent-name> 2>&1 | head -10
```
Expected: table printed with TelemetryHook + B0SafetyHook shown as mandatory.

- [x] **Step 8: Run full mur-core suite + lint**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p mur-core && \
  cargo clippy -p mur-core -- -D warnings && \
  cargo fmt --check
```

- [x] **Step 9: Run workspace-level test**

```bash
PATH="/Users/david/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test --workspace 2>&1 | tail -20
```
Expected: all pass.

- [x] **Step 10: Commit**

```bash
git add mur-core/src/cmd/agent_hooks.rs mur-core/src/main.rs \
        mur-core/tests/cmd_hooks_show.rs
git commit -m "A1.4.1: mur agent hooks show [--json] — A1 CLI"
```

---

## Cascade Merge (bottom-up)

Merge order: A1.1 → A1.2 → A1.3 → A1.4.

For each PR (starting from A1.1):

```bash
# 1. Open PR targeting main (or previous-milestone branch)
gh pr create --title "feat(common): A1.1 HooksConfig schema" \
  --base main --head feat/mur-agent-a1-handler-picker-a1.1-schema

# 2. After review, squash-merge
gh pr merge <PR#> --squash --delete-branch

# 3. Retarget the next PR to main
gh pr edit <next-PR#> --base main
```

Repeat for A1.2, A1.3, A1.4.

After all four PRs are merged, bump the workspace version:

```bash
# In Cargo.toml workspace [workspace.package] section:
# version = "2.13.0" → "2.14.0"
git add Cargo.toml Cargo.lock
git commit -m "chore: bump workspace version 2.13.0 → 2.14.0 (A1 handler picker)"
git tag -a v2.14.0 -m "A1 config-driven handler picker"
git push origin main --tags
```

---

## Self-Review Checklist

### Spec coverage
- [x] §3.1 `HooksConfig` struct — Task A1.1.1
- [x] §3.2 `AgentProfile.hooks` field — Task A1.1.1
- [x] §3.3 YAML examples — covered by tests in A1.1.1
- [x] §4 `build_chain()` with correct constructor calls — Task A1.2.1
- [x] §4 `mur_root` derivation note — A1.3.1 uses supervisor's existing `mur_home`
- [x] §4 VoiceInputHook graceful skip on missing model — Task A1.2.1
- [x] §5 `mur agent hooks show` table + `--json` — Task A1.4.1
- [x] §7 All 10 unit tests (4 common + 6 builder + 2 CLI) — Tasks A1.1.1, A1.2.1, A1.4.1
- [x] §8 All file changes listed — File Map section
- [x] §9 Backward-compat (`#[serde(default)]`) — Task A1.1.1 step 5

### Type consistency
- `build_chain(profile: &AgentProfile, agent_home: &Path, mur_home: &Path) -> HookChain` — used consistently in A1.2 and A1.3
- `cmd_hooks_show(name: &str, json: bool) -> Result<()>` — matches A1.4 wiring
- `HooksConfig.ledger: bool`, `.companion_voice: Option<bool>`, `.voice_input: Option<bool>` — consistent across all tasks
- `CompanionVoiceHook::new(Arc<String>)` — matches actual constructor in `companion_voice.rs:26`
- `VoiceInputHook::new(WhisperStt, VadGate, Option<String>, Duration)` — matches `voice_input.rs:48`
- `compose_with_overrides(Option<&Path>, Option<&Path>, VoiceInput)` — matches `voice.rs:38`
