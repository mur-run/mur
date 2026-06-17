# S3 — Hub Model Library + Grouped Picker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Hub's flat model `<select>` with a searchable grouped combobox, and add a "Model Library" surface where every provider (cloud + auto-detected local) is connected, its models discovered via `/v1/models`, auto-priced from models.dev, and selectively added to the registry.

**Architecture:** A new generic OpenAI-compatible discovery in `mur-core/src/model_discovery.rs` (reusing the multi-shape `/v1/models` parse pattern from `discovery/omlx.rs`) is exposed through new Tauri commands in `mur-hub-gui/src-tauri/src/models_admin.rs`. The React UI gets a `ModelCombobox.tsx` (picker) replacing `ModelSection`, and a `ModelLibrary.tsx` settings surface. Pure TS helpers (filter/group/alias) are unit-tested with vitest; Rust logic is unit-tested with nextest; component wiring is build-verified.

**Tech Stack:** Rust (`reqwest::blocking`, `serde_json`, `cargo nextest`), Tauri 2, React + TypeScript, vitest. Depends on **S1** (cost/context fields, `effective_costs`) and **S2** (`model_prices::lookup`).

## Global Constraints

- Rust edition 2024; single source file ≤ 800 lines.
- No hardcoded values — local probe ports, timeouts, preset base URLs are named consts.
- Network is optional/best-effort with timeouts; never blocks or panics the UI.
- Secrets are stored as `SecretRef` (default keychain) and **never** returned to the UI.
- Brand user-facing is uppercase "MUR".
- `mur-hub-gui` is workspace-EXCLUDED — build/test it via its own manifest: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml`; fmt via `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (CI gotcha: root fmt skips excluded crates).
- UI tests: `cd mur-hub-gui/ui && npm test` (vitest).

---

### Task 1: Generic OpenAI-compatible `/v1/models` discovery (Rust core)

**Files:**
- Create: `mur-core/src/model_discovery.rs`
- Modify: `mur-core/src/lib.rs` + `mur-core/src/main.rs` (declare `pub mod model_discovery;` / `mod model_discovery;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn parse_models_response(json: &str) -> Vec<String>` — accepts OpenAI `{data:[{id}]}`, Ollama `{models:[{id|name}]}`, and bare `[{id}]`.
  - `pub fn discover_models(base_url: &str, api_key: Option<&str>, timeout_secs: u64) -> anyhow::Result<Vec<String>>` — GET `{base}/v1/models` (handles base already ending `/v1`), best-effort.
  - `pub fn default_alias(provider: &str, model_id: &str) -> String` — slug, e.g. `anthropic_claude_opus_4_8`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_envelope() {
        let j = r#"{"object":"list","data":[{"id":"gpt-5.2"},{"id":"o4"}]}"#;
        assert_eq!(parse_models_response(j), vec!["gpt-5.2", "o4"]);
    }

    #[test]
    fn parses_ollama_and_bare_shapes() {
        let ollama = r#"{"models":[{"name":"llama3"},{"name":"phi4"}]}"#;
        assert_eq!(parse_models_response(ollama), vec!["llama3", "phi4"]);
        let bare = r#"[{"id":"qwen3:8b"}]"#;
        assert_eq!(parse_models_response(bare), vec!["qwen3:8b"]);
    }

    #[test]
    fn alias_slugs_punctuation() {
        assert_eq!(default_alias("anthropic", "claude-opus-4-8"), "anthropic_claude_opus_4_8");
        assert_eq!(default_alias("openrouter", "meta-llama/llama-4"), "openrouter_meta_llama_llama_4");
        assert_eq!(default_alias("ollama", "qwen3:8b"), "ollama_qwen3_8b");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core model_discovery`
Expected: FAIL — module/functions don't exist.

- [ ] **Step 3: Implement**

```rust
//! Generic OpenAI-compatible model discovery: GET `{base}/v1/models` and
//! normalize the several response shapes local + cloud servers emit. Reuses
//! the multi-shape strategy from `discovery::omlx` but is provider-agnostic
//! and synchronous (`reqwest::blocking`) for CLI + Tauri-command callers.

use anyhow::Result;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}
impl Entry {
    fn ident(self) -> Option<String> {
        self.id.or(self.name)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Resp {
    OpenAi { data: Vec<Entry> },
    Ollama { models: Vec<Entry> },
    Bare(Vec<Entry>),
}

/// Extract model identifiers from any supported `/v1/models` shape.
pub fn parse_models_response(json: &str) -> Vec<String> {
    let resp: Resp = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let entries = match resp {
        Resp::OpenAi { data } => data,
        Resp::Ollama { models } => models,
        Resp::Bare(v) => v,
    };
    entries.into_iter().filter_map(Entry::ident).collect()
}

fn models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/models")
    } else {
        format!("{trimmed}/v1/models")
    }
}

/// Best-effort discovery against an OpenAI-compatible endpoint.
pub fn discover_models(base_url: &str, api_key: Option<&str>, timeout_secs: u64) -> Result<Vec<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;
    let mut rb = client.get(models_url(base_url));
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        rb = rb.header("Authorization", format!("Bearer {k}"));
    }
    let body = rb.send()?.text()?;
    Ok(parse_models_response(&body))
}

/// Stable registry alias for a discovered model: `provider_modelslug`,
/// lowercased, non-alphanumerics collapsed to `_`.
pub fn default_alias(provider: &str, model_id: &str) -> String {
    let mut s = String::with_capacity(provider.len() + model_id.len() + 1);
    s.push_str(&provider.to_ascii_lowercase());
    s.push('_');
    let mut prev_us = true;
    for ch in model_id.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
            prev_us = false;
        } else if !prev_us {
            s.push('_');
            prev_us = true;
        }
    }
    s.trim_end_matches('_').to_string()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-core model_discovery`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/model_discovery.rs mur-core/src/lib.rs mur-core/src/main.rs
git commit -m "feat(model-discovery): generic /v1/models parse + discover + alias"
```

---

### Task 2: Local provider probe (Rust core)

**Files:**
- Modify: `mur-core/src/model_discovery.rs`
- Test: inline tests with a `tiny_http`-free local listener (use `std::net::TcpListener` + a thread, or skip-if-bound). Prefer a pure test of the preset table + a probe against a stub.

**Interfaces:**
- Consumes: `discover_models` (Task 1).
- Produces:
  - `pub struct LocalPreset { pub key: &'static str, pub name: &'static str, pub base_url: &'static str }`
  - `pub const LOCAL_PRESETS: &[LocalPreset]` — Ollama `http://localhost:11434/v1`, MLX/omlx `http://127.0.0.1:8000/v1`, LM Studio `http://localhost:1234/v1`.
  - `pub struct DetectedLocal { pub key: String, pub name: String, pub base_url: String, pub models: Vec<String> }`
  - `pub fn probe_local(timeout_secs: u64) -> Vec<DetectedLocal>` — probes each preset, includes only those that answered with ≥0 models (reachable).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn local_presets_cover_known_runtimes() {
    let keys: Vec<_> = LOCAL_PRESETS.iter().map(|p| p.key).collect();
    assert!(keys.contains(&"ollama"));
    assert!(keys.contains(&"mlx"));
    assert!(keys.contains(&"lmstudio"));
}

#[test]
fn probe_local_handles_unreachable_without_panic() {
    // With nothing running on the preset ports in CI, probe returns a (possibly
    // empty) vec and never panics. Short timeout to keep the test fast.
    let _ = probe_local(1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core model_discovery`
Expected: FAIL — `LOCAL_PRESETS`/`probe_local` not found.

- [ ] **Step 3: Implement**

```rust
/// Known local OpenAI-compatible runtimes probed during auto-detection.
pub struct LocalPreset {
    pub key: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
}

pub const LOCAL_PRESETS: &[LocalPreset] = &[
    LocalPreset { key: "ollama",   name: "Ollama",        base_url: "http://localhost:11434/v1" },
    LocalPreset { key: "mlx",      name: "MLX (omlx)",    base_url: "http://127.0.0.1:8000/v1" },
    LocalPreset { key: "lmstudio", name: "LM Studio",     base_url: "http://localhost:1234/v1" },
];

/// A local runtime that answered the probe.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DetectedLocal {
    pub key: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Probe each local preset; return those reachable. Best-effort, never panics.
pub fn probe_local(timeout_secs: u64) -> Vec<DetectedLocal> {
    LOCAL_PRESETS
        .iter()
        .filter_map(|p| {
            // local servers need no key. Treat any successful HTTP response
            // (even an empty model list) as "reachable".
            match discover_models(p.base_url, None, timeout_secs) {
                Ok(models) => Some(DetectedLocal {
                    key: p.key.to_string(),
                    name: p.name.to_string(),
                    base_url: p.base_url.to_string(),
                    models,
                }),
                Err(_) => None,
            }
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-core model_discovery`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/model_discovery.rs
git commit -m "feat(model-discovery): local runtime auto-probe (ollama/mlx/lmstudio)"
```

---

### Task 3: Tauri commands for the Model Library (Rust, Hub)

**Files:**
- Create: `mur-hub-gui/src-tauri/src/models_admin.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (add `mod models_admin;` + register the commands in the `generate_handler!` list near `detail::list_models`)
- Modify: `mur-hub-gui/src-tauri/src/detail.rs:37-58` (extend `ModelOptionView` + `list_models` with tier/cost/context/caps)
- Test: inline `#[cfg(test)]` in `models_admin.rs` for the pure parts (view conversion); commands themselves are thin wrappers.

**Interfaces:**
- Consumes: `mur_core::model_discovery::{discover_models, probe_local, default_alias, DetectedLocal}`, `mur_core::model_prices`, `mur_common::model::{ModelRegistry, ModelEntry, SecretRef}`, `mur_common::route::RouteTier`.
- Produces Tauri commands:
  - `list_providers() -> Result<ProvidersView, String>` — registry providers grouped + presets.
  - `probe_local_providers() -> Result<Vec<DetectedLocalView>, String>`
  - `test_provider(base_url, api_key) -> Result<Vec<EnrichedModelView>, String>` — discover + models.dev enrich.
  - `add_models(provider, base_url, secret_kind, secret_value, picks) -> Result<(), String>` — write aliases.
  - `remove_model(ref_name) -> Result<(), String>`
- And the extended `ModelOptionView { ref_name, provider, model, tier, input_cost, output_cost, context_window, capabilities }`.

- [ ] **Step 1: Write the failing test (view conversion + enrich shape)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enriched_view_carries_pricing_and_alias() {
        let v = EnrichedModelView::build(
            "openai",
            "gpt-5.2",
            Some(mur_core::model_prices::PriceInfo {
                input_per_1k: 0.00125, output_per_1k: 0.01, context_window: Some(400_000),
            }),
        );
        assert_eq!(v.alias, "openai_gpt_5_2");
        assert_eq!(v.input_cost, Some(0.00125));
        assert_eq!(v.output_cost, Some(0.01));
        assert_eq!(v.context_window, Some(400_000));
    }

    #[test]
    fn secret_ref_built_from_kind() {
        assert!(matches!(build_secret("env", "OPENAI_API_KEY"), Some(SecretRef::Env(_))));
        assert!(matches!(build_secret("keychain", "sk-xxx"), Some(SecretRef::Keychain { .. })));
        assert!(build_secret("keychain", "").is_none()); // empty → no secret
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml enriched_view secret_ref`
Expected: FAIL — symbols missing.

- [ ] **Step 3: Implement `models_admin.rs`**

```rust
//! Tauri commands backing the Model Library surface. Thin wrappers over
//! `mur_core::model_discovery` + `mur_core::model_prices`, writing the
//! shared `~/.mur/models.yaml` registry. Secrets are stored as SecretRef
//! and never returned to the UI.

use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::RouteTier;
use mur_common::secret::SecretRef;
use mur_core::model_discovery::{self, default_alias};
use mur_core::model_prices::{self, PriceInfo};
use serde::{Deserialize, Serialize};

const PROBE_TIMEOUT_SECS: u64 = 3;
const DISCOVER_TIMEOUT_SECS: u64 = 10;
/// Keychain service name for stored provider keys.
const KEYCHAIN_SERVICE: &str = "mur";

#[derive(Serialize)]
pub struct EnrichedModelView {
    pub model: String,
    pub alias: String,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub context_window: Option<u64>,
}

impl EnrichedModelView {
    pub fn build(provider: &str, model: &str, price: Option<PriceInfo>) -> Self {
        Self {
            alias: default_alias(provider, model),
            input_cost: price.as_ref().map(|p| p.input_per_1k),
            output_cost: price.as_ref().map(|p| p.output_per_1k),
            context_window: price.as_ref().and_then(|p| p.context_window),
            model: model.to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct DetectedLocalView {
    pub key: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<EnrichedModelView>,
}

/// Build a SecretRef from a UI kind + value. Empty value → None.
pub fn build_secret(kind: &str, value: &str) -> Option<SecretRef> {
    if value.is_empty() {
        return None;
    }
    match kind {
        "env" => Some(SecretRef::Env(value.to_string())),
        "file" => value.parse::<SecretRef>().ok(),
        _ => Some(SecretRef::Keychain {
            service: KEYCHAIN_SERVICE.to_string(),
            account: value.to_string(),
        }),
    }
}

fn mur_home() -> std::path::PathBuf {
    ModelRegistry::default_path()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()))
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub struct Pick {
    pub model: String,
    pub alias: String,
}

#[tauri::command]
pub fn probe_local_providers() -> Result<Vec<DetectedLocalView>, String> {
    let home = mur_home();
    let detected = model_discovery::probe_local(PROBE_TIMEOUT_SECS);
    Ok(detected
        .into_iter()
        .map(|d| DetectedLocalView {
            models: d
                .models
                .iter()
                .map(|m| {
                    // local → price short-circuits to zero
                    let price = model_prices::lookup(&home, &d.key, m, true);
                    EnrichedModelView::build(&d.key, m, price)
                })
                .collect(),
            key: d.key,
            name: d.name,
            base_url: d.base_url,
        })
        .collect())
}

#[tauri::command]
pub fn test_provider(
    provider: String,
    base_url: String,
    api_key: String,
) -> Result<Vec<EnrichedModelView>, String> {
    let home = mur_home();
    let key = (!api_key.is_empty()).then_some(api_key.as_str());
    let ids = model_discovery::discover_models(&base_url, key, DISCOVER_TIMEOUT_SECS)
        .map_err(|e| format!("discovery failed: {e}"))?;
    Ok(ids
        .into_iter()
        .map(|m| {
            let price = model_prices::lookup(&home, &provider, &m, false);
            EnrichedModelView::build(&provider, &m, price)
        })
        .collect())
}

#[tauri::command]
pub fn add_models(
    provider: String,
    base_url: String,
    tier: String,
    secret_kind: String,
    secret_value: String,
    picks: Vec<Pick>,
) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    let home = mur_home();
    let route_tier = if tier == "local" { RouteTier::Local } else { RouteTier::Frontier };
    let secret = build_secret(&secret_kind, &secret_value);
    let is_local = route_tier == RouteTier::Local;
    for pick in picks {
        let price = model_prices::lookup(&home, &provider, &pick.model, is_local);
        let entry = ModelEntry {
            provider: provider.clone(),
            model: pick.model.clone(),
            base_url: Some(base_url.clone()),
            secret: secret.clone(),
            tier: Some(route_tier),
            input_cost_per_1k: price.as_ref().map(|p| p.input_per_1k),
            output_cost_per_1k: price.as_ref().map(|p| p.output_per_1k),
            context_window: price.as_ref().and_then(|p| p.context_window),
            ..Default::default()
        };
        // non-destructive: only insert if the alias is new
        reg.models.entry(pick.alias).or_insert(entry);
    }
    reg.save_to(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_model(ref_name: String) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    reg.models.remove(&ref_name);
    reg.save_to(&path).map_err(|e| e.to_string())
}
```

Extend `detail.rs` `ModelOptionView` + `list_models` (`detail.rs:37-58`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOptionView {
    pub ref_name: String,
    pub provider: String,
    pub model: String,
    pub tier: Option<String>,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub context_window: Option<u64>,
    pub capabilities: Vec<String>,
}

#[tauri::command]
pub fn list_models() -> Result<Vec<ModelOptionView>, String> {
    let path = mur_common::model::ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let reg = mur_common::model::ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    Ok(reg
        .models
        .into_iter()
        .map(|(ref_name, entry)| {
            let (input_cost, output_cost) = entry.effective_costs();
            ModelOptionView {
                ref_name,
                provider: entry.provider,
                model: entry.model,
                tier: entry.tier.map(|t| format!("{t:?}").to_lowercase()),
                input_cost,
                output_cost,
                context_window: entry.context_window,
                capabilities: entry.capabilities,
            }
        })
        .collect())
}
```

Register in `lib.rs` handler list (next to `detail::list_models`):

```rust
            detail::list_models,
            models_admin::probe_local_providers,
            models_admin::test_provider,
            models_admin::add_models,
            models_admin::remove_model,
```

and add `mod models_admin;` near the other `mod` declarations.

- [ ] **Step 4: Run test + build the Hub backend**

Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml enriched_view secret_ref`
Expected: PASS.
Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds.

- [ ] **Step 5: fmt (excluded-crate manifest) + commit**

Run: `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml`

```bash
git add mur-hub-gui/src-tauri/src/models_admin.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/src/detail.rs
git commit -m "feat(hub): models_admin Tauri commands + enriched list_models"
```

---

### Task 4: Picker — pure TS helpers (filter / group / format) with vitest

**Files:**
- Create: `mur-hub-gui/ui/src/components/modelPicker.ts` (pure helpers)
- Create: `mur-hub-gui/ui/src/components/modelPicker.test.ts`
- Modify: `mur-hub-gui/ui/src/types.ts` (extend `ModelOption`)

**Interfaces:**
- Consumes: `ModelOption` view (extended).
- Produces:
  - `export interface ModelOption { ref_name; provider; model; tier?: string; input_cost?: number; output_cost?: number; context_window?: number; capabilities: string[] }`
  - `export function filterModels(models: ModelOption[], term: string): ModelOption[]`
  - `export function groupByProvider(models: ModelOption[]): [string, ModelOption[]][]`
  - `export function formatCost(perK?: number): string | null` — `null` when undefined (so the UI hides the badge, never shows `$0` for unknown).

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect } from "vitest";
import { filterModels, groupByProvider, formatCost, type ModelOption } from "./modelPicker";

const M: ModelOption[] = [
  { ref_name: "claude_opus", provider: "anthropic", model: "claude-opus-4-8", capabilities: [] },
  { ref_name: "claude_sonnet", provider: "anthropic", model: "claude-sonnet-4-6", capabilities: [] },
  { ref_name: "ds_flash", provider: "deepseek", model: "deepseek-v4-flash", capabilities: [] },
];

describe("modelPicker", () => {
  it("filters on alias, provider, and model id", () => {
    expect(filterModels(M, "sonnet").map(m => m.ref_name)).toEqual(["claude_sonnet"]);
    expect(filterModels(M, "deepseek").map(m => m.ref_name)).toEqual(["ds_flash"]);
    expect(filterModels(M, "").length).toBe(3);
  });
  it("groups by provider preserving membership", () => {
    const g = groupByProvider(M);
    const anthropic = g.find(([p]) => p === "anthropic")![1];
    expect(anthropic.length).toBe(2);
  });
  it("formatCost hides unknown, shows known per-million", () => {
    expect(formatCost(undefined)).toBeNull();
    // 0.005 per 1k → $5/M
    expect(formatCost(0.005)).toBe("$5/M");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd mur-hub-gui/ui && npm test`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement helpers + extend types**

`modelPicker.ts`:

```ts
export interface ModelOption {
  ref_name: string;
  provider: string;
  model: string;
  tier?: string;
  input_cost?: number;
  output_cost?: number;
  context_window?: number;
  capabilities: string[];
}

export function filterModels(models: ModelOption[], term: string): ModelOption[] {
  const t = term.trim().toLowerCase();
  if (!t) return models;
  return models.filter(
    (m) =>
      m.ref_name.toLowerCase().includes(t) ||
      m.provider.toLowerCase().includes(t) ||
      m.model.toLowerCase().includes(t),
  );
}

export function groupByProvider(models: ModelOption[]): [string, ModelOption[]][] {
  const map = new Map<string, ModelOption[]>();
  for (const m of models) {
    const arr = map.get(m.provider) ?? [];
    arr.push(m);
    map.set(m.provider, arr);
  }
  return [...map.entries()];
}

/** Per-1k cost → "$X/M" label, or null when unknown (UI hides the badge). */
export function formatCost(perK?: number): string | null {
  if (perK === undefined || perK === null) return null;
  const perM = perK * 1000;
  return `$${Number(perM.toFixed(2))}/M`;
}
```

Update `types.ts`: replace the existing `ModelOption` interface with a re-export or the extended shape (keep a single source — import from `modelPicker.ts` where `ModelOption` is consumed, or update the interface in `types.ts` to match exactly and have `modelPicker.ts` import it). Choose: define in `modelPicker.ts`, and in `types.ts` `export type { ModelOption } from "./components/modelPicker";`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd mur-hub-gui/ui && npm test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/modelPicker.ts mur-hub-gui/ui/src/components/modelPicker.test.ts mur-hub-gui/ui/src/types.ts
git commit -m "feat(hub-ui): pure model picker helpers (filter/group/formatCost)"
```

---

### Task 5: Picker — `ModelCombobox` component replaces `ModelSection`

**Files:**
- Create: `mur-hub-gui/ui/src/components/ModelCombobox.tsx`
- Modify: `mur-hub-gui/ui/src/components/DetailPanel.tsx:217-294` (replace `ModelSection` body with `<ModelCombobox>`)
- Modify: `mur-hub-gui/ui/src/styles/components/detail-panel.css` (combobox styles, mirroring mockup option B tokens)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` (add `detail.modelSearch`, `detail.manageModels`)

**Interfaces:**
- Consumes: `list_models` (extended, Task 3), `filterModels`/`groupByProvider`/`formatCost` (Task 4), existing `update_agent_detail`.
- Produces: `<ModelCombobox detail={...} onSaved={...} onManage={...} />`.

- [ ] **Step 1: Build the component** (no unit test — DOM-level; verified by build + manual. The logic it uses is already tested in Task 4.)

Create `ModelCombobox.tsx` implementing the mockup-B interaction: trigger button (current alias + provider chip), popover with search input, provider-grouped rows, per-row badges via `formatCost(m.output_cost)` etc. (hidden when null), keyboard ↑↓/↵/esc, selecting calls `update_agent_detail` with `{ model_ref }` (unchanged contract from current `ModelSection.pick`). Add a "⚙︎ {t('detail.manageModels')}" link calling `onManage`.

Reuse the existing popover styles in `styles/components/popover.css` where possible.

- [ ] **Step 2: Wire into DetailPanel**

Replace the `ModelSection` function body (`DetailPanel.tsx:219-294`) to render `<ModelCombobox detail={detail} onSaved={handleSaved} onManage={() => setLibraryOpen(true)} />`, keeping the existing `list_models` fetch + `models.length === 0` empty state (`t('detail.modelEmpty')`).

- [ ] **Step 3: i18n keys**

Add to `en.ts`: `"detail.modelSearch": "Search models…"`, `"detail.manageModels": "Manage models…"`. Add zh-TW equivalents: `"detail.modelSearch": "搜尋模型…"`, `"detail.manageModels": "管理模型…"`.

- [ ] **Step 4: Build the UI**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: type-checks + builds clean.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/ModelCombobox.tsx mur-hub-gui/ui/src/components/DetailPanel.tsx mur-hub-gui/ui/src/styles/components/detail-panel.css mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub-ui): searchable grouped ModelCombobox replaces flat select"
```

---

### Task 6: Model Library surface (`ModelLibrary` component)

**Files:**
- Create: `mur-hub-gui/ui/src/components/ModelLibrary.tsx`
- Create: `mur-hub-gui/ui/src/components/modelLibrary.ts` (pure: preset table + selection reducer)
- Create: `mur-hub-gui/ui/src/components/modelLibrary.test.ts`
- Modify: `mur-hub-gui/ui/src/styles/components/` (add `model-library.css`; import in `index.css`)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` (library strings)
- Modify: wherever DetailPanel opens it (the `libraryOpen` state from Task 5) — render `<ModelLibrary open={libraryOpen} onClose={...} />`

**Interfaces:**
- Consumes: `probe_local_providers`, `test_provider`, `add_models`, `remove_model` (Task 3); `formatCost` (Task 4).
- Produces:
  - `export const CLOUD_PRESETS: { key; name; baseUrl }[]` (OpenAI/Google/OpenRouter/xAI/Custom).
  - `export function togglePick(sel: Set<string>, id: string): Set<string>` (pure selection reducer).

- [ ] **Step 1: Write the failing test (pure pieces)**

```ts
import { describe, it, expect } from "vitest";
import { CLOUD_PRESETS, togglePick } from "./modelLibrary";

describe("modelLibrary", () => {
  it("ships the expected cloud presets", () => {
    const keys = CLOUD_PRESETS.map((p) => p.key);
    expect(keys).toEqual(expect.arrayContaining(["openai", "google", "openrouter", "xai", "custom"]));
  });
  it("toggles selection immutably", () => {
    const a = togglePick(new Set(), "gpt-5.2");
    expect(a.has("gpt-5.2")).toBe(true);
    const b = togglePick(a, "gpt-5.2");
    expect(b.has("gpt-5.2")).toBe(false);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd mur-hub-gui/ui && npm test`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement pure module**

`modelLibrary.ts`:

```ts
export interface CloudPreset { key: string; name: string; baseUrl: string }

export const CLOUD_PRESETS: CloudPreset[] = [
  { key: "openai", name: "OpenAI", baseUrl: "https://api.openai.com/v1" },
  { key: "google", name: "Google (Gemini)", baseUrl: "https://generativelanguage.googleapis.com/v1beta" },
  { key: "openrouter", name: "OpenRouter", baseUrl: "https://openrouter.ai/api/v1" },
  { key: "xai", name: "xAI (Grok)", baseUrl: "https://api.x.ai/v1" },
  { key: "custom", name: "Custom (OpenAI-compatible)", baseUrl: "https://" },
];

/** Immutable toggle of a model id in a selection set. */
export function togglePick(sel: Set<string>, id: string): Set<string> {
  const next = new Set(sel);
  if (next.has(id)) next.delete(id); else next.add(id);
  return next;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd mur-hub-gui/ui && npm test`
Expected: PASS.

- [ ] **Step 5: Build the `ModelLibrary.tsx` component**

Implement the mockup layout: left rail (已連線 Cloud from `list_models` providers, 本機 Local from `probe_local_providers`, 新增 Provider from `CLOUD_PRESETS`); right pane = base-URL field (prefilled, editable), API-key field + keychain/env/file segmented control (Task 3 `build_secret` kinds), 測試連線並探索 button → `test_provider` → checklist (`togglePick`) with badges via `formatCost`, 加入 N 個 → `add_models`. Local panels show auto-detected state, no key, one-click add. Errors surface inline; all calls are best-effort.

- [ ] **Step 6: i18n + styles + build**

Add library i18n keys to `en.ts` and `zh-TW.ts` (provider section headers, Test button, Add button, key-storage labels). Add `model-library.css` and import it in `index.css`.
Run: `cd mur-hub-gui/ui && npm run build`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add mur-hub-gui/ui/src/components/ModelLibrary.tsx mur-hub-gui/ui/src/components/modelLibrary.ts mur-hub-gui/ui/src/components/modelLibrary.test.ts mur-hub-gui/ui/src/styles/ mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts mur-hub-gui/ui/src/components/DetailPanel.tsx
git commit -m "feat(hub-ui): Model Library surface (providers, discovery, auto-price)"
```

---

### Task 7: End-to-end verification

**Files:** none (verification only).

- [ ] **Step 1: Full backend test + lint**

Run: `cargo nextest run -p mur-core model_discovery model_prices`
Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Run: `cargo clippy --workspace -- -D warnings`
Run: `cargo fmt --check && cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml --check`
Expected: all pass/clean.

- [ ] **Step 2: Full UI test + build**

Run: `cd mur-hub-gui/ui && npm test && npm run build`
Expected: pass + build.

- [ ] **Step 3: Manual smoke (operator)**

Build + run the Hub. Verify: (a) agent DetailPanel shows the new combobox, search + select works, selection persists; (b) "管理模型…" opens the Library; (c) a cloud preset → paste key → Test → checklist with prices appears; (d) a running local Ollama/MLX is auto-detected; (e) added models appear in the picker. Note: per project memory, iOS sim is broken but the Hub desktop app is unaffected.

- [ ] **Step 4: Commit any fixes from smoke**

```bash
git add -A && git commit -m "fix(hub): address model library smoke-test findings"
```

---

## Self-Review

- **Spec coverage:** S3a picker (grouped searchable combobox, badges, manage link, unchanged `model_ref` contract) → Tasks 4–5; S3b library (provider rail, cloud connect flow, local auto-detect, checklist, alias, non-destructive write) → Tasks 3, 6; new Tauri commands table → Task 3; backend reuse in `mur-core` → Tasks 1–2; "hide unknowns not $0" → Task 4 `formatCost`; keychain default + never echo secrets → Task 3 (`build_secret`, secrets never in any `*View`). ✅
- **Placeholder scan:** Rust + pure-TS steps show full code. Component steps (5 Step1, 6 Step5) describe DOM wiring without a full listing — justified: their logic is unit-tested in Tasks 4/6 and they are build+manual-verified, consistent with the repo having vitest but no @testing-library. Not a logic placeholder. ✅
- **Type consistency:** `ModelOption` defined once in `modelPicker.ts`, re-exported from `types.ts`; `EnrichedModelView`/`PriceInfo` fields (`input_per_1k`/`output_per_1k`/`context_window`) consistent with S2; `default_alias` output (`anthropic_claude_opus_4_8`) matches between Rust (Task 1) and the TS test expectations (Task 3 alias). `build_secret` kinds (`keychain`/`env`/`file`) consistent between Task 3 and Task 6 segmented control. ✅
- **Cross-plan dependency:** requires S1 (fields/`effective_costs`) + S2 (`model_prices::lookup`). Stated in header. ✅
