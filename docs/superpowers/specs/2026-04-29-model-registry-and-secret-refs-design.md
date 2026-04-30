# Model Registry and Secret References — Design

**Status**: proposed
**Author**: david + Claude Opus 4.7
**Date**: 2026-04-29
**Related**: P0a agent runtime, P1 GUI export

## Problem

Today every agent restates its provider+model inline in `profile.yaml`, and credentials are loose env vars (`ANTHROPIC_API_KEY`, etc.). This blocks three workflows:

1. **Multi-agent re-use** — N agents all using `claude-opus-4-7` can't share a single source of truth; updating to `claude-opus-4-8` means N edits.
2. **Per-key isolation** — no way to express "agent A uses my work key, agent B uses my personal Max OAuth token" without juggling env vars.
3. **GUI launch** — `Kelp.app` launched from Finder doesn't inherit shell env, so the sidecar never sees the API key. Today the workaround is "always start from CLI". This is a routine support-call shape.

## Research consensus

Three parallel research tracks (OSS landscape, mur-commander internals, cross-platform secret storage) converged on the same shape — see § 9 for citations.

- **Named registry of models, agents reference by name** is universal (LiteLLM, Continue.dev, aichat, mods, llm, Roo Code, mur-commander).
- **Secrets as references, never inline values** in the registry file; commit-safe registry + values living in env / OS keychain / encrypted file.
- **Resolution priority: env → keychain → file** (gh, aws, cargo all use this hierarchy).
- **macOS GUI sidecars need explicit env injection** — the Tauri main resolves and passes via `Command::env()`; sidecar ACL caveats apply.

## Decisions

1. **Add a model registry** at `~/.mur/models.yaml`. Agents reference entries by name (`model_ref:`). Existing inline `model:` block remains valid (legacy fallback).
2. **`SecretRef` lives in `mur-common::secret`** — variants `Env / Keychain / File / Cmd`, serde string codec (`env:VAR`, `keychain:service/account`, `file:/path`, `cmd:./script`), async resolver. **Variants and codec mirror mur-commander's `engine::secret` exactly** so a future merge is mechanical, but mur-commander stays untouched today (no published-crate coordination cost).
3. **No `Literal` variant** — raw key bytes never live in `SecretRef`. Tests use `env:` indirection.
4. **Use `keyring` crate v4.x** for OS keychain, replacing the `security` shell-out pattern from mur-commander. Native macOS / Windows / Linux Secret Service / kernel keyutils backends.
5. **`file:` auto-detects `.age`** suffix and decrypts via `age` crate. Identity at `~/.mur/age/identity.txt` (0600), can also be stored as `keychain:` ref.
6. **GUI gets a Model tab (B scope)** — list registry, switch active, edit secrets via modal. **Cannot create / delete entries** — that's CLI work.
7. **Migration is manual** — `mur model migrate` is opt-in. `mur agent create` doesn't auto-touch existing files.

## §1 Architecture

```
~/.mur/
├── models.yaml             ← named registry (commit-safe)
├── age/identity.txt        ← optional age private key, 0600
└── agents/<name>/
    ├── profile.yaml        ← `model_ref: <name>` (preferred) | `model: {...}` (legacy)
    └── secrets/<key>.age   ← optional per-agent age-encrypted key
```

```
mur-common::secret           (new module)
  ├── SecretRef {Env, Keychain, File, Cmd}
  ├── serde string codec (idempotent round-trip)
  └── async resolve() → SecretString

mur-common::model            (new module)
  ├── ModelEntry {provider, model, base_url, secret, capabilities, params}
  └── ModelRegistry { models: BTreeMap<String, ModelEntry> }

mur-agent-runtime/llm/anthropic.rs   ← from_secret_ref(SecretRef)
mur-core/src/cmd/agent.rs            ← `mur agent secret {set,list,delete}`
mur-core/src/cmd/model.rs            ← `mur model {add,list,remove,show,migrate}`
mur-agent-gui/src-tauri/             ← Resolve secrets pre-spawn; Command::env() inject
mur-agent-gui/ui/src/tabs/Model.tsx  ← New Model tab
```

**GUI launch dataflow**:
```
1. Tauri main reads ~/.mur/agents/<name>/profile.yaml
2. Reads ~/.mur/models.yaml, resolves model_ref → ModelEntry
3. Resolves entry.secret → SecretString (in-memory only)
4. Command::env(provider_env_var, key) on the sidecar's Command
5. Sidecar's existing AnthropicClient::from_env() picks it up
```

CLI launch is the same minus step 4 — sidecar resolves itself.

## §2 Schema

`~/.mur/models.yaml`:

```yaml
schema_version: 1
models:
  anthropic_opus_4_7:
    provider: anthropic
    model: claude-opus-4-7
    secret: env:ANTHROPIC_API_KEY
    capabilities: [chat, tools, vision]

  anthropic_oauth:
    provider: anthropic
    model: claude-opus-4-7
    secret: keychain:mur/anthropic-oauth

  ollama_llama3:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
    # secret omitted — Ollama doesn't need one

  openai_gpt5:
    provider: openai
    model: gpt-5.5
    secret: file:~/.mur/secrets/openai.age
```

`~/.mur/agents/<name>/profile.yaml` extension:

```yaml
model_ref: anthropic_oauth      # NEW — preferred
# model:                          # legacy — fallback when model_ref absent
#   provider: anthropic
#   name: claude-opus-4-7
```

Rust types (`mur-common::secret`):

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "String", into = "String")]
pub enum SecretRef {
    Env(String),
    Keychain { service: String, account: String },
    File(PathBuf),
    Cmd(String),
}

impl SecretRef {
    pub async fn resolve(&self) -> Result<SecretString, SecretError>;
    pub fn check(&self) -> bool;            // status without leaking value
}

#[derive(thiserror::Error, Debug)]
pub enum SecretError {
    #[error("env var {0} not set")]                                   EnvNotSet(String),
    #[error("keychain item not found: {service}/{account}")]
                                                                       KeychainNotFound{service:String, account:String},
    #[error("read file {path}")]                                      FileRead{path:String, source:std::io::Error},
    #[error("decrypt {0}")]                                           AgeDecrypt(String),
    #[error("cmd {cmd} exit {status}")]                               Cmd{cmd:String, status:i32},
}
```

`mur-common::model`:

```rust
#[derive(Serialize, Deserialize)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)] pub base_url: Option<String>,
    #[serde(default)] pub secret: Option<SecretRef>,
    #[serde(default)] pub capabilities: Vec<String>,
    #[serde(default)] pub params: serde_json::Value,
}

pub struct ModelRegistry { pub models: BTreeMap<String, ModelEntry> }
```

## §3 GUI Model tab (B scope)

New tab in `App.tsx::TABS` between `prompt` and `skills`:

```
┌─ Model ──────────────────────────────────────────┐
│ Active:  [ anthropic_oauth ▾ ]      [Reload]    │
│                                                  │
│ ─ Available models ────────────────────────────  │
│ ● anthropic_oauth                                │
│   anthropic / claude-opus-4-7                    │
│   keychain:mur/anthropic-oauth         ✓ set    │
│                                        [Edit]    │
│ ○ anthropic_opus_4_7                             │
│   anthropic / claude-opus-4-7                    │
│   env:ANTHROPIC_API_KEY                ✗ not set│
│                                        [Edit]    │
│ ○ ollama_llama3                                  │
│   ollama / llama3.2:3b                           │
│   (no secret needed)                   ✓ ready  │
└──────────────────────────────────────────────────┘
```

Edit modal lets the user choose storage backend (env / keychain / age) and write a value. Value flows one-way (UI → Tauri command → backend); GUI never reads back.

New Tauri commands (`commands.rs`):

```rust
#[tauri::command] pub fn list_models() -> Result<Vec<ModelEntryView>, String>;
#[tauri::command] pub fn get_active_model_ref() -> Result<String, String>;
#[tauri::command] pub fn set_active_model_ref(name: String) -> Result<(), String>;
#[tauri::command] pub fn check_secret(secret: SecretRef) -> Result<bool, String>;
#[tauri::command] pub fn set_secret(secret: SecretRef, value: String) -> Result<(), String>;
```

`ModelEntryView` carries name / provider / model / base_url / secret_ref / secret_status; **never the secret value**.

## §4 Migration

Resolution order in the runtime:

1. `profile.yaml.model_ref` present → load `~/.mur/models.yaml` → entry by name.
2. Else `profile.yaml.model` present → treat as inline `ModelEntry` (legacy).
3. Else → `error: agent has no model configured`.

`mur agent create`:
- If `~/.mur/models.yaml` exists and is non-empty, the interactive prompt offers "pick from registry" with a `New inline…` escape hatch.
- `--no-interactive` keeps writing inline `model:` (no behaviour change for tests / CI).

`mur model migrate` (opt-in, idempotent):
- Walks every agent dir, extracts inline `model:` to a synthesized name `<provider>_<model_id>` (claude-opus-4-7 → `anthropic_opus_4_7`), inserts into `models.yaml`, rewrites the agent's profile to use `model_ref`.
- `--dry-run` flag for preview.

`mur agent secret set <agent> <KEY> <value>` short-cut: writes the value to `keychain:mur-agent/<name>/<KEY>` and ensures the agent's resolved secret ref points there. Spares the user from learning the registry concept on day one.

`mur agent export` (P1 export pipeline) is unchanged: pkg/bin/gui all still bundle `profile.yaml` and now also `models.yaml` (filtered to only the entry the agent actually references — keeps secrets out of the bundle entirely).

## §5 Testing

**Unit** (`mur-common`):
- `SecretRef` round-trip serde for each variant; reject unknown prefixes.
- `resolve()` happy + sad cases:
  - `Env`: set / unset.
  - `Keychain`: mock backend (`keyring::mock` feature) — present / missing / locked.
  - `File`: plaintext / `.age` / mode 0644 (refuse) / not exist.
  - `Cmd`: zero exit + stdout / non-zero / timeout.
- `ModelRegistry::load(yaml)` schema tolerance — missing fields, unknown providers, empty map.

**Integration** (`mur-agent-runtime`):
- Legacy inline `model:` profile boots and replies.
- `model_ref` → unknown name → `LlmError::ConfigInvalid` (not panic).
- All 4 secret backends drive a mocked Anthropic HTTP server end-to-end.

**E2E** (`scripts/e2e/p1-model-secrets.sh`):
1. `mur model add anthropic_test --provider anthropic --model claude-opus-4-7 --secret env:TEST_KEY`
2. `mur agent create kelp_test --no-interactive --model-ref anthropic_test`
3. `TEST_KEY=foo mur_agent_kelp_test` → starts.
4. `mur agent secret set kelp_test ANTHROPIC_API_KEY foo` → keychain path.
5. Restart, confirm keychain path also works (no more env needed).

**GUI manual acceptance**:
- Finder-launched Kelp.app → Model tab shows `✗ not set` initially.
- Edit modal → keychain → status flips to `✓ set` → real chat succeeds (no echo fallback).

**Headless Linux**:
- Docker container, no D-Bus → keyring fails → `file:` fallback works.
- 0600 plaintext file works; explicit error (no silent echo fallback).

## §6 Open questions / out of scope

- **OAuth refresh** — Anthropic OAuth tokens expire. Today the runtime returns 401 and stops. Refresh flow is out of scope; user re-runs `claude login` and `mur agent secret set` re-pulls. Could be automated via `read_oauth_from_keychain` (`Claude Code-credentials`), but that's a P2 follow-up.
- **mur-commander convergence** — Once both projects are in production, lift `mur-common::secret` to a published crate so commander can drop its own copy. Until then they co-evolve with mirrored shape.
- **Secret rotation across agents** — When a registry entry's secret rotates, all agents using that entry pick up the new value at next request. No restart needed (secrets are resolved per-call, with a short TTL cache to avoid repeated keychain reads).

## §7 Sequencing

This isn't a single PR. Suggested split:

1. **PR-1**: `mur-common::secret` + `keyring` crate + `age` crate. Unit tests only. No consumer changes yet.
2. **PR-2**: `mur-common::model` + `ModelRegistry` loader. Plumb `model_ref` into runtime resolution; legacy `model:` still works. `mur agent send` proves the path.
3. **PR-3**: `mur model {add,list,remove,show}` + `mur agent secret {set,list,delete}` CLI verbs. `mur model migrate` last.
4. **PR-4**: Sidecar.rs in mur-agent-gui — resolve secrets in Tauri main, inject via `Command::env()`. GUI sees real keys without shell.
5. **PR-5**: New Model tab in mur-agent-gui (B scope).

## §8 Footguns to watch

1. **macOS launchd cold-boot**: login keychain locked → `keyring::Entry::get_password` fails. Resolver must fall through to `file:` cleanly. E2E: launchd plist + reboot.
2. **Linux headless** (Docker, CI): no D-Bus → `keyring` errors. Same fallthrough. Don't silently downgrade to plaintext like `gh` did before its `--insecure-storage` flag.
3. **Tauri sidecar codesign mismatch**: if `Kelp.app/Contents/MacOS/mur-agent-gui` and `mur-agent-runtime` are signed by different team-ids, macOS treats them as different apps for keychain ACL → silent prompt loop. The export pipeline already signs both; add a `codesign -dvvv` assertion.

## §9 References

- LiteLLM proxy / model_list: <https://docs.litellm.ai/docs/proxy/configs>
- Continue.dev config.yaml: <https://docs.continue.dev/reference>
- Datasette `llm` keys.json: <https://llm.datasette.io/en/stable/setup.html>
- aichat config.example.yaml: <https://github.com/sigoden/aichat/blob/main/config.example.yaml>
- mods config_template.yml: <https://github.com/charmbracelet/mods/blob/main/config_template.yml>
- mur-commander `engine::secret`: `crates/engine/src/secret.rs:11-95`
- mur-commander OAuth helpers: `crates/gateway/src/unified_handler/llm_service/provider.rs:295-350`
- `keyring` crate: <https://github.com/open-source-cooperative/keyring-rs>
- `age` crate: <https://docs.rs/age>
- `secrecy` crate: <https://docs.rs/secrecy>
- 1Password secret refs: <https://developer.1password.com/docs/cli/secret-reference-syntax/>
- gh keyring discussion: <https://github.com/cli/cli/discussions/8980>
- Tauri sidecar signing: <https://github.com/tauri-apps/tauri/discussions/2269>
