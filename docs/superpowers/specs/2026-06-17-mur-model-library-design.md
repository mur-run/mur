# MUR Model Library / Providers — Design

**Date:** 2026-06-17
**Status:** Approved (brainstorming) → pending plan
**Branch:** `feat/model-library-providers`
**Mockups:** `/tmp/mur-model-picker-mockup.html` (picker), `/tmp/mur-model-library-mockup.html` (library)

## Problem

Three related gaps in how MUR handles models:

1. **Model picker is a flat `<select>`.** In `mur-hub-gui` `DetailPanel.tsx`, every registry model is listed in one ungrouped dropdown (`ModelSection`). It works for 6 models but has no grouping, no metadata, and degrades as the registry grows.
2. **Cost data is single-valued and ambiguous.** `ModelEntry.cost_per_1k_tokens` is documented as "output" cost but used by the router as a single blended rate (`route/mod.rs:166`). Real LLM pricing separates input and output (output is typically 3–5× input). The user cannot see or reason about true cost.
3. **There is no GUI way to add a model.** The Hub only has read-only `list_models` (`detail.rs:46`). To add a model the user must drop to `mur model add` (CLI) or hand-edit `~/.mur/models.yaml`. Cost values must be looked up manually from each vendor's site.

## Core Insight

**MUR consumes models; it does not serve or download them.** Unlike Osaurus / LM Studio / Ollama (which *run* weights), MUR connects to OpenAI-compatible endpoints. Therefore the Osaurus-style "Models (download) vs Providers (connect)" split does **not** fit MUR.

The right model for MUR is **"everything is a Provider"**:
- A **cloud provider** needs a base URL + API key, and its models are discovered via `/v1/models`.
- A **local provider** is just a provider that runs on localhost, needs no key, is auto-detected, and is tier=local / $0.

This aligns with MUR's existing concepts: `RouteTier::Local/Frontier`, `SecretRef` (keychain/env/file/cmd), and `base_url` already being first-class (the user's own `models.yaml` routes Anthropic through a local cc-proxy).

## Scope — Three Sub-Projects

This is decomposed into three independently-shippable pieces. Each gets its own implementation plan.

| ID | Title | Layer | Depends on |
|----|-------|-------|-----------|
| **S1** | Registry schema: input/output cost | `mur-common`, `mur-core` | — |
| **S2** | Auto price fetch from models.dev | `mur-core` | S1 |
| **S3** | Hub Model Library + grouped picker | `mur-hub-gui`, `mur-core` | S1, S2 |

S1 is the foundation. S2 and S3 both consume it. S2 can land before or with S3; S3's GUI is the natural place S2's fetched prices surface.

---

## S1 — Registry Schema: Input/Output Cost

### Data model change

`ModelEntry` (`mur-common/src/model.rs`) gains two optional fields and keeps the old one for back-compat:

```rust
pub struct ModelEntry {
    // ...existing fields...
    /// Deprecated single blended rate. Retained for back-compat: when present
    /// and the split fields are absent, it is treated as the OUTPUT rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_tokens: Option<f64>,
    /// USD per 1000 INPUT tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_1k: Option<f64>,
    /// USD per 1000 OUTPUT tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_1k: Option<f64>,
    /// Context window in tokens (from discovery / models.dev). Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}
```

**Units stay per-1k** to match the existing field and CLI. (models.dev is per-1M; S2 converts on ingest — divide by 1000.)

### Back-compat accessor

A helper resolves effective rates so existing entries keep working:

```rust
impl ModelEntry {
    /// (input_per_1k, output_per_1k). Falls back to the legacy blended rate
    /// as the output rate; input falls back to the legacy rate when no split.
    pub fn effective_costs(&self) -> (Option<f64>, Option<f64>) {
        let out = self.output_cost_per_1k.or(self.cost_per_1k_tokens);
        let inp = self.input_cost_per_1k.or(self.cost_per_1k_tokens);
        (inp, out)
    }
}
```

### Router / ledger update

`route/mod.rs` cost estimation currently does `tokens/1000 * cost_per_1k`. Update `frontier_cost_per_1k()` and the counterfactual calc to use `effective_costs()`. Since the escalation estimate has only a single `estimated_tokens` (no input/output split), use the **output** rate as the conservative estimate (it dominates), and document that. No behavior change for entries that only set the legacy field.

### CLI

`mur model add` gains `--input-cost` and `--output-cost` (per 1k). `--cost-per-1k` retained as deprecated alias mapping to `--output-cost`. `mur model show` prints both rates + context window.

### Tests

- Round-trip serde with new fields present / absent.
- `effective_costs()` fallback matrix (legacy only, split only, both, none).
- Router cost estimate unchanged for legacy-only entries (regression).
- Existing `cost_per_1k_tokens: None` fixtures keep compiling (add the new fields via `..Default::default()` where a `Default` exists, else explicit `None`).

---

## S2 — Auto Price Fetch (models.dev)

### Source

**models.dev** `https://models.dev/api.json` — verified 2026-06-17 as current (returns Claude Opus 4.5, GPT-5.2). Structure: keyed by provider → `models` → per-model `cost.{input,output,cache_read,cache_write}` (per **1M** tokens) + `limit.{context,output}`. No auth to read.

LiteLLM's `model_prices_and_context_window.json` is an **optional** fallback when models.dev lacks an entry. Not required for v1.

### Module

New `mur-core/src/model_prices.rs`:

```rust
/// Looked-up pricing for a (provider, model). All per-1k tokens.
pub struct PriceInfo {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub context_window: Option<u64>,
}

/// Fetch + cache the models.dev catalog, then resolve one (provider, model).
/// Best-effort: returns None on any network/parse error or cache miss.
pub fn lookup(provider: &str, model: &str) -> Option<PriceInfo>;
```

### Behavior

- **Cache** the full catalog to `~/.mur/cache/model-prices.json` with a TTL (config `model_prices.ttl_hours`, default 168 = 7 days). Serve from cache when fresh; refetch when stale; serve stale on fetch failure (best-effort).
- **Network is optional.** All fetches are timeout-bounded (config `model_prices.timeout_secs`, default 10) and never block or fail the parent command. MUR is local-first — a price lookup failing is a non-event.
- **Local tier never fetched** — local models get input=output=0.
- **Matching:** exact `(provider, model)` first; then case-insensitive; then provider-namespaced id (e.g. `anthropic/claude-opus-4-8` for OpenRouter-style). Unmatched → None, leave costs unset.

### Wiring into `mur model add`

After writing the entry, if cloud tier and costs not explicitly given via flags, call `model_prices::lookup()` and fill `input_cost_per_1k` / `output_cost_per_1k` / `context_window`. Print what was auto-filled. Flags `--input-cost`/`--output-cost` always win; `--no-fetch` skips the lookup entirely.

### CLI surface

`mur model prices refresh` — force-refresh the cache. `mur model prices show <name>` — show resolved pricing for a registry entry.

### Tests

- Parse a fixture `api.json` slice → `PriceInfo` with correct per-1k conversion.
- Matching fallback order (exact / case-insensitive / namespaced).
- TTL: fresh cache not refetched; stale triggers refetch; fetch failure serves stale.
- Local tier short-circuits to 0 with no network.

---

## S3 — Hub Model Library + Grouped Picker

Two distinct surfaces. The **picker** selects from the registry; the **library** manages the registry.

### S3a — Picker (in `DetailPanel.tsx`)

Replace flat `<select>` `ModelSection` with a **searchable grouped combobox** (mockup option B):
- Trigger button shows current alias + model id + provider chip.
- Popover: search input filters on alias / provider / model id; results grouped by provider; keyboard ↑↓/↵/esc.
- Each row: alias (bold), `provider/model` (muted), badges: tier, `in $X/M` + `out $Y/M` (from `effective_costs()`, shown only when known), `ctx` (context window when known), capability chips.
- Selecting calls existing `update_agent_detail` with `model_ref` (unchanged contract).
- Next to the picker: a **"⚙︎ 管理模型…"** link opening the Library (S3b).

New component `ModelCombobox.tsx`; `ModelOption` view type extends with `tier`, `input_cost`, `output_cost`, `context_window`, `capabilities` (populated by `list_models`).

### S3b — Model Library (new settings surface)

New route/window section "模型庫". Layout (mockup): left rail = providers grouped into **已連線 Cloud**, **本機 Local (自動偵測)**, **新增 Provider** (presets: OpenAI, Google, OpenRouter, xAI, Custom). Right pane = selected provider detail.

**Cloud provider flow:**
1. Base URL prefilled from preset, editable (first-class — cc-proxy is normal).
2. API Key field → stored as `SecretRef`, default **keychain** (segmented control: Keychain / env:VAR / file:/path). Never echoed back.
3. **測試連線並探索** → backend calls `/v1/models`, returns model ids.
4. Discovered models shown as a **checklist**; models.dev fills input/output price + context per row. Not silently auto-added — user picks.
5. Each checked model → one registry alias (auto-generated `provider_modelslug`, editable), written via the model registry.

**Local provider flow:**
- Auto-detect running local servers by probing common ports (Ollama :11434, MLX/omlx :8000, LM Studio :1234). Detected → list its `/v1/models`, no key, tier=local, $0. One-click add. A "download more" link deep-links out to the runtime (MUR is not a downloader).

**Discovery defaults:** OpenCode-style — discovery runs on connect, merge is non-destructive (manually-configured entries win), but presented as a **checklist** rather than silent bulk-add (because shared-key gateways like cc-proxy can expose many models).

### New Tauri commands (`mur-hub-gui/src-tauri/src/detail.rs` or new `models_admin.rs`)

| Command | Purpose |
|---------|---------|
| `list_providers()` | registry providers + detected local servers (grouped) |
| `probe_local_providers()` | port-scan localhost for known runtimes |
| `test_provider(base_url, secret_ref)` | connect + `GET /v1/models`, return ids (best-effort) |
| `enrich_models(provider, ids[])` | models.dev lookup → price/ctx per id |
| `add_models(provider, base_url, secret_ref, [{id, alias}])` | write registry aliases |
| `remove_model(ref_name)` | delete a registry alias |

`test_provider` and `probe_local_providers` are network/IO — keep them best-effort with timeouts, surfacing errors to the UI without panicking.

### Backend reuse

`test_provider` and the `/v1/models` discovery should live in `mur-core` (e.g. `model_discovery.rs`) so both the Hub commands and a future `mur model discover` CLI share one implementation. `enrich_models` wraps `model_prices::lookup` from S2.

### Tests

- Rust: `/v1/models` response parsing (OpenAI shape); alias generation/slugging; local port-probe with a mock server; registry write round-trip.
- TS: combobox filter/group/keyboard; checklist selection state; secret-mode segmented control never renders the stored key.

---

## Error Handling Principles

- **Network is always optional and best-effort.** Discovery, price fetch, and local probing never block, never fail the parent action, always time out. local-first is non-negotiable.
- **Secrets never echoed.** Keychain default; the key field shows a masked placeholder when a secret exists, never the value.
- **Non-destructive registry writes.** Adding via discovery merges; never clobbers a manually-edited entry. YAML writes stay atomic (temp + rename, per existing `store/yaml.rs` pattern).
- **Graceful degradation.** Unknown price/context → omit the badge, never show `$0` or `0%` for "unknown". (Mirrors the OpenCode lesson: hide unknowns rather than show misleading zeros.)

## YAGNI / Out of Scope

- No weight downloading or local model management (MUR is not a runtime).
- No LiteLLM fallback in v1 (models.dev only; fallback is a later optional add).
- No cache-read/cache-write pricing in the registry (models.dev has it; defer until there's a consumer).
- No per-request live cost metering UI (this is registry/static pricing only).

## Open Questions

None blocking. Implementation-plan stage will decide: exact local ports to probe, whether S3b is a new window vs a tab in existing settings, and the precise `ModelOption` field additions in TS.
