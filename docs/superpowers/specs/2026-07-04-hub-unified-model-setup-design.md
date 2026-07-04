# Hub Unified Model Setup — Design

**Date:** 2026-07-04
**Status:** Approved (brainstorm 2026-07-04)
**Scope:** mur-common, mur-core (`cmd/init.rs`, new `model_setup`), mur-hub-gui (Settings › Models, first-run wizard)

## Problem

1. **`mur init` asks for models 3–5 times.** Step G walks the user through setup
   mode → cloud LLM provider/model → embedding provider/model → conversations
   models, each an interactive prompt with hardcoded model menus. Users cannot
   tell `reflector`/`curator`/`ask`/`compact` apart and should not have to.
2. **The Hub has no UI for pipeline model slots.** The Model Library manages
   `~/.mur/models.yaml` (provider connect, discovery, registry aliases) but the
   `~/.mur/config.yaml` slots that init sets (`llm.*`, `embedding.*`,
   `conversations.*`) are settable only via the init TTY flow or hand-editing
   YAML.
3. **Key-flow gap.** The Hub stores provider keys in the OS keychain
   (`ModelEntry.secret: SecretRef`), but pipeline backends resolve keys from
   env vars only (`api_key_env`). A cloud model connected in the Hub cannot
   authenticate CLI pipeline stages, so "connect once in the GUI" is a broken
   promise today.

## Goals

- One-page model management in Hub Settings: 3 user-facing slots, advanced
  per-slot overrides folded away.
- First-run wizard model step — shown only when no usable model config exists;
  skippable; re-runnable from Settings.
- `mur init` model setup collapses to a single question.
- A key connected via the Hub (keychain) works everywhere, including CLI
  pipeline stages.
- One shared recommendation engine used by both the Hub wizard and `mur init`.

## Non-goals

- No `config.yaml` → registry-alias indirection (`model_ref` in config slots).
  Approach C from the brainstorm; rejected as YAGNI — resolved values + a
  secret ref deliver all user-visible value without touching every factory
  call site.
- The wizard does not create agents.
- No change to agent model resolution (already registry-based).

## Design

### 1. `api_key_ref` — close the key-flow gap

Add `api_key_ref: Option<String>` (SecretRef wire string, e.g.
`keychain:mur/anthropic`, `env:ANTHROPIC_API_KEY`) to:

- `mur_common::config::LlmConfig`
- `mur_common::config::EmbeddingConfig`
- `mur_common::config::BackendConfig` (and the `LlmConfig → BackendConfig`
  synthesis carries it 1:1)

Serde default `None`; existing configs are untouched. No migration.

Key resolution order everywhere a pipeline backend authenticates
(conversations backend factory, learning LLM client, embedding client):

```
api_key_ref (SecretRef::resolve) → api_key_env → provider-default env var
```

Resolution failure produces one error naming every source tried.

### 2. Hub Settings › Models one-pager

Rework `ModelsSettings.tsx` into three primary slot rows, each a
`ModelCombobox` fed by registry models (`list_models`) plus detected local
models (`probe_local_providers`), grouped by provider:

| Slot (user-facing)   | Writes                                                              |
| -------------------- | ------------------------------------------------------------------- |
| Smart model          | `llm.provider/model/api_key_ref` **and** `conversations` ask/compact/rollup models (one pick fills all four — matches what init already does) |
| Search model         | `embedding.provider/model/dimensions/api_key_ref` (dimensions auto-filled from the known-dims table / probe) |
| Agent default brain  | existing default-brain mechanism (unchanged plumbing)                |

An **Advanced overrides** accordion exposes per-slot rows: conversations
`ask` / `compact` / `rollup` / `summarize`, and registry roles `reflector` /
`curator`.

**"Follow Smart model" is a UI-level linkage, not an on-disk marker.**
`config.yaml` always stores concrete model strings. A sub-slot is shown as
"following" when its value equals the Smart model's; changing the Smart model
rewrites only the sub-slots that were still following (value-equality
heuristic). Sub-slots with a different value display as overrides — which is
exactly what the recommendation engine produces when it keeps conversations
local (§4).

Selection semantics:

- Cloud registry model → write the provider's `secret` ref into the slot's
  `api_key_ref`.
- Detected local model → write endpoint, no key.

Each row shows a best-effort health state: `✓ ready` / `⚠ key missing` /
`⚠ runtime not running`.

New tauri commands in `src-tauri` (new `model_slots.rs`):
`model_slots_get` / `model_slots_set(slot, selection)`, reading/writing
`config.yaml` through mur-core's existing atomic load/save.

### 3. First-run wizard model step

Hooks into the existing first-launch flow (`check_first_launch` /
`replay_onboarding`); no new trigger machinery.

**Trigger:** first-launch **and** the no-usable-model predicate — registry is
empty AND `config.yaml` `llm` + `embedding` are factory defaults. Users who
configured via CLI never see it.

**Screen 1 — detection summary.** Auto-probe local runtimes, env keys, and
keychain-connected providers, then present:

- **Apply recommended setup** (primary CTA) — applies the plan from the shared
  engine (§4) and shows a one-line summary of what was set.
- **Customize…** — opens Settings › Models (§2).
- **Skip.**

**Screen 2 — connect a provider.** Shown only when nothing is detected or
connected. Reuses `NewProviderPanel` from the Model Library, then returns to
screen 1.

### 4. Shared recommendation engine (mur-core)

```rust
// mur-core/src/model_setup/mod.rs
pub fn recommend(discovered: &[DiscoveredModel],
                 env_keys: &EnvKeyProbe,
                 registry: &ModelRegistry) -> ModelSetupPlan;
pub fn apply(plan: &ModelSetupPlan, config: &mut Config);
```

`recommend` is pure (inputs in, plan out) so the decision matrix is
unit-testable. Policy (hybrid default):

- Smart model: best cloud model if any key is available (keychain-connected
  provider or env var), else best local LLM. "Best" is deterministic: the
  existing per-provider default table for cloud, and the existing
  `build_llm_menu` ranking for local — no new ranking is invented.
- Search model: prefer local embedding; fall back to the same cloud provider's
  embedding model when no local runtime exists (existing dims table).
- Conversations ask/compact/rollup: local-preferred, matching init's current
  policy (background tasks stay cheap/local even in cloud mode). The plan
  writes these as explicit values, so the Hub one-pager shows them as
  overrides rather than "following" a cloud Smart model (§2).

Used by both the Hub wizard (via a tauri command) and `mur init`.

### 5. `mur init` Step G — one question

```
Model setup:
  1) Use recommended defaults — <one-line preview of the detected plan>   (default)
  2) Configure later in MUR Hub
```

- Choice 1 (also taken in non-interactive runs): `recommend` + `apply`, print
  the summary line.
- Choice 2: skip, print "open MUR Hub → Settings → Models".
- Delete the interactive blocks `select_cloud_llm`, `select_cloud_embedding`,
  `select_conversations_models`, and the 4-way setup-mode menu. `init.rs`
  shrinks accordingly.

### 6. Data flow & error handling

- The Hub writes `config.yaml` through mur-core (temp file + rename, already
  atomic). CLI and Hub share the file; both re-read before write;
  last-writer-wins is acceptable for a single-user local file.
- Runtime SecretRef resolution failure → error naming the ref and the
  fallbacks tried; the Hub health check (§2) surfaces the same condition
  before the user hits it at runtime.
- Probe failures in the wizard degrade gracefully: an empty detection result
  routes to screen 2, never a dead end.

### 7. Testing

- Factory key-resolution order, including the env-only regression path
  (existing configs must behave identically).
- `recommend` matrix: {cloud key present/absent} × {local runtime
  present/absent} × {registry empty/non-empty}.
- Config slot write round-trip (set → reload → assert), embedding dims
  auto-fill.
- UI helper tests for the slot state (`modelSlots.ts`), mirroring
  `modelPicker.test.ts`.

### 8. Delivery — three independently mergeable PRs

1. **PR 1:** `api_key_ref` + `model_setup::recommend/apply` + init
   one-question flow.
2. **PR 2:** Hub Settings › Models one-pager (+ tauri commands, i18n
   en/zh-TW).
3. **PR 3:** wizard model step (trigger predicate + two screens).

## i18n & branding

All new Hub strings go in both `en.ts` and `zh-TW.ts`. User-facing brand is
uppercase **MUR** (CLAUDE.md rule 7).
