# Workflow Engine W3b-P2 — DAG Schema + Run-Ledger Enrichment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Workflow-engine v2 P2 — extend `ProcedureStep` with executable-DAG fields, unify the `Variable` type (v2 resolved decision #3), move `FailureAction`/`RetryConfig` into `skill::manifest` (prerequisite for the P3 executor lift), enrich `SkillEvent::Execution` into the full run-ledger record (duration/exit_code/env_class/confidence/trigger), add the env-class classifier, and emit a JSON Schema for the Hub DAG editor via `mur skill schema --json`.

**Architecture:** Pure schema + library work, no executor yet (that's W3b-P3). Everything is additive/serde-defaulted so existing skill.yaml files and events.jsonl lines keep parsing, and fleet sync's set-union merge (dedup_key unchanged) keeps working. The run-ledger is NOT a new file: `~/.mur/skills/<name>/events.jsonl` with enriched `Execution` events IS the ledger (v2 amendment: "the run-ledger as a projection of per-skill events.jsonl") — `apply_new_events_to_stats` is already the stats reducer and `skill_lifecycle/sweep.rs::run_sweep` already applies `next_state` + the A1 provenance gate.

**Verified current state (2026-06-11):**
- `ProcedureStep` = {description, tool, intent, tool_hint} (`mur-common/src/skill/manifest.rs:150`)
- `SkillEvent::Execution` = {ts, device_id, outcome, error, step} (`mur-common/src/skill/event_log.rs:25`); `append_event`/`read_events`/`union_events`/`apply_new_events_to_stats` all exist; writers: `sync/inbox.rs` (Commander signals)
- `manifest::Variable` uses `var_type: String` + `default: Option<serde_yaml_ng::Value>`; `workflow::Variable` uses typed `VarType` + `default_value: Option<String>` — decision #3 merges them (typed enum + string default + alias)
- `FailureAction`/`RetryConfig` live in `mur-common/src/workflow.rs:112-131`; `mur-common/src/pipeline.rs` imports them from there
- Provenance gate + lifecycle sweep DONE (`skill_lifecycle/sweep.rs`, `lifecycle.rs::cap_for_provenance`)
- schemars is NOT yet a dependency anywhere

**Out of scope:** P3 unified executor; P4 thresholds-to-config + Broken fast-path + Archived hard-delete; P7 workflows/→skills/ migration.

---

### Task 1: Unify `Variable` in `skill::manifest` (decision #3)

**Files:** `mur-common/src/skill/manifest.rs`, `mur-common/src/workflow.rs` (re-export), consumers via grep

- [ ] Step 1: `grep -rn "manifest::Variable\|skill::manifest::Variable" mur-core/src mur-agent-runtime/src` and `grep -rn "\.default\b" <those files>` — list every consumer of the `serde_yaml_ng::Value` default before changing it.
- [ ] Step 2: Replace `manifest::Variable` with the unified type:

```rust
/// Workflow/skill parameter (v2 resolved decision #3: ONE Variable type).
/// `default` is string-encoded; runtime coerces per `var_type`
/// (Number/Bool parsed, Array decoded as JSON or comma-separated).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Variable {
    pub name: String,
    #[serde(rename = "type", default)]
    pub var_type: VarType,
    #[serde(default)]
    pub required: bool,
    /// String-encoded default. `default_value` accepted for legacy workflow YAML.
    #[serde(default, alias = "default_value", skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Allowed values (renders as a dropdown in the Hub editor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VarType {
    #[default]
    String,
    Number,
    Bool,
    Array,
}
```

  Migration shim for old manifests whose `default:` was a YAML value: a custom deserializer is NOT needed if we accept breakage only for non-string defaults — check Step 1's consumer list; if any shipped skill.yaml uses non-string defaults, add `#[serde(deserialize_with = "string_or_value")]` that stringifies scalars. Decide from evidence, not speculation.
- [ ] Step 3: `workflow::Variable`/`VarType` become re-exports (`pub use crate::skill::manifest::{Variable, VarType};`) — delete the local definitions; fix `default_value` field accesses crate-wide (grep `default_value`).
- [ ] Step 4: Tests: legacy workflow YAML with `default_value:` parses (alias); legacy skill YAML with string `default:` parses. Run `cargo nextest run -p mur-common -p mur-core -E 'test(/variable|manifest|workflow/)'`. Commit.

### Task 2: Move `FailureAction` + `RetryConfig` into `skill::manifest`

- [ ] Step 1: Move both types (bodies unchanged) from `workflow.rs` to `manifest.rs`; leave `pub use crate::skill::manifest::{FailureAction, RetryConfig};` in workflow.rs so `pipeline.rs` and executor imports keep compiling.
- [ ] Step 2: `cargo nextest run -p mur-common -p mur-core -E 'test(/pipeline|workflow/)'`; clippy; commit.

### Task 3: `ProcedureStep` DAG + exec fields

- [ ] Step 1: Extend (all default — existing skill.yaml parses unchanged):

```rust
    // ── Executable-DAG fields (v2 P2; all Option/default) ──
    /// Stable step id for `depends_on` references. Defaults to the step index
    /// as a string when omitted (assigned at load, not serialized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Step ids this step depends on. Empty = root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Shell command (command-mode step). Intent-mode steps leave this None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub on_failure: FailureAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Pause for human approval before running (TTY prompt; non-TTY skips
    /// and marks `skipped_approval` — wired in P3).
    #[serde(default)]
    pub needs_approval: bool,
```

- [ ] Step 2: Sweep `ProcedureStep {` literals crate-wide (grep) adding `..Default::default()` where possible (derive `Default` on ProcedureStep). Round-trip test: yaml with depends_on parses; yaml without new fields parses. Commit.

### Task 4: Enrich `SkillEvent::Execution` (run-ledger record)

- [ ] Step 1: Add serde-defaulted fields to the `Execution` variant: `duration_ms: Option<u64>`, `exit_code: Option<i32>`, `env_class: Option<String>` ("workflow"|"env"), `confidence: Option<f64>`, `trigger: Option<String>` ("manual"|"schedule"|"agent"). `dedup_key()` unchanged (ts+kind+device) so fleet-sync union still dedups.
- [ ] Step 2: Back-compat test: old jsonl line without new fields parses; new line round-trips. `apply_new_events_to_stats` unchanged (outcome drives counters). Commit.

### Task 5: env-class classifier

**Files:** Create `mur-common/src/skill/env_class.rs`

- [ ] Step 1: Heuristic classifier per spec (workflow failure vs environment failure):

```rust
//! Classify a failed run as a *workflow* failure (the skill is broken) vs an
//! *environment* failure (network/credentials/missing binary) so a flaky
//! network never marks a workflow Broken (v2 spec, Layer 4).

pub struct EnvClassification {
    /// "workflow" | "env"
    pub class: &'static str,
    pub confidence: f64,
}

const ENV_MARKERS: &[&str] = &[
    "connection refused", "connection reset", "timed out", "timeout",
    "could not resolve", "dns", "network is unreachable", "tls", "certificate",
    "401", "403", "unauthorized", "forbidden", "credential", "permission denied",
    "no such file or directory", "command not found", "not found in path",
    "rate limit", "429", "disk full", "no space left",
];

pub fn classify_failure(stderr: &str) -> EnvClassification {
    let lower = stderr.to_lowercase();
    let hits = ENV_MARKERS.iter().filter(|m| lower.contains(*m)).count();
    match hits {
        0 => EnvClassification { class: "workflow", confidence: 0.6 },
        1 => EnvClassification { class: "env", confidence: 0.6 },
        _ => EnvClassification { class: "env", confidence: 0.9 },
    }
}
```

  Plus `record_run` convenience in `event_log.rs` building an enriched Execution event from an outcome + optional stderr (calls classify_failure on failure). Tests for both. Commit.

### Task 6: JSON Schema emit — `mur skill schema --json`

- [ ] Step 1: Add `schemars = "1"` (workspace dep) to mur-common; derive `JsonSchema` on `SkillManifest` and every type it embeds (Content, ProcedureStep, Variable, VarType, FailureAction, RetryConfig, Trigger, Category, …). Types that can't derive (serde_yaml_ng::Value remnants) must be gone after Task 1 — if any remain, `#[schemars(with = "String")]`.
- [ ] Step 2: New `SkillAction::Schema { json: bool }` → `cmd_skill_schema()`: `serde_json::to_string_pretty(&schemars::schema_for!(SkillManifest))`, written to stdout; `--out schema/skill.schema.json` optional flag writes the file.
- [ ] Step 3: Smoke: `mur skill schema --json | python3 -m json.tool > /dev/null`. Commit.

### Task 7: Final verification + PR

- [ ] `cargo fmt --check`, `cargo clippy --workspace` (0 warnings), `cargo nextest run -p mur-common -p mur-core` (rollup env-flaky pair excepted), build mur-agent-runtime (`cargo build -p mur-agent-runtime`) since manifest types feed the runtime.
- [ ] Docs: runtime-overview gets a short "P2 schema + ledger" subsection under the v2 heading; CLAUDE.md untouched (operational surface unchanged except `mur skill schema`).
- [ ] PR: `feat(skill): executable-DAG step schema, unified Variable, enriched run ledger, JSON Schema emit (v2 P2 / W3b)`.

## Self-review notes
- Decision #3 (one Variable) ✓ T1; decision #6 prerequisite (FailureAction/RetryConfig moved) ✓ T2; Layer-4 ledger record ✓ T4+T5; Hub editor schema ✓ T6.
- No executor changes; `mur run` behavior untouched (P3).
- Every schema change serde-defaulted; fleet-sync dedup_key untouched.
