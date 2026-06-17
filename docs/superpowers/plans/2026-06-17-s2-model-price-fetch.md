# S2 — Auto Price Fetch (models.dev) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Auto-fill input/output cost + context window for cloud models from the models.dev catalog, cached locally and best-effort, wired into `mur model add`.

**Architecture:** A new `mur-core/src/model_prices.rs` with a **pure** core (`parse_catalog` → `Catalog::lookup` with per-1M→per-1k conversion and fallback matching) testable from fixtures, wrapped by a network/cache layer (`reqwest::blocking` + a TTL'd `~/.mur/cache/model-prices.json`). Local-tier lookups short-circuit to zero with no network. `mur model add` calls it after writing the entry unless costs were given or `--no-fetch` is set.

**Tech Stack:** Rust (edition 2024), `reqwest::blocking`, `serde_json`, `cargo nextest`. Depends on **S1** (the `input_cost_per_1k`/`output_cost_per_1k`/`context_window` fields and `ModelEntry::effective_costs`).

## Global Constraints

- Rust edition 2024.
- No hardcoded values — TTL, timeout, URL, and cache filename are named `const`s at module top.
- Network is **optional and best-effort**: every fetch is timeout-bounded and never fails the parent command.
- Single source file ≤ 800 lines.
- Tests run under `cargo nextest`.
- Pure logic (parse/convert/match) is separated from IO so it is testable without network.

---

### Task 1: Pure catalog parse + per-1M→per-1k conversion + lookup

**Files:**
- Create: `mur-core/src/model_prices.rs`
- Modify: `mur-core/src/lib.rs` (add `pub mod model_prices;`)
- Modify: `mur-core/src/main.rs` (add `mod model_prices;` if main declares modules separately — check; mur-core declares mods in both lib.rs AND main.rs per project gotcha)
- Test: inline `#[cfg(test)]` in `model_prices.rs`

**Interfaces:**
- Consumes: nothing (pure).
- Produces:
  - `pub struct PriceInfo { pub input_per_1k: f64, pub output_per_1k: f64, pub context_window: Option<u64> }`
  - `pub struct Catalog` (opaque; wraps parsed data)
  - `pub fn parse_catalog(json: &str) -> anyhow::Result<Catalog>`
  - `impl Catalog { pub fn lookup(&self, provider: &str, model: &str) -> Option<PriceInfo> }`

- [ ] **Step 1: Write the failing test**

Put a small fixture inline (real models.dev shape: provider → `models` → per-model `cost.{input,output}` per **1M**, `limit.context`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "anthropic": {
        "id": "anthropic",
        "models": {
          "claude-opus-4-8": { "cost": { "input": 5, "output": 25 }, "limit": { "context": 200000 } }
        }
      },
      "deepseek": {
        "id": "deepseek",
        "models": {
          "deepseek-v4-flash": { "cost": { "input": 0.07, "output": 0.28 }, "limit": { "context": 128000 } }
        }
      }
    }"#;

    #[test]
    fn parses_and_converts_per_million_to_per_1k() {
        let cat = parse_catalog(FIXTURE).unwrap();
        let p = cat.lookup("anthropic", "claude-opus-4-8").unwrap();
        // 5 per 1M  → 0.005 per 1k ; 25 per 1M → 0.025 per 1k
        assert!((p.input_per_1k - 0.005).abs() < 1e-12);
        assert!((p.output_per_1k - 0.025).abs() < 1e-12);
        assert_eq!(p.context_window, Some(200_000));
    }

    #[test]
    fn lookup_miss_returns_none() {
        let cat = parse_catalog(FIXTURE).unwrap();
        assert!(cat.lookup("anthropic", "nonexistent-model").is_none());
        assert!(cat.lookup("no-such-provider", "x").is_none());
    }

    #[test]
    fn lookup_is_case_insensitive_on_model() {
        let cat = parse_catalog(FIXTURE).unwrap();
        assert!(cat.lookup("anthropic", "Claude-Opus-4-8").is_some());
    }

    #[test]
    fn lookup_matches_provider_namespaced_id() {
        // OpenRouter-style id "anthropic/claude-opus-4-8" under provider "openrouter"
        let cat = parse_catalog(FIXTURE).unwrap();
        let p = cat.lookup("openrouter", "anthropic/claude-opus-4-8");
        assert!(p.is_some(), "namespaced id should fall back to embedded provider/model");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core model_prices`
Expected: FAIL — module/functions don't exist.

- [ ] **Step 3: Implement the pure core**

```rust
//! models.dev price catalog: pure parse + lookup, plus a best-effort cached
//! fetch. Prices in the catalog are per 1,000,000 tokens; we convert to the
//! per-1k unit the registry uses (divide by 1000).

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

/// models.dev catalog endpoint (no auth required to read).
pub const CATALOG_URL: &str = "https://models.dev/api.json";
/// Cache filename under `~/.mur/cache/`.
pub const CACHE_FILE: &str = "model-prices.json";
/// Catalog cache freshness window.
pub const TTL_HOURS: u64 = 168; // 7 days
/// Per-request network timeout.
pub const TIMEOUT_SECS: u64 = 10;

/// Resolved pricing for one model, in the registry's per-1k unit.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceInfo {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub context_window: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    #[serde(default)]
    models: HashMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    cost: Option<RawCost>,
    #[serde(default)]
    limit: Option<RawLimit>,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawLimit {
    #[serde(default)]
    context: Option<u64>,
}

/// Parsed catalog: provider → (model → raw). Lookups apply unit conversion.
pub struct Catalog {
    providers: HashMap<String, RawProvider>,
}

/// Parse the models.dev JSON document.
pub fn parse_catalog(json: &str) -> Result<Catalog> {
    let providers: HashMap<String, RawProvider> = serde_json::from_str(json)?;
    Ok(Catalog { providers })
}

const PER_MILLION: f64 = 1_000_000.0 / 1_000.0; // = 1000: per-1M ÷ 1000 = per-1k

impl Catalog {
    /// Resolve `(provider, model)` to per-1k pricing. Matching order:
    /// exact → case-insensitive model → provider-namespaced id
    /// (`vendor/model`) resolved against the embedded vendor + model.
    pub fn lookup(&self, provider: &str, model: &str) -> Option<PriceInfo> {
        if let Some(p) = self.lookup_in(provider, model) {
            return Some(p);
        }
        // Namespaced id like "anthropic/claude-opus-4-8" (OpenRouter style).
        if let Some((vendor, sub)) = model.split_once('/') {
            if let Some(p) = self.lookup_in(vendor, sub) {
                return Some(p);
            }
        }
        None
    }

    fn lookup_in(&self, provider: &str, model: &str) -> Option<PriceInfo> {
        let prov = self.providers.get(provider)?;
        // exact, then case-insensitive
        let raw = prov.models.get(model).or_else(|| {
            prov.models
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(model))
                .map(|(_, v)| v)
        })?;
        let cost = raw.cost.as_ref()?;
        Some(PriceInfo {
            input_per_1k: cost.input? / PER_MILLION,
            output_per_1k: cost.output? / PER_MILLION,
            context_window: raw.limit.as_ref().and_then(|l| l.context),
        })
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-core model_prices`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/model_prices.rs mur-core/src/lib.rs mur-core/src/main.rs
git commit -m "feat(model-prices): pure models.dev catalog parse + lookup"
```

---

### Task 2: Cached best-effort fetch + `lookup()` orchestration

**Files:**
- Modify: `mur-core/src/model_prices.rs`
- Test: inline tests (filesystem cache, no real network)

**Interfaces:**
- Consumes: `parse_catalog`, `Catalog::lookup`, `PriceInfo` (Task 1).
- Produces:
  - `pub fn cache_path(mur_home: &Path) -> PathBuf`
  - `pub fn load_cached(mur_home: &Path, ttl_hours: u64) -> Option<Catalog>` (None if missing/stale/unparseable)
  - `pub fn lookup(mur_home: &Path, provider: &str, model: &str, tier_is_local: bool) -> Option<PriceInfo>` — local short-circuits to `Some(PriceInfo{0,0,None})`; otherwise serve fresh cache → else fetch+cache → else stale cache; all best-effort.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn local_tier_short_circuits_without_network() {
    let tmp = tempfile::tempdir().unwrap();
    let p = lookup(tmp.path(), "ollama", "llama3", true).unwrap();
    assert_eq!(p, PriceInfo { input_per_1k: 0.0, output_per_1k: 0.0, context_window: None });
    // no cache file created for local lookups
    assert!(!cache_path(tmp.path()).exists());
}

#[test]
fn serves_fresh_cache_without_network() {
    let tmp = tempfile::tempdir().unwrap();
    let cp = cache_path(tmp.path());
    std::fs::create_dir_all(cp.parent().unwrap()).unwrap();
    std::fs::write(&cp, super::tests::FIXTURE).unwrap();
    // TTL huge → cache is fresh → lookup resolves from disk, no network.
    let p = lookup(tmp.path(), "anthropic", "claude-opus-4-8", false).unwrap();
    assert!((p.output_per_1k - 0.025).abs() < 1e-12);
}

#[test]
fn stale_unparseable_cache_returns_none_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let cp = cache_path(tmp.path());
    std::fs::create_dir_all(cp.parent().unwrap()).unwrap();
    std::fs::write(&cp, "{ not json").unwrap();
    // load_cached must reject unparseable content.
    assert!(load_cached(tmp.path(), TTL_HOURS).is_none());
}
```

Add `tempfile` to `mur-core` dev-dependencies if absent (check `mur-core/Cargo.toml [dev-dependencies]`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core model_prices`
Expected: FAIL — `cache_path`/`load_cached`/`lookup` not found.

- [ ] **Step 3: Implement cache + orchestration**

Add to `model_prices.rs`:

```rust
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// `~/.mur/cache/model-prices.json` (mirrors skill_registry cache convention).
pub fn cache_path(mur_home: &Path) -> PathBuf {
    mur_home.join("cache").join(CACHE_FILE)
}

/// Load + parse the cache iff it exists and is younger than `ttl_hours`.
pub fn load_cached(mur_home: &Path, ttl_hours: u64) -> Option<Catalog> {
    let path = cache_path(mur_home);
    let meta = std::fs::metadata(&path).ok()?;
    let age = SystemTime::now().duration_since(meta.modified().ok()?).ok()?;
    if age > Duration::from_secs(ttl_hours * 3600) {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    parse_catalog(&body).ok()
}

/// Best-effort fetch the catalog and write it to cache. Returns the parsed
/// catalog on success; never panics, returns None on any failure.
fn fetch_and_cache(mur_home: &Path) -> Option<Catalog> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
        .ok()?;
    let body = client.get(CATALOG_URL).send().ok()?.text().ok()?;
    let cat = parse_catalog(&body).ok()?; // validate before caching
    let path = cache_path(mur_home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // atomic-ish write: temp + rename
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    Some(cat)
}

/// Resolve pricing for one model. Local tier short-circuits to zero with no
/// IO. Otherwise: fresh cache → network fetch (+cache) → stale cache. All
/// best-effort; returns None when nothing resolves.
pub fn lookup(
    mur_home: &Path,
    provider: &str,
    model: &str,
    tier_is_local: bool,
) -> Option<PriceInfo> {
    if tier_is_local {
        return Some(PriceInfo { input_per_1k: 0.0, output_per_1k: 0.0, context_window: None });
    }
    if let Some(cat) = load_cached(mur_home, TTL_HOURS) {
        return cat.lookup(provider, model);
    }
    if let Some(cat) = fetch_and_cache(mur_home) {
        return cat.lookup(provider, model);
    }
    // Last resort: stale cache (ignore TTL) so we degrade gracefully offline.
    let path = cache_path(mur_home);
    let body = std::fs::read_to_string(&path).ok()?;
    parse_catalog(&body).ok()?.lookup(provider, model)
}
```

Make the inline `FIXTURE` const visible to the cache tests by ensuring both test fns live in the same `tests` module (or mark `FIXTURE` `pub(crate)` and reference via `super::tests::FIXTURE` — the test above uses that path; simplest is to keep all tests in one `mod tests` and drop the `super::tests::` qualifier).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-core model_prices`
Expected: PASS (all tests; no network needed because fresh-cache and local paths are exercised).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/model_prices.rs mur-core/Cargo.toml
git commit -m "feat(model-prices): cached best-effort lookup with TTL + offline fallback"
```

---

### Task 3: Wire auto-fetch into `mur model add`; add `mur model prices` subcommands

**Files:**
- Modify: `mur-core/src/cmd/model.rs` (Add arm — call `model_prices::lookup`; add `--no-fetch`; add `Prices` subcommand)
- Test: `mur-core/tests/model_cli.rs` (extend) — test the fill helper purely

**Interfaces:**
- Consumes: `model_prices::lookup`, `PriceInfo`, S1's `ModelEntry` fields + `build_entry_costs`.
- Produces:
  - `pub fn apply_fetched_prices(e: ModelEntry, fetched: Option<PriceInfo>) -> ModelEntry` — fills only fields still `None` (explicit flags from S1 win).
  - CLI: `mur model add ... [--no-fetch]`; `mur model prices refresh`; `mur model prices show <name>`.

- [ ] **Step 1: Write the failing test**

```rust
use mur_core::model_prices::PriceInfo;

#[test]
fn fetched_prices_fill_only_empty_fields() {
    // user gave output via flag (S1); fetch must NOT overwrite it, but fills input + ctx.
    let e = mur_core::cmd::model::build_entry_costs(
        ModelEntry::default(), None, Some(0.99), None); // output=0.99 from flag
    let filled = mur_core::cmd::model::apply_fetched_prices(
        e,
        Some(PriceInfo { input_per_1k: 0.005, output_per_1k: 0.025, context_window: Some(200_000) }),
    );
    assert_eq!(filled.output_cost_per_1k, Some(0.99));      // flag preserved
    assert_eq!(filled.input_cost_per_1k, Some(0.005));      // filled
    assert_eq!(filled.context_window, Some(200_000));       // filled
}

#[test]
fn fetched_prices_none_is_noop() {
    let e = ModelEntry { input_cost_per_1k: Some(0.001), ..Default::default() };
    let filled = mur_core::cmd::model::apply_fetched_prices(e.clone(), None);
    assert_eq!(filled, e);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core fetched_prices`
Expected: FAIL — `apply_fetched_prices` not found.

- [ ] **Step 3: Implement helper + wire CLI**

Add to `mur-core/src/cmd/model.rs`:

```rust
use crate::model_prices::{self, PriceInfo};

/// Fill cost/context fields that are still `None` from a fetched PriceInfo.
/// Explicit values already on the entry (from flags) always win.
pub fn apply_fetched_prices(mut e: ModelEntry, fetched: Option<PriceInfo>) -> ModelEntry {
    if let Some(p) = fetched {
        if e.input_cost_per_1k.is_none() { e.input_cost_per_1k = Some(p.input_per_1k); }
        if e.output_cost_per_1k.is_none() { e.output_cost_per_1k = Some(p.output_per_1k); }
        if e.context_window.is_none() { e.context_window = p.context_window; }
    }
    e
}
```

Add `--no-fetch` to the `Add` variant:

```rust
        /// Skip the automatic models.dev price lookup.
        #[arg(long)]
        no_fetch: bool,
```

In the `Add` arm, after `build_entry_costs(...)` produces `entry`, before insert:

```rust
            let is_local = matches!(tier, Some(RouteTier::Local));
            let entry = if no_fetch {
                entry
            } else {
                let mur_home = ModelRegistry::default_path()?
                    .parent().map(|p| p.to_path_buf())
                    .unwrap_or_default();
                let fetched = model_prices::lookup(&mur_home, &entry.provider, &entry.model, is_local);
                let e = apply_fetched_prices(entry, fetched);
                if let (Some(i), Some(o)) = (e.input_cost_per_1k, e.output_cost_per_1k) {
                    println!("Auto-filled pricing from models.dev: in ${i}/1k · out ${o}/1k");
                }
                e
            };
            reg.models.insert(name.clone(), entry);
```

Add a `Prices` subcommand to `ModelCmd`:

```rust
    /// Inspect or refresh the models.dev price cache.
    Prices {
        #[command(subcommand)]
        sub: PricesSubCmd,
    },
```

```rust
#[derive(Subcommand, Debug)]
pub enum PricesSubCmd {
    /// Force-refresh the local price cache.
    Refresh,
    /// Show resolved pricing for a registry entry.
    Show { name: String },
}
```

Handle them in `run()`:

```rust
        ModelCmd::Prices { sub } => {
            let mur_home = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            match sub {
                PricesSubCmd::Refresh => {
                    // delete cache then force a lookup to repopulate
                    let _ = std::fs::remove_file(model_prices::cache_path(&mur_home));
                    match model_prices::lookup(&mur_home, "anthropic", "claude-opus-4-8", false) {
                        Some(_) => println!("Refreshed price cache at {}", model_prices::cache_path(&mur_home).display()),
                        None => println!("Could not refresh (offline?); cache unchanged."),
                    }
                }
                PricesSubCmd::Show { name } => {
                    let e = reg.models.get(&name)
                        .with_context(|| format!("no model '{name}' in registry"))?;
                    let is_local = matches!(e.tier, Some(RouteTier::Local));
                    match model_prices::lookup(&mur_home, &e.provider, &e.model, is_local) {
                        Some(p) => println!("{name}: in ${}/1k · out ${}/1k · ctx {:?}",
                            p.input_per_1k, p.output_per_1k, p.context_window),
                        None => println!("{name}: no pricing found on models.dev"),
                    }
                }
            }
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-core fetched_prices`
Expected: PASS.

- [ ] **Step 5: Lint + fmt**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/model.rs mur-core/tests/model_cli.rs
git commit -m "feat(cli): auto-fill pricing on model add; mur model prices refresh/show"
```

---

## Self-Review

- **Spec coverage:** S2 "Source/models.dev" → Task 1; "Cache + TTL + best-effort + offline" → Task 2; "Local tier never fetched" → Task 2 short-circuit; "Matching fallback order" → Task 1; "Wiring into model add" + "`--no-fetch`" + "`prices refresh/show`" → Task 3. ✅
- **Placeholder scan:** none — full code in every step. One verification pointer (check `[dev-dependencies]` for `tempfile`) is an explicit check, not a placeholder. ✅
- **Type consistency:** `PriceInfo { input_per_1k, output_per_1k, context_window }` identical across Tasks 1/2/3; `lookup(mur_home, provider, model, tier_is_local)` signature consistent; `apply_fetched_prices`/`build_entry_costs` reused from S1. ✅
- **Cross-plan dependency:** requires S1 merged (fields + `build_entry_costs`). Stated in header. ✅
