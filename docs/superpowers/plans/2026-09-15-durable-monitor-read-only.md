# Durable Monitor (read-only slice) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A persisted monitor engine that keeps checking asynchronous work (MUR runs, GitHub Actions, Codex / Claude Code subprocesses) across daemon restarts, distinguishes `failed` from `unknown`, tracks stalled / soft / hard deadlines with the spec's defaults, and is operable from `mur monitor add|list|show|cancel|retry`.

**Architecture:** A new workspace crate `mur-monitor` (below `mur-core`, so an agent runtime can register monitors later without pulling LanceDB) owns the `MonitorSpec` contract, the SQLite store (leases with fencing tokens, append-only observations and events), the backoff tables, the deadline evaluator, and a pure scheduler `tick` that takes `now` as a parameter (the same discipline `run_status::classify` uses). The three first-party adapters live in `mur-core/src/monitor/adapters/` because two of them need `mur-core` (`run_status`) and `reqwest`; `mur-core/src/monitor/service.rs` assembles store + registry into `tick_once` / `recover`, and `mur-daemon` runs that on its own OS thread every 15 s. Every state-changing function is pure over `(row, observation, now)` so the fake-clock tests the spec asks for need no clock crate.

**Tech Stack:** Rust 2024, `rusqlite` 0.32 (bundled, WAL — same pragmas as `mur-channel/src/index.rs`), `serde_yaml` for the spec, `reqwest::blocking` for GitHub, `mur_common::{secret::SecretRef, redact::redact_secrets, limits::parse_duration, lock_file::pid_alive}`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-11-durable-monitor-design.md` — this plan implements its 實作順序 steps **1–4** (契約與 store → scheduler → 唯讀 adapters → CLI). Steps 5–9 (policy/actions/HITL, AgentResolver, 自動註冊與 outbox, wakeup events, 通知與 metrics) are a second plan; the tables and columns they need (`monitor_actions`, `monitor_notifications`, `monitor_registration_outbox`, `remediation_attempts`) are either created empty here or deliberately left for that plan, and each such point is marked **plan-2** below.

## Global Constraints

- **`unknown` is a monitor problem, never a work failure.** Query failure, unparseable response, credential failure, rate limit, "run not found", "process gone with no exit record" → `Outcome::Unknown`. No code path may map any of those to `Failed`. (spec: 非目標 4, Outcome 語意, 錯誤處理)
- **A timeout is the caller giving up, not the work failing.** The MUR-run adapter queries by `run_id`; it never re-dispatches. (spec: MVP Adapter → MUR run)
- **Defaults are exactly:** stalled `20m`, soft `3h`, hard `8h`, `retain_monitoring_after_hard_deadline: true`, retain interval `2h`, pending backoff `30s → 1m → 2m → 5m → 15m → 30m` then held at 30m with bounded jitter, `max_remediation_attempts: 3`. All are named `const`s or `Policy` defaults — **no literal durations in logic** (CLAUDE.md rule 1).
- **Secrets never touch the store, history, or output.** `credential_ref` is stored as the reference string only; every `Observation.evidence` / `adapter_error` passes through `mur_common::redact::redact_secrets` before it is written or printed. (spec: 安全與隱私)
- **Deadlines count from `work_started_at`** (the monitored work's real start), never from daemon restart or check time. (spec: stalled 與期限)
- **Progress only advances on a changed `progress_token`.** Re-observing the same token must not move `last_progress_at`. (spec: stalled 與期限)
- **Leases are fenced.** Every write-back carries the `fence` it claimed under; a mismatch is silently dropped and counted, never applied. (spec: 租約)
- **Every source file ≤ 800 lines** (CLAUDE.md rule 4) — split `store/` into `mod.rs` + `lease.rs` + `observe.rs` from the start.
- **User-visible brand is `MUR`** (CLAUDE.md rule 7); the command is `mur monitor`.
- **Test runner is `cargo nextest`**, never bare `cargo test` (memory: `project_mur_core_flaky_tests`). For `mur-core`: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core <filter>`.
- **Adapters are read-only in this plan.** `SourceAdapter` has `validate_reference` + `observe` and nothing that mutates the source. Rerun / logs download / cancel-source are **plan-2** actions behind the risk gate.

## Build / test env

```bash
# mur-monitor (new crate; no native deps beyond rusqlite bundled)
cargo nextest run -p mur-monitor

# mur-core (adapters, service, CLI)
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 \
  cargo nextest run -p mur-core monitor

# mur-daemon (tick wiring)
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-daemon monitor_tick

# before every commit
cargo fmt --all && cargo clippy -p mur-monitor -p mur-core -p mur-daemon --all-targets -- -D warnings
```

Branch: `feat/durable-monitor-p1` from `main`, worktree under `.worktrees/` (memory: `feedback_mur_worktree_under_dot_worktrees`). Build the worktree's own `--target-dir` only if a second session is compiling the main tree (memory: `gotcha-cargo-stale-fingerprints-external-drive`).

## File structure

```
mur-monitor/                          NEW crate (workspace member)
  Cargo.toml
  src/lib.rs                          pub mod re-exports only
  src/spec.rs                         MonitorSpec + validate()          (Task 1)
  src/state.rs                        MonitorState, Outcome             (Task 2)
  src/backoff.rs                      pending/unknown tables, jitter    (Task 2)
  src/adapter.rs                      Observation, SourceAdapter, AdapterRegistry (Task 3)
  src/store/mod.rs                    open/migrate, create/get/list/set_state (Task 4)
  src/store/lease.rs                  claim_due, heartbeat, release, expire (Task 5)
  src/store/observe.rs                observations, events (dedup), apply_cycle (Task 6)
  src/deadline.rs                     advance_progress, evaluate        (Task 7)
  src/scheduler.rs                    plan_cycle (pure), tick, recover  (Task 8)
mur-core/src/monitor/mod.rs           registry(mur_home)                (Task 9)
mur-core/src/monitor/adapters/mod.rs
mur-core/src/monitor/adapters/mur_run.rs        (Task 9)
mur-core/src/monitor/adapters/github_actions.rs (Task 10)
mur-core/src/monitor/adapters/subprocess.rs     (Task 11)
mur-core/src/monitor/service.rs       tick_once, recover                (Task 12)
mur-core/src/cmd/monitor.rs           MonitorAction + run()             (Task 13)
mur-core/src/cli/mod.rs               Commands::Monitor                 (Task 13)
mur-core/src/dispatch.rs              dispatch arm                      (Task 13)
mur-daemon/src/monitor_tick.rs        OS-thread loop                    (Task 14)
mur-daemon/src/main.rs                spawn                             (Task 14)
CLAUDE.md, README.md                  CLI surface line + section        (Task 13)
```

## Decisions already made — do not relitigate

1. **`outcomes:` expressions are stored, not evaluated, in this plan.** The three first-party adapters already normalise to the five outcomes; the expression language belongs to the PolicyEngine (**plan-2**). `validate()` accepts any string there.
2. **`actions:` are parsed and validated by name, not executed.** A terminal outcome with a non-empty action list for that outcome parks the monitor in `action_pending` (so plan-2's executor finds it); with an empty list it goes straight to `completed`. This keeps the spec's state machine intact without stubbing an executor.
3. **Codex / Claude Code have no launcher session registry today** (`grep -rn session_id mur-agent-launcher/src` → nothing). The subprocess adapter therefore reads a **process record** `<mur_home>/monitor/procs/<id>.json` (`pid`, `started_at`, `log_path`, `exit_path`) whose id is the durable reference; the pid is a field inside it, satisfying "不以 PID 單獨作 durable identity". Whoever spawns the process writes the record (**plan-2** wires the launchers); in this plan `mur monitor add` users write it by hand or via the tested `write_record` helper.
4. **Time is a parameter.** No `Clock` trait, no fake-clock crate — every function that reasons about time takes `now: DateTime<Utc>`, exactly like `mur-core/src/run_status/mod.rs::classify`.
5. **Jitter is deterministic** from `(monitor_id, attempt)` via FNV-1a so tests can assert exact `next_check_at`. ±20 %.
6. **State strings are `snake_case` in SQLite and YAML** (`action_pending`); the CLI `--state` filter also accepts the spec's kebab form (`awaiting-approval`) by normalising `-` → `_`.
7. **`cancel` = state `completed` + event `cancelled`.** It never touches the source. Cancelling the source is a **plan-2** high-risk action.
8. **`retry` reactivates only `exhausted`**; `--reset-remediation-budget` zeroes the `remediation_attempts` column (created here, incremented only in **plan-2**) and records an event either way.
9. **Startup throttle = bounded claims per tick** (`TICK_MAX_CLAIMS = 8`, tick every 15 s) plus per-monitor jitter — no separate queue type.

---

### Task 1: Crate scaffold + `MonitorSpec` contract

**Files:**
- Create: `mur-monitor/Cargo.toml`
- Create: `mur-monitor/src/lib.rs`
- Create: `mur-monitor/src/spec.rs`
- Modify: `Cargo.toml` (workspace `members`, after `"mur-open-items"`)
- Test: `mur-monitor/src/spec.rs` (`mod tests`)

**Interfaces:**
- Consumes: `mur_common::secret::SecretRef` (`FromStr`), `mur_common::limits::parse_duration(&str) -> Option<std::time::Duration>`
- Produces: `MonitorSpec`, `SourceType`, `Policy` (with `stalled_after()/soft_deadline()/hard_deadline() -> Duration`), `Action`, `SpecError`, `MonitorSpec::from_yaml(&str) -> Result<Self, SpecError>`, `MonitorSpec::validate(&self) -> Result<(), SpecError>`, `KNOWN_ACTIONS`, `SCHEMA_VERSION`

- [ ] **Step 1: Scaffold the crate and register it**

`mur-monitor/Cargo.toml`:
```toml
[package]
name = "mur-monitor"
version.workspace = true
edition.workspace = true
description = "Durable monitor engine: spec, SQLite store, leases, backoff, scheduler"

[dependencies]
mur-common = { path = "../mur-common" }
serde = { workspace = true }
serde_json = { workspace = true }
serde_yaml = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
rusqlite = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tempfile = "3"
```
(Check `version.workspace`/`edition.workspace` against `mur-open-items/Cargo.toml` and copy whichever form that file uses.)

`mur-monitor/src/lib.rs`:
```rust
//! Durable monitor: keep checking asynchronous work until it is settled.
//! Sits below `mur-core` so an agent runtime can register a monitor without
//! pulling the vector store in. Adapters that need `mur-core` live there.
pub mod spec;
```

Root `Cargo.toml` `members`: add `"mur-monitor",` after `"mur-open-items",`.

- [ ] **Step 2: Write the failing tests**

`mur-monitor/src/spec.rs` (tests only for now — the file must compile, so put `pub struct MonitorSpec;` placeholder above `mod tests` only if you need it to; otherwise write types and tests together and expect assertion failures rather than compile failures):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
schema_version: 1
name: wait-for-ci
source:
  type: github_actions
  reference: owner/repo/123
  credential_ref: keychain:mur/github-default
actions:
  on_failure:
    - type: collect_logs
    - type: rerun
policy:
  stalled_after: 20m
  soft_deadline: 3h
  hard_deadline: 8h
idempotency_key: ci:owner/repo:123
created_by:
  actor: agent:commander
  reason: "CI was started and returned a trackable run id"
"#;

    #[test]
    fn parses_the_spec_example() {
        let s = MonitorSpec::from_yaml(EXAMPLE).unwrap();
        assert_eq!(s.name, "wait-for-ci");
        assert_eq!(s.source.r#type, SourceType::GithubActions);
        assert_eq!(s.actions.on_failure.len(), 2);
        assert_eq!(s.policy.stalled_after(), std::time::Duration::from_secs(20 * 60));
        assert!(s.policy.retain_monitoring_after_hard_deadline);
        s.validate().unwrap();
    }

    #[test]
    fn defaults_are_the_spec_defaults() {
        let p = Policy::default();
        assert_eq!(p.stalled_after(), std::time::Duration::from_secs(20 * 60));
        assert_eq!(p.soft_deadline(), std::time::Duration::from_secs(3 * 3600));
        assert_eq!(p.hard_deadline(), std::time::Duration::from_secs(8 * 3600));
        assert_eq!(p.max_remediation_attempts, 3);
        assert!(p.retain_monitoring_after_hard_deadline);
    }

    #[test]
    fn deadline_order_is_enforced() {
        let y = EXAMPLE.replace("soft_deadline: 3h", "soft_deadline: 10h");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::DeadlineOrder(_)), "{e}");
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let y = EXAMPLE.replace("schema_version: 1", "schema_version: 2");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::Schema(2)));
    }

    #[test]
    fn credential_ref_must_be_a_secret_ref_not_a_secret() {
        let y = EXAMPLE.replace("keychain:mur/github-default", "ghp_plaintexttoken");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::Credential(_)), "{e}");
    }

    #[test]
    fn unknown_action_name_is_rejected() {
        let y = EXAMPLE.replace("type: rerun", "type: deploy_prod");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::Action(ref n) if n == "deploy_prod"));
    }

    #[test]
    fn agent_created_monitor_needs_a_reason() {
        let y = EXAMPLE.replace(
            "reason: \"CI was started and returned a trackable run id\"",
            "reason: \"\"",
        );
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::MissingReason));
    }

    #[test]
    fn idempotency_key_and_reference_are_required() {
        let y = EXAMPLE.replace("idempotency_key: ci:owner/repo:123", "idempotency_key: \"\"");
        assert!(matches!(
            MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err(),
            SpecError::Empty("idempotency_key")
        ));
        let y = EXAMPLE.replace("reference: owner/repo/123", "reference: \"\"");
        assert!(matches!(
            MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err(),
            SpecError::Empty("source.reference")
        ));
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor spec::`
Expected: compile error — `MonitorSpec`, `SourceType`, `Policy`, `SpecError` not defined.

- [ ] **Step 4: Implement the contract**

Above the tests in `mur-monitor/src/spec.rs`:

```rust
//! The `MonitorSpec` contract. Auto- and hand-created monitors normalise to
//! this one shape (spec §`MonitorSpec` 契約). `validate()` covers the rules a
//! spec can check on its own (1, 4, 5, 7, 8); adapter reachability (2, 3) is
//! the CLI's job and idempotency uniqueness (6) is the store's.

use std::str::FromStr;
use std::time::Duration;

use mur_common::secret::SecretRef;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Action names the executor (plan-2) knows. Validation by name here so a
/// spec cannot smuggle an unknown verb through to a future executor.
pub const KNOWN_ACTIONS: &[&str] = &[
    "notify",
    "start_downstream",
    "collect_logs",
    "apply_known_remedy",
    "rerun",
    "reschedule_monitor",
];

const DEFAULT_MODE: &str = "hybrid";
const DEFAULT_MAX_REMEDIATION: u32 = 3;
const DEFAULT_STALLED_AFTER: &str = "20m";
const DEFAULT_SOFT_DEADLINE: &str = "3h";
const DEFAULT_HARD_DEADLINE: &str = "8h";
const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    GithubActions,
    MurRun,
    Codex,
    ClaudeCode,
    Custom,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::GithubActions => "github_actions",
            SourceType::MurRun => "mur_run",
            SourceType::Codex => "codex",
            SourceType::ClaudeCode => "claude_code",
            SourceType::Custom => "custom",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github_actions" => Some(SourceType::GithubActions),
            "mur_run" => Some(SourceType::MurRun),
            "codex" => Some(SourceType::Codex),
            "claude_code" => Some(SourceType::ClaudeCode),
            "custom" => Some(SourceType::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub r#type: SourceType,
    pub reference: String,
    /// A `SecretRef` string (`keychain:svc/acct`, `env:NAME`, …). Never the secret.
    #[serde(default)]
    pub credential_ref: Option<String>,
}

/// Stored verbatim; evaluated by the PolicyEngine (plan-2). Adapters already
/// normalise to the five outcomes, so nothing in this plan reads these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outcomes {
    #[serde(default)]
    pub success: Option<String>,
    #[serde(default)]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub r#type: String,
    #[serde(flatten, default)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Actions {
    #[serde(default)]
    pub on_success: Vec<Action>,
    #[serde(default)]
    pub on_failure: Vec<Action>,
    #[serde(default)]
    pub on_unknown: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default = "d_mode")]
    pub mode: String,
    #[serde(default = "d_max_remediation")]
    pub max_remediation_attempts: u32,
    #[serde(default = "d_stalled")]
    pub stalled_after: String,
    #[serde(default = "d_soft")]
    pub soft_deadline: String,
    #[serde(default = "d_hard")]
    pub hard_deadline: String,
    #[serde(default = "d_true")]
    pub retain_monitoring_after_hard_deadline: bool,
}

fn d_mode() -> String {
    DEFAULT_MODE.to_string()
}
fn d_max_remediation() -> u32 {
    DEFAULT_MAX_REMEDIATION
}
fn d_stalled() -> String {
    DEFAULT_STALLED_AFTER.to_string()
}
fn d_soft() -> String {
    DEFAULT_SOFT_DEADLINE.to_string()
}
fn d_hard() -> String {
    DEFAULT_HARD_DEADLINE.to_string()
}
fn d_true() -> bool {
    true
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: d_mode(),
            max_remediation_attempts: d_max_remediation(),
            stalled_after: d_stalled(),
            soft_deadline: d_soft(),
            hard_deadline: d_hard(),
            retain_monitoring_after_hard_deadline: d_true(),
        }
    }
}

impl Policy {
    /// Parsed durations. `validate()` guarantees these parse; the fallbacks
    /// exist only so a row read back from an old store never panics.
    pub fn stalled_after(&self) -> Duration {
        parse_or(&self.stalled_after, DEFAULT_STALLED_AFTER)
    }
    pub fn soft_deadline(&self) -> Duration {
        parse_or(&self.soft_deadline, DEFAULT_SOFT_DEADLINE)
    }
    pub fn hard_deadline(&self) -> Duration {
        parse_or(&self.hard_deadline, DEFAULT_HARD_DEADLINE)
    }
}

fn parse_or(s: &str, fallback: &str) -> Duration {
    mur_common::limits::parse_duration(s)
        .or_else(|| mur_common::limits::parse_duration(fallback))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Notifications {
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedBy {
    pub actor: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub originating_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSpec {
    pub schema_version: u32,
    pub name: String,
    pub source: Source,
    #[serde(default)]
    pub outcomes: Outcomes,
    #[serde(default)]
    pub actions: Actions,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub notifications: Notifications,
    pub idempotency_key: String,
    pub created_by: CreatedBy,
}

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("schema_version {0} is not supported (this build supports {SCHEMA_VERSION})")]
    Schema(u32),
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("name is longer than {MAX_NAME_LEN} characters")]
    NameTooLong,
    #[error("credential_ref is not a secret reference (env:/keychain:/file:/cmd:): {0}")]
    Credential(String),
    #[error("policy durations must satisfy stalled_after < soft_deadline < hard_deadline: {0}")]
    DeadlineOrder(String),
    #[error("unknown action type `{0}`")]
    Action(String),
    #[error("created_by.reason is required when the actor is an agent")]
    MissingReason,
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl MonitorSpec {
    pub fn from_yaml(s: &str) -> Result<Self, SpecError> {
        Ok(serde_yaml::from_str(s)?)
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SpecError::Schema(self.schema_version));
        }
        if self.name.trim().is_empty() {
            return Err(SpecError::Empty("name"));
        }
        if self.name.len() > MAX_NAME_LEN {
            return Err(SpecError::NameTooLong);
        }
        if self.source.reference.trim().is_empty() {
            return Err(SpecError::Empty("source.reference"));
        }
        if let Some(c) = &self.source.credential_ref {
            SecretRef::from_str(c).map_err(|e| SpecError::Credential(e.to_string()))?;
        }
        let (s, m, h) = (
            mur_common::limits::parse_duration(&self.policy.stalled_after),
            mur_common::limits::parse_duration(&self.policy.soft_deadline),
            mur_common::limits::parse_duration(&self.policy.hard_deadline),
        );
        match (s, m, h) {
            (Some(s), Some(m), Some(h)) if s < m && m < h => {}
            _ => {
                return Err(SpecError::DeadlineOrder(format!(
                    "{} / {} / {}",
                    self.policy.stalled_after, self.policy.soft_deadline, self.policy.hard_deadline
                )));
            }
        }
        for a in self
            .actions
            .on_success
            .iter()
            .chain(&self.actions.on_failure)
            .chain(&self.actions.on_unknown)
        {
            if !KNOWN_ACTIONS.contains(&a.r#type.as_str()) {
                return Err(SpecError::Action(a.r#type.clone()));
            }
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(SpecError::Empty("idempotency_key"));
        }
        if self.created_by.actor.trim().is_empty() {
            return Err(SpecError::Empty("created_by.actor"));
        }
        if self.created_by.actor.starts_with("agent:") && self.created_by.reason.trim().is_empty() {
            return Err(SpecError::MissingReason);
        }
        Ok(())
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor spec::`
Expected: 8 tests PASS. If `#[serde(flatten, default)]` on `params` fails to compile, drop `default` (flatten implies it).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add Cargo.toml Cargo.lock mur-monitor
git commit -m "feat(monitor): mur-monitor crate with the MonitorSpec contract"
```

---

### Task 2: State, outcome, and backoff tables

**Files:**
- Create: `mur-monitor/src/state.rs`
- Create: `mur-monitor/src/backoff.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod state; pub mod backoff;`)
- Test: both files (`mod tests`)

**Interfaces:**
- Produces: `MonitorState` (8 variants, `as_str`/`parse`), `Outcome` (5 variants, `as_str`/`parse`, `is_terminal`), `backoff::{pending_delay(u32), unknown_delay(u32), with_jitter(Duration, u64), clamp_recommended(Option<Duration>, Duration), seed(&str, u32), MIN_INTERVAL, RETAIN_INTERVAL, UNHEALTHY_AFTER_UNKNOWN}`

- [ ] **Step 1: Write the failing tests**

`mur-monitor/src/state.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_accepts_kebab() {
        for s in MonitorState::ALL {
            assert_eq!(MonitorState::parse(s.as_str()), Some(*s));
        }
        assert_eq!(MonitorState::parse("awaiting-approval"), Some(MonitorState::AwaitingApproval));
        assert_eq!(MonitorState::parse("nope"), None);
    }

    #[test]
    fn only_three_outcomes_are_terminal() {
        assert!(Outcome::Succeeded.is_terminal());
        assert!(Outcome::Failed.is_terminal());
        assert!(Outcome::Cancelled.is_terminal());
        assert!(!Outcome::Pending.is_terminal());
        assert!(!Outcome::Unknown.is_terminal());
        assert_eq!(Outcome::parse("unknown"), Some(Outcome::Unknown));
    }
}
```

`mur-monitor/src/backoff.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pending_follows_the_spec_sequence_then_holds() {
        let want = [30, 60, 120, 300, 900, 1800, 1800, 1800];
        for (i, secs) in want.iter().enumerate() {
            assert_eq!(pending_delay(i as u32), Duration::from_secs(*secs), "attempt {i}");
        }
    }

    #[test]
    fn unknown_backoff_is_shorter_and_capped() {
        assert_eq!(unknown_delay(0), Duration::from_secs(10));
        assert_eq!(unknown_delay(3), Duration::from_secs(300));
        assert_eq!(unknown_delay(50), Duration::from_secs(300));
        assert!(unknown_delay(50) < pending_delay(50));
    }

    #[test]
    fn jitter_is_bounded_and_deterministic() {
        let base = Duration::from_secs(1000);
        for seed in 0..500u64 {
            let j = with_jitter(base, seed);
            assert!(j >= Duration::from_secs(800) && j <= Duration::from_secs(1200), "{j:?}");
        }
        assert_eq!(with_jitter(base, 7), with_jitter(base, 7));
        assert_eq!(seed("m1", 3), seed("m1", 3));
        assert_ne!(seed("m1", 3), seed("m1", 4));
    }

    #[test]
    fn recommended_poll_after_cannot_beat_the_floor() {
        let computed = Duration::from_secs(120);
        assert_eq!(clamp_recommended(None, computed), computed);
        assert_eq!(clamp_recommended(Some(Duration::from_secs(1)), computed), MIN_INTERVAL);
        assert_eq!(
            clamp_recommended(Some(Duration::from_secs(600)), computed),
            Duration::from_secs(600)
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor state:: backoff::`
Expected: compile error (modules missing).

- [ ] **Step 3: Implement**

`mur-monitor/src/state.rs`:
```rust
//! Monitor runtime state and observed outcome are two different axes
//! (spec §狀態模型): the state says what the engine is doing with the
//! monitor, the outcome says what the source last told us about the work.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorState {
    Registering,
    Active,
    Checking,
    Sleeping,
    ActionPending,
    AwaitingApproval,
    Completed,
    Exhausted,
}

impl MonitorState {
    pub const ALL: &[MonitorState] = &[
        MonitorState::Registering,
        MonitorState::Active,
        MonitorState::Checking,
        MonitorState::Sleeping,
        MonitorState::ActionPending,
        MonitorState::AwaitingApproval,
        MonitorState::Completed,
        MonitorState::Exhausted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MonitorState::Registering => "registering",
            MonitorState::Active => "active",
            MonitorState::Checking => "checking",
            MonitorState::Sleeping => "sleeping",
            MonitorState::ActionPending => "action_pending",
            MonitorState::AwaitingApproval => "awaiting_approval",
            MonitorState::Completed => "completed",
            MonitorState::Exhausted => "exhausted",
        }
    }

    /// Accepts both `snake_case` (stored) and the spec's CLI kebab form.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.replace('-', "_");
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }

    /// States the scheduler may claim. Everything else is parked for a
    /// human, an executor (plan-2), or is finished.
    pub fn is_claimable(self) -> bool {
        matches!(self, MonitorState::Active | MonitorState::Sleeping)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pending,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pending => "pending",
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Outcome::Pending),
            "succeeded" => Some(Outcome::Succeeded),
            "failed" => Some(Outcome::Failed),
            "cancelled" => Some(Outcome::Cancelled),
            "unknown" => Some(Outcome::Unknown),
            _ => None,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Outcome::Succeeded | Outcome::Failed | Outcome::Cancelled)
    }
}
```

`mur-monitor/src/backoff.rs`:
```rust
//! Poll intervals (spec §排程、租約與退避). Two tables: `pending` for a
//! source that answered, `unknown` for a source that did not — the second is
//! shorter and capped lower because it is our problem to notice quickly, not
//! the work's.

use std::time::Duration;

/// 30s → 1m → 2m → 5m → 15m → 30m, then held at 30m.
const PENDING_STEPS_SECS: &[u64] = &[30, 60, 120, 300, 900, 1800];
/// 10s → 30s → 1m → 5m, then held at 5m.
const UNKNOWN_STEPS_SECS: &[u64] = &[10, 30, 60, 300];
/// No adapter recommendation may schedule a check sooner than this.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// Read-only cadence after a hard deadline when monitoring is retained.
pub const RETAIN_INTERVAL: Duration = Duration::from_secs(2 * 3600);
/// Consecutive `unknown` observations before a `monitor_unhealthy` event.
pub const UNHEALTHY_AFTER_UNKNOWN: u32 = 6;
/// ± this percentage of the base interval.
const JITTER_PCT: u64 = 20;

pub fn pending_delay(attempt: u32) -> Duration {
    step(PENDING_STEPS_SECS, attempt)
}

pub fn unknown_delay(streak: u32) -> Duration {
    step(UNKNOWN_STEPS_SECS, streak)
}

fn step(table: &[u64], i: u32) -> Duration {
    let idx = (i as usize).min(table.len() - 1);
    Duration::from_secs(table[idx])
}

/// Deterministic jitter in `[base − 20 %, base + 20 %]`. Deterministic so a
/// test can assert an exact `next_check_at`; spread so a herd of monitors
/// created together does not poll together.
pub fn with_jitter(base: Duration, seed: u64) -> Duration {
    let span = 2 * JITTER_PCT + 1;
    let offset_pct = (seed % span) as i64 - JITTER_PCT as i64;
    let base_ms = base.as_millis() as i64;
    let jittered = base_ms + base_ms * offset_pct / 100;
    Duration::from_millis(jittered.max(0) as u64)
}

/// An adapter's `recommended_poll_after` (e.g. GitHub `Retry-After`) may
/// lengthen a wait but never shorten it below the global floor.
pub fn clamp_recommended(recommended: Option<Duration>, computed: Duration) -> Duration {
    match recommended {
        Some(r) => r.max(MIN_INTERVAL),
        None => computed.max(MIN_INTERVAL),
    }
}

/// FNV-1a over `monitor_id` and `attempt` — stable across processes, no dep.
pub fn seed(monitor_id: &str, attempt: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in monitor_id.bytes().chain(attempt.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
```

`lib.rs`: add `pub mod backoff;` and `pub mod state;`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor state:: backoff::`
Expected: 6 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): state/outcome enums and the pending/unknown backoff tables"
```

---

### Task 3: `Observation`, `SourceAdapter`, `AdapterRegistry`

**Files:**
- Create: `mur-monitor/src/adapter.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod adapter;`)
- Test: `mur-monitor/src/adapter.rs`

**Interfaces:**
- Consumes: `state::Outcome`, `spec::SourceType`, `mur_common::redact::redact_secrets(&str) -> Cow<str>`
- Produces:
  ```rust
  pub struct Observation { pub outcome: Outcome, pub progress_token: Option<String>, pub evidence: String, pub recommended_poll_after: Option<Duration>, pub adapter_error: Option<String> }
  impl Observation { pub fn pending(token: impl Into<String>, evidence: impl Into<String>) -> Self; pub fn terminal(outcome: Outcome, evidence: impl Into<String>) -> Self; pub fn unknown(error: impl Into<String>) -> Self; pub fn with_poll_after(self, d: Duration) -> Self; pub fn redacted(self) -> Self }
  pub trait SourceAdapter: Send + Sync { fn source_type(&self) -> SourceType; fn validate_reference(&self, reference: &str) -> Result<(), String>; fn observe(&self, reference: &str, credential_ref: Option<&str>) -> Observation; }
  pub struct AdapterRegistry; impl AdapterRegistry { pub fn new() -> Self; pub fn register(&mut self, a: Box<dyn SourceAdapter>); pub fn get(&self, t: SourceType) -> Option<&dyn SourceAdapter>; pub fn types(&self) -> Vec<SourceType> }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl SourceAdapter for Fake {
        fn source_type(&self) -> SourceType {
            SourceType::Custom
        }
        fn validate_reference(&self, r: &str) -> Result<(), String> {
            if r.is_empty() { Err("empty".into()) } else { Ok(()) }
        }
        fn observe(&self, _r: &str, _c: Option<&str>) -> Observation {
            Observation::pending("t1", "fine")
        }
    }

    #[test]
    fn registry_finds_by_source_type() {
        let mut reg = AdapterRegistry::new();
        reg.register(Box::new(Fake));
        assert!(reg.get(SourceType::Custom).is_some());
        assert!(reg.get(SourceType::MurRun).is_none());
        assert_eq!(reg.types(), vec![SourceType::Custom]);
    }

    #[test]
    fn unknown_carries_the_error_and_is_not_terminal() {
        let o = Observation::unknown("http 503");
        assert_eq!(o.outcome, Outcome::Unknown);
        assert_eq!(o.adapter_error.as_deref(), Some("http 503"));
        assert!(!o.outcome.is_terminal());
    }

    #[test]
    fn redacted_scrubs_evidence_and_error() {
        let o = Observation::unknown("token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ab rejected")
            .redacted();
        assert!(!o.adapter_error.as_deref().unwrap().contains("ghp_ABCDEFGHIJ"), "{o:?}");
        let o = Observation::terminal(Outcome::Failed, "Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz")
            .redacted();
        assert!(!o.evidence.contains("sk-ant-api03-abcdef"), "{}", o.evidence);
    }
}
```
If `redact_secrets` does not catch one of those two shapes, look at `mur-common/src/redact.rs` for the patterns it does cover and use one of them in the test — the point is that the chokepoint is applied, not which regexes it has.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor adapter::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! What an adapter hands back, and the trait every source implements.
//! Read-only by construction: nothing on `SourceAdapter` can mutate the
//! source (spec: custom adapters "預設只讀"; actions are plan-2).

use std::collections::HashMap;
use std::time::Duration;

use crate::spec::SourceType;
use crate::state::Outcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub outcome: Outcome,
    /// Opaque; only a CHANGE resets the stalled timer (spec §stalled 與期限).
    pub progress_token: Option<String>,
    /// Short, human-readable, already redacted once `redacted()` has run.
    pub evidence: String,
    /// e.g. GitHub `Retry-After`. Clamped by `backoff::clamp_recommended`.
    pub recommended_poll_after: Option<Duration>,
    /// Set when `outcome == Unknown` for a monitor-side reason.
    pub adapter_error: Option<String>,
}

impl Observation {
    pub fn pending(token: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Pending,
            progress_token: Some(token.into()),
            evidence: evidence.into(),
            recommended_poll_after: None,
            adapter_error: None,
        }
    }
    pub fn terminal(outcome: Outcome, evidence: impl Into<String>) -> Self {
        debug_assert!(outcome.is_terminal());
        Self {
            outcome,
            progress_token: None,
            evidence: evidence.into(),
            recommended_poll_after: None,
            adapter_error: None,
        }
    }
    pub fn unknown(error: impl Into<String>) -> Self {
        let e = error.into();
        Self {
            outcome: Outcome::Unknown,
            progress_token: None,
            evidence: e.clone(),
            recommended_poll_after: None,
            adapter_error: Some(e),
        }
    }
    pub fn with_poll_after(mut self, d: Duration) -> Self {
        self.recommended_poll_after = Some(d);
        self
    }
    /// The single redaction chokepoint before store, CLI, or (plan-2) agent.
    pub fn redacted(mut self) -> Self {
        self.evidence = mur_common::redact::redact_secrets(&self.evidence).into_owned();
        self.adapter_error = self
            .adapter_error
            .map(|e| mur_common::redact::redact_secrets(&e).into_owned());
        self
    }
}

pub trait SourceAdapter: Send + Sync {
    fn source_type(&self) -> SourceType;
    /// Shape check only — no network. `add` also runs one real `observe`.
    fn validate_reference(&self, reference: &str) -> Result<(), String>;
    /// One read-only query. Must never panic and never block unbounded.
    fn observe(&self, reference: &str, credential_ref: Option<&str>) -> Observation;
}

#[derive(Default)]
pub struct AdapterRegistry {
    map: HashMap<SourceType, Box<dyn SourceAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, adapter: Box<dyn SourceAdapter>) {
        self.map.insert(adapter.source_type(), adapter);
    }
    pub fn get(&self, t: SourceType) -> Option<&dyn SourceAdapter> {
        self.map.get(&t).map(|b| b.as_ref())
    }
    pub fn types(&self) -> Vec<SourceType> {
        let mut v: Vec<_> = self.map.keys().copied().collect();
        v.sort_by_key(|t| t.as_str());
        v
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor adapter::`
Expected: 3 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): Observation, SourceAdapter trait, AdapterRegistry"
```

---

### Task 4: SQLite store — open/migrate, create (idempotent), get, list, set_state, reactivate

**Files:**
- Create: `mur-monitor/src/store/mod.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod store;`)
- Test: `mur-monitor/src/store/mod.rs`

**Interfaces:**
- Consumes: `spec::MonitorSpec`, `state::{MonitorState, Outcome}`
- Produces:
  ```rust
  pub const DB_FILE: &str = "monitors.db"; pub fn db_dir(mur_home: &Path) -> PathBuf  // <mur_home>/monitor
  pub struct MonitorStore { conn: rusqlite::Connection }
  pub struct MonitorRow { pub id: String, pub name: String, pub spec: MonitorSpec, pub state: MonitorState, pub outcome: Outcome, pub source_type: SourceType, pub reference: String, pub idempotency_key: String, pub created_at: DateTime<Utc>, pub work_started_at: DateTime<Utc>, pub next_check_at: DateTime<Utc>, pub last_checked_at: Option<DateTime<Utc>>, pub last_progress_at: DateTime<Utc>, pub progress_token: Option<String>, pub pending_attempts: u32, pub unknown_streak: u32, pub remediation_attempts: u32, pub cycle_id: String, pub stalled_since: Option<DateTime<Utc>>, pub soft_notified: bool, pub hard_reached: bool, pub fence: i64, pub version: i64 }
  pub struct Created { pub id: String, pub existing: bool, pub next_check_at: DateTime<Utc> }
  #[derive(Default)] pub struct ListFilter { pub state: Option<MonitorState>, pub include_completed: bool }
  impl MonitorStore {
    pub fn open(mur_home: &Path) -> anyhow::Result<Self>;
    pub fn create(&self, spec: &MonitorSpec, now: DateTime<Utc>, work_started_at: Option<DateTime<Utc>>) -> anyhow::Result<Created>;
    pub fn get(&self, id: &str) -> anyhow::Result<Option<MonitorRow>>;
    pub fn list(&self, f: &ListFilter) -> anyhow::Result<Vec<MonitorRow>>;
    pub fn set_state(&self, id: &str, state: MonitorState, now: DateTime<Utc>) -> anyhow::Result<bool>;
    pub fn reactivate(&self, id: &str, now: DateTime<Utc>, reset_budget: bool) -> anyhow::Result<bool>;
    pub(crate) fn conn(&self) -> &rusqlite::Connection;
  }
  pub(crate) fn ts(dt: DateTime<Utc>) -> String; pub(crate) fn parse_ts(s: &str) -> DateTime<Utc>; pub(crate) fn row_to_monitor(r: &rusqlite::Row<'_>) -> rusqlite::Result<MonitorRow>; pub(crate) const MONITOR_COLS: &str
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;

    pub(crate) fn spec(idem: &str) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: run-1 }}
idempotency_key: {idem}
created_by: {{ actor: user:test }}
"#
        ))
        .unwrap()
    }

    pub(crate) fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn open_twice_and_migrate_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        MonitorStore::open(d.path()).unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_USER_VERSION);
        assert!(d.path().join("monitor").join(DB_FILE).exists());
    }

    #[test]
    fn create_is_idempotent_on_active_key() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.create(&spec("k1"), t0(), None).unwrap();
        let b = s.create(&spec("k1"), t0(), None).unwrap();
        assert!(!a.existing);
        assert!(b.existing);
        assert_eq!(a.id, b.id);
        assert_eq!(a.next_check_at, t0(), "first check is immediate");
        let row = s.get(&a.id).unwrap().unwrap();
        assert_eq!(row.state, MonitorState::Active);
        assert_eq!(row.outcome, Outcome::Pending);
        assert_eq!(row.work_started_at, t0());
        assert_eq!(row.fence, 0);
        // a completed monitor no longer reserves its key
        assert!(s.set_state(&a.id, MonitorState::Completed, t0()).unwrap());
        let c = s.create(&spec("k1"), t0(), None).unwrap();
        assert!(!c.existing);
        assert_ne!(c.id, a.id);
    }

    #[test]
    fn work_started_at_is_the_callers_when_given() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let started = t0() - chrono::Duration::minutes(30);
        let c = s.create(&spec("k2"), t0(), Some(started)).unwrap();
        assert_eq!(s.get(&c.id).unwrap().unwrap().work_started_at, started);
    }

    #[test]
    fn list_hides_completed_by_default_and_filters_by_state() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.create(&spec("a"), t0(), None).unwrap();
        let b = s.create(&spec("b"), t0(), None).unwrap();
        s.set_state(&a.id, MonitorState::Completed, t0()).unwrap();
        s.set_state(&b.id, MonitorState::Exhausted, t0()).unwrap();
        let ids = |f: &ListFilter| -> Vec<String> { s.list(f).unwrap().into_iter().map(|r| r.id).collect() };
        assert_eq!(ids(&ListFilter::default()), vec![b.id.clone()], "exhausted needs a human, completed does not");
        assert_eq!(ids(&ListFilter { include_completed: true, ..Default::default() }).len(), 2);
        assert_eq!(ids(&ListFilter { state: Some(MonitorState::Completed), include_completed: true }), vec![a.id]);
    }

    #[test]
    fn reactivate_only_from_exhausted() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.create(&spec("a"), t0(), None).unwrap();
        assert!(!s.reactivate(&a.id, t0(), false).unwrap(), "active is not retryable");
        s.set_state(&a.id, MonitorState::Exhausted, t0()).unwrap();
        s.conn().execute("UPDATE monitors SET remediation_attempts = 3, unknown_streak = 9 WHERE id = ?1", [&a.id]).unwrap();
        assert!(s.reactivate(&a.id, t0(), false).unwrap());
        let r = s.get(&a.id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Active);
        assert_eq!(r.unknown_streak, 0);
        assert_eq!(r.remediation_attempts, 3, "budget kept unless asked");
        s.set_state(&a.id, MonitorState::Exhausted, t0()).unwrap();
        assert!(s.reactivate(&a.id, t0(), true).unwrap());
        assert_eq!(s.get(&a.id).unwrap().unwrap().remediation_attempts, 0);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor store::`
Expected: compile error.

- [ ] **Step 3: Implement**

`mur-monitor/src/store/mod.rs`:

```rust
//! SQLite persistence for monitors (spec §SQLite 資料模型). One file,
//! `<mur_home>/monitor/monitors.db`, WAL + busy_timeout exactly as
//! `mur-channel/src/index.rs` does — the CLI, the daemon and (plan-2) an
//! agent runtime open independent connections to it.
//!
//! Split: this file owns the schema and the monitor row; `lease.rs` owns
//! claim/heartbeat/expiry; `observe.rs` owns observations, events and the
//! write-back of one check cycle.

mod lease;
mod observe;

pub use lease::{Claimed, Lease};
pub use observe::{CycleUpdate, Event, EventRow, ObservationRow};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::spec::{MonitorSpec, SourceType};
use crate::state::{MonitorState, Outcome};

pub const DB_FILE: &str = "monitors.db";
/// Bumped when a migration below changes a column's meaning.
pub const SCHEMA_USER_VERSION: i64 = 1;

pub fn db_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("monitor")
}

pub struct MonitorStore {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct MonitorRow {
    pub id: String,
    pub name: String,
    pub spec: MonitorSpec,
    pub state: MonitorState,
    pub outcome: Outcome,
    pub source_type: SourceType,
    pub reference: String,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub work_started_at: DateTime<Utc>,
    pub next_check_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_progress_at: DateTime<Utc>,
    pub progress_token: Option<String>,
    pub pending_attempts: u32,
    pub unknown_streak: u32,
    pub remediation_attempts: u32,
    pub cycle_id: String,
    pub stalled_since: Option<DateTime<Utc>>,
    pub soft_notified: bool,
    pub hard_reached: bool,
    pub fence: i64,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct Created {
    pub id: String,
    pub existing: bool,
    pub next_check_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
pub struct ListFilter {
    pub state: Option<MonitorState>,
    pub include_completed: bool,
}

pub(crate) fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::UNIX_EPOCH)
}

pub(crate) const MONITOR_COLS: &str = "id, name, spec_json, state, outcome, source_type, reference, idempotency_key, \
     created_at, work_started_at, next_check_at, last_checked_at, last_progress_at, progress_token, \
     pending_attempts, unknown_streak, remediation_attempts, cycle_id, stalled_since, soft_notified, \
     hard_reached, fence, version";

fn conv<E: std::error::Error + Send + Sync + 'static>(e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

pub(crate) fn row_to_monitor(r: &rusqlite::Row<'_>) -> rusqlite::Result<MonitorRow> {
    let spec_json: String = r.get(2)?;
    let state: String = r.get(3)?;
    let outcome: String = r.get(4)?;
    let source_type: String = r.get(5)?;
    let last_checked: Option<String> = r.get(11)?;
    let stalled: Option<String> = r.get(18)?;
    Ok(MonitorRow {
        id: r.get(0)?,
        name: r.get(1)?,
        spec: serde_json::from_str(&spec_json).map_err(conv)?,
        state: MonitorState::parse(&state)
            .ok_or_else(|| conv(std::io::Error::other(format!("bad state {state}"))))?,
        outcome: Outcome::parse(&outcome)
            .ok_or_else(|| conv(std::io::Error::other(format!("bad outcome {outcome}"))))?,
        source_type: SourceType::parse(&source_type)
            .ok_or_else(|| conv(std::io::Error::other(format!("bad source {source_type}"))))?,
        reference: r.get(6)?,
        idempotency_key: r.get(7)?,
        created_at: parse_ts(&r.get::<_, String>(8)?),
        work_started_at: parse_ts(&r.get::<_, String>(9)?),
        next_check_at: parse_ts(&r.get::<_, String>(10)?),
        last_checked_at: last_checked.as_deref().map(parse_ts),
        last_progress_at: parse_ts(&r.get::<_, String>(12)?),
        progress_token: r.get(13)?,
        pending_attempts: r.get::<_, i64>(14)? as u32,
        unknown_streak: r.get::<_, i64>(15)? as u32,
        remediation_attempts: r.get::<_, i64>(16)? as u32,
        cycle_id: r.get(17)?,
        stalled_since: stalled.as_deref().map(parse_ts),
        soft_notified: r.get::<_, i64>(19)? != 0,
        hard_reached: r.get::<_, i64>(20)? != 0,
        fence: r.get(21)?,
        version: r.get(22)?,
    })
}

impl MonitorStore {
    pub fn open(mur_home: &Path) -> Result<Self> {
        let dir = db_dir(mur_home);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let conn = Connection::open(dir.join(DB_FILE)).context("open monitors.db")?;
        // busy_timeout BEFORE journal_mode — see mur-channel/src/index.rs for why.
        conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;")
            .context("configure monitors.db pragmas")?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS monitors (
                id                   TEXT PRIMARY KEY,
                name                 TEXT NOT NULL,
                spec_json            TEXT NOT NULL,
                state                TEXT NOT NULL,
                outcome              TEXT NOT NULL,
                source_type          TEXT NOT NULL,
                reference            TEXT NOT NULL,
                idempotency_key      TEXT NOT NULL,
                created_at           TEXT NOT NULL,
                work_started_at      TEXT NOT NULL,
                next_check_at        TEXT NOT NULL,
                last_checked_at      TEXT,
                last_progress_at     TEXT NOT NULL,
                progress_token       TEXT,
                pending_attempts     INTEGER NOT NULL DEFAULT 0,
                unknown_streak       INTEGER NOT NULL DEFAULT 0,
                remediation_attempts INTEGER NOT NULL DEFAULT 0,
                cycle_id             TEXT NOT NULL,
                stalled_since        TEXT,
                soft_notified        INTEGER NOT NULL DEFAULT 0,
                hard_reached         INTEGER NOT NULL DEFAULT 0,
                fence                INTEGER NOT NULL DEFAULT 0,
                version              INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS idx_monitors_due ON monitors(state, next_check_at);
            -- rule 6: unique among monitors that are not finished
            CREATE UNIQUE INDEX IF NOT EXISTS idx_monitors_open_key
                ON monitors(idempotency_key) WHERE state != 'completed';

            CREATE TABLE IF NOT EXISTS monitor_cycles (
                id               TEXT PRIMARY KEY,
                monitor_id       TEXT NOT NULL,
                parent_cycle_id  TEXT,
                reference        TEXT NOT NULL,
                started_at       TEXT NOT NULL,
                finished_at      TEXT,
                terminal_outcome TEXT
            );
            CREATE TABLE IF NOT EXISTS monitor_leases (
                monitor_id TEXT PRIMARY KEY,
                owner      TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                fence      INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS monitor_observations (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                monitor_id     TEXT NOT NULL,
                cycle_id       TEXT NOT NULL,
                fence          INTEGER NOT NULL,
                observed_at    TEXT NOT NULL,
                outcome        TEXT NOT NULL,
                progress_token TEXT,
                evidence       TEXT NOT NULL,
                adapter_error  TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_obs_monitor ON monitor_observations(monitor_id, id DESC);
            CREATE TABLE IF NOT EXISTS monitor_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                monitor_id TEXT NOT NULL,
                cycle_id   TEXT NOT NULL,
                kind       TEXT NOT NULL,
                dedup_key  TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(monitor_id, cycle_id, dedup_key)
            );
            -- plan-2 tables, created now so later migrations stay additive
            CREATE TABLE IF NOT EXISTS monitor_actions (
                action_key  TEXT PRIMARY KEY,
                monitor_id  TEXT NOT NULL,
                cycle_id    TEXT NOT NULL,
                risk        TEXT NOT NULL,
                approval_id TEXT,
                state       TEXT NOT NULL,
                attempt     INTEGER NOT NULL DEFAULT 0,
                result      TEXT,
                created_at  TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS monitor_notifications (
                event_key      TEXT PRIMARY KEY,
                monitor_id     TEXT NOT NULL,
                channel        TEXT NOT NULL,
                delivery_state TEXT NOT NULL,
                updated_at     TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS monitor_registration_outbox (
                id         TEXT PRIMARY KEY,
                spec_json  TEXT NOT NULL,
                created_at TEXT NOT NULL,
                attempts   INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );",
        )?;
        self.conn
            .pragma_update(None, "user_version", SCHEMA_USER_VERSION)?;
        Ok(())
    }

    /// Rule 6: a second `create` with the same key while the first is still
    /// open returns the first monitor, never a duplicate.
    pub fn create(
        &self,
        spec: &MonitorSpec,
        now: DateTime<Utc>,
        work_started_at: Option<DateTime<Utc>>,
    ) -> Result<Created> {
        let tx = self.conn.unchecked_transaction()?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT id, next_check_at FROM monitors WHERE idempotency_key = ?1 AND state != 'completed'",
                [&spec.idempotency_key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, next)) = existing {
            tx.commit()?;
            return Ok(Created { id, existing: true, next_check_at: parse_ts(&next) });
        }
        let id = uuid::Uuid::now_v7().to_string();
        let cycle_id = uuid::Uuid::now_v7().to_string();
        let started = work_started_at.unwrap_or(now);
        tx.execute(
            &format!(
                "INSERT INTO monitors ({MONITOR_COLS}) VALUES \
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, NULL, 0, 0, 0, ?13, NULL, 0, 0, 0, 1)"
            ),
            params![
                id,
                spec.name,
                serde_json::to_string(spec)?,
                MonitorState::Active.as_str(),
                Outcome::Pending.as_str(),
                spec.source.r#type.as_str(),
                spec.source.reference,
                spec.idempotency_key,
                ts(now),
                ts(started),
                ts(now),
                ts(started),
                cycle_id,
            ],
        )?;
        tx.execute(
            "INSERT INTO monitor_cycles (id, monitor_id, parent_cycle_id, reference, started_at) VALUES (?1, ?2, NULL, ?3, ?4)",
            params![cycle_id, id, spec.source.reference, ts(started)],
        )?;
        tx.execute(
            "INSERT INTO monitor_events (monitor_id, cycle_id, kind, dedup_key, payload, created_at) VALUES (?1, ?2, 'created', 'created', ?3, ?4)",
            params![
                id,
                cycle_id,
                serde_json::json!({
                    "schema_version": spec.schema_version,
                    "actor": spec.created_by.actor,
                    "reason": spec.created_by.reason,
                    "originating_run_id": spec.created_by.originating_run_id,
                })
                .to_string(),
                ts(now)
            ],
        )?;
        tx.commit()?;
        Ok(Created { id, existing: false, next_check_at: now })
    }

    pub fn get(&self, id: &str) -> Result<Option<MonitorRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {MONITOR_COLS} FROM monitors WHERE id = ?1"),
                [id],
                row_to_monitor,
            )
            .optional()?)
    }

    pub fn list(&self, f: &ListFilter) -> Result<Vec<MonitorRow>> {
        let mut sql = format!("SELECT {MONITOR_COLS} FROM monitors WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        if let Some(s) = f.state {
            sql.push_str(" AND state = ?1");
            args.push(s.as_str().to_string());
        } else if !f.include_completed {
            sql.push_str(" AND state != 'completed'");
        }
        sql.push_str(" ORDER BY next_check_at ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), row_to_monitor)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Direct state write for CLI verbs (`cancel`) — bumps `version`, leaves
    /// the fence alone (no lease is involved).
    pub fn set_state(&self, id: &str, state: MonitorState, now: DateTime<Utc>) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE monitors SET state = ?1, version = version + 1, last_checked_at = COALESCE(last_checked_at, ?2) WHERE id = ?3",
            params![state.as_str(), ts(now), id],
        )?;
        Ok(n == 1)
    }

    /// `mur monitor retry`: only an `exhausted` monitor comes back. Clears
    /// the unknown streak (a fresh look), keeps the remediation budget
    /// unless the caller explicitly resets it (spec §CLI).
    pub fn reactivate(&self, id: &str, now: DateTime<Utc>, reset_budget: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE monitors SET state = 'active', next_check_at = ?1, unknown_streak = 0, \
             remediation_attempts = CASE WHEN ?2 THEN 0 ELSE remediation_attempts END, \
             version = version + 1 WHERE id = ?3 AND state = 'exhausted'",
            params![ts(now), reset_budget, id],
        )?;
        Ok(n == 1)
    }
}
```

Create empty `mur-monitor/src/store/lease.rs` and `mur-monitor/src/store/observe.rs` for now containing only the `pub use` targets as stubs:

```rust
// lease.rs — filled in Task 5
pub struct Claimed;
pub struct Lease;
```
```rust
// observe.rs — filled in Task 6
pub struct CycleUpdate;
pub struct Event;
pub struct EventRow;
pub struct ObservationRow;
```

`lib.rs`: add `pub mod store;`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor store::`
Expected: 5 PASS. If `pragma_update` complains about the value type, use `self.conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_USER_VERSION}"))`.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): SQLite store with idempotent create, list, set_state, reactivate"
```

---

### Task 5: Leases with fencing tokens

**Files:**
- Modify: `mur-monitor/src/store/lease.rs` (replace the stub)
- Test: `mur-monitor/src/store/lease.rs`

**Interfaces:**
- Consumes: `store::{MonitorStore, MonitorRow, MONITOR_COLS, row_to_monitor, ts, parse_ts}`, `state::MonitorState`
- Produces:
  ```rust
  pub struct Claimed { pub row: MonitorRow, pub fence: i64 }
  pub struct Lease { pub owner: String, pub expires_at: DateTime<Utc>, pub fence: i64 }
  impl MonitorStore {
    pub fn claim_due(&self, now: DateTime<Utc>, owner: &str, lease: Duration, max: usize) -> anyhow::Result<Vec<Claimed>>;
    pub fn heartbeat(&self, id: &str, fence: i64, now: DateTime<Utc>, lease: Duration) -> anyhow::Result<bool>;
    pub fn release(&self, id: &str, fence: i64, back_to: MonitorState) -> anyhow::Result<bool>;
    pub fn expire_leases(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<String>>;
    pub fn lease_of(&self, id: &str) -> anyhow::Result<Option<Lease>>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{spec, t0};
    use std::time::Duration;

    const LEASE: Duration = Duration::from_secs(120);

    #[test]
    fn two_workers_one_wins() {
        let d = tempfile::tempdir().unwrap();
        let a = MonitorStore::open(d.path()).unwrap();
        let b = MonitorStore::open(d.path()).unwrap();
        a.create(&spec("k"), t0(), None).unwrap();
        let got_a = a.claim_due(t0(), "wa", LEASE, 10).unwrap();
        let got_b = b.claim_due(t0(), "wb", LEASE, 10).unwrap();
        assert_eq!(got_a.len(), 1);
        assert_eq!(got_b.len(), 0, "monitor is `checking` and leased");
        assert_eq!(got_a[0].fence, 1);
        assert_eq!(got_a[0].row.state, MonitorState::Checking);
        let l = a.lease_of(&got_a[0].row.id).unwrap().unwrap();
        assert_eq!(l.owner, "wa");
        assert_eq!(l.expires_at, t0() + chrono::Duration::seconds(120));
    }

    #[test]
    fn not_due_is_not_claimed() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        s.conn().execute("UPDATE monitors SET next_check_at = ?1 WHERE id = ?2", rusqlite::params![ts(t0() + chrono::Duration::seconds(30)), c.id]).unwrap();
        assert!(s.claim_due(t0(), "w", LEASE, 10).unwrap().is_empty());
        assert_eq!(s.claim_due(t0() + chrono::Duration::seconds(30), "w", LEASE, 10).unwrap().len(), 1);
    }

    #[test]
    fn expired_lease_is_recovered_and_reclaimed_with_a_higher_fence() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let first = s.claim_due(t0(), "dead-worker", LEASE, 10).unwrap();
        assert_eq!(first[0].fence, 1);
        let later = t0() + chrono::Duration::seconds(121);
        assert!(s.claim_due(later, "w2", LEASE, 10).unwrap().is_empty(), "still `checking` until recovered");
        assert_eq!(s.expire_leases(later).unwrap(), vec![c.id.clone()]);
        assert_eq!(s.get(&c.id).unwrap().unwrap().state, MonitorState::Active);
        let second = s.claim_due(later, "w2", LEASE, 10).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].fence, 2);
    }

    #[test]
    fn stale_fence_cannot_heartbeat_or_release() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s.claim_due(t0(), "w", LEASE, 10).unwrap();
        assert!(!s.heartbeat(&c.id, cl[0].fence - 1, t0(), LEASE).unwrap());
        assert!(s.heartbeat(&c.id, cl[0].fence, t0() + chrono::Duration::seconds(60), LEASE).unwrap());
        assert_eq!(s.lease_of(&c.id).unwrap().unwrap().expires_at, t0() + chrono::Duration::seconds(180));
        assert!(!s.release(&c.id, 99, MonitorState::Sleeping).unwrap());
        assert!(s.release(&c.id, cl[0].fence, MonitorState::Sleeping).unwrap());
        assert!(s.lease_of(&c.id).unwrap().is_none());
        assert_eq!(s.get(&c.id).unwrap().unwrap().state, MonitorState::Sleeping);
    }

    #[test]
    fn max_bounds_a_thundering_herd() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        for i in 0..20 {
            s.create(&spec(&format!("k{i}")), t0(), None).unwrap();
        }
        assert_eq!(s.claim_due(t0(), "w", LEASE, 8).unwrap().len(), 8);
        assert_eq!(s.claim_due(t0(), "w", LEASE, 8).unwrap().len(), 8);
        assert_eq!(s.claim_due(t0(), "w", LEASE, 8).unwrap().len(), 4);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor store::lease::`
Expected: compile error (stub structs have no fields; methods missing).

- [ ] **Step 3: Implement**

```rust
//! Leases (spec §租約). A claim is one `BEGIN IMMEDIATE` transaction that
//! flips the row to `checking`, bumps its fence, and writes the lease row —
//! so two daemons opening the same file cannot both take a monitor. The
//! fence is monotonic per monitor; every later write-back must present the
//! fence it claimed under, and a stale one is refused (Task 6).

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use super::{parse_ts, row_to_monitor, ts, MonitorRow, MonitorStore, MONITOR_COLS};
use crate::state::MonitorState;

#[derive(Debug, Clone)]
pub struct Claimed {
    pub row: MonitorRow,
    pub fence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub owner: String,
    pub expires_at: DateTime<Utc>,
    pub fence: i64,
}

fn expiry(now: DateTime<Utc>, lease: Duration) -> String {
    ts(now + chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::seconds(60)))
}

impl MonitorStore {
    /// Atomically claim up to `max` due monitors. Due = claimable state,
    /// `next_check_at <= now`, and no live lease. `max` is the per-tick
    /// throttle (spec §daemon 恢復: bounded work queue).
    pub fn claim_due(
        &self,
        now: DateTime<Utc>,
        owner: &str,
        lease: Duration,
        max: usize,
    ) -> Result<Vec<Claimed>> {
        let tx = self.conn().unchecked_transaction()?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT m.id FROM monitors m
                 LEFT JOIN monitor_leases l ON l.monitor_id = m.id
                 WHERE m.state IN ('active', 'sleeping')
                   AND m.next_check_at <= ?1
                   AND (l.monitor_id IS NULL OR l.expires_at <= ?1)
                 ORDER BY m.next_check_at ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![ts(now), max as i64], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            tx.execute(
                "UPDATE monitors SET state = 'checking', fence = fence + 1 WHERE id = ?1",
                [&id],
            )?;
            let row = tx.query_row(
                &format!("SELECT {MONITOR_COLS} FROM monitors WHERE id = ?1"),
                [&id],
                row_to_monitor,
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO monitor_leases (monitor_id, owner, expires_at, fence) VALUES (?1, ?2, ?3, ?4)",
                params![id, owner, expiry(now, lease), row.fence],
            )?;
            let fence = row.fence;
            out.push(Claimed { row, fence });
        }
        tx.commit()?;
        Ok(out)
    }

    /// Extend the lease during a long query. False = fence is stale (someone
    /// else holds this monitor now); the caller must stop and discard.
    pub fn heartbeat(&self, id: &str, fence: i64, now: DateTime<Utc>, lease: Duration) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE monitor_leases SET expires_at = ?1 WHERE monitor_id = ?2 AND fence = ?3",
            params![expiry(now, lease), id, fence],
        )?;
        Ok(n == 1)
    }

    /// Drop the lease and put the monitor back into `back_to` without
    /// recording an observation — the no-adapter path. `apply_cycle` (Task 6)
    /// releases as part of its own transaction.
    pub fn release(&self, id: &str, fence: i64, back_to: MonitorState) -> Result<bool> {
        let tx = self.conn().unchecked_transaction()?;
        let n = tx.execute(
            "DELETE FROM monitor_leases WHERE monitor_id = ?1 AND fence = ?2",
            params![id, fence],
        )?;
        if n == 1 {
            tx.execute(
                "UPDATE monitors SET state = ?1 WHERE id = ?2 AND fence = ?3",
                params![back_to.as_str(), id, fence],
            )?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    /// Recovery (spec §daemon 恢復 step 2): every expired lease is dropped,
    /// its monitor returned to `active` if it was mid-check, and a
    /// `lease_recovered` event appended. Returns the affected monitor ids.
    pub fn expire_leases(&self, now: DateTime<Utc>) -> Result<Vec<String>> {
        let tx = self.conn().unchecked_transaction()?;
        let expired: Vec<(String, String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT l.monitor_id, l.owner, l.fence FROM monitor_leases l WHERE l.expires_at <= ?1",
            )?;
            let rows = stmt.query_map([ts(now)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut ids = Vec::with_capacity(expired.len());
        for (id, owner, fence) in expired {
            tx.execute("DELETE FROM monitor_leases WHERE monitor_id = ?1", [&id])?;
            tx.execute(
                "UPDATE monitors SET state = 'active' WHERE id = ?1 AND state = 'checking'",
                [&id],
            )?;
            let cycle_id: String =
                tx.query_row("SELECT cycle_id FROM monitors WHERE id = ?1", [&id], |r| r.get(0))?;
            tx.execute(
                "INSERT OR IGNORE INTO monitor_events (monitor_id, cycle_id, kind, dedup_key, payload, created_at) \
                 VALUES (?1, ?2, 'lease_recovered', ?3, ?4, ?5)",
                params![
                    id,
                    cycle_id,
                    format!("lease_recovered:{fence}"),
                    serde_json::json!({ "owner": owner, "fence": fence }).to_string(),
                    ts(now)
                ],
            )?;
            ids.push(id);
        }
        tx.commit()?;
        Ok(ids)
    }

    pub fn lease_of(&self, id: &str) -> Result<Option<Lease>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT owner, expires_at, fence FROM monitor_leases WHERE monitor_id = ?1",
                [id],
                |r| {
                    Ok(Lease {
                        owner: r.get(0)?,
                        expires_at: parse_ts(&r.get::<_, String>(1)?),
                        fence: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor store::lease::`
Expected: 5 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): fenced leases — claim_due, heartbeat, release, expire"
```

---

### Task 6: Observations, deduped events, and `apply_cycle`

**Files:**
- Modify: `mur-monitor/src/store/observe.rs` (replace the stub)
- Test: `mur-monitor/src/store/observe.rs`

**Interfaces:**
- Consumes: `adapter::Observation`, `state::{MonitorState, Outcome}`, store internals
- Produces:
  ```rust
  pub struct Event { pub kind: &'static str, pub payload: serde_json::Value, pub dedup: bool }
  pub struct CycleUpdate { pub observation: Observation, pub observed_at: DateTime<Utc>, pub new_state: MonitorState, pub outcome: Outcome, pub next_check_at: DateTime<Utc>, pub pending_attempts: u32, pub unknown_streak: u32, pub last_progress_at: DateTime<Utc>, pub progress_token: Option<String>, pub stalled_since: Option<DateTime<Utc>>, pub soft_notified: bool, pub hard_reached: bool, pub finish_cycle: bool, pub events: Vec<Event> }
  pub struct ObservationRow { pub cycle_id: String, pub observed_at: DateTime<Utc>, pub outcome: Outcome, pub progress_token: Option<String>, pub evidence: String, pub adapter_error: Option<String> }
  pub struct EventRow { pub cycle_id: String, pub kind: String, pub payload: serde_json::Value, pub created_at: DateTime<Utc> }
  impl MonitorStore {
    pub fn apply_cycle(&self, id: &str, fence: i64, u: &CycleUpdate) -> anyhow::Result<bool>;
    pub fn append_event(&self, id: &str, cycle_id: &str, kind: &str, payload: serde_json::Value, dedup: bool, now: DateTime<Utc>) -> anyhow::Result<bool>;
    pub fn observations(&self, id: &str, limit: usize) -> anyhow::Result<Vec<ObservationRow>>;
    pub fn events(&self, id: &str) -> anyhow::Result<Vec<EventRow>>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Observation;
    use crate::store::tests::{spec, t0};
    use std::time::Duration;

    fn update(obs: Observation, state: MonitorState, now: DateTime<Utc>) -> CycleUpdate {
        CycleUpdate {
            outcome: obs.outcome,
            observation: obs,
            observed_at: now,
            new_state: state,
            next_check_at: now + chrono::Duration::seconds(30),
            pending_attempts: 1,
            unknown_streak: 0,
            last_progress_at: now,
            progress_token: Some("p1".into()),
            stalled_since: None,
            soft_notified: false,
            hard_reached: false,
            finish_cycle: state == MonitorState::Completed,
            events: vec![],
        }
    }

    #[test]
    fn apply_writes_observation_updates_row_and_releases_lease() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s.claim_due(t0(), "w", Duration::from_secs(60), 1).unwrap().remove(0);
        let ok = s.apply_cycle(&c.id, cl.fence, &update(Observation::pending("p1", "running"), MonitorState::Sleeping, t0())).unwrap();
        assert!(ok);
        let r = s.get(&c.id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Sleeping);
        assert_eq!(r.outcome, Outcome::Pending);
        assert_eq!(r.pending_attempts, 1);
        assert_eq!(r.progress_token.as_deref(), Some("p1"));
        assert_eq!(r.last_checked_at, Some(t0()));
        assert_eq!(r.version, 2);
        assert!(s.lease_of(&c.id).unwrap().is_none());
        let obs = s.observations(&c.id, 10).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].evidence, "running");
    }

    #[test]
    fn stale_fence_is_dropped_whole() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s.claim_due(t0(), "w", Duration::from_secs(60), 1).unwrap().remove(0);
        let ok = s.apply_cycle(&c.id, cl.fence + 1, &update(Observation::pending("p1", "late"), MonitorState::Sleeping, t0())).unwrap();
        assert!(!ok);
        assert!(s.observations(&c.id, 10).unwrap().is_empty(), "nothing from a stale worker lands");
        assert_eq!(s.get(&c.id).unwrap().unwrap().state, MonitorState::Checking);
        assert!(s.lease_of(&c.id).unwrap().is_some(), "the live worker's lease is untouched");
    }

    #[test]
    fn terminal_finishes_the_cycle() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s.claim_due(t0(), "w", Duration::from_secs(60), 1).unwrap().remove(0);
        let mut u = update(Observation::terminal(Outcome::Succeeded, "done"), MonitorState::Completed, t0());
        u.events.push(Event { kind: "terminal", payload: serde_json::json!({"outcome": "succeeded"}), dedup: true });
        assert!(s.apply_cycle(&c.id, cl.fence, &u).unwrap());
        let (finished, term): (Option<String>, Option<String>) = s
            .conn()
            .query_row("SELECT finished_at, terminal_outcome FROM monitor_cycles WHERE id = ?1", [&cl.row.cycle_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert!(finished.is_some());
        assert_eq!(term.as_deref(), Some("succeeded"));
        let ev = s.events(&c.id).unwrap();
        assert_eq!(ev.iter().filter(|e| e.kind == "terminal").count(), 1);
    }

    #[test]
    fn dedup_events_fire_once_per_cycle_plain_events_every_time() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cyc = s.get(&c.id).unwrap().unwrap().cycle_id;
        assert!(s.append_event(&c.id, &cyc, "stalled", serde_json::json!({}), true, t0()).unwrap());
        assert!(!s.append_event(&c.id, &cyc, "stalled", serde_json::json!({}), true, t0()).unwrap());
        assert!(s.append_event(&c.id, &cyc, "observed", serde_json::json!({}), false, t0()).unwrap());
        assert!(s.append_event(&c.id, &cyc, "observed", serde_json::json!({}), false, t0()).unwrap());
        let kinds: Vec<_> = s.events(&c.id).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["created", "stalled", "observed", "observed"]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor store::observe::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Append-only history (spec §冪等與事件紀錄) and the one write-back a check
//! cycle makes. `apply_cycle` is a single transaction guarded by the fence:
//! observation row, monitor columns, events, cycle finish, lease drop — all
//! or nothing, and nothing at all from a stale worker.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{parse_ts, ts, MonitorStore};
use crate::adapter::Observation;
use crate::state::{MonitorState, Outcome};

#[derive(Debug, Clone)]
pub struct Event {
    pub kind: &'static str,
    pub payload: serde_json::Value,
    /// True = at most once per (monitor, cycle, kind) — the notable events.
    /// False = every time (`observed`, `lease_recovered`).
    pub dedup: bool,
}

#[derive(Debug, Clone)]
pub struct CycleUpdate {
    pub observation: Observation,
    pub observed_at: DateTime<Utc>,
    pub new_state: MonitorState,
    pub outcome: Outcome,
    pub next_check_at: DateTime<Utc>,
    pub pending_attempts: u32,
    pub unknown_streak: u32,
    pub last_progress_at: DateTime<Utc>,
    pub progress_token: Option<String>,
    pub stalled_since: Option<DateTime<Utc>>,
    pub soft_notified: bool,
    pub hard_reached: bool,
    /// Stamp `finished_at` + `terminal_outcome` on the current cycle.
    pub finish_cycle: bool,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct ObservationRow {
    pub cycle_id: String,
    pub observed_at: DateTime<Utc>,
    pub outcome: Outcome,
    pub progress_token: Option<String>,
    pub evidence: String,
    pub adapter_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub cycle_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

fn insert_event(
    conn: &rusqlite::Connection,
    id: &str,
    cycle_id: &str,
    kind: &str,
    payload: &serde_json::Value,
    dedup: bool,
    now: DateTime<Utc>,
) -> rusqlite::Result<bool> {
    let dedup_key = if dedup {
        kind.to_string()
    } else {
        format!("{kind}:{}", uuid::Uuid::now_v7())
    };
    let n = conn.execute(
        "INSERT OR IGNORE INTO monitor_events (monitor_id, cycle_id, kind, dedup_key, payload, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, cycle_id, kind, dedup_key, payload.to_string(), ts(now)],
    )?;
    Ok(n == 1)
}

impl MonitorStore {
    /// Returns `false` (and writes nothing) when `fence` is not the fence
    /// the row currently carries — a worker whose lease expired and was
    /// reclaimed must not overwrite the newer cycle (spec §租約).
    pub fn apply_cycle(&self, id: &str, fence: i64, u: &CycleUpdate) -> Result<bool> {
        let tx = self.conn().unchecked_transaction()?;
        let current: Option<(i64, String)> = tx
            .query_row("SELECT fence, cycle_id FROM monitors WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok();
        let Some((cur_fence, cycle_id)) = current else {
            return Ok(false);
        };
        if cur_fence != fence {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO monitor_observations (monitor_id, cycle_id, fence, observed_at, outcome, progress_token, evidence, adapter_error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                cycle_id,
                fence,
                ts(u.observed_at),
                u.observation.outcome.as_str(),
                u.observation.progress_token,
                u.observation.evidence,
                u.observation.adapter_error,
            ],
        )?;
        tx.execute(
            "UPDATE monitors SET state = ?1, outcome = ?2, next_check_at = ?3, last_checked_at = ?4, \
             last_progress_at = ?5, progress_token = ?6, pending_attempts = ?7, unknown_streak = ?8, \
             stalled_since = ?9, soft_notified = ?10, hard_reached = ?11, version = version + 1 \
             WHERE id = ?12 AND fence = ?13",
            params![
                u.new_state.as_str(),
                u.outcome.as_str(),
                ts(u.next_check_at),
                ts(u.observed_at),
                ts(u.last_progress_at),
                u.progress_token,
                u.pending_attempts as i64,
                u.unknown_streak as i64,
                u.stalled_since.map(ts),
                u.soft_notified as i64,
                u.hard_reached as i64,
                id,
                fence,
            ],
        )?;
        for e in &u.events {
            insert_event(&tx, id, &cycle_id, e.kind, &e.payload, e.dedup, u.observed_at)?;
        }
        if u.finish_cycle {
            tx.execute(
                "UPDATE monitor_cycles SET finished_at = ?1, terminal_outcome = ?2 WHERE id = ?3 AND finished_at IS NULL",
                params![ts(u.observed_at), u.outcome.as_str(), cycle_id],
            )?;
        }
        tx.execute(
            "DELETE FROM monitor_leases WHERE monitor_id = ?1 AND fence = ?2",
            params![id, fence],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn append_event(
        &self,
        id: &str,
        cycle_id: &str,
        kind: &str,
        payload: serde_json::Value,
        dedup: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(insert_event(self.conn(), id, cycle_id, kind, &payload, dedup, now)?)
    }

    /// Newest first.
    pub fn observations(&self, id: &str, limit: usize) -> Result<Vec<ObservationRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT cycle_id, observed_at, outcome, progress_token, evidence, adapter_error \
             FROM monitor_observations WHERE monitor_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit as i64], |r| {
            let outcome: String = r.get(2)?;
            Ok(ObservationRow {
                cycle_id: r.get(0)?,
                observed_at: parse_ts(&r.get::<_, String>(1)?),
                outcome: Outcome::parse(&outcome).unwrap_or(Outcome::Unknown),
                progress_token: r.get(3)?,
                evidence: r.get(4)?,
                adapter_error: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Oldest first — it is a history.
    pub fn events(&self, id: &str) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT cycle_id, kind, payload, created_at FROM monitor_events WHERE monitor_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([id], |r| {
            let payload: String = r.get(2)?;
            Ok(EventRow {
                cycle_id: r.get(0)?,
                kind: r.get(1)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: parse_ts(&r.get::<_, String>(3)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor store::`
Expected: all store tests PASS (5 + 5 + 4).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): observations, deduped events, fenced apply_cycle"
```

---

### Task 7: Progress and deadline evaluator (pure)

**Files:**
- Create: `mur-monitor/src/deadline.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod deadline;`)
- Test: `mur-monitor/src/deadline.rs`

**Interfaces:**
- Consumes: `spec::Policy` (`stalled_after()/soft_deadline()/hard_deadline()`)
- Produces:
  ```rust
  pub fn advance_progress(prev_at: DateTime<Utc>, prev_token: Option<&str>, observed: Option<&str>, now: DateTime<Utc>) -> (DateTime<Utc>, Option<String>);
  pub struct DeadlineVerdict { pub stalled_since: Option<DateTime<Utc>>, pub stalled_newly: bool, pub recovered: bool, pub soft_notified: bool, pub soft_newly: bool, pub hard_reached: bool, pub hard_newly: bool }
  pub fn evaluate(policy: &Policy, work_started_at: DateTime<Utc>, last_progress_at: DateTime<Utc>, prev_stalled_since: Option<DateTime<Utc>>, prev_soft: bool, prev_hard: bool, now: DateTime<Utc>) -> DeadlineVerdict;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn same_token_does_not_advance_progress() {
        let (at, tok) = advance_progress(t0(), Some("a"), Some("a"), t0() + Duration::minutes(5));
        assert_eq!(at, t0());
        assert_eq!(tok.as_deref(), Some("a"));
    }

    #[test]
    fn new_token_advances_progress_and_none_keeps_previous() {
        let now = t0() + Duration::minutes(5);
        let (at, tok) = advance_progress(t0(), Some("a"), Some("b"), now);
        assert_eq!((at, tok.as_deref()), (now, Some("b")));
        let (at, tok) = advance_progress(t0(), Some("a"), None, now);
        assert_eq!((at, tok.as_deref()), (t0(), Some("a")), "an unknown answer is not progress and not regress");
        let (at, tok) = advance_progress(t0(), None, Some("first"), now);
        assert_eq!((at, tok.as_deref()), (now, Some("first")));
    }

    #[test]
    fn stalled_fires_once_at_twenty_minutes_and_recovers() {
        let p = Policy::default();
        let v = evaluate(&p, t0(), t0(), None, false, false, t0() + Duration::minutes(19));
        assert!(v.stalled_since.is_none() && !v.stalled_newly);
        let v = evaluate(&p, t0(), t0(), None, false, false, t0() + Duration::minutes(20));
        assert_eq!(v.stalled_since, Some(t0() + Duration::minutes(20)));
        assert!(v.stalled_newly);
        let again = evaluate(&p, t0(), t0(), v.stalled_since, false, false, t0() + Duration::minutes(25));
        assert_eq!(again.stalled_since, v.stalled_since, "keeps the first stall time");
        assert!(!again.stalled_newly, "only the first entry is an event");
        let rec = evaluate(&p, t0(), t0() + Duration::minutes(26), again.stalled_since, false, false, t0() + Duration::minutes(26));
        assert!(rec.stalled_since.is_none() && rec.recovered);
    }

    #[test]
    fn soft_and_hard_count_from_work_start_and_fire_once() {
        let p = Policy::default();
        let v = evaluate(&p, t0(), t0() + Duration::hours(3), None, false, false, t0() + Duration::hours(3));
        assert!(v.soft_notified && v.soft_newly && !v.hard_reached);
        let v2 = evaluate(&p, t0(), t0() + Duration::hours(4), None, true, false, t0() + Duration::hours(4));
        assert!(v2.soft_notified && !v2.soft_newly);
        let h = evaluate(&p, t0(), t0() + Duration::hours(8), None, true, false, t0() + Duration::hours(8));
        assert!(h.hard_reached && h.hard_newly);
        let h2 = evaluate(&p, t0(), t0() + Duration::hours(9), None, true, true, t0() + Duration::hours(9));
        assert!(h2.hard_reached && !h2.hard_newly);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor deadline::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Stalled / soft / hard semantics (spec §stalled 與期限). Pure: the caller
//! passes the previous flags and `now`; nothing here reads a clock or a
//! store. Deadlines run from `work_started_at`; the stalled timer runs from
//! `last_progress_at`, which only moves when the progress token CHANGES.

use chrono::{DateTime, Utc};

use crate::spec::Policy;

/// `observed == None` (an `unknown` answer) is neither progress nor a reset.
pub fn advance_progress(
    prev_at: DateTime<Utc>,
    prev_token: Option<&str>,
    observed: Option<&str>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, Option<String>) {
    match observed {
        Some(t) if Some(t) != prev_token => (now, Some(t.to_string())),
        _ => (prev_at, prev_token.map(str::to_string)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadlineVerdict {
    pub stalled_since: Option<DateTime<Utc>>,
    pub stalled_newly: bool,
    pub recovered: bool,
    pub soft_notified: bool,
    pub soft_newly: bool,
    pub hard_reached: bool,
    pub hard_newly: bool,
}

fn to_chrono(d: std::time::Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

pub fn evaluate(
    policy: &Policy,
    work_started_at: DateTime<Utc>,
    last_progress_at: DateTime<Utc>,
    prev_stalled_since: Option<DateTime<Utc>>,
    prev_soft: bool,
    prev_hard: bool,
    now: DateTime<Utc>,
) -> DeadlineVerdict {
    let is_stalled = now - last_progress_at >= to_chrono(policy.stalled_after());
    let stalled_since = if is_stalled { prev_stalled_since.or(Some(now)) } else { None };
    let soft = now - work_started_at >= to_chrono(policy.soft_deadline());
    let hard = now - work_started_at >= to_chrono(policy.hard_deadline());
    DeadlineVerdict {
        stalled_since,
        stalled_newly: is_stalled && prev_stalled_since.is_none(),
        recovered: !is_stalled && prev_stalled_since.is_some(),
        soft_notified: prev_soft || soft,
        soft_newly: soft && !prev_soft,
        hard_reached: prev_hard || hard,
        hard_newly: hard && !prev_hard,
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor deadline::`
Expected: 4 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): pure progress and stalled/soft/hard deadline evaluator"
```

---

### Task 8: Scheduler — `plan_cycle` (pure state machine), `tick`, `recover`

**Files:**
- Create: `mur-monitor/src/scheduler.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod scheduler;`)
- Test: `mur-monitor/src/scheduler.rs`

**Interfaces:**
- Consumes: everything above
- Produces:
  ```rust
  pub const DEFAULT_LEASE: Duration = Duration::from_secs(120);
  #[derive(Default)] pub struct TickReport { pub claimed: usize, pub observed: usize, pub completed: usize, pub action_pending: usize, pub exhausted: usize, pub unknown: usize, pub stale_fence: usize }
  pub struct RecoveryReport { pub recovered_leases: Vec<String>, pub overdue: usize }
  pub fn plan_cycle(row: &MonitorRow, obs: Observation, now: DateTime<Utc>) -> CycleUpdate;
  pub fn tick(store: &MonitorStore, registry: &AdapterRegistry, now: DateTime<Utc>, owner: &str, max_claims: usize) -> anyhow::Result<TickReport>;
  pub fn recover(store: &MonitorStore, now: DateTime<Utc>) -> anyhow::Result<RecoveryReport>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backoff::{pending_delay, seed, unknown_delay, with_jitter, RETAIN_INTERVAL, UNHEALTHY_AFTER_UNKNOWN};
    use crate::spec::{MonitorSpec, SourceType};
    use crate::store::tests::t0;
    use chrono::Duration as CD;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Hands back a scripted sequence; `unknown("script exhausted")` after.
    struct Scripted(Mutex<VecDeque<Observation>>);
    impl Scripted {
        fn new(v: Vec<Observation>) -> Self {
            Self(Mutex::new(v.into()))
        }
    }
    impl SourceAdapter for Scripted {
        fn source_type(&self) -> SourceType {
            SourceType::MurRun
        }
        fn validate_reference(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _: &str, _: Option<&str>) -> Observation {
            self.0.lock().unwrap().pop_front().unwrap_or_else(|| Observation::unknown("script exhausted"))
        }
    }

    fn registry(obs: Vec<Observation>) -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Box::new(Scripted::new(obs)));
        r
    }

    fn spec_yaml(actions: &str, retain: bool) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: run-1 }}
actions:
{actions}
policy: {{ retain_monitoring_after_hard_deadline: {retain} }}
idempotency_key: k
created_by: {{ actor: user:test }}
"#
        ))
        .unwrap()
    }

    fn fresh(actions: &str, retain: bool) -> (tempfile::TempDir, MonitorStore, String) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec_yaml(actions, retain), t0(), None).unwrap().id;
        (d, s, id)
    }

    fn cd(d: std::time::Duration) -> CD {
        CD::from_std(d).unwrap()
    }

    #[test]
    fn pending_sleeps_by_the_pending_table_with_deterministic_jitter() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let rep = tick(&s, &registry(vec![Observation::pending("p1", "running")]), t0(), "w", 8).unwrap();
        assert_eq!((rep.claimed, rep.observed, rep.unknown), (1, 1, 0));
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Sleeping);
        assert_eq!(r.pending_attempts, 1);
        assert_eq!(r.next_check_at, t0() + cd(with_jitter(pending_delay(0), seed(&id, 1))));
        assert!(s.lease_of(&id).unwrap().is_none());
    }

    #[test]
    fn unknown_uses_its_own_backoff_never_fails_and_flags_health_once() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![]); // every observe → unknown("script exhausted")
        let mut now = t0();
        for i in 1..=UNHEALTHY_AFTER_UNKNOWN + 2 {
            let rep = tick(&s, &reg, now, "w", 8).unwrap();
            assert_eq!(rep.unknown, 1, "tick {i}");
            let r = s.get(&id).unwrap().unwrap();
            assert_eq!(r.outcome, Outcome::Unknown);
            assert_ne!(r.state, MonitorState::Completed);
            assert_eq!(r.unknown_streak, i);
            assert_eq!(r.next_check_at, now + cd(with_jitter(unknown_delay(i - 1), seed(&id, i))));
            now = r.next_check_at;
        }
        let health = s.events(&id).unwrap().into_iter().filter(|e| e.kind == "monitor_unhealthy").count();
        assert_eq!(health, 1);
    }

    #[test]
    fn terminal_completes_once_and_is_never_reclaimed() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![
            Observation::terminal(Outcome::Succeeded, "done"),
            Observation::terminal(Outcome::Succeeded, "done again"),
        ]);
        let rep = tick(&s, &reg, t0(), "w", 8).unwrap();
        assert_eq!(rep.completed, 1);
        assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Completed);
        let rep2 = tick(&s, &reg, t0() + CD::hours(1), "w", 8).unwrap();
        assert_eq!(rep2.claimed, 0, "a completed monitor is never observed again");
        assert_eq!(s.events(&id).unwrap().iter().filter(|e| e.kind == "terminal").count(), 1);
    }

    #[test]
    fn terminal_with_actions_parks_in_action_pending_for_plan_2() {
        let (_d, s, id) = fresh("  on_failure:\n    - type: collect_logs", true);
        let rep = tick(&s, &registry(vec![Observation::terminal(Outcome::Failed, "exit 1")]), t0(), "w", 8).unwrap();
        assert_eq!(rep.action_pending, 1);
        assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::ActionPending);
        assert_eq!(tick(&s, &registry(vec![]), t0() + CD::hours(1), "w", 8).unwrap().claimed, 0);
    }

    #[test]
    fn cancelled_takes_the_failure_branch() {
        let (_d, s, id) = fresh("  on_failure:\n    - type: notify", true);
        tick(&s, &registry(vec![Observation::terminal(Outcome::Cancelled, "cancelled")]), t0(), "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!((r.state, r.outcome), (MonitorState::ActionPending, Outcome::Cancelled));
    }

    #[test]
    fn stalled_then_recovered_are_each_one_event() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![
            Observation::pending("p1", "a"),
            Observation::pending("p1", "same"),
            Observation::pending("p1", "same"),
            Observation::pending("p2", "moved"),
        ]);
        tick(&s, &reg, t0(), "w", 8).unwrap();
        tick(&s, &reg, t0() + CD::minutes(21), "w", 8).unwrap();
        tick(&s, &reg, t0() + CD::minutes(60), "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.stalled_since, Some(t0() + CD::minutes(21)));
        tick(&s, &reg, t0() + CD::minutes(90), "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert!(r.stalled_since.is_none());
        let kinds: Vec<_> = s.events(&id).unwrap().into_iter().map(|e| e.kind).filter(|k| k.starts_with("stalled")).collect();
        assert_eq!(kinds, vec!["stalled", "stalled_recovered"]);
    }

    #[test]
    fn hard_deadline_retains_at_two_hours_or_exhausts() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![Observation::pending("p", "x"), Observation::pending("p", "x")]);
        tick(&s, &reg, t0(), "w", 8).unwrap();
        let now = t0() + CD::hours(8);
        tick(&s, &reg, now, "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert!(r.hard_reached);
        assert_eq!(r.state, MonitorState::Sleeping);
        assert_eq!(r.next_check_at, now + cd(with_jitter(RETAIN_INTERVAL, seed(&id, 2))));
        assert_eq!(s.events(&id).unwrap().iter().filter(|e| e.kind == "hard_deadline").count(), 1);

        let (_d2, s2, id2) = fresh("  on_success: []", false);
        tick(&s2, &registry(vec![Observation::pending("p", "x")]), t0() + CD::hours(8), "w", 8).unwrap();
        let r2 = s2.get(&id2).unwrap().unwrap();
        assert_eq!(r2.state, MonitorState::Exhausted);
        assert_eq!(s2.events(&id2).unwrap().iter().filter(|e| e.kind == "exhausted").count(), 1);
    }

    #[test]
    fn soft_deadline_is_an_event_not_a_failure() {
        let (_d, s, id) = fresh("  on_success: []", true);
        tick(&s, &registry(vec![Observation::pending("p", "x")]), t0() + CD::hours(3), "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.outcome, Outcome::Pending);
        assert!(r.soft_notified && !r.hard_reached);
        assert_eq!(s.events(&id).unwrap().iter().filter(|e| e.kind == "soft_deadline").count(), 1);
    }

    #[test]
    fn missed_check_is_caught_up_after_recovery() {
        let (_d, s, id) = fresh("  on_success: []", true);
        // worker claims and dies mid-check
        let cl = s.claim_due(t0(), "dead", DEFAULT_LEASE, 8).unwrap();
        assert_eq!(cl.len(), 1);
        let later = t0() + CD::hours(2);
        let rec = recover(&s, later).unwrap();
        assert_eq!(rec.recovered_leases, vec![id.clone()]);
        assert_eq!(rec.overdue, 1);
        assert!(s.events(&id).unwrap().iter().any(|e| e.kind == "lease_recovered"));
        let rep = tick(&s, &registry(vec![Observation::pending("p", "x")]), later, "w2", 8).unwrap();
        assert_eq!(rep.observed, 1);
    }

    #[test]
    fn missing_adapter_is_an_unknown_not_a_crash() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let rep = tick(&s, &AdapterRegistry::new(), t0(), "w", 8).unwrap();
        assert_eq!((rep.observed, rep.unknown), (1, 1));
        let obs = s.observations(&id, 1).unwrap();
        assert!(obs[0].adapter_error.as_deref().unwrap().contains("no adapter"), "{obs:?}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor scheduler::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! The check loop (spec §排程, §daemon 恢復). `plan_cycle` is the entire
//! state machine as a pure function of (row, observation, now) — every rule
//! about backoff, deadlines, terminal settlement and health lives there and
//! is table-testable. `tick` is the thin I/O wrapper the daemon calls.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::adapter::{AdapterRegistry, Observation, SourceAdapter};
use crate::backoff::{
    clamp_recommended, pending_delay, seed, unknown_delay, with_jitter, RETAIN_INTERVAL,
    UNHEALTHY_AFTER_UNKNOWN,
};
use crate::deadline;
use crate::state::{MonitorState, Outcome};
use crate::store::{CycleUpdate, Event, ListFilter, MonitorRow, MonitorStore};

/// Longer than any single `observe` may take (GitHub client timeout is 30 s,
/// the others are local reads), so no heartbeat is needed in this plan.
pub const DEFAULT_LEASE: Duration = Duration::from_secs(120);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub claimed: usize,
    pub observed: usize,
    pub completed: usize,
    pub action_pending: usize,
    pub exhausted: usize,
    pub unknown: usize,
    pub stale_fence: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub recovered_leases: Vec<String>,
    pub overdue: usize,
}

fn ev(kind: &'static str, payload: serde_json::Value) -> Event {
    Event { kind, payload, dedup: true }
}

fn plus(now: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

pub fn plan_cycle(row: &MonitorRow, obs: Observation, now: DateTime<Utc>) -> CycleUpdate {
    let policy = &row.spec.policy;
    let mut events = Vec::new();
    let mut u = CycleUpdate {
        outcome: obs.outcome,
        observation: obs.clone(),
        observed_at: now,
        new_state: MonitorState::Sleeping,
        next_check_at: now,
        pending_attempts: row.pending_attempts,
        unknown_streak: row.unknown_streak,
        last_progress_at: row.last_progress_at,
        progress_token: row.progress_token.clone(),
        stalled_since: row.stalled_since,
        soft_notified: row.soft_notified,
        hard_reached: row.hard_reached,
        finish_cycle: false,
        events: Vec::new(),
    };

    if obs.outcome.is_terminal() {
        // Settlement. With no actions to run the monitor is done; with some,
        // it parks for the executor (plan-2). Either way it is never claimed
        // again, which is what makes the terminal side-effect fire once.
        u.unknown_streak = 0;
        let actions = match obs.outcome {
            Outcome::Succeeded => &row.spec.actions.on_success,
            _ => &row.spec.actions.on_failure,
        };
        u.new_state = if actions.is_empty() {
            MonitorState::Completed
        } else {
            MonitorState::ActionPending
        };
        u.finish_cycle = true;
        events.push(ev(
            "terminal",
            serde_json::json!({ "outcome": obs.outcome.as_str(), "evidence": obs.evidence }),
        ));
        u.events = events;
        return u;
    }

    let (progress_at, token) = deadline::advance_progress(
        row.last_progress_at,
        row.progress_token.as_deref(),
        obs.progress_token.as_deref(),
        now,
    );
    u.last_progress_at = progress_at;
    u.progress_token = token;
    let v = deadline::evaluate(
        policy,
        row.work_started_at,
        progress_at,
        row.stalled_since,
        row.soft_notified,
        row.hard_reached,
        now,
    );
    u.stalled_since = v.stalled_since;
    u.soft_notified = v.soft_notified;
    u.hard_reached = v.hard_reached;
    // dedup is per (monitor, cycle, kind): a second stall inside the same
    // cycle after a recovery is not re-announced. Acceptable for MVP; a
    // per-stall dedup key is the upgrade if that proves noisy in practice.
    if v.stalled_newly {
        events.push(ev("stalled", serde_json::json!({ "since": v.stalled_since })));
    }
    if v.recovered {
        events.push(ev("stalled_recovered", serde_json::json!({ "at": now })));
    }
    if v.soft_newly {
        events.push(ev("soft_deadline", serde_json::json!({ "at": now })));
    }
    if v.hard_newly {
        events.push(ev("hard_deadline", serde_json::json!({ "at": now })));
    }

    let (base, attempt_seed) = if obs.outcome == Outcome::Unknown {
        u.unknown_streak = row.unknown_streak + 1;
        if u.unknown_streak == UNHEALTHY_AFTER_UNKNOWN {
            events.push(ev(
                "monitor_unhealthy",
                serde_json::json!({ "streak": u.unknown_streak, "error": obs.adapter_error }),
            ));
        }
        (unknown_delay(u.unknown_streak - 1), u.unknown_streak)
    } else {
        u.unknown_streak = 0;
        u.pending_attempts = row.pending_attempts + 1;
        (pending_delay(row.pending_attempts), u.pending_attempts)
    };

    let base = if v.hard_reached && obs.outcome == Outcome::Pending {
        if policy.retain_monitoring_after_hard_deadline {
            RETAIN_INTERVAL
        } else {
            u.new_state = MonitorState::Exhausted;
            events.push(ev("exhausted", serde_json::json!({ "reason": "hard deadline, monitoring not retained" })));
            base
        }
    } else {
        base
    };

    let delay = clamp_recommended(obs.recommended_poll_after, with_jitter(base, seed(&row.id, attempt_seed)));
    u.next_check_at = plus(now, delay);
    u.events = events;
    u
}

pub fn tick(
    store: &MonitorStore,
    registry: &AdapterRegistry,
    now: DateTime<Utc>,
    owner: &str,
    max_claims: usize,
) -> Result<TickReport> {
    let claimed = store.claim_due(now, owner, DEFAULT_LEASE, max_claims)?;
    let mut rep = TickReport { claimed: claimed.len(), ..Default::default() };
    for c in claimed {
        let obs = match registry.get(c.row.source_type) {
            Some(a) => a.observe(&c.row.reference, c.row.spec.source.credential_ref.as_deref()),
            None => Observation::unknown(format!(
                "no adapter enabled for {}",
                c.row.source_type.as_str()
            )),
        }
        .redacted();
        let u = plan_cycle(&c.row, obs, now);
        match u.new_state {
            MonitorState::Completed => rep.completed += 1,
            MonitorState::ActionPending => rep.action_pending += 1,
            MonitorState::Exhausted => rep.exhausted += 1,
            _ => {}
        }
        if u.outcome == Outcome::Unknown {
            rep.unknown += 1;
        }
        if store.apply_cycle(&c.row.id, c.fence, &u)? {
            rep.observed += 1;
        } else {
            rep.stale_fence += 1;
            tracing::warn!(monitor = %c.row.id, fence = c.fence, "monitor: stale fence, cycle dropped");
        }
    }
    Ok(rep)
}

/// Daemon start (spec §daemon 恢復 steps 2 and 4). Step 3 (outbox) and
/// step 5 (claimed actions) are plan-2.
pub fn recover(store: &MonitorStore, now: DateTime<Utc>) -> Result<RecoveryReport> {
    let recovered_leases = store.expire_leases(now)?;
    let overdue = store
        .list(&ListFilter::default())?
        .iter()
        .filter(|r| r.state.is_claimable() && r.next_check_at <= now)
        .count();
    Ok(RecoveryReport { recovered_leases, overdue })
}

// keep the trait import used when no adapter is registered in a build
#[allow(dead_code)]
fn _assert_object_safe(_: &dyn SourceAdapter) {}
```
(If clippy flags `_assert_object_safe`, delete it and the `SourceAdapter` import — it exists only to keep the import honest while writing.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor`
Expected: every mur-monitor test PASS (scheduler: 10).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor
git commit -m "feat(monitor): scheduler — pure plan_cycle state machine, tick, recover"
```

---

### Task 9: MUR-run adapter + registry skeleton (mur-core)

**Files:**
- Modify: `mur-core/Cargo.toml` (add `mur-monitor = { path = "../mur-monitor" }` under `[dependencies]`)
- Modify: `mur-core/src/lib.rs` (add `pub mod monitor;` next to `pub mod run_status;`)
- Create: `mur-core/src/monitor/mod.rs`
- Create: `mur-core/src/monitor/adapters/mod.rs`
- Create: `mur-core/src/monitor/adapters/mur_run.rs`
- Test: `mur-core/src/monitor/adapters/mur_run.rs`

**Interfaces:**
- Consumes: `crate::run_status::{status_of, valid_run_id, RunStatus, State, Liveness, RunState, RunKind, store::save}`, `mur_monitor::adapter::{Observation, SourceAdapter, AdapterRegistry}`, `mur_monitor::spec::SourceType`, `mur_monitor::state::Outcome`
- Produces: `mur_core::monitor::registry(mur_home: &Path) -> AdapterRegistry`, `MurRunAdapter::new(mur_home: &Path) -> Self`, `pub fn map(s: RunStatus) -> Observation`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{Liveness, RunKind, RunState, RunStatus, State};
    use chrono::{TimeZone, Utc};

    fn run(state: State, beat: Option<chrono::DateTime<Utc>>) -> RunState {
        RunState {
            schema: 1,
            run_id: "run-1".into(),
            channel_id: None,
            kind: RunKind::Fleet,
            label: "deep-research".into(),
            pid: 0,
            started_at: Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap(),
            last_heartbeat_at: beat,
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: String::new(),
            build_sha: String::new(),
        }
    }
    fn status(state: State, liveness: Liveness, beat: Option<chrono::DateTime<Utc>>) -> RunStatus {
        RunStatus { state, liveness, run: run(state, beat) }
    }

    #[test]
    fn terminal_states_map_to_terminal_outcomes() {
        assert_eq!(map(status(State::Done, Liveness::NotApplicable, None)).outcome, Outcome::Succeeded);
        assert_eq!(map(status(State::Failed, Liveness::NotApplicable, None)).outcome, Outcome::Failed);
        assert_eq!(map(status(State::Stopped, Liveness::NotApplicable, None)).outcome, Outcome::Cancelled);
    }

    #[test]
    fn running_is_pending_with_the_heartbeat_as_progress() {
        let b1 = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 10).unwrap();
        let b2 = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 20).unwrap();
        let o1 = map(status(State::Running, Liveness::Alive, Some(b1)));
        let o2 = map(status(State::Running, Liveness::Alive, Some(b2)));
        assert_eq!(o1.outcome, Outcome::Pending);
        assert_ne!(o1.progress_token, o2.progress_token, "a new heartbeat is progress");
        assert_eq!(o1.progress_token, map(status(State::Running, Liveness::Stalled, Some(b1))).progress_token, "stalled is the deadline evaluator's call, not the adapter's");
        assert_eq!(map(status(State::Blocked, Liveness::Alive, Some(b1))).outcome, Outcome::Pending);
    }

    #[test]
    fn dead_process_without_terminal_record_is_unknown_not_failed() {
        let o = map(status(State::Running, Liveness::Dead, Some(Utc::now())));
        assert_eq!(o.outcome, Outcome::Unknown);
        assert!(o.adapter_error.is_some());
        assert_eq!(map(status(State::Running, Liveness::Unknown, None)).outcome, Outcome::Pending, "a rebuilt record with no heartbeat is still running as far as we know");
    }

    #[test]
    fn observe_reads_the_run_record_and_missing_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        let a = MurRunAdapter::new(d.path());
        assert_eq!(a.observe("nope", None).outcome, Outcome::Unknown);
        crate::run_status::store::save(d.path(), &run(State::Done, None)).unwrap();
        assert_eq!(a.observe("run-1", None).outcome, Outcome::Succeeded);
        assert!(a.validate_reference("run-1").is_ok());
        assert!(a.validate_reference("bad id!").is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core monitor::adapters::mur_run`
Expected: compile error (module missing).

- [ ] **Step 3: Implement**

`mur-core/src/monitor/mod.rs`:
```rust
//! Durable-monitor adapters and the daemon-facing service. The engine
//! itself is `mur-monitor`; this module is what needs `mur-core` (run
//! records) or a network client, and what assembles the registry.

pub mod adapters;

use std::path::Path;

use mur_monitor::adapter::AdapterRegistry;

/// Every adapter this build ships. Task 10 and 11 add theirs here.
pub fn registry(mur_home: &Path) -> AdapterRegistry {
    let mut r = AdapterRegistry::new();
    r.register(Box::new(adapters::mur_run::MurRunAdapter::new(mur_home)));
    r
}
```

`mur-core/src/monitor/adapters/mod.rs`:
```rust
pub mod mur_run;
```

`mur-core/src/monitor/adapters/mur_run.rs`:
```rust
//! `source.type: mur_run` — reads the unified run status by `run_id`
//! (spec §MVP Adapter → MUR run). Query only: a monitor NEVER re-dispatches
//! the run; a tool that stopped waiting is not evidence the run stopped.

use std::path::{Path, PathBuf};

use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::spec::SourceType;
use mur_monitor::state::Outcome;

use crate::run_status::{self, Liveness, RunStatus, State};

pub struct MurRunAdapter {
    mur_home: PathBuf,
}

impl MurRunAdapter {
    pub fn new(mur_home: &Path) -> Self {
        Self { mur_home: mur_home.to_path_buf() }
    }
}

/// Pure mapping from a classified run to an observation.
pub fn map(s: RunStatus) -> Observation {
    match s.state {
        State::Done => Observation::terminal(Outcome::Succeeded, "run state: done"),
        State::Failed => Observation::terminal(Outcome::Failed, "run state: failed"),
        State::Stopped => Observation::terminal(Outcome::Cancelled, "run state: stopped"),
        State::Running | State::Blocked => match s.liveness {
            // The process is gone and nothing wrote a terminal state: we do
            // not know what happened, and saying `failed` would be a guess.
            Liveness::Dead => Observation::unknown(
                "process is dead but no terminal state was recorded",
            ),
            _ => {
                let beat = s
                    .run
                    .last_heartbeat_at
                    .map(|b| b.to_rfc3339())
                    .unwrap_or_else(|| "-".into());
                Observation::pending(
                    format!("{:?}:{beat}:{}", s.state, s.run.steps.len()),
                    format!("run state: {:?}, liveness: {:?}, steps: {}", s.state, s.liveness, s.run.steps.len()),
                )
            }
        },
    }
}

impl SourceAdapter for MurRunAdapter {
    fn source_type(&self) -> SourceType {
        SourceType::MurRun
    }

    fn validate_reference(&self, reference: &str) -> Result<(), String> {
        if run_status::valid_run_id(reference) {
            Ok(())
        } else {
            Err("run id: letters, digits, `-` and `_` only, at most 96 chars".into())
        }
    }

    fn observe(&self, reference: &str, _credential_ref: Option<&str>) -> Observation {
        match run_status::status_of(&self.mur_home, reference) {
            Ok(Some(s)) => map(s),
            Ok(None) => Observation::unknown("no run record: not started yet, or recorded on another host"),
            Err(e) => Observation::unknown(format!("run record unreadable: {e:#}")),
        }
        .redacted()
    }
}
```
If `State`/`Liveness` do not derive `Debug` for the `{:?}` formats, add `as_str` helpers locally rather than editing `run_status` (out of scope).

- [ ] **Step 4: Run to verify it passes**

Run: same command as Step 2.
Expected: 4 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/Cargo.toml Cargo.lock mur-core/src/lib.rs mur-core/src/monitor
git commit -m "feat(monitor): mur_run adapter over the unified run status"
```

---

### Task 10: GitHub Actions adapter (read-only)

**Files:**
- Create: `mur-core/src/monitor/adapters/github_actions.rs`
- Modify: `mur-core/src/monitor/adapters/mod.rs` (add `pub mod github_actions;`)
- Modify: `mur-core/src/monitor/mod.rs` (register `GithubActionsAdapter::default()`)
- Test: `mur-core/src/monitor/adapters/github_actions.rs`

**Interfaces:**
- Consumes: `reqwest::blocking`, `mur_common::secret::SecretRef` (`from_str`, `resolve_to_string_blocking() -> Option<String>`)
- Produces: `GithubActionsAdapter { api_base: String, timeout: Duration }` (+ `Default`), `pub fn parse_reference(r: &str) -> Result<(String, String, u64), String>`, `pub fn classify(status: u16, body: &str, retry_after_secs: Option<u64>) -> Observation`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn run_json(status: &str, conclusion: Option<&str>, updated: &str, attempt: u32) -> String {
        serde_json::json!({
            "status": status,
            "conclusion": conclusion,
            "updated_at": updated,
            "run_attempt": attempt,
        })
        .to_string()
    }

    #[test]
    fn reference_is_owner_repo_run_id() {
        assert_eq!(parse_reference("mur-run/mur/123").unwrap(), ("mur-run".into(), "mur".into(), 123));
        assert!(parse_reference("mur-run/mur").is_err());
        assert!(parse_reference("mur-run/mur/abc").is_err());
        assert!(parse_reference("a/b/1/extra").is_err());
    }

    #[test]
    fn completed_conclusions_map_to_terminal() {
        assert_eq!(classify(200, &run_json("completed", Some("success"), "t", 1), None).outcome, Outcome::Succeeded);
        assert_eq!(classify(200, &run_json("completed", Some("failure"), "t", 1), None).outcome, Outcome::Failed);
        assert_eq!(classify(200, &run_json("completed", Some("timed_out"), "t", 1), None).outcome, Outcome::Failed);
        assert_eq!(classify(200, &run_json("completed", Some("cancelled"), "t", 1), None).outcome, Outcome::Cancelled);
        assert_eq!(classify(200, &run_json("completed", Some("mystery"), "t", 1), None).outcome, Outcome::Unknown);
        assert_eq!(classify(200, &run_json("completed", None, "t", 1), None).outcome, Outcome::Unknown);
    }

    #[test]
    fn in_progress_is_pending_and_updated_at_or_attempt_is_progress() {
        let a = classify(200, &run_json("in_progress", None, "2026-09-15T12:00:00Z", 1), None);
        let b = classify(200, &run_json("in_progress", None, "2026-09-15T12:05:00Z", 1), None);
        let c = classify(200, &run_json("queued", None, "2026-09-15T12:05:00Z", 2), None);
        assert_eq!(a.outcome, Outcome::Pending);
        assert_ne!(a.progress_token, b.progress_token);
        assert_ne!(b.progress_token, c.progress_token);
    }

    #[test]
    fn every_non_answer_is_unknown_never_failed() {
        for (status, body) in [
            (404, "{\"message\":\"Not Found\"}"),
            (401, "{\"message\":\"Bad credentials\"}"),
            (403, "{\"message\":\"API rate limit exceeded\"}"),
            (429, ""),
            (500, ""),
            (502, "<html>"),
            (200, "not json"),
            (200, "{\"status\":\"completed\"}"),
        ] {
            let o = classify(status, body, None);
            assert_eq!(o.outcome, Outcome::Unknown, "http {status}: {body}");
            assert!(o.adapter_error.is_some(), "http {status}");
        }
        assert!(classify(401, "", None).adapter_error.unwrap().contains("credential"));
        assert!(classify(404, "", None).adapter_error.unwrap().contains("not proven"));
    }

    #[test]
    fn rate_limit_recommends_retry_after_or_the_default() {
        assert_eq!(classify(429, "", Some(120)).recommended_poll_after, Some(Duration::from_secs(120)));
        assert_eq!(
            classify(403, "API rate limit exceeded", None).recommended_poll_after,
            Some(Duration::from_secs(RATE_LIMIT_DEFAULT_SECS))
        );
        assert_eq!(classify(403, "Resource not accessible by integration", None).recommended_poll_after, None);
    }

    #[test]
    fn evidence_is_truncated_and_redacted() {
        let body = format!("{{\"message\":\"token ghp_{} rejected\"}}", "A".repeat(36));
        let o = classify(401, &body, None);
        assert!(o.evidence.len() <= EVIDENCE_MAX_CHARS + 32, "{}", o.evidence);
        assert!(!o.evidence.contains(&"A".repeat(36)), "{}", o.evidence);
    }
}
```
As in Task 3: if `redact_secrets` does not match the `ghp_` shape, swap the test body for a shape it does match (see `mur-common/src/redact.rs`); the assertion is that the chokepoint ran.

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core monitor::adapters::github_actions`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! `source.type: github_actions` — one GET per check against
//! `/repos/{owner}/{repo}/actions/runs/{run_id}` (spec §MVP Adapter →
//! GitHub Actions). Read-only: rerun and log download are plan-2 actions.
//! `classify` is pure over (status, body) so every fixture in the spec's
//! adapter contract tests runs without a network.

use std::str::FromStr;
use std::time::Duration;

use mur_common::secret::SecretRef;
use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::spec::SourceType;
use mur_monitor::state::Outcome;

const DEFAULT_API_BASE: &str = "https://api.github.com";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Used when GitHub rate-limits without a `Retry-After` header.
pub const RATE_LIMIT_DEFAULT_SECS: u64 = 60;
/// Body bytes kept as evidence — enough to read an error, never a log dump.
pub const EVIDENCE_MAX_CHARS: usize = 160;
const USER_AGENT: &str = concat!("mur/", env!("CARGO_PKG_VERSION"));

pub struct GithubActionsAdapter {
    pub api_base: String,
    pub timeout: Duration,
}

impl Default for GithubActionsAdapter {
    fn default() -> Self {
        Self { api_base: DEFAULT_API_BASE.into(), timeout: DEFAULT_TIMEOUT }
    }
}

pub fn parse_reference(r: &str) -> Result<(String, String, u64), String> {
    let parts: Vec<&str> = r.split('/').collect();
    let [owner, repo, run] = parts.as_slice() else {
        return Err("reference must be owner/repo/run_id".into());
    };
    if owner.is_empty() || repo.is_empty() {
        return Err("owner and repo must not be empty".into());
    }
    let run_id = run.parse::<u64>().map_err(|_| "run_id must be a number".to_string())?;
    Ok((owner.to_string(), repo.to_string(), run_id))
}

fn snippet(body: &str) -> String {
    body.chars().take(EVIDENCE_MAX_CHARS).collect()
}

pub fn classify(status: u16, body: &str, retry_after_secs: Option<u64>) -> Observation {
    let obs = match status {
        200 => classify_ok(body),
        401 => Observation::unknown(format!("credential rejected (401): {}", snippet(body))),
        403 if body.to_ascii_lowercase().contains("rate limit") => {
            Observation::unknown("rate limited (403)")
                .with_poll_after(Duration::from_secs(retry_after_secs.unwrap_or(RATE_LIMIT_DEFAULT_SECS)))
        }
        403 => Observation::unknown(format!("forbidden (403) — token may lack actions:read: {}", snippet(body))),
        404 => Observation::unknown("not found (404): eventual consistency, permissions, or deleted — not proven"),
        429 => Observation::unknown("rate limited (429)")
            .with_poll_after(Duration::from_secs(retry_after_secs.unwrap_or(RATE_LIMIT_DEFAULT_SECS))),
        s if s >= 500 => Observation::unknown(format!("github {s}: {}", snippet(body))),
        s => Observation::unknown(format!("unexpected http {s}: {}", snippet(body))),
    };
    obs.redacted()
}

fn classify_ok(body: &str) -> Observation {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return Observation::unknown(format!("malformed response: {e}; {}", snippet(body))),
    };
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let attempt = v.get("run_attempt").and_then(|a| a.as_u64()).unwrap_or(0);
    let updated = v.get("updated_at").and_then(|u| u.as_str()).unwrap_or("");
    match status {
        "completed" => match v.get("conclusion").and_then(|c| c.as_str()) {
            Some("success") => Observation::terminal(Outcome::Succeeded, format!("success (attempt {attempt})")),
            Some(c @ ("failure" | "timed_out" | "startup_failure")) => {
                Observation::terminal(Outcome::Failed, format!("{c} (attempt {attempt})"))
            }
            Some("cancelled") => Observation::terminal(Outcome::Cancelled, format!("cancelled (attempt {attempt})")),
            Some(other) => Observation::unknown(format!("completed with unrecognised conclusion `{other}`")),
            None => Observation::unknown("completed but no conclusion in the response"),
        },
        "queued" | "in_progress" | "waiting" | "requested" | "pending" => Observation::pending(
            format!("{status}:{updated}:{attempt}"),
            format!("{status} (attempt {attempt}, updated {updated})"),
        ),
        other => Observation::unknown(format!("unrecognised run status `{other}`")),
    }
}

impl GithubActionsAdapter {
    fn fetch(&self, owner: &str, repo: &str, run_id: u64, token: Option<&str>) -> Observation {
        let client = match reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(self.timeout)
            .build()
        {
            Ok(c) => c,
            Err(e) => return Observation::unknown(format!("http client: {e}")),
        };
        let url = format!("{}/repos/{owner}/{repo}/actions/runs/{run_id}", self.api_base);
        let mut req = client.get(url).header("Accept", "application/vnd.github+json");
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        match req.send() {
            Err(e) => Observation::unknown(format!("request failed: {e}")),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok());
                let body = resp.text().unwrap_or_default();
                classify(status, &body, retry_after)
            }
        }
    }
}

impl SourceAdapter for GithubActionsAdapter {
    fn source_type(&self) -> SourceType {
        SourceType::GithubActions
    }

    fn validate_reference(&self, reference: &str) -> Result<(), String> {
        parse_reference(reference).map(|_| ())
    }

    fn observe(&self, reference: &str, credential_ref: Option<&str>) -> Observation {
        let (owner, repo, run_id) = match parse_reference(reference) {
            Ok(p) => p,
            Err(e) => return Observation::unknown(e),
        };
        // A configured credential that cannot be resolved pauses the query
        // (spec §錯誤處理: credential 失效) — we do not fall back to
        // unauthenticated and quietly get a different answer.
        let token = match credential_ref {
            None => None,
            Some(c) => match SecretRef::from_str(c).ok().and_then(|r| r.resolve_to_string_blocking()) {
                Some(t) => Some(t),
                None => {
                    return Observation::unknown(format!(
                        "credential_ref `{c}` could not be resolved — update the reference"
                    ))
                    .redacted();
                }
            },
        };
        self.fetch(&owner, &repo, run_id, token.as_deref())
    }
}
```

`mur-core/src/monitor/mod.rs` `registry`: add `r.register(Box::new(adapters::github_actions::GithubActionsAdapter::default()));`.

- [ ] **Step 4: Run to verify it passes**

Run: same as Step 2.
Expected: 6 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/monitor
git commit -m "feat(monitor): read-only GitHub Actions adapter with pure classify"
```

---

### Task 11: Subprocess adapter (Codex / Claude Code) over a process record

**Files:**
- Create: `mur-core/src/monitor/adapters/subprocess.rs`
- Modify: `mur-core/src/monitor/adapters/mod.rs` (add `pub mod subprocess;`)
- Modify: `mur-core/src/monitor/mod.rs` (register for `SourceType::Codex` and `SourceType::ClaudeCode`)
- Test: `mur-core/src/monitor/adapters/subprocess.rs`

**Interfaces:**
- Consumes: `mur_common::lock_file::pid_alive(u32) -> bool`
- Produces:
  ```rust
  pub struct ProcessRecord { pub pid: u32, pub started_at: DateTime<Utc>, pub log_path: PathBuf, pub exit_path: PathBuf }
  pub fn procs_dir(mur_home: &Path) -> PathBuf;                                  // <mur_home>/monitor/procs
  pub fn write_record(mur_home: &Path, id: &str, rec: &ProcessRecord) -> anyhow::Result<()>;
  pub fn load_record(mur_home: &Path, id: &str) -> anyhow::Result<Option<ProcessRecord>>;
  pub fn observe_record(rec: &ProcessRecord) -> Observation;                     // pure over the filesystem
  pub struct SubprocessAdapter; impl SubprocessAdapter { pub fn new(mur_home: &Path, kind: SourceType) -> Self }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn rec(dir: &Path, pid: u32) -> ProcessRecord {
        ProcessRecord {
            pid,
            started_at: Utc::now(),
            log_path: dir.join("out.log"),
            exit_path: dir.join("exit"),
        }
    }

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().expect("spawn true");
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn alive_process_is_pending_and_log_growth_is_progress() {
        let d = tempfile::tempdir().unwrap();
        let r = rec(d.path(), std::process::id());
        std::fs::write(&r.log_path, "abc").unwrap();
        let a = observe_record(&r);
        std::fs::write(&r.log_path, "abcdef").unwrap();
        let b = observe_record(&r);
        assert_eq!(a.outcome, Outcome::Pending);
        assert_ne!(a.progress_token, b.progress_token);
        assert_eq!(observe_record(&r).progress_token, b.progress_token, "no new output, no progress");
    }

    #[test]
    fn exit_record_decides_terminal_outcome() {
        let d = tempfile::tempdir().unwrap();
        let r = rec(d.path(), dead_pid());
        std::fs::write(&r.exit_path, "0\n").unwrap();
        assert_eq!(observe_record(&r).outcome, Outcome::Succeeded);
        std::fs::write(&r.exit_path, "3").unwrap();
        assert_eq!(observe_record(&r).outcome, Outcome::Failed);
        std::fs::write(&r.exit_path, "garbage").unwrap();
        assert_eq!(observe_record(&r).outcome, Outcome::Unknown);
    }

    #[test]
    fn gone_without_exit_record_is_unknown_not_failed() {
        let d = tempfile::tempdir().unwrap();
        let o = observe_record(&rec(d.path(), dead_pid()));
        assert_eq!(o.outcome, Outcome::Unknown);
        assert!(o.adapter_error.unwrap().contains("no exit record"));
    }

    #[test]
    fn record_round_trips_and_missing_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        let a = SubprocessAdapter::new(d.path(), SourceType::Codex);
        assert_eq!(a.source_type(), SourceType::Codex);
        assert_eq!(a.observe("sess-1", None).outcome, Outcome::Unknown);
        let r = rec(d.path(), std::process::id());
        write_record(d.path(), "sess-1", &r).unwrap();
        assert_eq!(load_record(d.path(), "sess-1").unwrap().unwrap().pid, r.pid);
        assert_eq!(a.observe("sess-1", None).outcome, Outcome::Pending);
        assert!(a.validate_reference("sess-1").is_ok());
        assert!(a.validate_reference("../etc").is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core monitor::adapters::subprocess`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! `source.type: codex | claude_code` — an agentic subprocess the launcher
//! registered as `<mur_home>/monitor/procs/<id>.json`. The record id is the
//! durable identity; the pid inside it is just one field (spec §MVP Adapter
//! → Codex／Claude Code: "不以 PID 單獨作 durable identity"). No launcher
//! writes these yet (plan-2 wires them); `write_record` is the contract.
//!
//! Liveness reads: exit file → terminal; pid alive → pending with the log
//! length as the progress token; pid gone with no exit file → `unknown`.
//! Losing the stdout pipe is never a failure verdict.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::spec::SourceType;
use mur_monitor::state::Outcome;
use serde::{Deserialize, Serialize};

const MAX_ID_LEN: usize = 96;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub log_path: PathBuf,
    /// Written by the launcher's wait loop with the exit code, once.
    pub exit_path: PathBuf,
}

pub fn procs_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("monitor").join("procs")
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Temp file + rename, like every other MUR YAML/JSON write.
pub fn write_record(mur_home: &Path, id: &str, rec: &ProcessRecord) -> Result<()> {
    if !valid_id(id) {
        anyhow::bail!("process record id: letters, digits, `-` and `_` only, at most {MAX_ID_LEN} chars");
    }
    let dir = procs_dir(mur_home);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{id}.json"));
    let tmp = dir.join(format!(".{id}.json.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(rec)?)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

pub fn load_record(mur_home: &Path, id: &str) -> Result<Option<ProcessRecord>> {
    if !valid_id(id) {
        return Ok(None);
    }
    let path = procs_dir(mur_home).join(format!("{id}.json"));
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn observe_record(rec: &ProcessRecord) -> Observation {
    if let Ok(raw) = std::fs::read_to_string(&rec.exit_path) {
        return match raw.trim().parse::<i32>() {
            Ok(0) => Observation::terminal(Outcome::Succeeded, "exit 0"),
            Ok(n) => Observation::terminal(Outcome::Failed, format!("exit {n}")),
            Err(_) => Observation::unknown(format!("exit record is not a code: {}", raw.trim())),
        };
    }
    if mur_common::lock_file::pid_alive(rec.pid) {
        let log_len = std::fs::metadata(&rec.log_path).map(|m| m.len()).unwrap_or(0);
        return Observation::pending(
            format!("log:{log_len}"),
            format!("pid {} alive, {log_len} bytes of output", rec.pid),
        );
    }
    Observation::unknown(format!("pid {} is gone and there is no exit record", rec.pid))
}

pub struct SubprocessAdapter {
    mur_home: PathBuf,
    kind: SourceType,
}

impl SubprocessAdapter {
    pub fn new(mur_home: &Path, kind: SourceType) -> Self {
        Self { mur_home: mur_home.to_path_buf(), kind }
    }
}

impl SourceAdapter for SubprocessAdapter {
    fn source_type(&self) -> SourceType {
        self.kind
    }

    fn validate_reference(&self, reference: &str) -> Result<(), String> {
        if valid_id(reference) {
            Ok(())
        } else {
            Err(format!("process record id: letters, digits, `-` and `_` only, at most {MAX_ID_LEN} chars"))
        }
    }

    fn observe(&self, reference: &str, _credential_ref: Option<&str>) -> Observation {
        match load_record(&self.mur_home, reference) {
            Ok(Some(rec)) => observe_record(&rec),
            Ok(None) => Observation::unknown(format!("no process record `{reference}` under {}", procs_dir(&self.mur_home).display())),
            Err(e) => Observation::unknown(format!("process record unreadable: {e:#}")),
        }
        .redacted()
    }
}
```

`registry`: add
```rust
    r.register(Box::new(adapters::subprocess::SubprocessAdapter::new(mur_home, SourceType::Codex)));
    r.register(Box::new(adapters::subprocess::SubprocessAdapter::new(mur_home, SourceType::ClaudeCode)));
```
with `use mur_monitor::spec::SourceType;` at the top of `monitor/mod.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: same as Step 2.
Expected: 4 PASS. (`pid_alive` on Windows uses `OpenProcess`; the tests use real pids so they hold there too.)

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/monitor
git commit -m "feat(monitor): subprocess adapter over a launcher process record"
```

---

### Task 12: Service — `tick_once` / `recover` and the offline catch-up proof

**Files:**
- Create: `mur-core/src/monitor/service.rs`
- Modify: `mur-core/src/monitor/mod.rs` (add `pub mod service;`)
- Test: `mur-core/src/monitor/service.rs`

**Interfaces:**
- Consumes: `registry(mur_home)`, `mur_monitor::store::MonitorStore`, `mur_monitor::scheduler::{tick, recover, TickReport, RecoveryReport, DEFAULT_LEASE}`
- Produces:
  ```rust
  pub const TICK_INTERVAL: Duration = Duration::from_secs(15);
  pub const TICK_MAX_CLAIMS: usize = 8;
  pub fn tick_once(mur_home: &Path, now: DateTime<Utc>, owner: &str) -> anyhow::Result<TickReport>;
  pub fn recover(mur_home: &Path, now: DateTime<Utc>) -> anyhow::Result<RecoveryReport>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{store as run_store, RunKind, RunState, State};
    use chrono::{Duration as CD, TimeZone, Utc};
    use mur_monitor::spec::MonitorSpec;
    use mur_monitor::state::{MonitorState, Outcome};
    use mur_monitor::store::MonitorStore;

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    fn run(state: State, beat: chrono::DateTime<Utc>) -> RunState {
        RunState {
            schema: 1,
            run_id: "run-1".into(),
            channel_id: None,
            kind: RunKind::Fleet,
            label: "x".into(),
            pid: std::process::id(),
            started_at: t0(),
            last_heartbeat_at: Some(beat),
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: String::new(),
            build_sha: String::new(),
        }
    }

    fn spec() -> MonitorSpec {
        MonitorSpec::from_yaml(
            "schema_version: 1\nname: e2e\nsource: { type: mur_run, reference: run-1 }\nidempotency_key: e2e\ncreated_by: { actor: user:test }\n",
        )
        .unwrap()
    }

    #[test]
    fn tick_interval_is_well_inside_the_lease() {
        assert!(TICK_INTERVAL * 4 < mur_monitor::scheduler::DEFAULT_LEASE);
    }

    #[test]
    fn end_to_end_mur_run_pending_then_done_settles_once() {
        let d = tempfile::tempdir().unwrap();
        run_store::save(d.path(), &run(State::Running, t0())).unwrap();
        let id = MonitorStore::open(d.path()).unwrap().create(&spec(), t0(), None).unwrap().id;

        let r1 = tick_once(d.path(), t0(), "t").unwrap();
        assert_eq!((r1.claimed, r1.observed), (1, 1));
        let row = MonitorStore::open(d.path()).unwrap().get(&id).unwrap().unwrap();
        assert_eq!((row.state, row.outcome), (MonitorState::Sleeping, Outcome::Pending));

        run_store::save(d.path(), &run(State::Done, t0())).unwrap();
        let r2 = tick_once(d.path(), row.next_check_at, "t").unwrap();
        assert_eq!(r2.completed, 1);
        let r3 = tick_once(d.path(), row.next_check_at + CD::hours(1), "t").unwrap();
        assert_eq!(r3.claimed, 0, "settled once, never re-observed");
        let s = MonitorStore::open(d.path()).unwrap();
        assert_eq!(s.events(&id).unwrap().iter().filter(|e| e.kind == "terminal").count(), 1);
    }

    #[test]
    fn daemon_offline_across_a_check_catches_up_on_restart() {
        let d = tempfile::tempdir().unwrap();
        run_store::save(d.path(), &run(State::Running, t0())).unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec(), t0(), None).unwrap().id;
        // a worker claimed it and the daemon died before writing back
        s.claim_due(t0(), "old-daemon", mur_monitor::scheduler::DEFAULT_LEASE, 8).unwrap();
        drop(s);

        let restart = t0() + CD::hours(3);
        let rec = recover(d.path(), restart).unwrap();
        assert_eq!(rec.recovered_leases, vec![id.clone()]);
        assert_eq!(rec.overdue, 1);
        let r = tick_once(d.path(), restart, "new-daemon").unwrap();
        assert_eq!(r.observed, 1);
        let s = MonitorStore::open(d.path()).unwrap();
        let kinds: Vec<_> = s.events(&id).unwrap().into_iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&"lease_recovered".to_string()), "{kinds:?}");
        assert!(kinds.contains(&"stalled".to_string()), "3h with no heartbeat change is stalled: {kinds:?}");
        assert!(kinds.contains(&"soft_deadline".to_string()), "{kinds:?}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core monitor::service`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! What the daemon calls. Opens the store, builds the registry, runs one
//! scheduler pass. Kept in `mur-core` (not the daemon) so the CLI tests and
//! the daemon exercise the identical assembly — one derivation, many
//! surfaces.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_monitor::scheduler::{self, RecoveryReport, TickReport};
use mur_monitor::store::MonitorStore;

/// How often the daemon thread wakes. Well inside `DEFAULT_LEASE` so a
/// slow tick never lets its own leases expire under it.
pub const TICK_INTERVAL: Duration = Duration::from_secs(15);
/// Per-tick claim bound — the startup throttle (spec §daemon 恢復).
pub const TICK_MAX_CLAIMS: usize = 8;

pub fn tick_once(mur_home: &Path, now: DateTime<Utc>, owner: &str) -> Result<TickReport> {
    let store = MonitorStore::open(mur_home)?;
    let registry = super::registry(mur_home);
    scheduler::tick(&store, &registry, now, owner, TICK_MAX_CLAIMS)
}

pub fn recover(mur_home: &Path, now: DateTime<Utc>) -> Result<RecoveryReport> {
    let store = MonitorStore::open(mur_home)?;
    scheduler::recover(&store, now)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: same as Step 2.
Expected: 3 PASS. If `run_store::save` needs the runs directory pre-created, check `run_status/store.rs:26` — `save` should `create_dir_all`; if it does not, the test creates `run_store::runs_dir(d.path())` first.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/monitor
git commit -m "feat(monitor): service tick_once/recover with the offline catch-up proof"
```

---

### Task 13: `mur monitor add|list|show|cancel|retry` + docs

**Files:**
- Create: `mur-core/src/cmd/monitor.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod monitor;` alphabetically)
- Modify: `mur-core/src/cli/mod.rs` (`Commands::Monitor`, next to `Job` at `:137`)
- Modify: `mur-core/src/dispatch.rs` (arm next to `Commands::Job` at `:242`)
- Modify: `CLAUDE.md` (CLI Surface bullet), `README.md` (section after "Deep research, simplified")
- Test: `mur-core/src/cmd/monitor.rs`

**Interfaces:**
- Consumes: `crate::monitor::registry`, `MonitorStore`, `MonitorSpec`, `MonitorState`, `crate::paths::mur_root(None)`
- Produces: `pub enum MonitorAction`, `pub fn run(mur_home: &Path, action: MonitorAction) -> Result<()>`, `pub fn run_to(mur_home: &Path, action: MonitorAction, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{store as run_store, RunKind, RunState, State};
    use chrono::{TimeZone, Utc};
    use mur_monitor::store::{ListFilter, MonitorStore};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    fn home() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        run_store::save(
            d.path(),
            &RunState {
                schema: 1,
                run_id: "run-1".into(),
                channel_id: None,
                kind: RunKind::Fleet,
                label: "x".into(),
                pid: std::process::id(),
                started_at: t0(),
                last_heartbeat_at: Some(t0()),
                state: State::Running,
                steps: vec![],
                blocked_on: None,
                binary_version: String::new(),
                build_sha: String::new(),
            },
        )
        .unwrap();
        d
    }

    fn spec_file(d: &Path, source: &str, reference: &str) -> PathBuf {
        let p = d.join("spec.yaml");
        std::fs::write(
            &p,
            format!("schema_version: 1\nname: t\nsource: {{ type: {source}, reference: {reference} }}\nidempotency_key: k\ncreated_by: {{ actor: user:test }}\n"),
        )
        .unwrap();
        p
    }

    fn go(d: &Path, a: MonitorAction) -> Result<String> {
        let mut out = Vec::new();
        run_to(d, a, &mut out, t0())?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn add_creates_once_and_reports_the_existing_one() {
        let d = home();
        let f = spec_file(d.path(), "mur_run", "run-1");
        let first = go(d.path(), MonitorAction::Add { file: f.clone(), started_at: None }).unwrap();
        assert!(first.starts_with("monitor "), "{first}");
        assert!(first.contains("first check"), "{first}");
        let second = go(d.path(), MonitorAction::Add { file: f, started_at: None }).unwrap();
        assert!(second.contains("already exists"), "{second}");
        assert_eq!(MonitorStore::open(d.path()).unwrap().list(&ListFilter::default()).unwrap().len(), 1);
    }

    #[test]
    fn add_refuses_what_can_never_be_queried() {
        let d = home();
        let e = go(d.path(), MonitorAction::Add { file: spec_file(d.path(), "custom", "x"), started_at: None }).unwrap_err();
        assert!(e.to_string().contains("no adapter"), "{e:#}");
        let e = go(d.path(), MonitorAction::Add { file: spec_file(d.path(), "mur_run", "'bad id!'"), started_at: None }).unwrap_err();
        assert!(e.to_string().contains("reference"), "{e:#}");
    }

    #[test]
    fn list_show_cancel_retry() {
        let d = home();
        go(d.path(), MonitorAction::Add { file: spec_file(d.path(), "mur_run", "run-1"), started_at: None }).unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();

        let list = go(d.path(), MonitorAction::List { state: None, all: false }).unwrap();
        assert!(list.contains(&id[..8]) && list.contains("active"), "{list}");
        let show = go(d.path(), MonitorAction::Show { id: id.clone(), history: true }).unwrap();
        assert!(show.contains("mur_run run-1") && show.contains("created"), "{show}");

        let e = go(d.path(), MonitorAction::Retry { id: id.clone(), reset_remediation_budget: false }).unwrap_err();
        assert!(e.to_string().contains("exhausted"), "{e:#}");

        let c = go(d.path(), MonitorAction::Cancel { id: id.clone() }).unwrap();
        assert!(c.contains("NOT cancelled"), "{c}");
        assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Completed);
        assert!(go(d.path(), MonitorAction::List { state: None, all: false }).unwrap().contains("no monitors"));

        s.conn_for_test_set_state(&id, MonitorState::Exhausted);
        let r = go(d.path(), MonitorAction::Retry { id: id.clone(), reset_remediation_budget: true }).unwrap();
        assert!(r.contains("reactivated"), "{r}");
        assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Active);
    }

    #[test]
    fn bad_state_filter_lists_the_valid_ones() {
        let d = home();
        let e = go(d.path(), MonitorAction::List { state: Some("bogus".into()), all: false }).unwrap_err();
        assert!(e.to_string().contains("awaiting_approval"), "{e:#}");
    }
}
```
`conn_for_test_set_state` does not exist — replace that line with `s.set_state(&id, MonitorState::Exhausted, t0()).unwrap();` (it is public; the placeholder name is here only to make you read the line).

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core cmd::monitor`
Expected: compile error.

- [ ] **Step 3: Implement the command**

```rust
//! `mur monitor` — the user's first diagnostic surface (spec §CLI, §可觀測性).
//! Every verb is `run_to` over an explicit writer and clock so the tests
//! read what a user reads.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use mur_monitor::adapter::SourceAdapter;
use mur_monitor::spec::MonitorSpec;
use mur_monitor::state::MonitorState;
use mur_monitor::store::{ListFilter, MonitorRow, MonitorStore};

/// Observations shown by `show` without `--history`.
const SHOW_RECENT_OBSERVATIONS: usize = 5;
const ID_SHORT: usize = 8;

#[derive(Debug, Subcommand)]
pub enum MonitorAction {
    /// Validate a spec file, probe the source once, and register the monitor.
    Add {
        /// Path to a MonitorSpec YAML file.
        #[arg(long)]
        file: PathBuf,
        /// When the monitored work really started (RFC 3339). Deadlines count from here. Default: now.
        #[arg(long)]
        started_at: Option<String>,
    },
    /// List monitors that still need attention (add --all for completed ones).
    List {
        /// Only this state (active, sleeping, checking, action_pending, awaiting_approval, completed, exhausted).
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// One monitor in detail: spec, lease, recent evidence.
    Show {
        id: String,
        /// Also print the full append-only event history.
        #[arg(long)]
        history: bool,
    },
    /// Stop monitoring. Does NOT cancel the monitored work.
    Cancel { id: String },
    /// Bring an exhausted monitor back to active.
    Retry {
        id: String,
        /// Also reset the automatic remediation counter to zero.
        #[arg(long)]
        reset_remediation_budget: bool,
    },
}

pub fn run(mur_home: &Path, action: MonitorAction) -> Result<()> {
    run_to(mur_home, action, &mut std::io::stdout(), Utc::now())
}

pub fn run_to(mur_home: &Path, action: MonitorAction, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()> {
    let store = MonitorStore::open(mur_home)?;
    match action {
        MonitorAction::Add { file, started_at } => add(mur_home, &store, &file, started_at.as_deref(), out, now),
        MonitorAction::List { state, all } => list(&store, state.as_deref(), all, out, now),
        MonitorAction::Show { id, history } => show(&store, &id, history, out),
        MonitorAction::Cancel { id } => cancel(&store, &id, out, now),
        MonitorAction::Retry { id, reset_remediation_budget } => retry(&store, &id, reset_remediation_budget, out, now),
    }
}

fn add(mur_home: &Path, store: &MonitorStore, file: &Path, started_at: Option<&str>, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()> {
    let yaml = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let spec = MonitorSpec::from_yaml(&yaml)?;
    spec.validate()?;
    let registry = crate::monitor::registry(mur_home);
    let Some(adapter) = registry.get(spec.source.r#type) else {
        let enabled: Vec<_> = registry.types().iter().map(|t| t.as_str()).collect();
        bail!("no adapter enabled for source type `{}` (enabled: {})", spec.source.r#type.as_str(), enabled.join(", "));
    };
    adapter
        .validate_reference(&spec.source.reference)
        .map_err(|e| anyhow::anyhow!("source.reference `{}`: {e}", spec.source.reference))?;
    // Rule 3: one read-only query now. A permission problem must be said
    // out loud instead of becoming a monitor that can never answer.
    let probe = adapter.observe(&spec.source.reference, spec.source.credential_ref.as_deref());
    if let Some(err) = &probe.adapter_error
        && err.contains("credential")
    {
        bail!("{err} — fix the credential reference before adding; a monitor that can never query is not created");
    }
    let started = match started_at {
        Some(s) => Some(DateTime::parse_from_rfc3339(s).context("--started-at must be RFC 3339")?.with_timezone(&Utc)),
        None => None,
    };
    let c = store.create(&spec, now, started)?;
    if c.existing {
        writeln!(out, "monitor {} already exists for idempotency_key {}", c.id, spec.idempotency_key)?;
    } else {
        writeln!(
            out,
            "monitor {} · {} {} · first check {} · probe: {}",
            c.id,
            spec.source.r#type.as_str(),
            spec.source.reference,
            c.next_check_at.to_rfc3339(),
            probe.outcome.as_str()
        )?;
    }
    Ok(())
}

fn hard_deadline_at(r: &MonitorRow) -> DateTime<Utc> {
    r.work_started_at + chrono::Duration::from_std(r.spec.policy.hard_deadline()).unwrap_or(chrono::Duration::MAX)
}

fn list(store: &MonitorStore, state: Option<&str>, all: bool, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()> {
    let state = match state {
        None => None,
        Some(s) => Some(MonitorState::parse(s).ok_or_else(|| {
            let valid: Vec<_> = MonitorState::ALL.iter().map(|v| v.as_str()).collect();
            anyhow::anyhow!("unknown state `{s}` (valid: {})", valid.join(", "))
        })?),
    };
    let rows = store.list(&ListFilter { state, include_completed: all })?;
    if rows.is_empty() {
        writeln!(out, "no monitors")?;
        return Ok(());
    }
    writeln!(out, "{:<8}  {:<20}  {:<17}  {:<9}  {:>10}  {:>10}  {}", "ID", "NAME", "STATE", "OUTCOME", "PROGRESS", "NEXT", "HARD DEADLINE")?;
    for r in rows {
        writeln!(
            out,
            "{:<8}  {:<20}  {:<17}  {:<9}  {:>10}  {:>10}  {}",
            &r.id[..ID_SHORT.min(r.id.len())],
            truncate(&r.name, 20),
            r.state.as_str(),
            r.outcome.as_str(),
            ago(now, r.last_progress_at),
            until(now, r.next_check_at),
            hard_deadline_at(&r).to_rfc3339(),
        )?;
    }
    Ok(())
}

fn show(store: &MonitorStore, id: &str, history: bool, out: &mut dyn Write) -> Result<()> {
    let r = store.get(id)?.with_context(|| format!("no monitor `{id}`"))?;
    writeln!(out, "monitor {} ({})", r.id, r.name)?;
    writeln!(out, "  source:        {} {}", r.source_type.as_str(), r.reference)?;
    if let Some(c) = &r.spec.source.credential_ref {
        writeln!(out, "  credential:    {c} (reference only)")?;
    }
    writeln!(out, "  state/outcome: {} / {}", r.state.as_str(), r.outcome.as_str())?;
    writeln!(out, "  work started:  {}", r.work_started_at.to_rfc3339())?;
    writeln!(out, "  deadlines:     stalled {} · soft {} · hard {}", r.spec.policy.stalled_after, r.spec.policy.soft_deadline, r.spec.policy.hard_deadline)?;
    writeln!(out, "  last progress: {}  token: {}", r.last_progress_at.to_rfc3339(), r.progress_token.as_deref().unwrap_or("-"))?;
    writeln!(out, "  next check:    {}  (pending attempts {}, unknown streak {})", r.next_check_at.to_rfc3339(), r.pending_attempts, r.unknown_streak)?;
    match store.lease_of(id)? {
        Some(l) => writeln!(out, "  lease:         {} until {} (fence {})", l.owner, l.expires_at.to_rfc3339(), l.fence)?,
        None => writeln!(out, "  lease:         none (fence {})", r.fence)?,
    }
    writeln!(out, "  recent observations:")?;
    for o in store.observations(id, SHOW_RECENT_OBSERVATIONS)? {
        writeln!(out, "    {}  {:<9}  {}{}", o.observed_at.to_rfc3339(), o.outcome.as_str(), o.evidence, o.adapter_error.map(|e| format!("  [{e}]")).unwrap_or_default())?;
    }
    if history {
        writeln!(out, "  history:")?;
        for e in store.events(id)? {
            writeln!(out, "    {}  {:<18}  {}", e.created_at.to_rfc3339(), e.kind, e.payload)?;
        }
    }
    Ok(())
}

fn cancel(store: &MonitorStore, id: &str, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()> {
    let r = store.get(id)?.with_context(|| format!("no monitor `{id}`"))?;
    if r.state == MonitorState::Completed {
        bail!("monitor {id} is already completed");
    }
    store.set_state(id, MonitorState::Completed, now)?;
    store.append_event(id, &r.cycle_id, "cancelled", serde_json::json!({ "by": "cli" }), true, now)?;
    writeln!(out, "monitor {id} cancelled — the monitored work itself was NOT cancelled")?;
    Ok(())
}

fn retry(store: &MonitorStore, id: &str, reset_budget: bool, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()> {
    let r = store.get(id)?.with_context(|| format!("no monitor `{id}`"))?;
    if !store.reactivate(id, now, reset_budget)? {
        bail!("only an exhausted monitor can be retried (state: {})", r.state.as_str());
    }
    store.append_event(id, &r.cycle_id, "retried", serde_json::json!({ "reset_remediation_budget": reset_budget }), false, now)?;
    writeln!(out, "monitor {id} reactivated{}", if reset_budget { ", remediation budget reset" } else { "" })?;
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n - 1).chain(std::iter::once('…')).collect() }
}

fn ago(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    human(now.signed_duration_since(t)) + " ago"
}

fn until(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    if t <= now { "due".into() } else { "in ".to_string() + &human(t.signed_duration_since(now)) }
}

fn human(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    if s < 60 { format!("{s}s") } else if s < 3600 { format!("{}m", s / 60) } else { format!("{}h{}m", s / 3600, (s % 3600) / 60) }
}
```

Wiring — `mur-core/src/cli/mod.rs` after the `Job` variant:
```rust
    /// Durable monitors for asynchronous work (CI runs, MUR runs, subprocesses)
    Monitor {
        #[command(subcommand)]
        action: crate::cmd::monitor::MonitorAction,
    },
```
`mur-core/src/dispatch.rs`, mirroring the `Commands::Job` arm at `:242` (same `mur_root(None)` + `?` shape):
```rust
        Commands::Monitor { action } => {
            let mur_home = crate::paths::mur_root(None);
            cmd::monitor::run(&mur_home, action)?
        }
```
`mur-core/src/cmd/mod.rs`: `pub mod monitor;`.

- [ ] **Step 4: Run to verify it passes**

Run: same as Step 2, then `cargo run -- monitor --help` to see the five verbs.
Expected: 4 PASS; help lists add/list/show/cancel/retry.

- [ ] **Step 5: Docs**

`CLAUDE.md` → "CLI Surface (top level)", add after the `mur model` bullet:
```
- `mur monitor {add|list|show|cancel|retry}` — durable monitors for asynchronous work (MUR runs, GitHub Actions runs, Codex/Claude Code subprocesses via a process record). SQLite at `~/.mur/monitor/monitors.db`; the daemon polls due monitors every 15 s with fenced leases and catches up missed checks after a restart. `unknown` (query failed, not found, credential rejected, process gone with no exit record) is never reported as `failed`; stalled/soft/hard deadlines default to 20m/3h/8h and count from the work's real start. Read-only in this slice — actions, HITL, auto-registration and notifications are `docs/superpowers/specs/2026-09-11-durable-monitor-design.md` steps 5–9.
```
`README.md` → after the "Deep research, simplified" section:
````
### Durable monitors

Work that outlives the turn that started it — a CI run, a MUR fleet run, a Codex
or Claude Code subprocess — gets a monitor that keeps checking until the source
gives a real answer, across daemon restarts:

```
mur monitor add --file wait-for-ci.yaml   # validates, probes once, registers
mur monitor list                          # what still needs attention
mur monitor show <id> --history           # evidence + append-only history
mur monitor cancel <id>                   # stop watching (never cancels the work)
mur monitor retry <id>                    # bring an exhausted monitor back
```

`unknown` — the API rate-limited us, the run is not found yet, the process is
gone without an exit record — is reported as exactly that, never as `failed`.
Deadlines (stalled 20m · soft 3h · hard 8h) count from when the work really
started. Design: `docs/superpowers/specs/2026-09-11-durable-monitor-design.md`.
````
Docs site + product page are external (`update-docs` skill) — file the follow-up issue as #1324 was for deep-research; do not block this PR on it.

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/cmd/monitor.rs mur-core/src/cmd/mod.rs mur-core/src/cli/mod.rs mur-core/src/dispatch.rs CLAUDE.md README.md
git commit -m "feat(cli): mur monitor add/list/show/cancel/retry"
```

---

### Task 14: Daemon thread

**Files:**
- Create: `mur-daemon/src/monitor_tick.rs`
- Modify: `mur-daemon/src/main.rs` (`mod monitor_tick;` at the top; `monitor_tick::spawn(mur_dir.clone());` right after `snapshot_requests::spawn(mur_dir.clone());` at `:105`)

**Interfaces:**
- Consumes: `mur_core::monitor::service::{recover, tick_once, TICK_INTERVAL}`
- Produces: `pub fn spawn(mur_home: PathBuf)`

- [ ] **Step 1: Implement**

```rust
//! Durable-monitor worker: recovery once at start, then one scheduler pass
//! every `TICK_INTERVAL` on a plain OS thread — rusqlite is synchronous and
//! the adapters block (a 30 s GitHub timeout at worst), so this never runs
//! on the tokio runtime. Everything it does is in `mur_core::monitor::service`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use mur_core::monitor::service::{self, TICK_INTERVAL};

pub fn spawn(mur_home: PathBuf) {
    std::thread::Builder::new()
        .name("mur-monitor".into())
        .spawn(move || run_loop(&mur_home))
        .expect("spawn mur-monitor thread");
}

fn run_loop(mur_home: &Path) {
    let owner = format!("daemon-{}", std::process::id());
    match service::recover(mur_home, Utc::now()) {
        Ok(r) => tracing::info!(recovered_leases = r.recovered_leases.len(), overdue = r.overdue, "monitor: recovered"),
        Err(e) => tracing::error!(error = %e, "monitor: recovery failed; ticking anyway"),
    }
    loop {
        match service::tick_once(mur_home, Utc::now(), &owner) {
            Ok(r) if r.claimed > 0 => tracing::info!(
                claimed = r.claimed, observed = r.observed, unknown = r.unknown,
                completed = r.completed, action_pending = r.action_pending,
                exhausted = r.exhausted, stale_fence = r.stale_fence,
                "monitor tick"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "monitor tick failed"),
        }
        std::thread::sleep(TICK_INTERVAL);
    }
}
```

- [ ] **Step 2: Build, clippy, and a live smoke**

```bash
cargo fmt --all && cargo clippy -p mur-daemon --all-targets -- -D warnings
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build -p mur-daemon -p mur-core
```
Smoke (record the transcript in the commit message):
1. `mur fleet run <any small fleet>` or any command that leaves a `~/.mur/runs/<id>/run.json`; note the run id.
2. Write `m.yaml` with `source: { type: mur_run, reference: <run id> }`, `mur monitor add --file m.yaml` → prints `monitor … first check …`.
3. Start the daemon (`RUST_LOG=info cargo run -p mur-daemon`), wait ≥ 15 s, `mur monitor show <id>` → one observation with the run's state; the daemon log has one `monitor tick` line.
4. Kill the daemon, wait 3 minutes, restart → log shows `monitor: recovered` with `overdue = 1` and the next `show` has a newer observation.

- [ ] **Step 3: Commit**

```bash
git add mur-daemon/src/monitor_tick.rs mur-daemon/src/main.rs
git commit -m "feat(daemon): durable-monitor worker thread — recover, then tick every 15s"
```

---

## Self-review

**Spec coverage** (spec section → task; **plan-2** = deliberately deferred, tables/columns already reserved):

| Spec section | Task(s) |
|---|---|
| `MonitorSpec` 契約 + 建立時驗證 1,4,5,7,8 | 1 |
| 建立時驗證 2,3 (adapter enabled, one read-only probe) | 13 `add` |
| 建立時驗證 6 (idempotency_key unique among open monitors) | 4 |
| 自動註冊邊界 / registration outbox | **plan-2** (table created in 4) |
| 狀態模型 (8 states × 5 outcomes) | 2, 8 |
| pending backoff 30s→30m + jitter + `recommended_poll_after` floor | 2, 8 |
| unknown backoff, monitor-health notice | 2, 8 (`monitor_unhealthy` event) |
| 租約: atomic claim, heartbeat, expiry reclaim, fencing, cycle id on every write | 5, 6 |
| stalled/soft/hard from work start; token-change-only progress; retain 2h; not-retain → exhausted | 7, 8 |
| 混合處置策略 / 風險政策 / 3-attempt cap / HITL | **plan-2** (`remediation_attempts` column + `action_pending` parking in 8) |
| 冪等 action keys | **plan-2** (`monitor_actions` table) |
| history append-only: created, every observation, transitions, recovery, cancel/retry | 4, 5, 6, 13 |
| 通知策略 | **plan-2** — the deduped `stalled`/`stalled_recovered`/`soft_deadline`/`hard_deadline`/`terminal`/`monitor_unhealthy`/`exhausted` events written in 8 are the notifier's input |
| MVP Adapter: MUR run (no re-dispatch; timeout ≠ failure) | 9 |
| MVP Adapter: GitHub Actions (status/conclusion/updated_at/attempt; 404/401/403/429/5xx → unknown) | 10 |
| MVP Adapter: Codex/Claude Code (durable id ≠ pid; gone without exit record → unknown; lost pipe ≠ failure) | 11 (Decision 3) |
| Custom adapter contract | trait in 3; `SourceType::Custom` accepted by spec, no adapter registered → `add` refuses (13) |
| CLI add/list/show/cancel/retry semantics | 13 |
| SQLite 資料模型 (8 tables, `version`, fencing) | 4 |
| daemon 恢復 steps 1, 2, 4 + throttle | 8 `recover`, 12, 14 (`TICK_MAX_CLAIMS`) |
| daemon 恢復 steps 3, 5, 6 | **plan-2** |
| 錯誤處理 table rows for rate limit / credential / parse / crash-during-query / not-found | 10, 8, 6 |
| 安全與隱私: ref-only credentials, redaction chokepoint | 1, 3, 10 |
| 測試策略: unit / store-scheduler / adapter contract / time semantics / e2e 2,4 | 1–12 |
| 測試策略 e2e 1, 3, 5 and action/recovery tests | **plan-2** |
| MVP 驗收: offline catch-up + recovery history | 12 |
| MVP 驗收: three adapters distinguish failed vs unknown | 9, 10, 11 |
| MVP 驗收: terminal side-effect once | 8 (`terminal_completes_once_and_is_never_reclaimed`), 12 |
| MVP 驗收: defaults 20m/3h/8h | 1 |
| MVP 驗收: hard deadline stops remediation, retains read-only | 8 |
| MVP 驗收: quiet polling, deduped notable events | 8 |
| MVP 驗收: low-risk remediation ≤ 3, HITL provable, auto-registration | **plan-2** |

**Placeholder scan:** the only "fill this in" instructions are the two `redact_secrets` shape checks (Tasks 3, 10) and `conn_for_test_set_state` (Task 13), each with the exact replacement stated inline. No TBD/TODO.

**Type consistency:** `Observation::{pending,terminal,unknown}` (3) used in 8–13; `claim_due(now, owner, lease, max)` (5) in 8, 12; `apply_cycle(id, fence, &u)` (6) in 8; `CycleUpdate` fields (6) filled completely in 8; `registry(mur_home)` (9) in 12, 13; `service::{tick_once(mur_home, now, owner), recover(mur_home, now), TICK_INTERVAL}` (12) in 14; `MonitorState::ALL` (2) in 13; `store.list(&ListFilter)` (4) in 8, 13.

**Known ceilings, named:** stalled re-entry within one cycle is not re-announced (comment in 8); `tick` heartbeats each claimed monitor's lease right before its own `observe` call (task 12's `scheduler::tick`), so a single-monitor timeout is covered by construction — the remaining ceiling is a `max_claims`-sized *batch* running serially against one lease length, which the per-claim heartbeat is exactly what bounds; `list` prints RFC 3339 rather than local time.

---

### Task 15: `monitor(n)` in the murmur footer + a shortcut

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/footer.rs` (pure count → label)
- Modify: `mur-core/src/cmd/agent/cli/app/mod.rs` or wherever `App` state lives (cached count + refresh stamp)
- Modify: `mur-core/src/cmd/agent/cli/events.rs` (the Ctrl+T binding, beside the other `if ctrl` arms at `:515-532`)
- Create: `mur-core/src/cmd/agent/cli/monitor.rs` (the `/monitor` handler)
- Modify: `mur-core/src/cmd/agent/cli/slash_cmds.rs` + `app.rs` parser + `complete.rs` (the `/monitor` slash command, mirroring `/deep-research` from #1320)
- Test: `footer.rs` (pure), `app/tests/` (key action), `complete.rs` (parity)

**Interfaces:**
- Consumes: `mur_monitor::store::{MonitorStore, ListFilter}`, `mur_monitor::state::MonitorState`, `mur_core::cmd::monitor` (Task 13)
- Produces: `footer::{has_condition(&MonitorRow) -> bool, conditions(&[MonitorRow]) -> usize, monitor_label(usize) -> Option<String>}`, `App::monitor_conditions` + `App::refresh_monitor_counts(now)`, `SlashCmd::Monitor(Vec<String>)`

**Design decisions — do not relitigate:**
1. **`Ctrl+M` is forbidden.** In a terminal `^M` IS Enter (carriage return); crossterm delivers it as `KeyCode::Enter`, so a `Char('m') + CONTROL` arm either never fires or shadows submitting a message. The binding is **`Ctrl+T`** (moni**t**or), which is free — `events.rs:515-532` already uses Ctrl+D/C/U/V/O/R and `:470-474` uses Ctrl+P/N.
2. **The count is cached, never computed during render.** Opening SQLite on every frame is a per-keystroke file open. `App` holds `monitor_counts: (usize, usize)` plus a `last_monitor_refresh: Instant`, refreshed at most every `MONITOR_REFRESH_SECS` (30) from the existing event-loop tick — the same cadence the daemon polls at, so a fresher number would be fiction anyway.
3. **Silent unless something has happened.** The segment counts monitors with a live *condition*, not monitors that exist. Three monitors quietly polling a healthy CI run show NOTHING — a permanent number in a status bar stops being read within a day, and this mirrors the design doc's own rule for notifications (正常 polling 不通知). `monitor_label` returns `None` at `n == 0`.
4. **What counts as a condition** — computable from `MonitorRow` alone, no extra query:
   - `state` is `exhausted` or `action_pending` — it will not move again without a human;
   - `stalled_since.is_some()` — no progress for `stalled_after`;
   - `unknown_streak >= UNHEALTHY_AFTER_UNKNOWN` — the monitor itself is sick (credential dead, source unreachable), which is a monitor problem, not a work failure.
   Anything else — `active`, `sleeping`, healthy `pending` — contributes nothing. One number, no `!` split: `monitor(2)` means two things want you.
   The count clears when the condition clears (a stall recovers, a retry reactivates an exhausted monitor); there is no separate acknowledge state. `exhausted`/`action_pending` persist until a human acts, which is correct — they genuinely still need one.
5. **The shortcut opens nothing modal.** `Ctrl+T` runs the same handler as `/monitor`, which prints the list into the scrollback as a card — no overlay, no alternate screen. The TUI's overlay path has a standing defect (a HITL request is invisible outside `--plain`), and a status list is not worth inheriting it.

- [ ] **Step 1: Write the failing tests**

`footer.rs` `mod tests`:
```rust
#[test]
fn monitor_label_is_silent_without_a_condition() {
    assert_eq!(monitor_label(0), None);
}

#[test]
fn monitor_label_counts_conditions() {
    assert_eq!(monitor_label(1).as_deref(), Some("monitor(1)"));
    assert_eq!(monitor_label(4).as_deref(), Some("monitor(4)"));
}

#[test]
fn quiet_monitors_do_not_count() {
    // The three states a healthy monitor cycles through contribute nothing;
    // only a live condition does.
    let quiet = row(MonitorState::Sleeping, None, 0);
    let active = row(MonitorState::Active, None, 0);
    let checking = row(MonitorState::Checking, None, 0);
    assert_eq!(conditions(&[quiet, active, checking]), 0);
}

#[test]
fn each_condition_counts_once() {
    let needs_human = row(MonitorState::Exhausted, None, 0);
    let parked = row(MonitorState::ActionPending, None, 0);
    let stalled = row(MonitorState::Sleeping, Some(t0()), 0);
    let sick = row(MonitorState::Sleeping, None, UNHEALTHY_AFTER_UNKNOWN);
    assert_eq!(conditions(&[needs_human, parked, stalled, sick]), 4);
    // A monitor that is both stalled AND sick is still one monitor.
    let both = row(MonitorState::Sleeping, Some(t0()), UNHEALTHY_AFTER_UNKNOWN);
    assert_eq!(conditions(&[both]), 1);
}
```
(`row(state, stalled_since, unknown_streak)` is a local helper building a `MonitorRow`; `conditions(&[MonitorRow]) -> usize` is the pure counter you implement in Step 3 beside `monitor_label`.)

`app/tests/overlay_key_action_tests.rs` (or beside the existing key tests):
```rust
#[test]
fn ctrl_t_is_the_monitor_shortcut_and_ctrl_m_is_never_bound() {
    // ^M is Enter on every terminal; binding it would shadow submit.
    assert!(!binds_ctrl(KeyCode::Char('m')), "Ctrl+M must never be bound");
    assert!(binds_ctrl(KeyCode::Char('t')));
}
```
(Write `binds_ctrl` against whatever the existing key-dispatch test helper is; if there is none, assert on the `overlay_key_action`/`events` path the neighbouring tests already use.)

`complete.rs` parity test: `/monitor` appears in the completion list between its alphabetical neighbours, and `SlashCmd::Monitor` parses from both `monitor` and `mon`.

- [ ] **Step 2: Run to verify they fail**

`ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core cli:: footer::`
Expected: compile errors — `monitor_label`, `SlashCmd::Monitor` undefined.

- [ ] **Step 3: Implement `monitor_label` (pure)**

```rust
/// Does this monitor want a human or a second look? Quiet polling does not
/// count: a footer number that is always present stops being read.
pub fn has_condition(row: &MonitorRow) -> bool {
    matches!(row.state, MonitorState::Exhausted | MonitorState::ActionPending)
        || row.stalled_since.is_some()
        || row.unknown_streak >= UNHEALTHY_AFTER_UNKNOWN
}

/// How many monitors currently have a condition. One per monitor, however
/// many conditions it has at once.
pub fn conditions(rows: &[MonitorRow]) -> usize {
    rows.iter().filter(|r| has_condition(r)).count()
}

/// Footer segment, or `None` when nothing wants attention.
pub fn monitor_label(n: usize) -> Option<String> {
    (n > 0).then(|| format!("monitor({n})"))
}
```

- [ ] **Step 4: Cache the counts on `App`**

Add `monitor_conditions: usize` and `last_monitor_refresh: Option<Instant>`; `refresh_monitor_counts` opens `MonitorStore`, calls `list(&ListFilter::default())`, passes the rows to `conditions()`, and returns early if the last refresh is newer than `MONITOR_REFRESH_SECS`. A store that fails to open (or does not exist yet — the common case for a user who has never added a monitor) leaves the previous count and is not an error the user sees: the footer is not a diagnostic surface.

- [ ] **Step 5: Bind Ctrl+T and add `/monitor`**

In `events.rs`, beside `Char('o') if ctrl`: `KeyCode::Char('t') if ctrl => monitor::handle(app, &[], tx),`. Add `SlashCmd::Monitor(Vec<String>)` following the `DeepResearch` shape from #1320 (parser arm, dispatch arm, `help_text` row, `complete.rs` entry). `monitor::handle` renders the same rows `mur monitor list` prints, into the scrollback as a card — reuse Task 13's row formatting rather than writing a second renderer.

- [ ] **Step 6: Manual check** (record in the commit message)

`mur agent cli <agent>` with zero monitors → no footer segment. Add a healthy one (`mur monitor add --file …`) → **still no segment** (it is quietly polling; this is the point of the change). Force a condition — `mur monitor cancel` is not one, so use a monitor whose source is unreachable until `unknown_streak` reaches the threshold, or point one at a finished run with a failure action so it parks in `action_pending` → `monitor(1)` appears within 30s. `Ctrl+T` prints the list without disturbing a half-typed message. Press Enter → the message still sends (proves Ctrl+M was not shadowed).

- [ ] **Step 7:** fmt + clippy; commit `feat(murmur): monitor(n) in the footer and Ctrl+T to list them`.
