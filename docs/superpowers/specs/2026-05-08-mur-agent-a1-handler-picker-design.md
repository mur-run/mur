# mur Agent A1 — Config-Driven Handler Picker Design

**Date:** 2026-05-08
**Status:** Draft
**Predecessor:** A0 frozen hook surface (`docs/superpowers/specs/2026-04-30-mur-agent-hooks-a0.md`)
**Roadmap ref:** `2026-04-30-mur-agent-harness-roadmap-design.md` §3.3

---

## 1. Goal

Add a `hooks:` block to `profile.yaml` / `AgentProfile` that controls which built-in handlers are included in the `HookChain` at agent startup. No user-defined Rust code; no change to the A0 `Hook` trait surface.

Two concrete wins this unlocks:

1. **CompanionVoiceHook and VoiceInputHook are formally wired** — they exist as modules since D1/D5 but have never been added to the chain in `supervisor.rs`. A1 makes them first-class chain members, activated automatically from existing feature flags.
2. **Power users can suppress optional handlers** — e.g. a bare-metal bridge agent can set `hooks.ledger: false` to skip outbox ledger writes.

---

## 2. Design Decisions

### 2.1 Two-tier handler model

| Tier | Handlers | Configurable? |
|---|---|---|
| **Mandatory** | `TelemetryHook` (pos 0), `B0SafetyHook` (pos 1) | Never — security and observability invariants |
| **Optional** | `LedgerHook`, `CompanionVoiceHook`, `VoiceInputHook` | Yes — via `hooks:` config + auto-wire |

Mandatory handlers always appear first, in fixed order. The A0 trait surface is unchanged.

### 2.2 Convention over configuration

Optional handlers auto-wire from existing feature flags — no YAML required for the common case:

| Handler | Default | Auto-wire source |
|---|---|---|
| `LedgerHook` | **on** | always (default `true`) |
| `CompanionVoiceHook` | **off** | `profile.companion.enabled` |
| `VoiceInputHook` | **off** | `profile.voice.enabled` |

The `hooks:` block only needs to be written when overriding a default.

### 2.3 Chain order

Fixed order within each tier:

```
TelemetryHook → B0SafetyHook → LedgerHook → CompanionVoiceHook → VoiceInputHook
```

Rationale: telemetry first (spans the whole chain); B0 second (security gate fires before any mutation); optional handlers after mandatory. Per-method ordering is A3 scope.

---

## 3. Schema

### 3.1 `HooksConfig` (mur-common)

```rust
// mur-common/src/hooks_config.rs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Whether the outbox LedgerHook runs. Default: true.
    #[serde(default = "default_true")]
    pub ledger: bool,

    /// Whether CompanionVoiceHook runs.
    /// `None` = auto (follows `profile.companion.enabled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_voice: Option<bool>,

    /// Whether VoiceInputHook runs.
    /// `None` = auto (follows `profile.voice.enabled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_input: Option<bool>,
}

fn default_true() -> bool { true }

impl Default for HooksConfig {
    fn default() -> Self {
        Self { ledger: true, companion_voice: None, voice_input: None }
    }
}
```

`HooksConfig` is re-exported from `mur_common` alongside `AgentProfile`.

### 3.2 `AgentProfile` addition (mur-common)

```rust
// mur-common/src/agent.rs — add field to AgentProfile
#[serde(default)]
pub hooks: HooksConfig,
```

`#[serde(default)]` preserves backward-compat: existing `profile.yaml` files without a `hooks:` block load cleanly with all defaults applied.

### 3.3 Example `profile.yaml` fragments

```yaml
# Most agents — no hooks block needed; defaults are correct.

# Bridge agent (no companion, no ledger):
hooks:
  ledger: false

# Force companion_voice on even if companion.enabled = false (edge case):
hooks:
  companion_voice: true

# Explicitly disable voice_input even when voice.enabled = true:
hooks:
  voice_input: false
```

---

## 4. Chain Builder

Move chain assembly out of `supervisor.rs` into a dedicated builder function. This isolates the "which handlers run" policy from the supervisor's transport/lifecycle code.

`TelemetryHook` reads its emitter from `HookCtx` at call time (not from its constructor), so `build_chain` needs no emitter parameter. `CompanionVoiceHook` requires a pre-rendered voice string; `VoiceInputHook` requires a loaded `WhisperStt` model. Both are initialised inside the builder using `agent_home` and `mur_root`.

```rust
// mur-agent-runtime/src/hooks/builder.rs

use std::sync::Arc;
use std::path::Path;
use anyhow::Result;
use mur_common::AgentProfile;
use super::{
    Hook, HookChain,
    b0::B0SafetyHook,
    companion_voice::CompanionVoiceHook,
    ledger::LedgerHook,
    telemetry::TelemetryHook,
    voice_input::VoiceInputHook,
};
use crate::companion::voice::{VoiceInput, compose_with_overrides};
use crate::voice::stt::{VadGate, WhisperStt};

/// Build the `HookChain` for `profile`.
///
/// Chain order (fixed):
///   mandatory: TelemetryHook → B0SafetyHook
///   optional:  LedgerHook → CompanionVoiceHook → VoiceInputHook
///
/// `agent_home` — `~/.mur/agents/<name>/`
/// `mur_root`   — `~/.mur/`  (for whisper model + user-level voice templates)
pub fn build_chain(
    profile: &AgentProfile,
    agent_home: &Path,
    mur_root: &Path,
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
        let rendered = compose_with_overrides(
            Some(agent_home),
            Some(mur_root),
            VoiceInput {
                relationship: profile.companion.relationship,
                locale: &profile.companion.locale,
                name_for_user: &profile.persona.display_name,
                first_memory: None,  // injected by D2 onboarding at runtime
                formality: "",
                extra_instructions: "",
            },
        );
        chain.push(Arc::new(CompanionVoiceHook::new(Arc::new(rendered))));
    }

    let want_voice_input = cfg.voice_input
        .unwrap_or(profile.voice.enabled);
    if want_voice_input {
        // Whisper model lives at mur_root/voices/whisper-large-v3-turbo-q5_1.bin.
        // If absent (fresh install before first download), log a warning and skip
        // VoiceInputHook so the agent still starts cleanly.
        let model_path = mur_root.join("voices/whisper-large-v3-turbo-q5_1.bin");
        match WhisperStt::new(&model_path) {
            Ok(stt) => chain.push(Arc::new(VoiceInputHook::new(
                stt,
                VadGate::default(),
                profile.voice.input_device.clone(),
                std::time::Duration::from_secs(30),
            ))),
            Err(e) => tracing::warn!(
                model = %model_path.display(),
                error = %e,
                "VoiceInputHook skipped: whisper model not found; run `mur voice install` to download"
            ),
        }
    }

    HookChain::new(chain)
}
```

`supervisor.rs` replaces its inline `HookChain::new(vec![...])` block with:

```rust
let hook_chain = hooks::builder::build_chain(&profile.inner, &agent_home, &mur_root);
```

`mur_root` is already available as `agent_home.parent().parent().unwrap_or(&agent_home)` (path: `~/.mur/agents/<name>` → `~/.mur`).

---

## 5. CLI — `mur agent hooks show`

New subcommand under `mur agent` that prints the active chain for a named agent without starting the runtime.

```
$ mur agent hooks show alice
── Hook chain for agent "alice" ─────────────────────────────────
 [mandatory]  TelemetryHook       all 10 methods
 [mandatory]  B0SafetyHook        pre_tool_use · on_prompt_submit · post_tool_use · ...
 [optional]   LedgerHook          on (hooks.ledger = true)
 [optional]   CompanionVoiceHook  on  (auto ← companion.enabled = true)
 [optional]   VoiceInputHook      off (auto ← voice.enabled = false)
─────────────────────────────────────────────────────────────────
```

Implementation: reads `profile.yaml`, calls `build_chain` with a no-op telemetry emitter, then calls `chain.names()` and annotates each entry from the config.

`--json` flag emits a machine-readable array:

```json
[
  {"name":"TelemetryHook","tier":"mandatory","enabled":true},
  {"name":"B0SafetyHook","tier":"mandatory","enabled":true},
  {"name":"LedgerHook","tier":"optional","enabled":true,"source":"hooks.ledger"},
  {"name":"CompanionVoiceHook","tier":"optional","enabled":true,"source":"auto:companion.enabled"},
  {"name":"VoiceInputHook","tier":"optional","enabled":false,"source":"auto:voice.enabled"}
]
```

---

## 6. Validation

On `AgentProfile::validate()` (called at profile load):

1. `hooks.companion_voice: true` is allowed even when `companion.enabled = false` (pre-enable for testing).
2. `hooks.voice_input: true` is allowed even when `voice.enabled = false`.
3. No validation errors for any `HooksConfig` value — the schema is permissive. Bad combinations produce no-op handlers (e.g. VoiceInputHook with no voice model configured logs a warning on `on_startup`).

---

## 7. Testing

### Unit tests (mur-common)

- `hooks_config_defaults_roundtrip` — `HooksConfig::default()` serializes without a `hooks:` key; a bare `profile.yaml` deserializes to defaults.
- `hooks_config_partial_override` — `hooks: { ledger: false }` deserializes with `ledger=false`, all other fields at defaults.

### Unit tests (mur-agent-runtime/tests/)

- `builder_mandatory_always_present` — chain always starts with `TelemetryHook`, `B0SafetyHook` regardless of `hooks:` config.
- `builder_ledger_disabled` — `hooks.ledger: false` → chain has no `LedgerHook`.
- `builder_companion_voice_auto` — `companion.enabled=true`, no `hooks:` block → `CompanionVoiceHook` present.
- `builder_voice_input_auto` — `voice.enabled=true`, no `hooks:` block → `VoiceInputHook` present.
- `builder_explicit_override` — `companion.enabled=false` + `hooks.companion_voice: true` → `CompanionVoiceHook` present.
- `builder_explicit_suppress` — `voice.enabled=true` + `hooks.voice_input: false` → `VoiceInputHook` absent.

### CLI test

- `cmd_hooks_show_table` — parses output of `mur agent hooks show <name>` for a fixture profile; asserts mandatory handlers listed and correct optional enable/disable state.
- `cmd_hooks_show_json` — `--json` flag output passes serde round-trip and matches expected `enabled`/`source` fields.

---

## 8. File Changes

| File | Action |
|---|---|
| `mur-common/src/hooks_config.rs` | CREATE — `HooksConfig` struct |
| `mur-common/src/lib.rs` | MODIFY — `pub mod hooks_config; pub use hooks_config::HooksConfig;` |
| `mur-common/src/agent.rs` | MODIFY — add `pub hooks: HooksConfig` to `AgentProfile` |
| `mur-common/tests/hooks_config.rs` | CREATE — roundtrip + partial-override tests |
| `mur-agent-runtime/src/hooks/builder.rs` | CREATE — `build_chain()` |
| `mur-agent-runtime/src/hooks/mod.rs` | MODIFY — `pub mod builder;` |
| `mur-agent-runtime/src/supervisor.rs` | MODIFY — replace inline chain assembly with `hooks::builder::build_chain(...)` |
| `mur-agent-runtime/tests/hook_builder.rs` | CREATE — 6 builder unit tests |
| `mur-core/src/cmd/agent/` | MODIFY — add `hooks show [--json]` subcommand |
| `mur-core/tests/cmd_hooks_show.rs` | CREATE — CLI output tests |

---

## 9. Scope Boundaries

| In A1 | Deferred |
|---|---|
| `HooksConfig` schema + `AgentProfile` field | Per-method handler lists (A3) |
| `build_chain()` builder | WASM / Lua plugin loading (A2) |
| CompanionVoiceHook + VoiceInputHook formally wired | Conditions / retry policy (A3) |
| `mur agent hooks show` CLI | Visual editor (A4) |
| Profile load backward-compat | Runtime handler swap (out of scope) |

---

## 10. Forward Compatibility

`HooksConfig` uses `#[serde(default)]` on all fields. Future optional handlers (e.g. an A2 WASM plugin loader) can be added as new optional fields without breaking existing profiles. The mandatory-tier concept (hardcoded in `build_chain`) is replaced in A3 by a full composition spec that assigns tiers and ordering rules declaratively.
