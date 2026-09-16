# Durable Monitor Actions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a durable monitor *act* on what it observes — run the spec's terminal actions under a risk gate, park high-risk ones for human approval, cap remediation at three attempts — and register a monitor atomically when an agent starts trackable async work.

**Architecture:** The previous two slices left three `MonitorState` variants unreachable: the scheduler already parks a settled monitor in `ActionPending` when its spec has actions, and nothing drains it; `AwaitingApproval` and `Registering` have no writer at all. This plan makes those three states live. The executor claims each action under a unique `action_key`, classifies its risk from a fixed table (never from the action's own text), routes anything above `Read` through the **existing** `mur-core::hitl::gate` — not a second gate — and records the outcome in the `monitor_actions` table that already exists. Auto-registration adds a `Registering` row before the work starts where the source allows it, and a durable outbox for the case where it does not.

**Tech Stack:** Rust 2024, rusqlite (WAL), `mur-monitor` (store + scheduler + spec), `mur-core` (adapters, service, CLI), `mur-common::hitl` (`RiskTier`, `HitlRequest`, `action_hash`), `mur-channel` (the approval log the gate reads and writes), tokio (`Handle::block_on` only — see Global Constraints).

**Spec:** `docs/superpowers/specs/2026-09-11-durable-monitor-design.md` — §混合處置策略, §風險政策, §冪等與事件紀錄, §自動註冊邊界, §錯誤處理.

## Global Constraints

- **Risk tier is NEVER LLM-asserted and never derived from an action's own text.** `mur_common::hitl::RiskTier`'s own doc says so: "Tier is resolved most-restrictive-wins and is NEVER LLM-asserted." The spec repeats it: 「agent 不得靠改寫動作名稱繞過分類」. Classification is a fixed match on the action *type*, in code.
- **Do not build a second HITL gate.** `mur-core::hitl::gate::gate` is the one risk gate in this repo: SHA-256-pinned via `action_hash`, approvals matched on the hash (never `hitl_id`), 7-day TTL, `mur channel approve <channel_id> <hitl_id>` as the surface. Reuse it.
- **The monitor tick is a plain OS thread and must stay one.** `mur-daemon/src/monitor_tick.rs`'s module doc: "rusqlite is synchronous and the adapters block (a 30 s GitHub timeout at worst), so this never runs on the tokio runtime." `gate()` is `async`. Bridge with a `tokio::runtime::Handle` passed in from the daemon and `handle.block_on(...)` — correct from a non-runtime thread, and the *only* sanctioned bridge here. The predecessor slice shipped a CRITICAL panic from the mirror-image mistake (a `reqwest::blocking` client built inside a `block_on`); read `mur-core/src/monitor/adapters/github_actions.rs`'s comment on that before writing this.
- **`yes: false` on every unattended path.** `GatePolicy { yes: true }` is for interactive/explicit use only. The monitor daemon is unattended by definition and passes `false`, always.
- **`Unanswered::Defer` is the daemon's mode.** No TTY means park the request and report the action *blocked*, never *failed*; a later approval releases it on a subsequent tick.
- **`unknown` is a monitor problem, never a work failure.** Unchanged from the shipped slices, and it now also means: never run an `on_failure` action because a query failed.
- **Every side effect is claimed before it runs**, under the stable key `<monitor-id>:<cycle-id>:<observed-terminal-version>:<action-type>:<action-index>` (spec §冪等與事件紀錄, verbatim).
- **Secrets never reach history.** 「Secrets、完整環境變數與未去敏 logs 不得寫入 history。」Action results are redacted through `mur_common::redact` before they are stored — redact first, truncate second (the predecessor slice shipped that bug the other way round).
- Max 3 automatic remediation attempts (`policy.max_remediation_attempts`, default 3); at the cap the monitor goes `Exhausted`, notifies, and stops remediating while read-only monitoring may continue.
- Source files ≤ 800 lines. No hardcoded values in logic. Comments in English; a `spec §<heading>` citation may keep the design doc's Chinese heading label. User-visible text says **MUR** uppercase; the CLI command `mur` stays lowercase.
- `cargo nextest`, never bare `cargo test`. `mur-core` needs `MUR_WEB_DIST` to build.

---

## Deliberately out of scope

Named here so no task invents them and no reviewer charges their absence as a gap.

- **AgentResolver** (spec §混合處置策略 step 3 — the LLM that decides when structured rules do not cover a failure). It is the *third* step of the disposition order, gated behind "rules did not match or the remedy failed", so it structurally requires everything in this plan to exist first. It also needs its own de-identification contract and a test strategy for LLM behavior. Its own plan.
- **`apply_known_remedy`.** The verb is in `KNOWN_ACTIONS` but no remedy catalogue exists anywhere in the repo or the spec. Implementing it would mean inventing the catalogue's schema, storage and matching rules in a fix round. Out.
- **`rerun` and `start_downstream`.** Both are real and both are *writes to an external system*. `rerun` needs a GitHub token with `actions:write`; today's adapter asks for `actions:read` and the whole shipped slice is read-only by construction. Adding write credentials is a security-surface decision with its own review, not a task inside an executor plan. The executor this plan builds is verb-agnostic — adding these later is a classification-table row plus an executor function, no machinery change.
- **Metrics / dashboard** (spec §可觀測性). No consumer.

The three verbs this plan *does* execute — `notify`, `collect_logs`, `reschedule_monitor` — are exactly the ones that are local, need no new credential, and can be tested end to end. The deliverable is the machinery; more verbs are cheap once it exists.

---

## File structure

| File | Responsibility |
|---|---|
| `mur-monitor/src/action/mod.rs` (new) | `ActionKey` construction + parsing, `ActionState`, `ActionRow`. Pure. |
| `mur-monitor/src/action/risk.rs` (new) | `classify(action_type) -> RiskTier`. A fixed match, no input from the action's params. |
| `mur-monitor/src/store/action.rs` (new) | Claim under the unique key, record result, list by monitor, transition state. Owns `monitor_actions`. |
| `mur-core/src/monitor/actions/mod.rs` (new) | `ActionExecutor` trait, the registry, and `execute_one`. |
| `mur-core/src/monitor/actions/local.rs` (new) | `notify`, `collect_logs`, `reschedule_monitor`. |
| `mur-core/src/monitor/actions/gate.rs` (new) | The bridge to `mur-core::hitl::gate`: channel id for a monitor, `ActionRequest` construction, `Handle::block_on`. |
| `mur-core/src/monitor/service.rs` (modify) | `drain_actions(mur_home, handle, now)` alongside `tick_once` / `drain_notifications`. |
| `mur-monitor/src/register.rs` (new) | `register_or_outbox`, outbox row types, `Registering` transitions. |
| `mur-monitor/src/store/outbox.rs` (new) | Owns `monitor_registration_outbox`: enqueue, claim, drop, backoff. |
| `mur-daemon/src/monitor_tick.rs` (modify) | Pass the runtime `Handle`; call `drain_actions` and `drain_outbox`. |
| `mur-core/src/cmd/monitor.rs` (modify) | `show` renders actions and their approval state. |
| `mur-core/src/cmd/fleet/run.rs` (modify) | One real auto-registration call site. |

`monitor_actions` and `monitor_registration_outbox` **already exist** in `mur-monitor/src/store/mod.rs`'s `migrate()` with the columns this plan needs. No `SCHEMA_USER_VERSION` bump anywhere in this plan; if a task thinks it needs one, that is a signal to stop and re-read, not to bump.

---

## Part A — the deterministic executor

### Task 1: Action key and risk classification (pure)

**Files:**
- Create: `mur-monitor/src/action/mod.rs`, `mur-monitor/src/action/risk.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod action;`)

**Interfaces:**
- Consumes: `mur_monitor::spec::{Action, KNOWN_ACTIONS}`, `mur_common::hitl::RiskTier`.
- Produces: `ActionKey::new(monitor_id, cycle_id, terminal_version, action_type, index) -> String`, `ActionState::{Claimed, Blocked, Done, Failed}` with `as_str`/`parse`, `risk::classify(&str) -> RiskTier`.

The key format is the spec's, verbatim from §冪等與事件紀錄:

```text
<monitor-id>:<cycle-id>:<observed-terminal-version>:<action-type>:<action-index>
```

`observed-terminal-version` is the monitor's `fence` at the moment the terminal observation was written.

**Why the fence is stable here, even though it bumps on every lease claim.** A fence that moved each tick would give every tick a different key, the claim would never collide, and the action would run again and again — the exact failure the claim exists to prevent. It does not move, because on a terminal observation the scheduler sets the state to `ActionPending` or `Completed`, and `MonitorState::is_claimable` is `Active | Sleeping` only. A settled monitor is never claimed again, so its fence is frozen at the value it had when the terminal landed. If a future remedy re-activates the monitor, the fence bumps then — which is precisely when a fresh claim *should* be possible. Verify this with `is_claimable` before writing the key; do not assume it.

**`cycle_id` contributes nothing to uniqueness and that is fine.** `finish_cycle` only stamps `finished_at` on the `monitor_cycles` row; it never mints a new `cycle_id` on the monitor. The id is therefore constant for a monitor's life — the same fact that broke the predecessor slice's `(monitor, cycle, kind)` notification dedup. It stays in the key because the spec's format says so and because it costs nothing, **not** because it discriminates.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::hitl::RiskTier;

    #[test]
    fn the_key_is_the_specs_five_field_shape() {
        // spec §冪等與事件紀錄: <monitor-id>:<cycle-id>:<version>:<type>:<index>
        assert_eq!(ActionKey::new("m1", "c1", 7, "notify", 0), "m1:c1:7:notify:0");
    }

    #[test]
    fn a_re_observed_terminal_gets_a_different_key() {
        // Same monitor, same cycle, same action — but a new terminal
        // observation (higher fence). A fresh claim must be possible, or a
        // child cycle after a remedy could never act.
        assert_ne!(
            ActionKey::new("m1", "c1", 7, "notify", 0),
            ActionKey::new("m1", "c1", 8, "notify", 0)
        );
    }

    #[test]
    fn a_restart_replaying_the_same_terminal_gets_the_same_key() {
        // The whole point of the claim: this must collide so the unique
        // constraint refuses the second attempt.
        assert_eq!(
            ActionKey::new("m1", "c1", 7, "collect_logs", 2),
            ActionKey::new("m1", "c1", 7, "collect_logs", 2)
        );
    }

    #[test]
    fn every_known_action_has_a_tier_and_none_is_read_by_accident() {
        // A verb added to KNOWN_ACTIONS without a tier must not silently
        // become auto-executable. `classify` is total and its fallback is
        // the most restrictive tier, not the least.
        for a in mur_monitor::spec::KNOWN_ACTIONS {
            let t = risk::classify(a);
            if *a == "notify" || *a == "collect_logs" || *a == "reschedule_monitor" {
                assert_eq!(t, RiskTier::Read, "{a}");
            } else {
                assert!(t > RiskTier::Read, "{a} must not be auto-executable");
            }
        }
    }

    #[test]
    fn an_unknown_verb_classifies_as_privileged_not_read() {
        // The safety property: classification is total, and anything the
        // table does not name is the most restrictive tier. A verb that
        // slipped past spec validation must not become auto-executable by
        // being unrecognised.
        assert_eq!(risk::classify("definitely_not_a_verb"), RiskTier::Privileged);
        assert_eq!(risk::classify(""), RiskTier::Privileged);
    }

    #[test]
    fn start_downstream_is_not_read_even_though_it_runs_on_success() {
        // spec §風險政策 calls this out by name: 「成功就部署 production」
        // 不因寫在 success action 就自動成為低風險.
        assert!(risk::classify("start_downstream") > RiskTier::Read);
    }

    #[test]
    fn action_state_round_trips() {
        for s in [ActionState::Claimed, ActionState::Blocked, ActionState::Done, ActionState::Failed] {
            assert_eq!(ActionState::parse(s.as_str()), Some(s));
        }
        assert_eq!(ActionState::parse("nonsense"), None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p mur-monitor -E 'test(/action::/)'`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement**

`mur-monitor/src/action/risk.rs`:

```rust
//! Risk classification for monitor actions (spec §風險政策).
//!
//! A fixed match on the action *type*. Never on its parameters, never on
//! anything an agent wrote, and never from a model:
//! `mur_common::hitl::RiskTier`'s own doc says "Tier is resolved
//! most-restrictive-wins and is NEVER LLM-asserted", and the spec says
//! 「agent 不得靠改寫動作名稱繞過分類」.
//!
//! The fallback is `Privileged`, not `Read`. A verb this table does not
//! name is a verb nobody classified, and the failure mode of guessing low
//! is an unattended process doing something nobody approved.

use mur_common::hitl::RiskTier;

pub fn classify(action_type: &str) -> RiskTier {
    match action_type {
        // Local, no external write, no new credential.
        "notify" => RiskTier::Read,
        "collect_logs" => RiskTier::Read,
        "reschedule_monitor" => RiskTier::Read,
        // Writes to an external system. spec §風險政策: a rerun is only
        // low-risk for a job explicitly marked flaky and under its cap —
        // a condition this slice has no way to establish, so it asks.
        "rerun" => RiskTier::Write,
        // spec §風險政策 names this one: being written under on_success
        // does not make deploying production low-risk.
        "start_downstream" => RiskTier::Privileged,
        // No remedy catalogue exists; anything claiming to apply one is
        // unclassifiable by definition.
        "apply_known_remedy" => RiskTier::Privileged,
        _ => RiskTier::Privileged,
    }
}
```

`mur-monitor/src/action/mod.rs`:

```rust
//! Action identity and lifecycle (spec §冪等與事件紀錄).

pub mod risk;

/// The stable claim key. Spec §冪等與事件紀錄, verbatim:
/// `<monitor-id>:<cycle-id>:<observed-terminal-version>:<action-type>:<action-index>`
///
/// `observed_terminal_version` is the monitor's fence when the terminal
/// observation was written. Including it means a re-observed terminal (a
/// child cycle after a remedy) claims afresh, while a daemon restart
/// replaying the same terminal collides with the row already there.
pub struct ActionKey;

impl ActionKey {
    pub fn new(
        monitor_id: &str,
        cycle_id: &str,
        observed_terminal_version: i64,
        action_type: &str,
        action_index: usize,
    ) -> String {
        format!("{monitor_id}:{cycle_id}:{observed_terminal_version}:{action_type}:{action_index}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    /// Claimed, not yet run.
    Claimed,
    /// Gated and parked: a human has not answered. NOT a failure.
    Blocked,
    Done,
    Failed,
}

impl ActionState {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionState::Claimed => "claimed",
            ActionState::Blocked => "blocked",
            ActionState::Done => "done",
            ActionState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claimed" => Some(ActionState::Claimed),
            "blocked" => Some(ActionState::Blocked),
            "done" => Some(ActionState::Done),
            "failed" => Some(ActionState::Failed),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p mur-monitor -E 'test(/action::/)'`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-monitor/src/action/ mur-monitor/src/lib.rs
git commit -F - <<'MSG'
feat(monitor): action keys and a risk table that fails closed

The key is the spec's five-field shape including the observed terminal
version, so a re-observed terminal claims afresh while a restart replaying
the same one collides. Classification is a fixed match on the action type
with Privileged as the fallback — an unclassified verb must not become
auto-executable by being unrecognised.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
MSG
```

---

### Task 2: Claim an action exactly once

**Files:**
- Create: `mur-monitor/src/store/action.rs`
- Modify: `mur-monitor/src/store/mod.rs` (add `mod action;`)

**Interfaces:**
- Consumes: Task 1's `ActionKey`, `ActionState`, `risk::classify`; `MonitorStore`.
- Produces: `MonitorStore::{claim_action, finish_action, block_action, actions_for, pending_actions}`; `ActionRow { action_key, monitor_id, cycle_id, risk, approval_id, state, attempt, result, created_at }`.

`monitor_actions` already exists in `migrate()` with exactly these columns. **Do not add a table and do not bump `SCHEMA_USER_VERSION`.**

The claim is the whole point: `action_key` is the PRIMARY KEY, so a second claim of the same key is refused by the database, not by a check-then-act in Rust. `claim_action` returns `Ok(false)` when the row already exists — that is the normal, expected path after a daemon restart, not an error.

Follow `mur-monitor/src/store/lease.rs`'s house pattern for the transaction: explicit `BEGIN IMMEDIATE` / `COMMIT` with `ROLLBACK` on every error path. Its module doc explains why deferred transactions are wrong here under WAL; read it before writing this.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_of_the_same_key_is_refused() {
        let (_d, s, id, cyc) = fixture();
        let k = ActionKey::new(&id, &cyc, 1, "notify", 0);
        assert!(s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap());
        assert!(
            !s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap(),
            "the unique key must refuse the second claim"
        );
        assert_eq!(s.actions_for(&id).unwrap().len(), 1, "and must not write a second row");
    }

    #[test]
    fn eight_threads_racing_one_key_produce_exactly_one_winner() {
        // The property a check-then-act in Rust would fail. This test is
        // the reason the claim is a PRIMARY KEY insert inside BEGIN
        // IMMEDIATE and not a SELECT followed by an INSERT.
        let d = tempfile::tempdir().unwrap();
        {
            let s = MonitorStore::open(d.path()).unwrap();
            let id = s.create(&spec(), t0(), None).unwrap().id;
            let cyc = s.get(&id).unwrap().unwrap().cycle_id;
            let k = ActionKey::new(&id, &cyc, 1, "notify", 0);
            let wins = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    let (p, k, id, cyc, wins) =
                        (d.path(), k.clone(), id.clone(), cyc.clone(), wins.clone());
                    scope.spawn(move || {
                        let s = MonitorStore::open(p).unwrap();
                        if s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap() {
                            wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    });
                }
            });
            assert_eq!(wins.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn finishing_records_the_state_and_the_redacted_result() {
        let (_d, s, id, cyc) = fixture();
        let k = ActionKey::new(&id, &cyc, 1, "collect_logs", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap();
        s.finish_action(&k, ActionState::Done, "token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ok", t0())
            .unwrap();
        let row = &s.actions_for(&id).unwrap()[0];
        assert_eq!(row.state, ActionState::Done);
        let result = row.result.as_deref().unwrap();
        assert!(
            !result.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "spec §安全與隱私: a secret must never reach history — got {result:?}"
        );
        assert!(result.contains("ok"), "the non-secret part must survive: {result:?}");
    }

    #[test]
    fn a_blocked_action_stays_pending_and_carries_its_approval_id() {
        // Blocked is NOT failed: a later tick must pick it up again.
        let (_d, s, id, cyc) = fixture();
        let k = ActionKey::new(&id, &cyc, 1, "rerun", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Write, t0()).unwrap();
        s.block_action(&k, "hitl-abc", t0()).unwrap();
        let row = &s.actions_for(&id).unwrap()[0];
        assert_eq!(row.state, ActionState::Blocked);
        assert_eq!(row.approval_id.as_deref(), Some("hitl-abc"));
        let pending: Vec<_> = s.pending_actions(t0(), 10).unwrap();
        assert_eq!(pending.len(), 1, "a blocked action must come back on a later tick");
    }

    #[test]
    fn a_done_action_never_comes_back() {
        let (_d, s, id, cyc) = fixture();
        let k = ActionKey::new(&id, &cyc, 1, "notify", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap();
        s.finish_action(&k, ActionState::Done, "sent", t0()).unwrap();
        assert!(s.pending_actions(t0(), 10).unwrap().is_empty());
    }

    #[test]
    fn attempt_counts_up_across_blocks() {
        let (_d, s, id, cyc) = fixture();
        let k = ActionKey::new(&id, &cyc, 1, "rerun", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Write, t0()).unwrap();
        s.block_action(&k, "h1", t0()).unwrap();
        s.block_action(&k, "h1", t0()).unwrap();
        assert_eq!(s.actions_for(&id).unwrap()[0].attempt, 2);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p mur-monitor -E 'test(/store::action/)'`
Expected: FAIL — `claim_action` not found.

- [ ] **Step 3: Implement**

```rust
//! The `monitor_actions` table (spec §冪等與事件紀錄): claim before you act.
//!
//! `action_key` is the PRIMARY KEY, so "has this side effect already run?"
//! is answered by the database refusing a duplicate INSERT, never by a
//! SELECT followed by an INSERT in Rust — two daemons, or one daemon
//! restarting mid-action, would both pass a check-then-act.
//!
//! `claim_action` returning `Ok(false)` is the ordinary path after a
//! restart, not an error: the terminal is being replayed and the action
//! already has a row.
//!
//! Read-then-write runs under an explicit `BEGIN IMMEDIATE` / `COMMIT`
//! with `ROLLBACK` on every error path — same reasoning and shape as
//! `store/lease.rs`, whose module doc explains why a deferred transaction
//! is wrong here under WAL.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_common::hitl::RiskTier;
use rusqlite::OptionalExtension;

use crate::action::{ActionState};
use crate::store::{MonitorStore, ts};

#[derive(Debug, Clone)]
pub struct ActionRow {
    pub action_key: String,
    pub monitor_id: String,
    pub cycle_id: String,
    pub risk: RiskTier,
    pub approval_id: Option<String>,
    pub state: ActionState,
    pub attempt: u32,
    pub result: Option<String>,
}

/// How much of an action's result is kept in history. Redaction runs
/// BEFORE this cut, never after: the shipped observation path had that
/// bug the other way round, and a fixed-length secret pattern straddling
/// the cut survives unmatched.
const RESULT_MAX_CHARS: usize = 400;

fn store_result(raw: &str) -> String {
    let redacted = mur_common::redact::redact_secrets(raw);
    redacted.chars().take(RESULT_MAX_CHARS).collect()
}

impl MonitorStore {
    /// `Ok(true)` when this call created the row — the caller owns the
    /// side effect. `Ok(false)` when a row already exists.
    pub fn claim_action(
        &self,
        action_key: &str,
        monitor_id: &str,
        cycle_id: &str,
        risk: RiskTier,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let risk_s = serde_json::to_string(&risk)?.trim_matches('"').to_string();
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO monitor_actions \
             (action_key, monitor_id, cycle_id, risk, state, attempt, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            rusqlite::params![
                action_key,
                monitor_id,
                cycle_id,
                risk_s,
                ActionState::Claimed.as_str(),
                ts(now)
            ],
        )?;
        Ok(n == 1)
    }

    /// Terminal for this action: `Done` or `Failed`. The result is
    /// redacted then truncated.
    pub fn finish_action(
        &self,
        action_key: &str,
        state: ActionState,
        result: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE monitor_actions SET state = ?1, result = ?2, created_at = created_at \
             WHERE action_key = ?3",
            rusqlite::params![state.as_str(), store_result(result), action_key],
        )?;
        let _ = now;
        Ok(())
    }

    /// Parked awaiting a human. Bumps `attempt` and records which approval
    /// request is outstanding. NOT a failure — `pending_actions` returns it.
    pub fn block_action(&self, action_key: &str, approval_id: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn()
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin block_action")?;
        let r = (|| -> Result<()> {
            self.conn().execute(
                "UPDATE monitor_actions SET state = ?1, approval_id = ?2, attempt = attempt + 1 \
                 WHERE action_key = ?3",
                rusqlite::params![ActionState::Blocked.as_str(), approval_id, action_key],
            )?;
            Ok(())
        })();
        match r {
            Ok(()) => {
                self.conn().execute_batch("COMMIT").context("commit block_action")?;
                let _ = now;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn().execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Everything recorded for one monitor, oldest first.
    pub fn actions_for(&self, monitor_id: &str) -> Result<Vec<ActionRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT action_key, monitor_id, cycle_id, risk, approval_id, state, attempt, result \
             FROM monitor_actions WHERE monitor_id = ?1 ORDER BY created_at ASC, action_key ASC",
        )?;
        let rows = stmt.query_map([monitor_id], row_to_action)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
            .map_err(Into::into)
    }

    /// Actions still owed work: `claimed` (never started) or `blocked`
    /// (waiting on a human, and a later approval may have landed).
    pub fn pending_actions(&self, now: DateTime<Utc>, max: usize) -> Result<Vec<ActionRow>> {
        let _ = now;
        let mut stmt = self.conn().prepare(
            "SELECT action_key, monitor_id, cycle_id, risk, approval_id, state, attempt, result \
             FROM monitor_actions WHERE state IN (?1, ?2) \
             ORDER BY created_at ASC, action_key ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                ActionState::Claimed.as_str(),
                ActionState::Blocked.as_str(),
                max as i64
            ],
            row_to_action,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
            .map_err(Into::into)
    }
}

/// `None` for a row whose `state` or `risk` no longer parses — a forward
/// compatibility hole, not a crash: a newer build's state name must not
/// panic an older one mid-tick.
fn row_to_action(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<ActionRow>> {
    let state: String = r.get(5)?;
    let risk: String = r.get(3)?;
    let (Some(state), Ok(risk)) = (
        ActionState::parse(&state),
        serde_json::from_value::<RiskTier>(serde_json::Value::String(risk)),
    ) else {
        return Ok(None);
    };
    Ok(Some(ActionRow {
        action_key: r.get(0)?,
        monitor_id: r.get(1)?,
        cycle_id: r.get(2)?,
        risk,
        approval_id: r.get(4)?,
        state,
        attempt: r.get::<_, i64>(6)? as u32,
        result: r.get(7)?,
    }))
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p mur-monitor -E 'test(/store::action/)'`
Expected: PASS (6 tests).

- [ ] **Step 5: Mutation-check the claim**

Replace `INSERT OR IGNORE` with plain `INSERT` and confirm the race test errors rather than reporting two winners; then replace the `n == 1` with `true` and confirm `a_second_claim_of_the_same_key_is_refused` goes red. Restore. **Verify the mutation is actually present** (`grep` for it) before trusting a red or a green — a heredoc that silently failed to apply has produced a meaningless PASS in this project before.

- [ ] **Step 6: Commit**

```bash
git add mur-monitor/src/store/action.rs mur-monitor/src/store/mod.rs
git commit -F - <<'MSG'
feat(monitor): claim an action before running it, exactly once

action_key is the PRIMARY KEY, so "did this side effect already run?" is
answered by the database refusing a duplicate insert rather than by a
check-then-act two daemons would both pass. Results are redacted before
they are truncated, not after.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
MSG
```

---

### Task 3: The three local executors

**Files:**
- Create: `mur-core/src/monitor/actions/mod.rs`, `mur-core/src/monitor/actions/local.rs`
- Modify: `mur-core/src/monitor/mod.rs` (add `pub mod actions;`)

**Interfaces:**
- Consumes: `mur_monitor::spec::Action`, `mur_monitor::store::MonitorStore`, `MonitorRow`.
- Produces: `trait ActionExecutor { fn verb(&self) -> &'static str; fn run(&self, ctx: &ActionCtx<'_>, params: &serde_json::Map<String, serde_json::Value>) -> Result<String, String>; }`, `ActionCtx<'a> { pub store: &'a MonitorStore, pub row: &'a MonitorRow, pub now: DateTime<Utc> }`, `pub fn executor_for(verb: &str) -> Option<&'static dyn ActionExecutor>`.

`run` returns `Result<String, String>`: `Ok(summary)` → `ActionState::Done` with the summary as the result; `Err(reason)` → `ActionState::Failed`. Neither ever panics and neither ever touches the network in this slice.

The three verbs:

- **`notify`** — append a `action_notify` event so the shipped notifier picks it up. It does NOT call a channel directly: the notification path is `append_event` → transition guard → queue → drain, and bypassing it would produce a message with no delivery state and no dedup.
- **`collect_logs`** — ask the monitor's adapter for its evidence and record it. Read-only by construction: it calls `SourceAdapter::observe` and keeps `observation.evidence`. It must NOT fetch raw logs from a network source in this slice (no new credential scope; see "Deliberately out of scope").
- **`reschedule_monitor`** — push `next_check_at` out by the policy's `unknown` backoff and return the monitor to `Sleeping`. This is the `on_unknown` action in the spec's example and the only one that makes a monitor keep living.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn notify_appends_an_event_rather_than_delivering_directly() {
    let (_d, s, row) = fixture();
    let ctx = ActionCtx { store: &s, row: &row, now: t0() };
    let out = executor_for("notify").unwrap().run(&ctx, &Default::default()).unwrap();
    let kinds: Vec<_> = s.events(&row.id).unwrap().into_iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&"action_notify".to_string()), "{kinds:?}");
    assert!(!out.is_empty());
}

#[test]
fn reschedule_pushes_the_next_check_out_and_returns_it_to_sleeping() {
    let (_d, s, row) = fixture();
    let before = s.get(&row.id).unwrap().unwrap().next_check_at;
    let ctx = ActionCtx { store: &s, row: &row, now: t0() };
    executor_for("reschedule_monitor").unwrap().run(&ctx, &Default::default()).unwrap();
    let after = s.get(&row.id).unwrap().unwrap();
    assert!(after.next_check_at > before, "next check must move forward");
    assert_eq!(after.state, MonitorState::Sleeping);
}

#[test]
fn collect_logs_records_evidence_and_no_secret() {
    // The adapter fixture returns evidence containing a token-shaped string;
    // the executor's own output must already be clean, not rely on the
    // store's redaction as the only line of defence.
    let (_d, s, row) = fixture_with_evidence("token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ok");
    let ctx = ActionCtx { store: &s, row: &row, now: t0() };
    let out = executor_for("collect_logs").unwrap().run(&ctx, &Default::default()).unwrap();
    assert!(!out.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"), "{out}");
    assert!(out.contains("ok"), "{out}");
}

#[test]
fn an_unknown_verb_has_no_executor() {
    // Pairs with Task 1's Privileged fallback: an unclassified verb is both
    // gated AND unrunnable. Two independent defences, on purpose.
    assert!(executor_for("apply_known_remedy").is_none());
    assert!(executor_for("rerun").is_none());
    assert!(executor_for("nonsense").is_none());
}

#[test]
fn every_executor_is_registered_under_the_verb_it_reports() {
    for v in ["notify", "collect_logs", "reschedule_monitor"] {
        assert_eq!(executor_for(v).unwrap().verb(), v);
    }
}
```

- [ ] **Step 2: Run to verify they fail.** `cargo nextest run -p mur-core -E 'test(/monitor::actions/)'`

- [ ] **Step 3: Implement**

```rust
pub struct ActionCtx<'a> {
    pub store: &'a MonitorStore,
    pub row: &'a MonitorRow,
    pub now: DateTime<Utc>,
}

pub trait ActionExecutor: Sync {
    fn verb(&self) -> &'static str;
    /// `Ok(summary)` → `ActionState::Done`; `Err(reason)` → `Failed`.
    /// Never panics, never touches the network in this slice.
    fn run(
        &self,
        ctx: &ActionCtx<'_>,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, String>;
}

static NOTIFY: local::Notify = local::Notify;
static COLLECT: local::CollectLogs = local::CollectLogs;
static RESCHEDULE: local::Reschedule = local::Reschedule;

/// `None` means "this build cannot run that verb". Paired with Task 1's
/// `Privileged` fallback, an unclassified verb is both gated and unrunnable
/// — two independent defences, deliberately.
pub fn executor_for(verb: &str) -> Option<&'static dyn ActionExecutor> {
    match verb {
        "notify" => Some(&NOTIFY),
        "collect_logs" => Some(&COLLECT),
        "reschedule_monitor" => Some(&RESCHEDULE),
        _ => None,
    }
}
```

`local.rs` holds the three. `Notify::run` calls `store.append_event(&row.id, &row.cycle_id, "action_notify", payload, false, ctx.now)` and returns a one-line summary — it never calls a channel, because the shipped path is event → transition guard → queue → drain, and bypassing it yields a message with no delivery state and no dedup. `CollectLogs::run` builds the adapter for `row.source_type`, calls `SourceAdapter::observe`, and returns `mur_common::redact::redact_secrets(&obs.evidence)` — redacting in the executor, not relying on the store's redaction as the only line of defence. `Reschedule::run` computes the next check from `mur_monitor::backoff`'s existing unknown schedule (never a literal duration) and writes `next_check_at` plus `MonitorState::Sleeping`.

- [ ] **Step 4: Run to verify they pass.**

- [ ] **Step 5: Commit** — `feat(monitor): the three local action executors`.

---

### Task 4: The gate bridge

**Files:**
- Create: `mur-core/src/monitor/actions/gate.rs`
- Test: same file.

**Interfaces:**
- Consumes: `mur_core::hitl::gate::{gate, ActionRequest, GatePolicy, GateDecision}`, `mur_common::hitl::{RiskTier, Unanswered}`, Task 1's `risk::classify`.
- Produces: `pub fn channel_id_for(monitor_id: &str) -> String`, `pub fn decide(handle: &tokio::runtime::Handle, mur_home: &Path, row: &MonitorRow, action_type: &str, action_index: usize, params: &serde_json::Value, now: DateTime<Utc>) -> Result<GateDecision>`.

This is the task with the sharp edge. Three rules, none optional:

1. **`gate()` is `async`; the monitor tick is a plain OS thread that must never become a tokio worker.** Bridge with `handle.block_on(gate(...))`, where `handle` is a `tokio::runtime::Handle` passed down from `mur-daemon/src/main.rs` (it is `#[tokio::main]`, so `Handle::current()` there is valid). `Handle::block_on` from a non-runtime thread is the documented, correct use. What is *not* correct — and is the mirror image of the CRITICAL panic the predecessor slice shipped — is building blocking clients inside an async context or calling `block_on` from within a runtime worker.
2. **`GatePolicy { yes: false, unanswered: Unanswered::Defer, auto_approve_tiers: <from config> }`.** `yes: true` never appears on this path. `Defer` is not a preference: with no TTY, `Wait` would block the whole monitor thread for the gate timeout and stop every other monitor from being polled.
3. **The channel id is derived, not stored**: `format!("monitor-{monitor_id}")`. The gate matches approvals on `action_hash`, which already includes the channel id, so a stable derivation is all that is needed — and it means an approval survives anything that rewrites the monitor row.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_read_tier_action_is_allowed_without_asking_anyone() {
    let (_d, home, row) = fixture();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let d = decide(rt.handle(), &home, &row, "notify", 0, &json!({}), t0()).unwrap();
    assert!(d.allow, "{}", d.reason);
    assert!(!d.deferred);
}

#[test]
fn a_write_tier_action_defers_instead_of_blocking_the_thread() {
    // The property that keeps one gated action from stopping every other
    // monitor: no TTY means park and return at once.
    let (_d, home, row) = fixture();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let started = std::time::Instant::now();
    let d = decide(rt.handle(), &home, &row, "rerun", 0, &json!({}), t0()).unwrap();
    assert!(!d.allow);
    assert!(d.deferred, "unattended must defer, not wait: {}", d.reason);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "deferring must not wait on the gate timeout"
    );
}

#[test]
fn the_decision_carries_a_hash_the_caller_can_re_verify() {
    let (_d, home, row) = fixture();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let a = decide(rt.handle(), &home, &row, "rerun", 0, &json!({"job": "x"}), t0()).unwrap();
    let b = decide(rt.handle(), &home, &row, "rerun", 0, &json!({"job": "x"}), t0()).unwrap();
    let c = decide(rt.handle(), &home, &row, "rerun", 0, &json!({"job": "y"}), t0()).unwrap();
    assert_eq!(a.action_hash, b.action_hash, "same action, same pin");
    assert_ne!(a.action_hash, c.action_hash, "changing the params must invalidate the pin");
    assert!(!a.action_hash.is_empty());
}

#[test]
fn an_approval_already_on_the_channel_releases_the_gate() {
    // Write a HitlResponse for the exact action_hash, then gate again.
    // This is the whole point of deferring: the answer arrives later and
    // the NEXT tick proceeds without asking again.
    let (_d, home, row) = fixture();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = decide(rt.handle(), &home, &row, "rerun", 0, &json!({}), t0()).unwrap();
    approve_on_channel(&home, &channel_id_for(&row.id), &first.action_hash);
    let second = decide(rt.handle(), &home, &row, "rerun", 0, &json!({}), t0()).unwrap();
    assert!(second.allow, "a settled approval must release the gate: {}", second.reason);
}

#[test]
fn the_channel_id_is_derived_from_the_monitor_id() {
    assert_eq!(channel_id_for("01a0a75e-c532"), "monitor-01a0a75e-c532");
}
```

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Implement.** `decide` builds `ActionRequest { tier: risk::classify(action_type), tool_name: format!("monitor:{action_type}"), tool_input: params.clone(), step_or_call_id: format!("{}:{action_index}", row.cycle_id), agent_id: format!("monitor:{}", row.id), summary: <one line naming the monitor, its source and the verb> }`, then `handle.block_on(gate(mur_home, &channel_id_for(&row.id), &req, &policy, None, None))`.

- [ ] **Step 4: Run to verify they pass.**

- [ ] **Step 5: Commit** — `feat(monitor): route actions through the repo's one risk gate`.

---

### Task 5: Drain `ActionPending` on the tick

**Files:**
- Modify: `mur-core/src/monitor/service.rs`, `mur-daemon/src/monitor_tick.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces: `pub const DRAIN_MAX_ACTIONS_PER_TICK: usize = 10;`, `pub struct ActionReport { pub executed: usize, pub blocked: usize, pub failed: usize, pub exhausted: usize }`, `pub fn drain_actions(mur_home: &Path, handle: &tokio::runtime::Handle, now: DateTime<Utc>) -> Result<ActionReport>`.

The loop, per monitor in `ActionPending` (plus every `blocked` row from `pending_actions`):

1. Resolve the action list: `Outcome::Succeeded` → `spec.actions.on_success`, a terminal failure → `on_failure`, and **`unknown` → `on_unknown` only** — never `on_failure`, because a query that failed is not work that failed.
2. For each action, by index: `claim_action`. `Ok(false)` → skip, it already has a row.
3. `decide(...)`. `allow` → run the executor; `deferred` → `block_action(key, hitl_id)` and move on; refused → `finish_action(Failed, reason)`.
4. **Re-verify the pin immediately before executing.** `GateDecision::action_hash` is the pin and the gate's own doc says the caller MUST re-verify it, fail-closed on mismatch.
5. Count a *remediation* attempt only for actions above `Read` tier, in **`MonitorRow::remediation_attempts`** — the field already exists on the row and nothing has ever incremented it. Do not derive the count by taking a max over `monitor_actions`; that conflates a retried single action with three distinct remedies. When it reaches `policy.max_remediation_attempts`, stop, set `Exhausted`, and append an `exhausted` event — the shipped notifier already treats `exhausted` as notifiable, so this needs no notification code.
6. When every action for the monitor is `Done` or `Failed` (none `Claimed` or `Blocked`), the monitor moves `ActionPending` → `Completed`.

`drain_actions` must never fail the tick (same rule as `drain_notifications`) and must never create a store — `open_existing`, and a no-op empty report when absent. **Assert that on the filesystem**, not only on the returned report: an empty report comes back either way, so a return-value assertion passes without the fix. The shipped slice has this test twice; copy its shape.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_home_with_no_store_drains_no_actions_and_creates_nothing() {
    let d = tempfile::tempdir().unwrap();
    let dir = mur_monitor::store::db_dir(d.path());
    assert!(!dir.exists());
    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(drain_actions(d.path(), rt.handle(), t0()).unwrap(), ActionReport::default());
    assert!(!dir.exists(), "draining must not create the store");
}

#[test]
fn a_settled_monitor_with_one_read_action_runs_it_once_and_completes() {
    let (_d, home, id) = settled_with(&["notify"], Outcome::Succeeded);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(first.executed, 1);
    let second = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(second.executed, 0, "the claim must stop a second run");
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Completed);
}

#[test]
fn an_unknown_outcome_runs_on_unknown_and_never_on_failure() {
    // spec's central invariant, at the action layer: a monitor that cannot
    // read its source must not act as though the work failed.
    let (_d, home, id) = settled_unknown_with_both_lists();
    let rt = tokio::runtime::Runtime::new().unwrap();
    drain_actions(&home, rt.handle(), t0()).unwrap();
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    let verbs: Vec<_> = s.actions_for(&id).unwrap().iter()
        .map(|a| a.action_key.split(':').nth(3).unwrap().to_string()).collect();
    assert_eq!(verbs, vec!["reschedule_monitor"], "on_failure must not have run: {verbs:?}");
}

#[test]
fn a_gated_action_blocks_the_action_without_failing_the_monitor() {
    let (_d, home, id) = settled_with(&["rerun"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!((r.executed, r.blocked, r.failed), (0, 1, 0));
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    let m = s.get(&id).unwrap().unwrap();
    assert_eq!(m.state, MonitorState::AwaitingApproval, "blocked is not failed");
}

#[test]
fn reaching_the_remediation_cap_exhausts_the_monitor_and_stops_acting() {
    // policy.max_remediation_attempts = 3 (the spec's default and the
    // fixture's value — read from the policy, never a literal 3 in code).
    let (_d, home, id) = settled_with_failing_remediation();
    let rt = tokio::runtime::Runtime::new().unwrap();
    for _ in 0..6 { drain_actions(&home, rt.handle(), t0()).unwrap(); }
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    let m = s.get(&id).unwrap().unwrap();
    assert_eq!(m.state, MonitorState::Exhausted);
    let attempts = s.actions_for(&id).unwrap().iter().map(|a| a.attempt).max().unwrap();
    let cap = m.spec.policy.max_remediation_attempts;
    assert!(attempts <= cap, "attempts {attempts} exceeded the cap {cap}");
    let kinds: Vec<_> = s.events(&id).unwrap().into_iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&"exhausted".to_string()), "{kinds:?}");
}

#[test]
fn a_failing_executor_does_not_fail_the_tick() {
    // spec §錯誤處理. Same rule the notification drain follows.
    let (_d, home, _id) = settled_with(&["collect_logs"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = drain_actions(&home, rt.handle(), make_the_adapter_error()).unwrap();
    assert_eq!(r.failed, 1);
    // `unwrap()` above IS the assertion: an Err would have panicked here.
}
```

- [ ] **Step 2–4: Red, implement, green.**

- [ ] **Step 5: Wire the daemon.** `monitor_tick::spawn(mur_home, handle)` — take a `tokio::runtime::Handle` and call `drain_actions` after `tick_once` and before `drain_notifications`, so an action that appends an event gets it delivered on the same tick rather than 15 s later. Log only when something happened, matching the two lines already there.

- [ ] **Step 6: Mutation-check the `unknown` routing.** Change the `unknown` arm to use `on_failure` and confirm `an_unknown_outcome_runs_on_unknown_and_never_on_failure` goes red. Verify the mutation is present before trusting the result.

- [ ] **Step 7: Commit** — `feat(monitor): drain action-pending monitors on the tick`.

---

### Task 6: `show` renders actions and approvals

**Files:**
- Modify: `mur-core/src/cmd/monitor.rs`, `mur-core/src/cmd/monitor_tests.rs`

Render an `actions:` block between the existing `notifications:` block and `recent observations:`, omitted entirely when there are none. Each line: the verb, its tier, its state, its attempt count, and — when blocked — the exact command that unblocks it, which is the existing one:

```
  actions:
    rerun         write  blocked (1 attempt)  → mur channel approve monitor-01a0a75e hitl-abc
    collect_logs  read   done
```

The approve command is not new surface: `mur channel approve <channel_id> <hitl_id>` already exists and the gate already reads its result. Printing it is the whole UX of this slice — a blocked action the user cannot find the command for is a monitor that has silently stopped.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn show_renders_actions_with_the_command_that_unblocks_them() {
    let (d, id) = home_with_blocked_action("rerun", "hitl-abc");
    let out = go(d.path(), MonitorAction::Show { id: id.clone(), history: false }).unwrap();
    assert!(out.contains("  actions:"), "{out}");
    assert!(
        out.lines().any(|l| l.contains("rerun") && l.contains("blocked")
            && l.contains("mur channel approve") && l.contains("hitl-abc")),
        "a blocked action must print the command that releases it: {out}"
    );
}

#[test]
fn show_omits_the_actions_section_when_there_are_none() {
    let (d, id) = home_with_monitor();
    let out = go(d.path(), MonitorAction::Show { id, history: false }).unwrap();
    assert!(!out.contains("actions:"), "{out}");
    assert!(out.contains("recent observations:"), "{out}");
}
```

- [ ] **Steps 2–4: Red, implement, green.**
- [ ] **Step 5: Commit** — `feat(monitor): show actions and how to unblock them`.

---

## Part C — auto-registration

Spec §自動註冊邊界. Today every monitor is hand-made with `mur monitor add`. This part makes an agent that starts trackable async work end up monitored — and, when registration fails, makes it say so instead of silently losing the work.

The spec's four clauses, and what each becomes:

1. Source allows record-then-start → persist `Registering` first, start, then fill in the reference. → Task 7.
2. Source only yields an id after starting → register immediately after; **if that fails, the turn must explicitly report "work started but NOT monitored" with the trackable id**, and the spec goes to a recoverable outbox. → Tasks 7 + 8.
3. A vague promise with no queryable reference → **do not claim it is monitored.** → Task 7 (a validation refusal, not an outbox row).
4. The user can still add the same spec by hand. → already shipped.

### Task 7: `register_or_outbox`

**Files:**
- Create: `mur-monitor/src/register.rs`, `mur-monitor/src/store/outbox.rs`
- Modify: `mur-monitor/src/lib.rs`, `mur-monitor/src/store/mod.rs`

**Interfaces:**
- Produces: `pub enum Registered { Monitored(String), Outboxed(String), Refused(String) }`, `pub fn register_or_outbox(store: &MonitorStore, spec: &MonitorSpec, now) -> Registered`, `pub fn begin_registering(store, spec, now) -> Result<String>`, `pub fn attach_reference(store, id, reference, now) -> Result<()>`; and on `MonitorStore`: `outbox_enqueue`, `outbox_due`, `outbox_drop`, `outbox_record_failure`.

`monitor_registration_outbox` already exists (`id`, `spec_json`, `created_at`, `attempts`, `last_error`). No schema change.

`Registered` is deliberately three-valued, and the three are not interchangeable:
- `Monitored(id)` — a real monitor exists and the daemon will poll it.
- `Outboxed(id)` — the work is running and is **not** monitored yet; the caller MUST say so to the user. Returning this and printing nothing is the failure the spec's clause 2 exists to prevent.
- `Refused(reason)` — no queryable reference, so nothing was created and nothing is pending. The caller must not claim monitoring.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_spec_with_a_queryable_reference_is_monitored_immediately() {
    let (_d, s) = store();
    match register_or_outbox(&s, &spec_with_reference("run-1"), t0()) {
        Registered::Monitored(id) => assert!(s.get(&id).unwrap().is_some()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_spec_with_no_reference_is_refused_and_creates_nothing() {
    // spec §自動註冊邊界 clause 3: 「不得聲稱已監看」.
    let (_d, s) = store();
    let before = s.list(&ListFilter::default()).unwrap().len();
    match register_or_outbox(&s, &spec_with_reference(""), t0()) {
        Registered::Refused(r) => assert!(!r.is_empty()),
        other => panic!("must refuse, got {other:?}"),
    }
    assert_eq!(s.list(&ListFilter::default()).unwrap().len(), before, "nothing may be created");
    assert!(s.outbox_due(t0(), 10).unwrap().is_empty(), "and nothing may be queued");
}

#[test]
fn a_registration_that_fails_lands_in_the_outbox_not_on_the_floor() {
    // spec clause 2. The store is made to fail the insert; the spec must
    // survive somewhere recoverable.
    let (_d, s) = store_that_fails_create();
    match register_or_outbox(&s, &spec_with_reference("run-1"), t0()) {
        Registered::Outboxed(_) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(s.outbox_due(t0(), 10).unwrap().len(), 1);
}

#[test]
fn begin_registering_then_attach_makes_a_pollable_monitor() {
    // clause 1: record first, start, then fill in the reference. Until the
    // reference lands the monitor is `Registering`, which `is_claimable`
    // excludes — the daemon must not poll a monitor with no reference yet.
    let (_d, s) = store();
    let id = begin_registering(&s, &spec_with_reference(""), t0()).unwrap();
    let m = s.get(&id).unwrap().unwrap();
    assert_eq!(m.state, MonitorState::Registering);
    assert!(!m.state.is_claimable(), "a registering monitor must not be polled");
    attach_reference(&s, &id, "run-7", t0()).unwrap();
    let m = s.get(&id).unwrap().unwrap();
    assert_eq!(m.spec.source.reference, "run-7");
    assert!(m.state.is_claimable(), "attaching the reference must make it pollable");
}

#[test]
fn the_idempotency_key_returns_the_existing_monitor_rather_than_a_second_one() {
    // spec §建立時驗證 clause 6, already enforced by `create`; asserted here
    // because auto-registration is the path that will actually retry.
    let (_d, s) = store();
    let a = register_or_outbox(&s, &spec_with_reference("run-1"), t0());
    let b = register_or_outbox(&s, &spec_with_reference("run-1"), t0());
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert_eq!(s.list(&ListFilter::default()).unwrap().len(), 1);
}
```

- [ ] **Steps 2–4: Red, implement, green.**
- [ ] **Step 5: Commit** — `feat(monitor): register a monitor atomically, or outbox it`.

---

### Task 8: Drain the outbox on the tick

**Files:**
- Modify: `mur-core/src/monitor/service.rs`, `mur-daemon/src/monitor_tick.rs`

**Interfaces:**
- Produces: `pub const OUTBOX_MAX_PER_TICK: usize = 10;`, `pub const OUTBOX_MAX_ATTEMPTS: u32 = 8;`, `pub fn drain_outbox(mur_home: &Path, now: DateTime<Utc>) -> Result<OutboxReport>` with `OutboxReport { registered, retried, gave_up }`.

Retry with the existing `mur_monitor::backoff` schedule. At `OUTBOX_MAX_ATTEMPTS` the row is dropped and an event is appended so the give-up is visible rather than silent — the same discipline as a parked notification.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_home_with_no_store_drains_no_outbox_and_creates_nothing() {
    let d = tempfile::tempdir().unwrap();
    let dir = mur_monitor::store::db_dir(d.path());
    assert!(!dir.exists());
    assert_eq!(drain_outbox(d.path(), t0()).unwrap(), OutboxReport::default());
    assert!(!dir.exists());
}

#[test]
fn an_outboxed_spec_becomes_a_real_monitor_on_the_next_tick() {
    let (_d, home, _) = home_with_outboxed_spec("run-1");
    let r = drain_outbox(&home, t0()).unwrap();
    assert_eq!(r.registered, 1);
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    assert_eq!(s.list(&ListFilter::default()).unwrap().len(), 1);
    assert!(s.outbox_due(t0(), 10).unwrap().is_empty(), "a registered row must leave the outbox");
}

#[test]
fn a_spec_that_keeps_failing_is_given_up_on_visibly_not_silently() {
    let (_d, home, _) = home_with_permanently_unregisterable_spec();
    let mut gave_up = 0;
    for i in 0..(OUTBOX_MAX_ATTEMPTS + 2) {
        gave_up += drain_outbox(&home, t0() + chrono::Duration::hours(i as i64)).unwrap().gave_up;
    }
    assert_eq!(gave_up, 1);
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    assert!(s.outbox_due(t0() + chrono::Duration::days(9), 10).unwrap().is_empty());
}
```

- [ ] **Steps 2–4: Red, implement, green.** Call `drain_outbox` first in the tick body, before `tick_once` — a spec registered this tick should be polled this tick, not next.
- [ ] **Step 5: Commit** — `feat(monitor): drain the registration outbox on the tick`.

---

### Task 9: One real call site — `mur fleet run`

**Files:**
- Modify: `mur-core/src/cmd/fleet/run.rs` (around the `run_id` mint at `run.rs:403-435`)
- Test: `mur-core/src/cmd/fleet/` tests, or a new `run_register_tests.rs` beside it if the existing file is near the 800-line cap.

`mur fleet run` mints a `run_id` that `MurRunAdapter` already knows how to read — it is the one place in the repo where trackable async work starts and the reader already exists. Wire exactly this one; the others (GitHub Actions, Codex/Claude Code) are the same three lines once the shape is proven, and belong with whatever plan touches those paths.

Behaviour: after the run id is minted, build a `mur_run` spec with `idempotency_key: format!("fleet:{fleet}:{run_id}")` and `created_by.actor = "agent:fleet"` with a `reason`, then `register_or_outbox`. **Registration never fails the run** — the run is already going; a monitor is bookkeeping. On `Outboxed` or `Refused`, print one line saying the work is running and is not monitored, naming the run id.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_fleet_run_registers_a_monitor_for_its_run_id() {
    let (_d, home) = fleet_home();
    let out = run_fleet(&home, "demo");
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    let refs: Vec<_> = s.list(&ListFilter::default()).unwrap().iter()
        .map(|m| m.spec.source.reference.clone()).collect();
    assert!(refs.contains(&out.run_id), "{refs:?} should contain {}", out.run_id);
}

#[test]
fn a_failed_registration_says_so_and_does_not_fail_the_run() {
    // spec §自動註冊邊界 clause 2: the turn must report the work started
    // but is not monitored, with the trackable id.
    let (_d, home) = fleet_home_where_registration_fails();
    let out = run_fleet(&home, "demo");
    assert!(out.ok, "the run itself must still succeed");
    assert!(out.stderr.contains("not monitored"), "{}", out.stderr);
    assert!(out.stderr.contains(&out.run_id), "must name the id: {}", out.stderr);
}

#[test]
fn registering_twice_for_the_same_run_id_does_not_make_two_monitors() {
    let (_d, home) = fleet_home();
    let out = run_fleet(&home, "demo");
    let _ = run_fleet_again_with_same_id(&home, &out.run_id);
    let s = MonitorStore::open_existing(&home).unwrap().unwrap();
    assert_eq!(s.list(&ListFilter::default()).unwrap().len(), 1);
}
```

- [ ] **Steps 2–4: Red, implement, green.**
- [ ] **Step 5: Commit** — `feat(fleet): a fleet run registers its own monitor`.

---

### Task 10: Docs

**Files:** `CLAUDE.md` (the `mur monitor` bullet), `README.md` (the "Durable monitors" section).

The existing CLAUDE.md bullet says *"Actions, HITL, and auto-registration remain out of scope — … steps 5–8."* That becomes false. Rewrite it to name only what is still out of scope — **AgentResolver, `apply_known_remedy`, `rerun`/`start_downstream`** — and add:

> Terminal actions run under the repo's risk gate: `read`-tier verbs (`notify`, `collect_logs`, `reschedule_monitor`) run unattended, anything above parks a pinned approval and the monitor sits in `awaiting-approval` until `mur channel approve monitor-<id> <hitl-id>`; the tier comes from a fixed table and is never taken from the action's own text. Remediation stops at `policy.max_remediation_attempts` (default 3) and the monitor goes `exhausted`. A `mur fleet run` registers its own monitor; when that fails the run still proceeds and says the work is not monitored.

README, after the existing paragraphs:

> A monitor can act on what it finds. Reading-only steps — recording evidence, rescheduling the next check, sending the notification — run on their own. Anything that would change something outside MUR stops and asks: the request is pinned to the exact action, and approving a different action never releases it. MUR gives up after three attempts rather than retrying forever, and says so.

- [ ] **Step 1:** Make both edits.
- [ ] **Step 2:** `mur verify --file README.md` and `--file CLAUDE.md` to catch stale claims.
- [ ] **Step 3: Commit** — `docs(monitor): the action executor and auto-registration`.

---

## Self-review

Run against the spec with fresh eyes after the plan was written. Five defects found and fixed inline; they are recorded rather than quietly patched, because each is a thing an implementer would otherwise have shipped.

**Fixed during review:**

1. **The action key would never have collided.** The first draft said `observed-terminal-version` is the monitor's `fence` without saying why that is stable. The fence bumps on *every* lease claim, so a naive reading gives every tick a different key, no claim ever collides, and every action runs on every tick — the exact failure the claim exists to prevent. It is in fact stable, because a terminal observation moves the monitor to `ActionPending`/`Completed` and `is_claimable` is `Active | Sleeping` only, so the monitor is never claimed again and the fence freezes. That reasoning is now in Task 1, with an instruction to verify it against `is_claimable` rather than trust the plan.
2. **`cycle_id` discriminates nothing.** `finish_cycle` stamps `finished_at` on the `monitor_cycles` row and never mints a new `cycle_id` on the monitor — the same fact that broke the predecessor slice's notification dedup. It stays in the key because the spec's format says so, and Task 1 now says explicitly that it is not carrying uniqueness.
3. **`events_for` does not exist.** Three tasks called it. The real reader is `MonitorStore::events(&self, id)` (`store/observe.rs:234`). Fixed.
4. **The remediation counter already exists and the plan was about to duplicate it.** `MonitorRow::remediation_attempts` is on the row and nothing has ever incremented it. Task 5 now uses it, with a note on why deriving the count from a max over `monitor_actions` is wrong: it conflates one action retried three times with three distinct remedies.
5. **Two placeholder bodies and one prose-only implementation step.** Task 2's `actions_for` / `pending_actions` were `/* SELECT … */` comments and Task 3's Step 3 described the registry instead of showing it. Both now carry real code, including a `row_to_action` that returns `None` for an unparseable state rather than panicking an older build mid-tick.

**Spec coverage, §混合處置策略's six steps:**

| Step | Task | Note |
|---|---|---|
| 1 結構化規則 | 5 | The `Actions` schema *is* the rule language — `on_success` / `on_failure` / `on_unknown`. There is no richer matcher (`error code`, `branch`, `attempt`) in the shipped types, so the plan implements outcome routing and nothing invents a DSL. |
| 2 已知補救 | — | Out of scope: no remedy catalogue exists. Named in "Deliberately out of scope". |
| 3 AgentResolver | — | Out of scope, its own plan. |
| 4 風險閘門 | 1, 4, 5 | The repo's existing gate, not a new one. |
| 5 重新觀測 / child cycle | — | **Unreachable in this slice, by construction.** A child cycle can only arise from a remedy that starts new work, and every such verb (`rerun`, `start_downstream`, `apply_known_remedy`) is out of scope. `monitor_cycles.parent_cycle_id` already exists and stays unwritten. Stated here so a reviewer does not charge it as a gap. |
| 6 上限 | 5 | `policy.max_remediation_attempts`, read from the policy. |

**§冪等與事件紀錄:** the key (Task 1) and the claim (Task 2) are covered. Two clauses are unreachable and deliberately so: passing a native idempotency key to an external API, and "if the external action succeeded but we crashed before recording it, check external state first and escalate to a human rather than blindly retrying" — both require an action that touches an external system, and none is in scope. The history list is covered by Task 5 appending claim/result/`exhausted` events and Task 2 storing the redacted result.

**§風險政策:** the low-risk list maps to the three `Read` verbs. One deliberate divergence: the spec calls a rerun of a job 「明確標記 flaky 且未達上限」 low-risk, and Task 1 classifies `rerun` as `Write`. Nothing in the shipped schema can establish "explicitly marked flaky", so the condition the spec attaches its low-risk verdict to cannot be evaluated — classifying low on an unverifiable precondition is how an unattended process ends up doing something nobody approved. Recorded in Task 1's table comment.

**Type consistency:** `ActionCtx` / `ActionExecutor` / `executor_for` (Task 3) are consumed by Task 5. `GateDecision` (Task 4) is consumed by Task 5, which re-verifies `action_hash` before executing. `ActionRow` / `ActionState` / `RiskTier` (Tasks 1–2) are consumed by Tasks 5 and 6. `Registered` (Task 7) is consumed by Tasks 8 and 9. One thing an implementer must watch: `MonitorRow` carries both `spec.source.reference` and a denormalized top-level `reference`, so Task 7's `attach_reference` has to write **both** or `list` and `show` will disagree with the adapter.

**Known ceilings, named:** the drain is bounded at `DRAIN_MAX_ACTIONS_PER_TICK = 10` per tick, so a large fan-out spreads across ticks; a deferred approval is only re-checked when the monitor's action is re-drained, i.e. within one `TICK_INTERVAL` (15 s) of the approval landing, not instantly; and the gate reads the whole channel log to find a prior decision, which is fine at a monitor's event volume and would not be at a chat channel's.

---
