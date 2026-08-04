# Conversations backend — collapsing the dual track

**Date:** 2026-08-03
**Status:** design approved, awaiting implementation plan
**Scope:** `conversations.{ask,compact,rollup}` model resolution in `mur-common`
and `mur-core`. `llm.*`, `embedding.*` (except one dead field), the model
registry roles (`reflector`, `curator`, `summarize_model`), and
`mur-agent-runtime` model selection are all out of scope.

## Problem

`conversations.{ask,compact,rollup}` resolve their chat model through two
parallel mechanisms:

- **legacy** — a bare model-name string plus an `ollama_endpoint`
  (`ask.model`, `compact.{extractive,abstractive}_model`,
  `rollup.{extractive,abstractive}_model`)
- **override** — an optional `BackendConfig` object carrying
  provider/model/endpoint/key (`ask.{backend,rewriter_backend}`,
  `compact.{extractive,abstractive}_backend`)

When the override is `None`, resolution *fabricates an Ollama identity*:

```rust
// mur-common/src/config.rs:785-794 (also :805, :980, :995)
pub fn synthesize_backend(&self) -> BackendConfig {
    self.backend.clone().unwrap_or_else(|| BackendConfig {
        provider: "ollama".into(),                       // ← hardcoded
        model: self.model.clone(),
        endpoint: Some(self.ollama_endpoint.clone()),
        ...
    })
}
```

`rollup` is worse: it has no override field at all and hardcodes the same thing
inline, with a comment conceding the gap
(`mur-core/src/conversations/summarize/rollup.rs:176-187`, repeated at `:437`).

So a model name is stored without the runtime it belongs to. Point the name at
a non-Ollama model and every one of these stages silently dials
`localhost:11434` asking for a model that runtime has never heard of.

### This is produced by MUR itself, not by hand-editing

`mur-core/src/model_setup/mod.rs:206`:

```rust
let conversations_model = local_llm.as_ref().map(|m| m.id.clone());
```

`DiscoveredModel` carries `backend: Backend::{Ollama, OMlx}`. This line discards
it and keeps only the id; `apply()` (`:245-247`) then writes that bare string
into all three stages. The correct mapping — `local_slot_choice(m)`, which turns
`Backend::OMlx` into `provider: "openai"` + `openai_url` + `api_key_ref` — sits
11 lines above at `:141-156` and is simply not used on this path.

Observed instance: a user selected an omlx model in setup and got
`ask.model: Qwen3.5-4B-MLX-4bit` with `provider` still resolving to `ollama` and
`endpoint` still `http://localhost:11434` — where no Ollama was running at all.
All six call sites were dead and nothing reported it.

### Nothing reports it

`mur chat doctor` is the only tool that could have caught this, and it cannot:

- `mur-core/src/cmd/conversations_cmd.rs:512-526` probes
  `compact.ollama_endpoint` **unconditionally**, ignoring whether any stage
  still uses Ollama. Its "compact + ask will degrade" line is false whenever an
  override is set. `:773-800` repeats the same probe.
- `:529-535` filters cloud probes to `provider == "anthropic"`, so
  `openai` / `openrouter` / `gemini` backends are never probed and report as
  "no cloud providers in active config".

Neither line tells you which endpoint a given stage actually dials. And both
`compact` and `ask` short-circuit before touching an LLM when the conversation
store is empty, so a fresh install has **no way at all** to verify its model
configuration.

## Decisions taken

| Question | Chosen |
| --- | --- |
| Track shape | **Single track.** `None` means "follow the smart slot", never "fabricate Ollama" |
| Legacy fields | **Deleted**, after a one-shot migration |
| Migration trigger | **Automatic on config load**, once, idempotent |
| Untouched vs customized | Factory-default legacy value → `None` (inherit). Customized → pinned `BackendConfig` preserving today's exact behavior |
| Local preference for conversations | **Kept** — setup still writes a local backend into the three stages even when `smart` is cloud |
| doctor depth | **Per-stage listing + live probe** of each unique endpoint |

## Design

### 1. `None` means "follow smart"

This is already the documented intent —
`mur-core/src/model_setup/slots.rs:1-7` says the conversation stages "default to
following `smart`". Today that is faked by copying the model *name* between
config fields. The real mapping already exists and already handles omlx:

```rust
// mur-common/src/config.rs:500-514
pub fn to_backend_config(&self) -> BackendConfig {
    let provider = match self.provider.as_str() {
        "anthropic" | "openai" | "openrouter" | "gemini" | "ollama" => self.provider.clone(),
        _ if self.openai_url.is_some() => "openai".into(),   // omlx, mlx, any local OpenAI-compat
        other => other.into(),
    };
    BackendConfig { provider, model: self.model.clone(), endpoint: self.openai_url.clone(), ... }
}
```

The four `synthesize_*_backend()` functions (`config.rs:785, 805, 980, 995`) and
rollup's inline block (`rollup.rs:180-187`) are replaced by one resolver per
stage config:

```rust
/// Override wins; otherwise inherit the smart slot. The stage's own
/// timeout is baked in when inheriting, exactly as synthesize_* did.
pub fn effective_backend(&self, llm: &LlmConfig) -> BackendConfig
```

`AskConfig` keeps two resolvers (`effective_backend`, `effective_rewriter_backend`)
because the rewriter has its own tighter timeout budget and must not fall through
to the answer backend's — the distinction `synthesize_rewriter_backend`
(`config.rs:796-816`) exists to preserve.

Note this makes `provider: "omlx"` work as an LLM for the first time: the chat
factory (`mur-core/src/conversations/backend/factory.rs:92-142`) only accepts
`ollama | anthropic | openai | openrouter | gemini` and rejects everything else
with `unsupported provider`. `omlx` is currently an embedding-only alias
(`mur-core/src/store/embedding.rs:109`). Routing through `to_backend_config()`
maps it to `openai` before the factory ever sees it.

### 2. Schema

| Action | Fields |
| --- | --- |
| Remove | `ask.{model, ollama_endpoint}`, `compact.{extractive_model, abstractive_model, ollama_endpoint}`, `rollup.{extractive_model, abstractive_model, ollama_endpoint}` |
| Add | `rollup.{extractive_backend, abstractive_backend}: Option<BackendConfig>` |
| Change | `embedding.ollama_endpoint`: `String` → `Option<String>` with `#[serde(skip_serializing_if = "Option::is_none")]` |

The embedding change fixes a separate reported confusion: with
`embedding.provider: omlx`, `ollama_endpoint` is dead — it is read only in the
`_ =>` fallback arm of `EmbeddingConfig::from_config`
(`mur-core/src/store/embedding.rs:107-125`), while the omlx path uses
`openai_url`. It survives in the file because the field is a non-`Option`
`String` with `#[serde(default)]`, so every serialization re-emits it and
deleting it by hand is undone by the next `save_config()`.
`llm.openai_url` already uses the `Option` + `skip_serializing_if` shape; this
adopts it.

### 3. Migration runs on load, not on command

**The manual alternative has an unclosable data-loss window.** `save_config` /
`save_config_at` have 27 call sites across unrelated commands (`mur sleep`,
`mur team`, `mur source`, `model_setup/slots.rs:364`, `cmd/init.rs` ×6). Once the
legacy fields leave the struct, the first of those to run rewrites
`config.yaml` without them — before the user has any reason to run a migrate
command.

The existing protection does not cover this. `merge_over_existing`
(`mur-core/src/store/config.rs:47-83`) carries over only **top-level** blocks
absent from the new document — it was added after `research_gateway` was
"measurably lost on every `mur sleep`" (#778). Our legacy keys are nested under
`conversations`, and that top-level block is always present in the new document,
so they are erased.

Therefore: `Config` load detects legacy keys, converts, writes back once, and
prints a single-line notice. Precedent for writing during load exists at
`store/config.rs:12-16` (first run writes defaults).

Conversion rule, per stage. A stage counts as **untouched** only when *both*
its legacy model string(s) and its own `ollama_endpoint` still hold the shipped
defaults — a default model name pointed at a customized endpoint (a remote
Ollama box) is a deliberate choice and must be pinned, not inherited away.

- **untouched** → write `None`; the stage starts following smart. (Precedent for
  an "is this still factory?" check: `is_factory_default_models()`,
  `model_setup/mod.rs:251-263`.)
- **touched** → write
  `Some(BackendConfig { provider: "ollama", model, endpoint: <that stage's ollama_endpoint> })`,
  reproducing today's behavior byte for byte. No silent re-routing of anyone's
  traffic on upgrade.

`compact` owns one `ollama_endpoint` shared by its extractive and abstractive
models, so its two backends migrate against the same endpoint value; `ask` and
`rollup` each own theirs.

`mur migrate --conversations --dry-run` prints the planned conversion without
writing. Without `--dry-run` it is a no-op after the automatic pass, which is
the point: the command exists to *preview*, not to be required.

The migration must be idempotent — a second load finds no legacy keys and does
nothing.

### 4. Writers stop writing legacy fields

| File | Change |
| --- | --- |
| `model_setup/mod.rs:46, 206, 245-247` | `ModelSetupPlan.conversations_model: Option<String>` → `Option<SlotChoice>`; derive it with the existing `local_slot_choice(m)` (`:141-156`); `apply()` writes a real `BackendConfig` into all three stages |
| `model_setup/slots.rs:236, 252, 267` | `Local` selections write `Some(BackendConfig)` instead of a legacy string; the `rollup` arm's `bail!("this stage runs locally; pick a local model")` is removed — rollup now accepts `Registry` like every other stage |
| `model_setup/slots.rs:69, 78, 86` | `*_pair()` reads `effective_backend(&cfg.llm)`, making `follows_smart` a real value comparison instead of a string coincidence |
| `conversations/summarize/rollup.rs:180-187, 437` | Inline hardcoded `provider: "ollama"` replaced by the new `rollup.*_backend` resolution |
| `cmd/conversations_cmd.rs:1242, 1302` | `ask_cfg.model` / `ask_cfg.ollama_endpoint` replaced by `effective_backend` |

**Local preference is deliberately preserved.** `smart` prefers cloud when an
API key is present (`model_setup/mod.rs:165-172`) while `conversations_model`
deliberately picks `local_llm` (`:206`) — an implicit "conversation stages stay
on-device" policy. Collapsing the tracks must not delete it: setup keeps writing
an explicit local backend, and inheritance from a cloud `smart` happens only
when the user clears the override themselves.

### 5. doctor tells you what each stage dials

Replace both unconditional Ollama probes (`conversations_cmd.rs:512-526`,
`:773-800`) and the `provider == "anthropic"` filter (`:532`) with a per-stage
table plus one probe per unique endpoint:

```
conversations backends
  ask.generate         openai  Qwen3.5-4B-MLX-4bit  http://127.0.0.1:8000/v1  [pinned]
  ask.rewriter         openai  Qwen3.5-4B-MLX-4bit  http://127.0.0.1:8000/v1  [pinned]
  compact.extractive   openai  Qwen3.5-4B-MLX-4bit  http://127.0.0.1:8000/v1  [pinned]
  compact.abstractive  ollama  qwen3:4b             http://localhost:11434    [follows smart]
  rollup.extractive    …
  rollup.abstractive   …
  ✓ http://127.0.0.1:8000/v1 — 3 models, Qwen3.5-4B-MLX-4bit present
  ✗ http://localhost:11434 — unreachable (used by: compact.abstractive)
```

Probes dedup by `(provider, model, endpoint)` and dispatch by provider, 2s
timeout each, non-fatal:

- `ollama` → `GET {endpoint}/api/tags`, then check the model is listed
- OpenAI-compatible (`openai`, `openrouter`, including local runtimes) →
  `GET {endpoint}/models`, then check the model is listed
- `anthropic`, `gemini` → resolve the key only; no live API call, so the check
  costs nothing. The secret value is never printed.

`ok = false` requires an endpoint that **a stage actually uses** to be
unreachable. An idle Ollama nobody references must not turn doctor red — which
is exactly what happens today.

This is what closes "you cannot verify your model configuration until
conversation data exists".

## Testing

- Legacy YAML fixtures → migrate → per-stage assertions covering all three
  shapes: fully default → `None`; customized model → pinned Ollama
  `BackendConfig`; **default model at a customized `ollama_endpoint`** → pinned,
  not inherited
- Migration idempotency: a second run produces an identical document
- `effective_backend` inheritance: `llm { provider: "omlx", openai_url: Some(..) }`
  with `backend: None` resolves to `provider: "openai"` at that endpoint
- **A `rollup` override reaches the factory** — impossible today; this is the
  regression test for the gap
- doctor golden output for a mixed config (one pinned `openai`, two inheriting),
  endpoints stubbed with `wiremock` (already used in
  `conversations/backend/factory.rs` tests)
- `embedding.ollama_endpoint: None` is absent from the serialized YAML
- `save_config()` round-trip over a migrated config does not resurrect any
  legacy key

## PR breakdown

1. **PR1 + PR2 (single PR).** Schema change, `effective_backend`, rollup's two
   new fields, `embedding.ollama_endpoint` → `Option`, all call sites, plus the
   load-time migration and `--dry-run` preview. **These must ship together**: a
   release containing the schema change without the migration reopens the §3
   data-loss window.
2. **PR3.** Writers — `model_setup::apply()`, `slots.rs`.
3. **PR4.** doctor.

### Not doing

`mur-common/src/config.rs` is 2396 lines and
`mur-core/src/cmd/conversations_cmd.rs` is 1592, both over the 800-line rule in
`CLAUDE.md`. This change is net-subtractive there (four `synthesize_*` functions,
seven fields, their default constructors and tests all leave), so it does not
make the situation worse and no pure-movement split PR is bundled in. The debt
stands, recorded, unaddressed here.
