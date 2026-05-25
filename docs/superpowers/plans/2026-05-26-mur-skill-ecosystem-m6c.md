# M6c — LLM-Augmented Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn three M5-deferred stubs into working LLM-backed checks: `api-drift` (currently a `Severity::Unknown` placeholder), `consolidate` contradiction adjudication (currently rule-based first-step-tool diff only), and `coverage-gap` (new). All three share a single maintenance-call helper that mediates between `mur-core` and the existing model registry. **Every check degrades gracefully to its pre-M6c state when no model is available — LLMs are an upgrade, never a hard dependency.**

**Spec mapping:** §M6 LLM-driven api-drift bullet, §M6 LLM contradiction adjudication, §M6 coverage-gap detection. M5a §"Out of scope" #6, M5b §"Out of scope" #2 + #3.

**Hard dependency on M5a + M5b + scoping doc:**
- M5a: `mur skill doctor` Check trait + `api-drift` registered as `Severity::Unknown` stub.
- M5b: `mur skill consolidate` rule-based contradiction pass + JSONL report writer.
- M6 scoping doc §4.5: helper lives at `mur-core::skill_llm`, internally consumes `mur model` registry. **Not** an extension of `mur model` itself.
- M6c.1 (recommended, not required): if LanceDB skill vector index exists, the contradiction adjudicator can use it as a candidate filter before LLM calls. If absent, falls back to rule-based pre-filter.

**No hard dependency on M6a / M6b:** M6c can ship in parallel with the MCP track. The api-drift check operates on trace clusters + manifest text; it does not need `mcp_requirements` or `intent`.

**What M6c ships:**
1. `mur-core::skill_llm` — `maintenance_call(prompt, model_ref, budget) -> Result<Option<String>>`. `Ok(None)` = unavailable, caller degrades.
2. `LlmConfig::role(&str)` helper that picks an appropriate model from the registry for `"maintenance"` role (falls back to user's default chat role).
3. Content-hash cache for maintenance calls: identical (skill, prompt-template, content-hash) → cached response in `~/.mur/skill_llm_cache/`. TTL 30 days, configurable.
4. `api-drift` LLM implementation: cluster recent traces, ask the LLM whether the skill's procedure still matches observed tool usage. Real Severity (Info / Warning / Error) replaces the stub.
5. `consolidate` LLM contradiction adjudicator: when rule-based pass flags a pair OR vector dedup flags a borderline pair (cosine in `[0.85, 0.92]` from M6c.1), the LLM gives a verdict.
6. `coverage-gap` doctor check: cluster recent failed-skill traces, ask the LLM what intent / step would have unblocked them.
7. CLI flags: `mur skill doctor --llm` and `mur skill consolidate --llm-adjudicate`. Both default OFF.
8. Cost budget: per-call cap (default 1500 tokens out + 4000 in), per-day cap (default $0.50). Configurable in `~/.mur/config.yaml`.

**What M6c does NOT ship:**
- Auto-fix derived from LLM verdicts. The LLM emits findings; user / `--apply` decides. No silent skill rewrites.
- Streaming LLM output — maintenance calls are one-shot.
- A new model provider abstraction. Re-uses the existing `extract_llm.rs` config + HTTP client.
- LLM-driven `mcp_requirements` inference (mentioned in M6a as future work) — narrowly scoped to the three checks above. Add a separate plan if needed later.
- Cross-skill batch prompting (e.g., one LLM call adjudicates 20 contradiction pairs). Per-pair calls only — simpler accounting, cleaner cache keys.

**Tech Stack:** Rust 2024. Re-uses `tokio`, `reqwest`, `serde_json` (already in `mur-core`). New deps:
- `sha2` (likely already present via M5a) — content-hash cache keys.
- `chrono` (already present) — TTL math.

**Deployment assumption:** Single-host. User has at least one model configured via `mur model add`. If no model is configured, M6c checks behave exactly like M5a/M5b — that is the explicit graceful-degradation contract.

---

## File Structure

**Create:**
- `mur-core/src/skill_llm/mod.rs` — `maintenance_call`, `TokenBudget`, `SkillLlmError`, role resolution.
- `mur-core/src/skill_llm/cache.rs` — content-hash cache with TTL + atomic write.
- `mur-core/src/skill_llm/budget.rs` — per-call and per-day budget tracking via a JSON ledger at `~/.mur/skill_llm_budget.json`.
- `mur-core/src/skill_llm/prompts.rs` — prompt templates for each of the three checks (versioned constants so we can detect cache invalidation).
- `mur-core/src/skill_doctor/checks/api_drift.rs` — replace M5a stub with the LLM-backed real check.
- `mur-core/src/skill_doctor/checks/coverage_gap.rs` — new Warning-level check.
- `mur-core/src/skill_consolidate/contradiction_llm.rs` — adjudicator layered on top of the M5b rule-based pass.
- `mur-core/src/skill_traces/cluster.rs` — trace clustering helper (input to api-drift + coverage-gap).
- `mur-core/tests/skill_llm_cache.rs` — TTL + atomic write tests, no network.
- `mur-core/tests/skill_llm_budget.rs` — daily cap tests, no network.
- `mur-core/tests/skill_doctor_api_drift.rs` — uses a mock `MaintenanceClient` trait impl that returns canned responses.
- `mur-core/tests/skill_doctor_coverage_gap.rs` — same pattern.
- `mur-core/tests/skill_consolidate_llm.rs` — adjudicator over a borderline-similarity fixture pair.

**Modify:**
- `mur-core/src/skill_doctor/checks/api_drift.rs` (the existing stub) — promote from `Severity::Unknown` to LLM-driven; if `--llm` flag is OFF or no model available, keep emitting the stub finding with a clarifying message.
- `mur-core/src/skill_consolidate/contradiction.rs` (M5b) — expose `RuleBasedContradiction` so the adjudicator can wrap it without re-implementing.
- `mur-core/src/cmd/skill_doctor.rs` — add `--llm` flag.
- `mur-core/src/cmd/skill_consolidate.rs` — add `--llm-adjudicate` flag.
- `mur-core/src/lib.rs` — `pub mod {skill_llm, skill_traces};`
- `mur-core/src/store/config.rs` — add `skill_llm: SkillLlmConfig` section with `per_call_token_cap`, `per_day_usd_cap`, `cache_ttl_days`, optional `model_ref` override.

**Do not modify:**
- `mur-common::skill::manifest` — no schema change. LLM verdicts are doctor findings, not manifest content.
- `mur-common::skill::stats` — no new counters in M6c (cache hits are observability; track in tracing spans, not stats). If a counter becomes useful in field use, add it under M5b Task 0's additive policy.
- `extract_llm.rs` — separate caller, separate cache, separate budget. Sharing would couple workflow extraction's session-scoped budget to skill maintenance's per-day budget. Keep them isolated.
- DSSE signing — LLM never sees the signature; never writes back to the manifest.

---

### Task 1 — `skill_llm` helper foundation

**Files:** `mur-core/src/skill_llm/{mod.rs,budget.rs,cache.rs,prompts.rs}` (new), `mur-core/src/store/config.rs` (modify).

- [ ] **Step 1: Types**

```rust
// mur-core/src/skill_llm/mod.rs

pub mod budget;
pub mod cache;
pub mod prompts;

use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct TokenBudget {
    pub max_input: u32,
    pub max_output: u32,
}

impl TokenBudget {
    pub const DEFAULT: TokenBudget = TokenBudget { max_input: 4000, max_output: 1500 };
}

#[derive(Debug, Error)]
pub enum SkillLlmError {
    #[error("no model configured for maintenance role")]
    NoModel,
    #[error("daily budget exhausted (${spent_usd:.4} / ${cap_usd:.4})")]
    BudgetExhausted { spent_usd: f64, cap_usd: f64 },
    #[error("model returned invalid response: {0}")]
    InvalidResponse(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Reference to a model in `~/.mur/models.yaml`. Constructed from a role
/// name (preferred) or a literal entry key.
#[derive(Debug, Clone)]
pub struct ModelRef {
    pub entry_key: String,
}
```

- [ ] **Step 2: Role resolution**

```rust
// mur-core/src/skill_llm/mod.rs (continued)

/// Pick the model for skill-maintenance calls.
/// Order: user's `skill_llm.model_ref` config override → `roles.maintenance.primary`
/// → user's default chat role → first model with `capabilities: [chat]`.
pub fn resolve_maintenance_model(
    registry: &mur_common::model::ModelRegistry,
    cfg: &crate::store::config::SkillLlmConfig,
) -> Option<ModelRef> {
    if let Some(key) = &cfg.model_ref {
        if registry.models.contains_key(key) {
            return Some(ModelRef { entry_key: key.clone() });
        }
    }
    if let Some(role) = registry.resolve_role("maintenance") {
        return Some(ModelRef { entry_key: role.to_string() });
    }
    if let Some(role) = registry.resolve_role("chat") {
        return Some(ModelRef { entry_key: role.to_string() });
    }
    registry.models.iter()
        .find(|(_, m)| m.capabilities.iter().any(|c| c == "chat"))
        .map(|(k, _)| ModelRef { entry_key: k.clone() })
}
```

`registry.resolve_role` already exists (`mur-common/src/model.rs:105`). The new `"maintenance"` role is a user-opt-in convention: `mur model role set maintenance <model_key>`. We do not invent the role automatically.

- [ ] **Step 3: `maintenance_call` skeleton**

```rust
pub async fn maintenance_call(
    prompt: &str,
    model: ModelRef,
    budget: TokenBudget,
    ctx: &MaintenanceCtx,
) -> Result<Option<String>, SkillLlmError> {
    // Cache lookup
    let cache_key = cache::key(&model.entry_key, prompt, budget);
    if let Some(hit) = cache::load(&cache_key, ctx.cache_ttl)? {
        tracing::debug!(target: "skill_llm", model = %model.entry_key, "cache hit");
        return Ok(Some(hit));
    }

    // Budget check (pre-flight)
    let projected_cost = estimate_cost(&model.entry_key, prompt.len() as u32, budget.max_output);
    budget::check_and_reserve(&ctx.budget_ledger, projected_cost, ctx.daily_cap_usd)
        .map_err(|spent| SkillLlmError::BudgetExhausted { spent_usd: spent, cap_usd: ctx.daily_cap_usd })?;

    // Actual call — delegate to the same HTTP client extract_llm.rs uses.
    let resp = match crate::extract_llm::raw_completion(&model.entry_key, prompt, budget).await {
        Ok(r) => r,
        Err(e) => {
            // Network / auth failure: return None, not Err. Caller degrades.
            tracing::warn!(target: "skill_llm", error = %e, "maintenance_call soft-failed");
            return Ok(None);
        }
    };

    // Budget settle (actual cost vs projected)
    let actual_cost = estimate_cost(&model.entry_key, prompt.len() as u32, count_tokens(&resp));
    budget::settle(&ctx.budget_ledger, projected_cost, actual_cost)?;

    // Cache
    cache::save(&cache_key, &resp)?;

    Ok(Some(resp))
}
```

`raw_completion` is the part of `extract_llm.rs` that does the actual HTTP request. If it is not currently `pub`, expose it (refactor: rename `extract_llm::call_anthropic` or equivalent to `extract_llm::raw_completion` and make it crate-public). Decide at Step 3 after reading the existing function signature.

`estimate_cost` is a cheap heuristic — char-count / 4 → tokens, multiply by per-provider USD/1M-token rates. Hard-code the rates in `mur-core/src/skill_llm/pricing.rs` (a 6th file, add to "Create" list above) with a CC-by-license-style table. Outdated rates → over-/under-estimate budget, never block correctness.

- [ ] **Step 4: Cache implementation**

```rust
// mur-core/src/skill_llm/cache.rs

pub fn key(model: &str, prompt: &str, budget: TokenBudget) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(model.as_bytes());
    h.update(b"\x00");
    h.update(prompt.as_bytes());
    h.update(b"\x00");
    h.update(budget.max_input.to_le_bytes());
    h.update(budget.max_output.to_le_bytes());
    format!("{:x}", h.finalize())
}

pub fn load(key: &str, ttl: chrono::Duration) -> anyhow::Result<Option<String>> {
    let path = cache_path(key);
    if !path.exists() { return Ok(None); }
    let meta = std::fs::metadata(&path)?;
    let mtime = chrono::DateTime::<chrono::Utc>::from(meta.modified()?);
    if (chrono::Utc::now() - mtime) > ttl {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(&path)?))
}

pub fn save(key: &str, body: &str) -> anyhow::Result<()> {
    let path = cache_path(key);
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    // Atomic write — temp + rename, matches store/yaml.rs pattern.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn cache_path(key: &str) -> std::path::PathBuf {
    // ~/.mur/skill_llm_cache/<2-char-prefix>/<rest>.json
    let home = mur_common::paths::mur_home();
    home.join("skill_llm_cache").join(&key[..2]).join(format!("{}.json", &key[2..]))
}
```

- [ ] **Step 5: Budget ledger**

```rust
// mur-core/src/skill_llm/budget.rs

#[derive(Debug, Serialize, Deserialize, Default)]
struct DayLedger {
    date: chrono::NaiveDate,
    spent_usd: f64,
    reserved_usd: f64,    // pre-flight reservations, settled by `settle()`
}

pub fn check_and_reserve(path: &Path, projected_usd: f64, daily_cap_usd: f64) -> Result<(), f64> {
    let mut ledger = load_or_init(path);
    let today = chrono::Utc::now().date_naive();
    if ledger.date != today {
        ledger = DayLedger { date: today, spent_usd: 0.0, reserved_usd: 0.0 };
    }
    let total = ledger.spent_usd + ledger.reserved_usd + projected_usd;
    if total > daily_cap_usd {
        return Err(ledger.spent_usd);
    }
    ledger.reserved_usd += projected_usd;
    save_atomic(path, &ledger);
    Ok(())
}

pub fn settle(path: &Path, reserved: f64, actual: f64) -> anyhow::Result<()> {
    let mut ledger = load_or_init(path);
    ledger.reserved_usd = (ledger.reserved_usd - reserved).max(0.0);
    ledger.spent_usd += actual;
    save_atomic(path, &ledger);
    Ok(())
}
```

The reserve / settle pattern prevents two parallel callers from over-spending. Concurrent writers race on the file but the worst case is one over-estimated reservation (fine — under-spending is acceptable; over-spending is what we guard against).

- [ ] **Step 6: Config section**

```rust
// mur-core/src/store/config.rs (modify)

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillLlmConfig {
    #[serde(default = "default_per_call_token_cap")]
    pub per_call_token_cap: u32,                // 1500
    #[serde(default = "default_per_day_usd_cap")]
    pub per_day_usd_cap: f64,                   // 0.50
    #[serde(default = "default_cache_ttl_days")]
    pub cache_ttl_days: u32,                    // 30
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,              // None → use role resolution
}
```

- [ ] **Step 7: Build + commit**

```
cargo build -p mur-core
cargo test -p mur-core --test skill_llm_cache --test skill_llm_budget
git add mur-core/src/skill_llm/ mur-core/src/store/config.rs mur-core/src/lib.rs
git commit -m "feat(skill): skill_llm maintenance-call helper (cache + budget)"
```

---

### Task 2 — Trace clustering helper

**Files:** `mur-core/src/skill_traces/{mod.rs,cluster.rs}` (new).

Two checks (api-drift + coverage-gap) need to load recent skill traces and group them by skill. Encapsulate in one helper rather than duplicating the JSONL reader logic in each check.

- [ ] **Step 1: API**

```rust
// mur-core/src/skill_traces/mod.rs

pub mod cluster;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct SkillTrace {
    pub skill_name: String,
    pub skill_version: String,
    pub outcome: TraceOutcome,
    pub timestamp: DateTime<Utc>,
    pub tools_used: Vec<String>,
    pub error: Option<String>,
    pub trace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceOutcome { Success, Failure, Cancelled }
```

- [ ] **Step 2: Load + cluster**

```rust
// mur-core/src/skill_traces/cluster.rs

/// Load all skill traces from the JSONL store within `window`.
/// Reuses the same JSONL parser M5a's reindex uses.
pub fn load_window(window: chrono::Duration) -> anyhow::Result<Vec<SkillTrace>>;

/// Group traces by (skill_name, skill_version). Stable order: most-recent first within each group.
pub fn group_by_skill(traces: Vec<SkillTrace>) -> BTreeMap<(String, String), Vec<SkillTrace>>;

/// Pick the N most-recent traces per skill (cap to keep prompts small).
pub fn cap_per_skill(groups: BTreeMap<(String, String), Vec<SkillTrace>>, n: usize)
    -> BTreeMap<(String, String), Vec<SkillTrace>>;
```

- [ ] **Step 3: Build + commit**

```
cargo build -p mur-core
git add mur-core/src/skill_traces/ mur-core/src/lib.rs
git commit -m "feat(skill): skill_traces clustering helper"
```

---

### Task 3 — `api-drift` LLM check

**Files:** `mur-core/src/skill_doctor/checks/api_drift.rs` (replace stub), `mur-core/src/skill_llm/prompts.rs` (extend).

The check: load the last N traces for this skill, build a prompt that contains the skill's procedure + a summary of observed tool usage in traces, ask the model "does the procedure still match observed behaviour, or has the API drifted?" The response is JSON with `verdict: aligned | drifted | unknown` and `evidence`.

- [ ] **Step 1: Prompt template**

```rust
// mur-core/src/skill_llm/prompts.rs

pub const API_DRIFT_V1: &str = r#"You are a skill-maintenance assistant. Decide whether a skill's procedure still matches recent observed tool usage.

## Skill procedure
{procedure}

## Recent traces (last {trace_count} executions)
{trace_summary}

## Output (JSON only, no prose)
{
  "verdict": "aligned" | "drifted" | "unknown",
  "evidence": "one short sentence",
  "drifted_steps": [<step indices, only if verdict == drifted>]
}
"#;

pub const API_DRIFT_VERSION: u32 = 1;
```

Versioned so future prompt revisions invalidate the cache (cache key includes the prompt body → any change invalidates).

- [ ] **Step 2: Check implementation**

```rust
// mur-core/src/skill_doctor/checks/api_drift.rs

pub struct ApiDrift;

impl Check for ApiDrift {
    fn id(&self) -> CheckId { CheckId("api-drift") }

    async fn run_async(&self, skill: &Skill, ctx: &super::Ctx) -> Vec<Finding> {
        // Pre-conditions: this is the LLM check; if --llm OFF, return the M5a stub
        if !ctx.llm_enabled {
            return vec![Finding {
                check: self.id(), severity: Severity::Unknown,
                skill: skill.manifest.name.clone(),
                message: "api-drift requires --llm; enable to analyze".into(),
                fixable: false,
            }];
        }

        let traces = match skill_traces::cluster::load_recent_for(&skill.manifest.name, 20) {
            Ok(t) if !t.is_empty() => t,
            _ => return vec![],  // no traces, nothing to compare
        };

        let prompt = render_prompt(skill, &traces);

        let model = match resolve_maintenance_model(&ctx.model_registry, &ctx.cfg.skill_llm) {
            Some(m) => m,
            None => return vec![one_finding_no_model(self.id(), &skill.manifest.name)],
        };

        let result = match maintenance_call(&prompt, model, TokenBudget::DEFAULT, &ctx.maint_ctx).await {
            Ok(Some(body)) => body,
            Ok(None) => return vec![one_finding_unavailable(self.id(), &skill.manifest.name)],
            Err(SkillLlmError::BudgetExhausted { .. }) => return vec![one_finding_budget(self.id(), &skill.manifest.name)],
            Err(_) => return vec![],
        };

        match parse_verdict(&result) {
            Ok(Verdict::Aligned) => vec![],
            Ok(Verdict::Drifted { evidence, steps }) => vec![Finding {
                check: self.id(), severity: Severity::Warning,
                skill: skill.manifest.name.clone(),
                message: format!("api-drift: {evidence} (steps: {steps:?})"),
                fixable: false,
            }],
            Ok(Verdict::Unknown) => vec![],
            Err(_) => vec![],  // malformed JSON → silent skip; logged via tracing inside parse_verdict
        }
    }
}
```

Async note: M5a's Check trait is sync. M6c needs an async variant — either add `async fn run_async` as a default-implemented trait method that defers to `run`, or split into two traits. Pick at Task 3 Step 2 after reading the existing trait. Async-capable doctor already exists for M5b's `--fix --apply`, so the runtime piece is solved.

- [ ] **Step 3: Verdict parser + tests**

```rust
fn parse_verdict(json: &str) -> anyhow::Result<Verdict> {
    let v: serde_json::Value = serde_json::from_str(json.trim())?;
    match v.get("verdict").and_then(|x| x.as_str()) {
        Some("aligned") => Ok(Verdict::Aligned),
        Some("unknown") => Ok(Verdict::Unknown),
        Some("drifted") => Ok(Verdict::Drifted {
            evidence: v.get("evidence").and_then(|x| x.as_str()).unwrap_or("(no evidence)").into(),
            steps: v.get("drifted_steps").and_then(|x| x.as_array())
                .map(|arr| arr.iter().filter_map(|s| s.as_u64()).map(|n| n as usize).collect())
                .unwrap_or_default(),
        }),
        _ => anyhow::bail!("missing or invalid 'verdict' field"),
    }
}
```

Tests use a mock `MaintenanceClient` trait implementation that returns canned responses — no network in CI.

- [ ] **Step 4: Build + commit**

```
cargo test -p mur-core --test skill_doctor_api_drift
git add mur-core/src/skill_doctor/checks/api_drift.rs mur-core/src/skill_llm/prompts.rs mur-core/tests/skill_doctor_api_drift.rs
git commit -m "feat(skill-doctor): api-drift LLM check"
```

---

### Task 4 — `coverage-gap` doctor check

**Files:** `mur-core/src/skill_doctor/checks/coverage_gap.rs` (new), `mur-core/src/skill_llm/prompts.rs` (extend).

Different signal than api-drift: api-drift compares one skill's procedure to its own traces. Coverage-gap looks at **failed traces NOT covered by any skill** and asks the LLM what skill (or what step on an existing skill) would have unblocked them.

- [ ] **Step 1: Trace selection**

Load recent failures across all skills (or, if a skill name is provided, scoped to that skill's failures). Cluster by error signature (`error_summary` derived from `trace.error`). For each cluster ≥ 3 occurrences, generate one finding.

- [ ] **Step 2: Prompt template**

```rust
// mur-core/src/skill_llm/prompts.rs

pub const COVERAGE_GAP_V1: &str = r#"You are a skill-maintenance assistant. Given a cluster of repeated failures, determine whether an existing skill should be extended or a new skill is needed.

## Failure cluster ({count} occurrences)
{error_signature}

## Sample failed steps
{sample_steps}

## Existing skills (name + abstract)
{skill_inventory}

## Output (JSON only)
{
  "recommendation": "extend" | "new" | "ignore",
  "target_skill": "<existing skill name if recommendation==extend>",
  "suggested_step": "<one-sentence step description if recommendation==extend or new>",
  "rationale": "<one sentence>"
}
"#;

pub const COVERAGE_GAP_VERSION: u32 = 1;
```

- [ ] **Step 3: Check implementation**

Mirrors Task 3 structurally: pre-flight checks (`--llm` enabled, trace count ≥ 3, model available), maintenance call, parse verdict, emit Warning-level finding.

Important constraint: cluster the failures **before** calling the LLM, so one LLM call covers a whole cluster. Coverage-gap is the most likely to blow budget if implemented naively per-trace.

- [ ] **Step 4: Tests + commit**

```
cargo test -p mur-core --test skill_doctor_coverage_gap
git add mur-core/src/skill_doctor/checks/coverage_gap.rs mur-core/src/skill_llm/prompts.rs mur-core/tests/skill_doctor_coverage_gap.rs
git commit -m "feat(skill-doctor): coverage-gap LLM check"
```

---

### Task 5 — `consolidate` LLM contradiction adjudication

**Files:** `mur-core/src/skill_consolidate/contradiction_llm.rs` (new), `mur-core/src/skill_consolidate/contradiction.rs` (modify), `mur-core/src/skill_consolidate/mod.rs` (modify), `mur-core/src/skill_llm/prompts.rs` (extend).

The M5b rule-based pass flags pairs where the first-step tool differs on overlapping triggers. M6c layers an adjudicator that:
1. Takes M5b's `Vec<ContradictionPair>` plus M6c.1's borderline-cosine pairs (cosine ∈ `[0.85, 0.92]`).
2. For each, asks the LLM: "are these two skills actually contradictory, or do they cover different cases?"
3. Verdict overrides the rule-based / vector verdict. `consolidate --apply` only acts on LLM-confirmed contradictions when `--llm-adjudicate` is on.

- [ ] **Step 1: Prompt template**

```rust
pub const CONTRADICTION_ADJUDICATE_V1: &str = r#"You are a skill-maintenance assistant. Two skills appear to overlap. Decide whether they contradict (one is wrong or duplicates the other) or coexist (they cover different cases).

## Skill A: {name_a}
{procedure_a}

## Skill B: {name_b}
{procedure_b}

## Reported overlap
{overlap_summary}

## Output (JSON only)
{
  "verdict": "contradict" | "coexist" | "duplicate",
  "rationale": "<one sentence>"
}
"#;
```

`duplicate` is distinct from `contradict` — duplicate means one should be removed, contradict means both wrong somewhere.

- [ ] **Step 2: Adjudicator**

```rust
// mur-core/src/skill_consolidate/contradiction_llm.rs

pub async fn adjudicate(
    rule_pairs: Vec<ContradictionPair>,
    vec_borderline: Vec<DuplicatePair>,
    skills: &[SkillView],
    ctx: &MaintCtx,
) -> Vec<AdjudicatedFinding> {
    let mut out = Vec::new();
    let model = match resolve_maintenance_model(...) { Some(m) => m, None => return rule_pairs.into_iter().map(...).collect() };

    for pair in rule_pairs {
        let prompt = render(&pair, skills);
        let Some(resp) = maintenance_call(&prompt, model.clone(), TokenBudget::DEFAULT, ctx).await.ok().flatten()
            else { continue; };
        if let Ok(v) = parse_adj(&resp) {
            out.push(AdjudicatedFinding::from(pair, v));
        }
    }
    for pair in vec_borderline {
        // same loop, distinct prompt-template variant (no first-step diff)
    }
    out
}
```

- [ ] **Step 3: Surface in report**

`ConsolidateReport.contradictions: Vec<ContradictionPair>` gets an additional optional field `adjudication: Option<Verdict>`. JSONL writer serializes it. `--apply` consults the field: only acts on `Verdict::Contradict` (or `Duplicate` if vector pair).

- [ ] **Step 4: CLI flag**

```rust
#[arg(long)]
pub llm_adjudicate: bool,
```

Default OFF. When ON and no model is available, prints a clear "no model configured for skill_llm; falling back to rule-based contradictions" message and continues.

- [ ] **Step 5: Tests + commit**

Test with two fixtures:
- Two skills both about "search Google" → adjudicator returns `duplicate`.
- Two skills, one about "search Google" and one about "search arXiv" → both flagged by overlap rule but adjudicator returns `coexist` (no finding in `--apply` mode).

```
cargo test -p mur-core --test skill_consolidate_llm
git add mur-core/src/skill_consolidate/{contradiction.rs,contradiction_llm.rs,mod.rs} mur-core/src/cmd/skill_consolidate.rs mur-core/tests/skill_consolidate_llm.rs
git commit -m "feat(skill): consolidate --llm-adjudicate"
```

---

### Task 6 — Documentation + observability

**Files:** `docs/architecture/runtime-overview.md` (modify), `mur-core/src/cmd/skill_doctor.rs` (modify).

- [ ] **Step 1: Docs section**

Under the skills section, add `Skill LLM maintenance`:
1. Three checks: api-drift, coverage-gap, contradiction adjudication. Each is `--llm` opt-in.
2. Config: `skill_llm.{per_call_token_cap,per_day_usd_cap,cache_ttl_days,model_ref}`.
3. Role: `mur model role set maintenance <model_key>` is the recommended way to dedicate a cheap model (e.g., Haiku) to maintenance.
4. Graceful degradation contract: no model → checks behave as M5a/M5b stubs. Documented as a feature, not a bug.

- [ ] **Step 2: `mur skill doctor --llm-status`**

Tiny diagnostic flag that prints:
```
LLM maintenance status:
  model:        anthropic_haiku_4_5 (via roles.maintenance)
  per-call cap: 1500 output tokens
  per-day cap:  $0.50
  spent today:  $0.0214 (cache hits: 12, misses: 3)
  cache:        ~/.mur/skill_llm_cache (TTL 30d)
```

Cheap to implement (no LLM call), high value for operators trying to figure out why a check returned `Unknown`.

- [ ] **Step 3: Commit**

```
git add mur-core/src/cmd/skill_doctor.rs docs/architecture/runtime-overview.md
git commit -m "feat(skill): skill_llm docs + doctor --llm-status"
```

---

## Operator Walkthrough

```sh
# 1. Pick a cheap model for maintenance.
$ mur model role set maintenance anthropic_haiku_4_5

# 2. Confirm.
$ mur skill doctor --llm-status
LLM maintenance status:
  model: anthropic_haiku_4_5
  per-day cap: $0.50
  spent today: $0.00
  cache: ~/.mur/skill_llm_cache (TTL 30d)

# 3. Run doctor with LLM checks.
$ mur skill doctor --llm research-prices
[Warning] api-drift: traces show 12/20 invocations use `browser.search`, but the
  procedure declares `browser.navigate` (steps: [0])
[Info] no coverage-gap clusters in the last 7 days

# 4. Run consolidate with adjudication.
$ mur skill consolidate --method=both --llm-adjudicate --dry-run
[Adjudicated duplicate] keeper=web-search loser=web-search-v2 (cosine 0.91, verdict: duplicate)
[Skipped coexist]      a=search-google b=search-arxiv (rule overlap, verdict: coexist)
```

---

## Out of scope — deferred / future

1. **LLM-driven `mcp_requirements` inference** — M6a Task 5 risk #3 mentioned this; explicitly not part of M6c. Add a small follow-up plan if a user asks.
2. **Batch prompting (multi-pair contradiction in one call)** — saves cost but breaks the per-pair cache contract. Revisit only if cost becomes a problem.
3. **Adjudication of api-drift verdicts by a second model** — bigger model judges smaller model's output. Theoretical reliability gain, real cost. Skip until field data shows the smaller model is too unreliable.
4. **Auto-fix derived from LLM verdicts** — would let `mur skill doctor --fix --apply` rewrite a procedure based on api-drift evidence. Too risky for v1; requires UX for "preview + accept" that we don't have.
5. **Streaming partial responses for long maintenance runs** — one-shot is enough for the per-skill workloads we expect.

## Risks

| Risk | Mitigation |
|---|---|
| LLM returns invalid JSON | Parser failures are logged and silently skip the finding; never panic. Doctor continues with other checks. |
| Daily budget exhausts mid-doctor-run | `Ok(None) → Severity::Info` finding with "budget exhausted today" message; rest of skill is still doctored. |
| Cache disk fills up | TTL prunes on read; add a `mur skill llm-cache prune` command in a follow-up if user reports growth. ~few KB per entry, 30-day TTL ⇒ bounded. |
| Two parallel `mur skill doctor` runs race on the budget ledger | Reservation pattern (Step 5) over-counts under contention but never under-counts; over-counting just blocks earlier. Acceptable. |
| Prompt template change silently uses stale cache | Cache key hashes the prompt body. Any template change auto-invalidates cache for that template. `PROMPT_VERSION` constants are belt-and-suspenders for grep-ability. |
| Mock-only tests miss real-model issues | Add a `cargo test --features integration-llm` opt-in test crate (not in CI) that hits a real model with one fixture each. Out of scope for landing M6c; document the gap. |
| User confused why a check returns `Unknown` | `--llm-status` diagnostic flag (Task 6 Step 2) covers this. |
| Confidence collapse from a single LLM verdict | Doctor findings are advisory, never mutate the manifest. `--apply` is consolidate-only (M5b) + per-finding confirmation. |
