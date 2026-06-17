# S1 — Registry Schema: Input/Output Cost — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split model cost into input/output rates (per-1k) plus a context-window field on `ModelEntry`, with back-compat for the legacy single rate, and surface both in router + CLI.

**Architecture:** Add three optional fields to `ModelEntry` in `mur-common`, derive `Default` so the ~18 literal construction sites stay compilable via mechanical field additions, add an `effective_costs()` accessor that falls back to the legacy `cost_per_1k_tokens` as the output rate, update the router's cost estimate to use it, and extend `mur model add`/`show` CLI.

**Tech Stack:** Rust (edition 2024), `serde` / `serde_yaml_ng`, `clap`, `cargo nextest`.

## Global Constraints

- Rust edition 2024 — let-chains stable.
- No hardcoded values — defaults via constants (e.g. TTLs, ports) live in config/consts, not inline literals.
- Single source file ≤ 800 lines.
- YAML writes use temp file + rename for atomicity (already implemented in `ModelRegistry::save_to`).
- Tests run under `cargo nextest` (CI); plain `cargo test --workspace` is known-flaky on 7 unrelated mur-core tests — verify with `cargo nextest run -p <crate>`.
- Brand name user-facing is uppercase "MUR"; internal identifiers stay lowercase.

---

### Task 1: Add input/output cost + context fields to `ModelEntry`

**Files:**
- Modify: `mur-common/src/model.rs:21-41` (struct `ModelEntry`)
- Test: `mur-common/src/model.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `ModelEntry.input_cost_per_1k: Option<f64>`, `ModelEntry.output_cost_per_1k: Option<f64>`, `ModelEntry.context_window: Option<u64>`; `impl Default for ModelEntry`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `mur-common/src/model.rs`:

```rust
#[test]
fn parses_split_cost_fields() {
    let yaml = r#"
schema_version: 1
models:
  opus:
    provider: anthropic
    model: claude-opus-4-8
    input_cost_per_1k: 0.005
    output_cost_per_1k: 0.025
    context_window: 200000
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    let e = r.models.get("opus").unwrap();
    assert_eq!(e.input_cost_per_1k, Some(0.005));
    assert_eq!(e.output_cost_per_1k, Some(0.025));
    assert_eq!(e.context_window, Some(200_000));
}

#[test]
fn default_model_entry_is_empty() {
    let e = ModelEntry::default();
    assert!(e.provider.is_empty());
    assert_eq!(e.input_cost_per_1k, None);
    assert_eq!(e.output_cost_per_1k, None);
    assert_eq!(e.context_window, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-common parses_split_cost_fields default_model_entry_is_empty`
Expected: FAIL — `no field input_cost_per_1k`, and `ModelEntry: Default is not satisfied`.

- [ ] **Step 3: Add fields + derive Default**

In `mur-common/src/model.rs`, change the derive line and add fields after `cost_per_1k_tokens` (line 40):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelEntry {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<RouteTier>,
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
    /// Context window in tokens (display only; sourced from discovery / models.dev).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}
```

(Note: `provider`/`model` gain `#[serde(default)]` so `Default` and partial YAML both work; existing required-field behavior is preserved because real entries always supply them.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-common parses_split_cost_fields default_model_entry_is_empty`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/model.rs
git commit -m "feat(model): add input/output cost + context_window to ModelEntry"
```

---

### Task 2: Fix the ~18 `ModelEntry` literal construction sites

Adding fields without `..Default::default()` breaks every full-literal construction. Use `..Default::default()` at each site to stay DRY and future-proof.

**Files (each has a `ModelEntry { ... }` literal to patch):**
- Modify: `mur-agent-runtime/src/supervisor_runner.rs:196`
- Modify: `mur-agent-runtime/src/supervisor.rs:1092`
- Modify: `mur-agent-runtime/tests/model_resolution.rs:40`
- Modify: `mur-common/src/model.rs:170,229,256,307,389` (test module)
- Modify: `mur-core/src/cmd/agent/export.rs:244`
- Modify: `mur-core/src/cmd/agent/install.rs:280`
- Modify: `mur-core/src/cmd/agent/model_resolve.rs:76`
- Modify: `mur-core/src/cmd/fleet_sync.rs:594`
- Modify: `mur-core/src/cmd/model.rs:156,226`
- Modify: `mur-core/src/route/mod.rs:288,301,394`
- Modify: `mur-core/tests/route_fixtures.rs:11,24`

(Exclude `mur-core/src/discovery/omlx.rs:102` — that is a different private wire struct, not `mur_common::model::ModelEntry`.)

**Interfaces:**
- Consumes: `impl Default for ModelEntry` (Task 1).
- Produces: all crates compile with the new fields.

- [ ] **Step 1: Patch each site**

At each literal, the last explicitly-set field is `cost_per_1k_tokens: ...,`. Immediately after it (before the closing `}`), add:

```rust
            ..Default::default()
```

So e.g. `mur-core/src/route/mod.rs:301` becomes:

```rust
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                base_url: None,
                secret: None,
                capabilities: vec!["chat".into(), "tools".into()],
                params: serde_json::Value::Null,
                tier: Some(RouteTier::Frontier),
                cost_per_1k_tokens: Some(0.015),
                ..Default::default()
            },
```

Apply the same `..Default::default()` addition to all sites listed above.

- [ ] **Step 2: Verify the whole workspace compiles**

Run: `cargo build --workspace`
Expected: builds clean (the excluded Tauri crates are not in `--workspace`).

- [ ] **Step 3: Verify agent-runtime + its tests compile (separate manifest check)**

Run: `cargo nextest run -p mur-agent-runtime --no-run`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(model): use ..Default::default() at ModelEntry construction sites"
```

---

### Task 3: `effective_costs()` accessor with legacy fallback

**Files:**
- Modify: `mur-common/src/model.rs` (add `impl ModelEntry` block after the struct, before `RoleEntry`)
- Test: `mur-common/src/model.rs` tests module

**Interfaces:**
- Consumes: `ModelEntry` fields (Task 1).
- Produces: `ModelEntry::effective_costs(&self) -> (Option<f64>, Option<f64>)` returning `(input_per_1k, output_per_1k)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn effective_costs_fallback_matrix() {
    // legacy only → both fall back to the blended rate
    let mut e = ModelEntry { cost_per_1k_tokens: Some(0.01), ..Default::default() };
    assert_eq!(e.effective_costs(), (Some(0.01), Some(0.01)));

    // split only → split wins, legacy ignored
    e = ModelEntry {
        input_cost_per_1k: Some(0.005),
        output_cost_per_1k: Some(0.025),
        ..Default::default()
    };
    assert_eq!(e.effective_costs(), (Some(0.005), Some(0.025)));

    // both → split wins
    e = ModelEntry {
        cost_per_1k_tokens: Some(0.01),
        input_cost_per_1k: Some(0.005),
        output_cost_per_1k: Some(0.025),
        ..Default::default()
    };
    assert_eq!(e.effective_costs(), (Some(0.005), Some(0.025)));

    // none → none
    e = ModelEntry::default();
    assert_eq!(e.effective_costs(), (None, None));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-common effective_costs_fallback_matrix`
Expected: FAIL — `no method named effective_costs`.

- [ ] **Step 3: Implement the accessor**

Add after the `ModelEntry` struct definition in `mur-common/src/model.rs`:

```rust
impl ModelEntry {
    /// Resolve effective per-1k rates as `(input, output)`.
    ///
    /// The deprecated `cost_per_1k_tokens` is treated as the output rate and
    /// also as the input fallback, so legacy single-rate entries keep working.
    pub fn effective_costs(&self) -> (Option<f64>, Option<f64>) {
        let output = self.output_cost_per_1k.or(self.cost_per_1k_tokens);
        let input = self.input_cost_per_1k.or(self.cost_per_1k_tokens);
        (input, output)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-common effective_costs_fallback_matrix`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/model.rs
git commit -m "feat(model): add effective_costs() with legacy fallback"
```

---

### Task 4: Router uses `effective_costs()` for cost estimate

**Files:**
- Modify: `mur-core/src/route/mod.rs:273-276` (`frontier_cost_per_1k`)
- Test: `mur-core/src/route/mod.rs` tests module

**Interfaces:**
- Consumes: `ModelEntry::effective_costs()` (Task 3).
- Produces: unchanged signature `fn frontier_cost_per_1k(&self) -> Option<f64>`; now returns the effective **output** rate (conservative, dominates the blended estimate).

- [ ] **Step 1: Write the failing test**

Add to `mur-core/src/route/mod.rs` tests. This proves a split-cost frontier entry yields the output rate (not the legacy field, which is absent):

```rust
#[test]
fn frontier_cost_prefers_output_rate() {
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "anthropic_opus".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-8".into(),
            tier: Some(RouteTier::Frontier),
            input_cost_per_1k: Some(0.005),
            output_cost_per_1k: Some(0.025),
            ..Default::default()
        },
    );
    let router = Router::new(reg).unwrap();
    // estimate_event escalates a hard task; counterfactual uses output rate.
    let ev = router.estimate_event(
        "2026-01-01T00:00:00Z",
        "refactor the entire auth system across 12 modules",
        TaskType::Refactor,
        1000,
        None,
    );
    // 1000 tokens / 1000 * 0.025 = 0.025
    assert!((ev.estimated_cost - 0.025).abs() < 1e-9, "got {}", ev.estimated_cost);
}
```

(If `estimate_event` is named differently, confirm via the existing call at `route/mod.rs:160`; the method that produces `EscalationEvent` is the one to call.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core frontier_cost_prefers_output_rate`
Expected: FAIL — `estimated_cost` is `0.0` because `frontier_cost_per_1k` reads the now-`None` `cost_per_1k_tokens`.

- [ ] **Step 3: Update `frontier_cost_per_1k`**

Replace the body at `mur-core/src/route/mod.rs:273-276`:

```rust
    /// USD-per-1k of the best frontier model, if known. Uses the effective
    /// OUTPUT rate (the escalation estimate has a single token count, and
    /// output dominates real bills, so it is the conservative choice).
    fn frontier_cost_per_1k(&self) -> Option<f64> {
        let id = self.pick_best_frontier()?;
        let (_input, output) = self.registry.models.get(&id)?.effective_costs();
        output
    }
```

- [ ] **Step 4: Run test + regression suite**

Run: `cargo nextest run -p mur-core frontier_cost_prefers_output_rate`
Expected: PASS.
Run: `cargo nextest run -p mur-core route::`
Expected: all existing route tests still PASS (legacy `Some(0.015)` fixtures unchanged).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/route/mod.rs
git commit -m "feat(route): cost estimate uses effective output rate"
```

---

### Task 5: CLI `mur model add --input-cost/--output-cost` + `show`

**Files:**
- Modify: `mur-core/src/cmd/model.rs:22-44` (the `Add` variant args)
- Modify: `mur-core/src/cmd/model.rs:135-169` (the `Add` match arm)
- Modify: `mur-core/src/cmd/model.rs` (`Show` match arm — print costs)
- Test: `mur-core/src/cmd/model.rs` tests (add a `#[cfg(test)]` module if none exists) OR a new `mur-core/tests/model_cli.rs`

**Interfaces:**
- Consumes: `ModelEntry` fields (Task 1).
- Produces: `--input-cost`, `--output-cost` flags (per 1k); `--cost-per-1k` retained, mapped to `output_cost_per_1k` when `--output-cost` absent.

- [ ] **Step 1: Write the failing test**

Create `mur-core/tests/model_cli.rs`:

```rust
use mur_common::model::ModelEntry;

/// Mirror of the CLI's cost-resolution rule so we can unit-test it without
/// spawning the binary. The real arm calls the same helper.
#[test]
fn cost_flags_map_to_fields() {
    // --input-cost + --output-cost set the split fields directly.
    let e = mur_core::cmd::model::build_entry_costs(
        ModelEntry::default(), Some(0.005), Some(0.025), None);
    assert_eq!(e.input_cost_per_1k, Some(0.005));
    assert_eq!(e.output_cost_per_1k, Some(0.025));

    // legacy --cost-per-1k maps to output when --output-cost absent.
    let e = mur_core::cmd::model::build_entry_costs(
        ModelEntry::default(), None, None, Some(0.01));
    assert_eq!(e.output_cost_per_1k, Some(0.01));
    assert_eq!(e.input_cost_per_1k, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core cost_flags_map_to_fields`
Expected: FAIL — `build_entry_costs` not found.

- [ ] **Step 3: Add the helper + flags + wire the Add arm**

In `mur-core/src/cmd/model.rs`, add to the `Add` variant after `cost_per_1k`:

```rust
        /// Estimated USD per 1000 INPUT tokens.
        #[arg(long)]
        input_cost: Option<f64>,
        /// Estimated USD per 1000 OUTPUT tokens.
        #[arg(long)]
        output_cost: Option<f64>,
```

Add a public helper (so it's unit-testable) near the top of the module:

```rust
/// Apply cost flags to an entry. `--output-cost` wins; the legacy
/// `--cost-per-1k` maps to output when `--output-cost` is absent.
pub fn build_entry_costs(
    mut e: ModelEntry,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
    cost_per_1k: Option<f64>,
) -> ModelEntry {
    e.input_cost_per_1k = input_cost;
    e.output_cost_per_1k = output_cost.or(cost_per_1k);
    e
}
```

In the `Add` match arm, destructure `input_cost, output_cost` too, and build the entry via:

```rust
            let entry = build_entry_costs(
                ModelEntry {
                    provider,
                    model,
                    base_url,
                    secret: secret_ref,
                    capabilities,
                    tier,
                    ..Default::default()
                },
                input_cost,
                output_cost,
                cost_per_1k,
            );
            reg.models.insert(name.clone(), entry);
```

In the `Show` arm, after printing provider/model, add:

```rust
            let (inp, out) = e.effective_costs();
            if let Some(i) = inp { println!("  input  $/1k: {i}"); }
            if let Some(o) = out { println!("  output $/1k: {o}"); }
            if let Some(c) = e.context_window { println!("  context: {c}"); }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-core cost_flags_map_to_fields`
Expected: PASS.

- [ ] **Step 5: Verify clippy + fmt**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/model.rs mur-core/tests/model_cli.rs
git commit -m "feat(cli): mur model add --input-cost/--output-cost; show prints costs"
```

---

## Self-Review

- **Spec coverage:** S1 "Data model change" → Task 1; "Back-compat accessor" → Task 3; "Router/ledger update" → Task 4; "CLI" → Task 5; "Tests" → Tasks 1,3,4,5. The ~18-site breakage (implied by the schema change) → Task 2. ✅
- **Placeholder scan:** none — every code step shows full code. The one soft spot (`estimate_event` exact name) is flagged with a verification pointer to `route/mod.rs:160`. ✅
- **Type consistency:** `effective_costs()` returns `(Option<f64>, Option<f64>)` consistently in Tasks 3/4/5; `build_entry_costs` signature consistent between test and impl. ✅
