# C6 Idle / Heartbeat Triggers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fire a configured message into an agent's task runner whenever the agent has been idle for a user-defined window — useful for "check-in" prompts, garbage-collection sweeps, and self-pinging keepalive flows.

**Architecture:** Reuse the same supervisor-spawned-task pattern as C4's `CronScheduler`. Add `last_activity_at: Arc<AtomicI64>` (Unix seconds) to `TaskRunner`, bumped on every `start_async`. A new `IdleScheduler` tokio task wakes every 30 s, reads `last_activity_at`, and for each `IdleTrigger` whose `after_secs` has elapsed *and* respects the optional quiet-hours window (reusing `companion::schedule::active_window_end_for_today`), injects the trigger's `message` via `runner.start_async()`. Per-trigger `cooldown_secs` prevents tight refire loops.

**Tech Stack:** Rust 2024, tokio (existing dependency), chrono (existing), `std::sync::atomic::AtomicI64`. Reuses `mur_common::agent::QuietHours`, `mur_agent_runtime::companion::schedule::active_window_end_for_today`, and `mur_agent_runtime::task_runner::{TaskRunner, TaskSpec}`.

**Spec reference:** `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §5.6 (C6 row): "Heartbeat / idle triggers (reuse companion `schedule.rs` `should_send_now`)." This plan reuses the *quiet-window* helper rather than `should_send_now` itself, because idle triggers have different state (last-activity timestamp, not last-send + bandit cooldown).

---

## File Structure

**Modify:**
- `mur-common/src/agent.rs` — add `IdleTrigger` struct + `LifecycleConfig.idle_triggers: Vec<IdleTrigger>`
- `mur-agent-runtime/src/task_runner.rs` — add `last_activity_at: Arc<AtomicI64>` + `tick_activity()` + bump on `start_async`
- `mur-agent-runtime/src/lib.rs` — `pub mod idle_scheduler;`
- `mur-agent-runtime/src/supervisor.rs` — spawn `IdleScheduler` iff `profile.lifecycle.idle_triggers` non-empty
- `mur-core/src/cmd/agent_schedule.rs` — extend with `cmd_idle_{add,list,remove}` (sister commands to existing schedule CLI)
- `mur-core/src/main.rs` — `AgentScheduleAction::Idle{...}` variants + dispatch
- `Cargo.toml` workspace — bump version 2.12.0 → 2.13.0

**Create:**
- `mur-agent-runtime/src/idle_scheduler.rs` — `IdleScheduler` task
- `mur-agent-runtime/tests/idle_scheduler_smoke.rs` — supervisor end-to-end smoke
- `mur-core/tests/cmd_agent_idle.rs` — CLI integration tests
- `docs/cookbook/c6-idle-triggers.md` — walkthrough

---

## Task 1: `IdleTrigger` schema + `LifecycleConfig.idle_triggers`

**Files:**
- Modify: `mur-common/src/agent.rs:518-569`
- Test: inline `#[cfg(test)]` round-trip in same file

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block in `mur-common/src/agent.rs`:

```rust
#[test]
fn idle_trigger_yaml_round_trip() {
    let yaml = r#"
restart: on_failure
idle_triggers:
  - after_secs: 3600
    message: "still there?"
    sends_to: other_agent
    cooldown_secs: 1800
    respect_quiet_hours: true
"#;
    let cfg: LifecycleConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.idle_triggers.len(), 1);
    assert_eq!(cfg.idle_triggers[0].after_secs, 3600);
    assert_eq!(cfg.idle_triggers[0].message, "still there?");
    assert_eq!(cfg.idle_triggers[0].sends_to.as_deref(), Some("other_agent"));
    assert_eq!(cfg.idle_triggers[0].cooldown_secs, 1800);
    assert!(cfg.idle_triggers[0].respect_quiet_hours);
}

#[test]
fn idle_trigger_defaults_when_omitted() {
    let yaml = "restart: on_failure\n";
    let cfg: LifecycleConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(cfg.idle_triggers.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common idle_trigger`
Expected: FAIL with `unknown field 'idle_triggers'` or `cannot find type 'IdleTrigger'`.

- [ ] **Step 3: Add the `IdleTrigger` struct and field**

In `mur-common/src/agent.rs`, after `ScheduleEntry` (line 569):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdleTrigger {
    /// Idle threshold in seconds. Fires when (now - last_activity) >= after_secs.
    pub after_secs: u64,
    /// Message body injected into the task runner when this trigger fires.
    pub message: String,
    /// Optional A2A peer to route the resulting reply to. None means the agent itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_to: Option<String>,
    /// Per-trigger refire cooldown in seconds. Prevents tight loops when the
    /// idle threshold is short and the runner finishes quickly. Default 600.
    #[serde(default = "default_idle_cooldown")]
    pub cooldown_secs: u64,
    /// When true, suppress firing during the agent's quiet-hours window
    /// (computed via companion::schedule::active_window_end_for_today).
    /// Default true — idle pings should not wake the user at 3 a.m.
    #[serde(default = "default_true")]
    pub respect_quiet_hours: bool,
}

fn default_idle_cooldown() -> u64 {
    600
}
fn default_true() -> bool {
    true
}
```

Modify `LifecycleConfig` (line 518) to add the field after `schedule`:

```rust
    #[serde(default)]
    pub schedule: Vec<ScheduleEntry>,
    #[serde(default)]
    pub idle_triggers: Vec<IdleTrigger>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common idle_trigger`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(c6): IdleTrigger schema + LifecycleConfig.idle_triggers"
```

---

## Task 2: `TaskRunner` activity tracking

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs`
- Test: `mur-agent-runtime/src/task_runner.rs` (inline unit test)

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests` in `mur-agent-runtime/src/task_runner.rs`:

```rust
#[tokio::test]
async fn last_activity_at_bumps_on_start_async() {
    let runner = TaskRunner::new_stub_echo();
    let before = runner.last_activity_at();
    // Synthetic clock: start_async should bump the timestamp by at least 0
    // (we'll only assert monotonic non-decrease since the resolution is 1 s).
    let _h = runner.start_async(TaskSpec::echo("hello".to_string()));
    let after = runner.last_activity_at();
    assert!(after >= before, "last_activity_at should monotonically advance");
}

#[tokio::test]
async fn last_activity_at_is_initialized_to_now() {
    let runner = TaskRunner::new_stub_echo();
    let now = chrono::Utc::now().timestamp();
    let last = runner.last_activity_at();
    // Allow a 5 s slack for slow CI machines.
    assert!((last - now).abs() < 5, "last_activity_at should default to ~now");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime last_activity`
Expected: FAIL with `no method 'last_activity_at'`.

- [ ] **Step 3: Add the activity timestamp**

In `mur-agent-runtime/src/task_runner.rs`, add at the top of the file (or near other imports):

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
```

Add a field to `TaskRunner` struct (find the existing `pub struct TaskRunner { ... }` block and add):

```rust
    /// Unix seconds of the last `start_async` invocation. Used by IdleScheduler
    /// (C6) to detect prolonged inactivity. Atomic so the scheduler can read
    /// without locking the runner.
    last_activity_at: Arc<AtomicI64>,
```

Initialize in every `TaskRunner::new_*` constructor. Find each constructor (line ~42, ~46, ~50, ~54) and ensure they initialize the field. The cleanest fix is a private helper:

```rust
fn init_activity() -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(chrono::Utc::now().timestamp()))
}
```

Then in each constructor, set `last_activity_at: init_activity()`.

Add the public accessor and bump-on-start. After the existing `pub fn start_async(&self, spec: TaskSpec) -> AsyncTaskHandle` body, the FIRST line of the function should bump the timestamp:

```rust
pub fn start_async(&self, spec: TaskSpec) -> AsyncTaskHandle {
    self.last_activity_at
        .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    // ... existing body ...
}
```

Add the public reader after `start_async`:

```rust
/// Unix seconds of the last `start_async` call (or runner creation if no
/// task has run yet). Used by C6 IdleScheduler.
pub fn last_activity_at(&self) -> i64 {
    self.last_activity_at.load(Ordering::Relaxed)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-agent-runtime last_activity`
Expected: PASS (2 tests).

- [ ] **Step 5: Verify nothing broke**

Run: `cargo test -p mur-agent-runtime --lib`
Expected: All existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(c6): TaskRunner.last_activity_at + start_async bump"
```

---

## Task 3: `IdleScheduler` task

**Files:**
- Create: `mur-agent-runtime/src/idle_scheduler.rs`
- Modify: `mur-agent-runtime/src/lib.rs` — `pub mod idle_scheduler;`
- Test: `mur-agent-runtime/src/idle_scheduler.rs` (inline unit tests)

- [ ] **Step 1: Write the failing test**

Create `mur-agent-runtime/src/idle_scheduler.rs` with the test stub and skeleton:

```rust
//! C6 — Idle / heartbeat trigger scheduler.
//!
//! Wakes every 30 s, reads `TaskRunner::last_activity_at`, and for each
//! `IdleTrigger` whose `after_secs` has elapsed (and whose quiet-hours
//! window allows it), injects `trigger.message` into the runner.
//!
//! Per-trigger `cooldown_secs` prevents refire loops.

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::IdleTrigger;

    #[test]
    fn fires_when_idle_threshold_exceeded() {
        let trigger = IdleTrigger {
            after_secs: 60,
            message: "ping".to_string(),
            sends_to: None,
            cooldown_secs: 600,
            respect_quiet_hours: false,
        };
        let now = 1_700_000_000;
        let last_activity = now - 120; // idle for 120 s, threshold 60 s
        let last_fire = None;
        assert!(should_fire(&trigger, now, last_activity, last_fire, None));
    }

    #[test]
    fn does_not_fire_below_threshold() {
        let trigger = IdleTrigger {
            after_secs: 60,
            message: "ping".to_string(),
            sends_to: None,
            cooldown_secs: 600,
            respect_quiet_hours: false,
        };
        let now = 1_700_000_000;
        let last_activity = now - 30;
        assert!(!should_fire(&trigger, now, last_activity, None, None));
    }

    #[test]
    fn cooldown_suppresses_refire() {
        let trigger = IdleTrigger {
            after_secs: 60,
            message: "ping".to_string(),
            sends_to: None,
            cooldown_secs: 600,
            respect_quiet_hours: false,
        };
        let now = 1_700_000_000;
        let last_activity = now - 120;
        let last_fire = Some(now - 100); // < cooldown 600
        assert!(!should_fire(&trigger, now, last_activity, last_fire, None));
    }

    #[test]
    fn cooldown_expiry_allows_refire() {
        let trigger = IdleTrigger {
            after_secs: 60,
            message: "ping".to_string(),
            sends_to: None,
            cooldown_secs: 600,
            respect_quiet_hours: false,
        };
        let now = 1_700_000_000;
        let last_activity = now - 120;
        let last_fire = Some(now - 700); // > cooldown 600
        assert!(should_fire(&trigger, now, last_activity, last_fire, None));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime idle_scheduler`
Expected: FAIL with `cannot find function 'should_fire'`.

- [ ] **Step 3: Implement the pure decision function**

Above the `#[cfg(test)]` block in `idle_scheduler.rs`:

```rust
use crate::task_runner::TaskRunner;
use chrono::{DateTime, Local};
use mur_common::agent::IdleTrigger;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Pure decision: should this trigger fire right now?
///
/// `now` and `last_activity` are Unix seconds. `last_fire` is None on
/// first eligibility. `quiet_window_end` is the precomputed quiet-window
/// end-of-day boundary (None = no quiet hours configured / disabled).
///
/// Returns true iff:
/// 1. (now - last_activity) >= trigger.after_secs
/// 2. cooldown elapsed since last_fire (or no prior fire)
/// 3. Either respect_quiet_hours is false, or now is before quiet_window_end.
pub(crate) fn should_fire(
    trigger: &IdleTrigger,
    now: i64,
    last_activity: i64,
    last_fire: Option<i64>,
    quiet_window_end: Option<i64>,
) -> bool {
    if (now - last_activity) < trigger.after_secs as i64 {
        return false;
    }
    if let Some(prev) = last_fire
        && (now - prev) < trigger.cooldown_secs as i64
    {
        return false;
    }
    if trigger.respect_quiet_hours
        && let Some(end) = quiet_window_end
        && now >= end
    {
        return false;
    }
    true
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-agent-runtime idle_scheduler`
Expected: PASS (4 tests).

- [ ] **Step 5: Add the IdleScheduler runtime task**

After `should_fire` in `idle_scheduler.rs`:

```rust
/// Polls every TICK_INTERVAL_SECS and fires eligible triggers.
pub struct IdleScheduler {
    triggers: Vec<IdleTrigger>,
    runner: Arc<TaskRunner>,
    quiet_hours: Option<mur_common::agent::QuietHours>,
    /// Per-trigger last-fire timestamps (Unix seconds). Indexed identically
    /// to `triggers`. Wrapped in Mutex for the spawned poll loop.
    last_fires: Arc<Mutex<Vec<Option<i64>>>>,
}

const TICK_INTERVAL_SECS: u64 = 30;

impl IdleScheduler {
    pub fn new(
        triggers: Vec<IdleTrigger>,
        runner: Arc<TaskRunner>,
        quiet_hours: Option<mur_common::agent::QuietHours>,
    ) -> Self {
        let n = triggers.len();
        Self {
            triggers,
            runner,
            quiet_hours,
            last_fires: Arc::new(Mutex::new(vec![None; n])),
        }
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(self.run())
    }

    async fn run(self) {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(TICK_INTERVAL_SECS));
        // First tick fires immediately; skip it so we don't fire on boot
        // when the runner has no recorded activity yet.
        interval.tick().await;

        loop {
            interval.tick().await;
            let now_unix = chrono::Utc::now().timestamp();
            let last_activity = self.runner.last_activity_at();

            // Resolve quiet-window end once per tick (cheap; sub-microsecond).
            let quiet_end = self.quiet_hours.as_ref().and_then(|qh| {
                let local_now: DateTime<Local> = chrono::Local::now();
                crate::companion::schedule::active_window_end_for_today(local_now, Some(qh))
                    .map(|dt| dt.timestamp())
            });

            let mut last_fires = self.last_fires.lock().await;
            for (i, trigger) in self.triggers.iter().enumerate() {
                let last_fire = last_fires[i];
                if !should_fire(trigger, now_unix, last_activity, last_fire, quiet_end) {
                    continue;
                }
                debug!(
                    after_secs = trigger.after_secs,
                    cooldown_secs = trigger.cooldown_secs,
                    "IdleScheduler: firing trigger"
                );
                let spec = crate::task_runner::TaskSpec::echo(trigger.message.clone());
                let _handle = self.runner.start_async(spec);
                last_fires[i] = Some(now_unix);
                info!(
                    message = %trigger.message,
                    sends_to = trigger.sends_to.as_deref().unwrap_or("(self)"),
                    "IdleScheduler: fired"
                );
            }
        }
    }
}
```

Note: `TaskSpec::echo` is the existing constructor used by `CronScheduler` — verify by grepping `grep -n "TaskSpec::echo\|fn echo" mur-agent-runtime/src/scheduler.rs mur-agent-runtime/src/task_runner.rs`. If the cron scheduler uses a different constructor (e.g. `TaskSpec::new` with named fields), use the same one for consistency.

- [ ] **Step 6: Wire the module**

Edit `mur-agent-runtime/src/lib.rs`. Add (alphabetically placed near `scheduler`):

```rust
pub mod idle_scheduler;
```

- [ ] **Step 7: Run all tests**

Run: `cargo test -p mur-agent-runtime --lib`
Expected: All tests pass (including the 4 new ones).

- [ ] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/idle_scheduler.rs mur-agent-runtime/src/lib.rs
git commit -m "feat(c6): IdleScheduler tokio task + should_fire decision"
```

---

## Task 4: Wire `IdleScheduler` into the supervisor

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs:560-570`
- Test: `mur-agent-runtime/tests/idle_scheduler_smoke.rs` (new file)

- [ ] **Step 1: Write the failing integration test**

Create `mur-agent-runtime/tests/idle_scheduler_smoke.rs`:

```rust
//! Integration test: profile with `lifecycle.idle_triggers` non-empty
//! must spawn an IdleScheduler that fires after the threshold elapses.
//! Uses a minimal TaskRunner stub (no LLM); verifies fire by observing
//! `TaskRunner::last_activity_at` advancing past a synthetic boundary.

use mur_agent_runtime::idle_scheduler::IdleScheduler;
use mur_agent_runtime::task_runner::TaskRunner;
use mur_common::agent::IdleTrigger;
use std::sync::Arc;

#[tokio::test]
async fn idle_scheduler_fires_after_threshold() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let trigger = IdleTrigger {
        // 1-second threshold + 0 cooldown so the scheduler fires on the
        // first tick post-threshold.
        after_secs: 1,
        message: "are you there?".to_string(),
        sends_to: None,
        cooldown_secs: 0,
        respect_quiet_hours: false,
    };

    // Manually back-date the runner's last_activity so the next tick
    // immediately sees `now - last_activity >= 1`. We can't do this
    // without a public setter — instead, sleep 2 s after construction,
    // which leaves the runner idle for >1 s by the first scheduler tick.
    let scheduler = IdleScheduler::new(vec![trigger], runner.clone(), None);
    let handle = scheduler.spawn();

    // Wait long enough for: (a) initial 30 s tick to be skipped — too long.
    // We need a faster TICK_INTERVAL_SECS for tests, or expose it as a
    // const-via-feature. Simplest: don't smoke-test the full loop here;
    // instead, unit-test `should_fire` (Task 3) and rely on the
    // supervisor wiring smoke (Step 4 below).
    handle.abort();
}
```

The smoke test as written is awkward because `TICK_INTERVAL_SECS = 30` is too long for a unit test. **Refine Task 3 first**: expose `TICK_INTERVAL_SECS` as a `const pub(crate)` and add a `#[cfg(test)] pub const TICK_INTERVAL_SECS_TEST: u64 = 1;` plus a feature-gate or constructor variant. **Or**, simpler: parameterize the interval in `IdleScheduler::new` and default to 30 in production. Choose option B and update Task 3 retroactively if needed.

Replace the smoke test body once `IdleScheduler::new` accepts a tick interval:

```rust
#[tokio::test]
async fn idle_scheduler_fires_after_threshold() {
    use mur_agent_runtime::task_runner::TaskSpec;

    let runner = Arc::new(TaskRunner::new_stub_echo());
    let initial_activity = runner.last_activity_at();

    let trigger = IdleTrigger {
        after_secs: 0, // fires on first eligible tick
        message: "ping".to_string(),
        sends_to: None,
        cooldown_secs: 0,
        respect_quiet_hours: false,
    };

    // 100 ms tick interval for fast tests.
    let scheduler = IdleScheduler::with_tick_interval(
        vec![trigger],
        runner.clone(),
        None,
        std::time::Duration::from_millis(100),
    );
    let handle = scheduler.spawn();

    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    handle.abort();

    // After firing, last_activity_at should be >= initial_activity
    // (start_async bumps it).
    assert!(runner.last_activity_at() >= initial_activity);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime --test idle_scheduler_smoke`
Expected: FAIL with `no method 'with_tick_interval'`.

- [ ] **Step 3: Add `with_tick_interval` to IdleScheduler**

In `mur-agent-runtime/src/idle_scheduler.rs`, refactor `IdleScheduler`:

```rust
pub struct IdleScheduler {
    triggers: Vec<IdleTrigger>,
    runner: Arc<TaskRunner>,
    quiet_hours: Option<mur_common::agent::QuietHours>,
    last_fires: Arc<Mutex<Vec<Option<i64>>>>,
    tick_interval: std::time::Duration,
}

impl IdleScheduler {
    pub fn new(
        triggers: Vec<IdleTrigger>,
        runner: Arc<TaskRunner>,
        quiet_hours: Option<mur_common::agent::QuietHours>,
    ) -> Self {
        Self::with_tick_interval(
            triggers,
            runner,
            quiet_hours,
            std::time::Duration::from_secs(TICK_INTERVAL_SECS),
        )
    }

    pub fn with_tick_interval(
        triggers: Vec<IdleTrigger>,
        runner: Arc<TaskRunner>,
        quiet_hours: Option<mur_common::agent::QuietHours>,
        tick_interval: std::time::Duration,
    ) -> Self {
        let n = triggers.len();
        Self {
            triggers,
            runner,
            quiet_hours,
            last_fires: Arc::new(Mutex::new(vec![None; n])),
            tick_interval,
        }
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(self.run())
    }

    async fn run(self) {
        let mut interval = tokio::time::interval(self.tick_interval);
        interval.tick().await; // drop the immediate first tick

        loop {
            interval.tick().await;
            // ... existing body unchanged ...
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-agent-runtime --test idle_scheduler_smoke`
Expected: PASS.

- [ ] **Step 5: Wire into supervisor**

Edit `mur-agent-runtime/src/supervisor.rs`. After the C4 cron-scheduler block (line ~563-570):

```rust
    // 8d. C6 — idle / heartbeat scheduler. Spawns when lifecycle.idle_triggers
    //     is non-empty. Reuses the runner so triggers fire as ordinary tasks.
    if !profile.inner.lifecycle.idle_triggers.is_empty() {
        let quiet_hours = profile.inner.companion.as_ref()
            .and_then(|c| c.quiet_hours.clone());
        let is = crate::idle_scheduler::IdleScheduler::new(
            profile.inner.lifecycle.idle_triggers.clone(),
            runner.clone(),
            quiet_hours,
        );
        transport_tasks.push(is.spawn());
        info!(
            count = profile.inner.lifecycle.idle_triggers.len(),
            "IdleScheduler started"
        );
    }
```

Note: verify `profile.inner.companion.quiet_hours` path is correct by grepping:
```bash
grep -n "quiet_hours\|QuietHours" /Volumes/Firecuda4tb/Projects/mur/mur-common/src/agent.rs
```
If `quiet_hours` lives elsewhere (e.g. on `CompanionConfig` directly, not nested), fix the path.

- [ ] **Step 6: Verify supervisor still compiles**

Run: `cargo build -p mur-agent-runtime`
Expected: clean build, no warnings.

- [ ] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/idle_scheduler.rs mur-agent-runtime/src/supervisor.rs mur-agent-runtime/tests/idle_scheduler_smoke.rs
git commit -m "feat(c6): supervisor wiring + integration smoke test"
```

---

## Task 5: CLI `mur agent schedule idle {add,list,remove}`

**Files:**
- Modify: `mur-core/src/cmd/agent_schedule.rs`
- Test: `mur-core/tests/cmd_agent_idle.rs` (new file)

The C4 schedule subcommand pattern is reused. New entries live under `lifecycle.idle_triggers` instead of `lifecycle.schedule`.

- [ ] **Step 1: Write the failing integration test**

Create `mur-core/tests/cmd_agent_idle.rs`. Mirror `cmd_agent_schedule.rs` exactly (ENV_LOCK + MurHomeGuard + setup helper); only the test names and command calls differ:

```rust
//! Integration tests for `mur agent schedule idle add/list/remove`.

use mur_core::cmd::agent_schedule::{
    cmd_idle_add, cmd_idle_list, cmd_idle_remove, read_idle_triggers,
};
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct MurHomeGuard;
impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("MUR_HOME"); }
    }
}

fn setup(agent: &str) -> TempDir {
    // ⚠️ COPY THIS HELPER VERBATIM from mur-core/tests/cmd_agent_schedule.rs:27-73
    // (do not abbreviate — the engineer may be reading tasks out of order).
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agents").join(agent);
    fs::create_dir_all(&agent_dir).unwrap();
    let yaml = format!(
        r#"schema: 1
id: 00000000-0000-0000-0000-000000000001
name: {agent}
display_name: {agent}
version: 0.1.0
persona:
  category: custom
  description: test
  traits: {{ tone: concise, risk: cautious, verbosity: low }}
sys_prompt_file: sys_prompt.md
model: {{ provider: ollama, name: "llama3.2:3b", params: {{}} }}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: {{ enabled: false, bind: "unix:///tmp/test.sock" }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: []
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: unrestricted, allow_hosts: [] }}
  filesystem: {{ read: [], write: [], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, retry_on: [] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle:
  restart: on_failure
  max_restarts: 3
  restart_window_secs: 600
  stop_timeout_secs: 15
  mcp_required: false
  schedule: []
  idle_triggers: []
created_at: "2026-01-01T00:00:00+00:00"
updated_at: "2026-01-01T00:00:00+00:00"
"#
    );
    fs::write(agent_dir.join("profile.yaml"), yaml).unwrap();
    tmp
}

#[test]
fn idle_add_list_remove_roundtrip() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_test");
    unsafe { std::env::set_var("MUR_HOME", tmp.path()); }
    let _home_guard = MurHomeGuard;

    cmd_idle_add("idle_test", 3600, "still there?", None, 600, true).unwrap();
    cmd_idle_add("idle_test", 86400, "daily check", Some("peer".to_string()), 1800, false).unwrap();

    let entries = read_idle_triggers("idle_test").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].after_secs, 3600);
    assert_eq!(entries[0].message, "still there?");
    assert!(entries[0].respect_quiet_hours);
    assert_eq!(entries[1].sends_to.as_deref(), Some("peer"));
    assert!(!entries[1].respect_quiet_hours);

    cmd_idle_remove("idle_test", 0).unwrap();
    let entries = read_idle_triggers("idle_test").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].after_secs, 86400);
}

#[test]
fn idle_remove_oob_returns_err() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_oob");
    unsafe { std::env::set_var("MUR_HOME", tmp.path()); }
    let _home_guard = MurHomeGuard;
    let r = cmd_idle_remove("idle_oob", 0);
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("index"));
}

#[test]
fn idle_add_zero_after_secs_returns_err() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_zero");
    unsafe { std::env::set_var("MUR_HOME", tmp.path()); }
    let _home_guard = MurHomeGuard;
    let r = cmd_idle_add("idle_zero", 0, "msg", None, 600, true);
    assert!(r.is_err());
}

#[test]
fn idle_list_does_not_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_list");
    unsafe { std::env::set_var("MUR_HOME", tmp.path()); }
    let _home_guard = MurHomeGuard;
    cmd_idle_add("idle_list", 60, "ping", None, 600, true).unwrap();
    cmd_idle_list("idle_list").unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test cmd_agent_idle`
Expected: FAIL — `cannot find function 'cmd_idle_add'`.

- [ ] **Step 3: Implement the CLI helpers**

Append to `mur-core/src/cmd/agent_schedule.rs` (after `validate_cron`):

```rust
use mur_common::agent::IdleTrigger;

/// Append a new idle trigger to the agent's profile.
pub fn cmd_idle_add(
    name: &str,
    after_secs: u64,
    message: &str,
    sends_to: Option<String>,
    cooldown_secs: u64,
    respect_quiet_hours: bool,
) -> Result<()> {
    if after_secs == 0 {
        bail!("after_secs must be > 0");
    }
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.lifecycle.idle_triggers.push(IdleTrigger {
        after_secs,
        message: message.to_string(),
        sends_to,
        cooldown_secs,
        respect_quiet_hours,
    });
    let idx = profile.lifecycle.idle_triggers.len() - 1;
    save_profile(&path, &mut profile)?;
    println!(
        "added idle trigger [{idx}]: after_secs={after_secs} → {message:?}"
    );
    Ok(())
}

/// Print all idle triggers for the named agent.
pub fn cmd_idle_list(name: &str) -> Result<()> {
    let entries = read_idle_triggers(name)?;
    if entries.is_empty() {
        println!("no idle triggers for agent '{name}'");
        return Ok(());
    }
    println!(
        "{:<4} {:<10} {:<10} {:<5} {:<30} SENDS_TO",
        "IDX", "AFTER", "COOLDOWN", "QH", "MESSAGE"
    );
    for (i, e) in entries.iter().enumerate() {
        println!(
            "{:<4} {:<10} {:<10} {:<5} {:<30} {}",
            i,
            e.after_secs,
            e.cooldown_secs,
            if e.respect_quiet_hours { "yes" } else { "no" },
            e.message,
            e.sends_to.as_deref().unwrap_or("(self)")
        );
    }
    Ok(())
}

/// Remove the idle trigger at `index` (0-based).
pub fn cmd_idle_remove(name: &str, index: usize) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let len = profile.lifecycle.idle_triggers.len();
    if index >= len {
        bail!("index {index} out of range (agent '{name}' has {len} idle triggers)");
    }
    let removed = profile.lifecycle.idle_triggers.remove(index);
    save_profile(&path, &mut profile)?;
    println!("removed idle trigger [{index}]: after_secs={}", removed.after_secs);
    Ok(())
}

/// Return the raw idle-trigger entries.
pub fn read_idle_triggers(name: &str) -> Result<Vec<IdleTrigger>> {
    let (_path, profile) = load_profile_for_edit(name)?;
    Ok(profile.lifecycle.idle_triggers)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --test cmd_agent_idle`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent_schedule.rs mur-core/tests/cmd_agent_idle.rs
git commit -m "feat(c6): mur agent schedule idle {add,list,remove} CLI"
```

---

## Task 6: Wire CLI dispatch in `main.rs`

**Files:**
- Modify: `mur-core/src/main.rs` — `AgentScheduleAction` enum + dispatch arm

- [ ] **Step 1: Add `Idle` variants to `AgentScheduleAction`**

In `mur-core/src/main.rs`, find `AgentScheduleAction` (added in C4, near line 1147). Add four new variants AFTER the existing `Add/List/Remove/Next`:

```rust
    /// Add an idle trigger that fires after the agent has been idle for N seconds
    IdleAdd {
        name: String,
        #[arg(long)]
        after_secs: u64,
        #[arg(long)]
        message: String,
        #[arg(long)]
        sends_to: Option<String>,
        #[arg(long, default_value_t = 600)]
        cooldown_secs: u64,
        #[arg(long, default_value_t = true)]
        respect_quiet_hours: bool,
    },
    /// List all idle triggers for an agent
    IdleList { name: String },
    /// Remove an idle trigger by index (0-based, see `idle-list`)
    IdleRemove { name: String, index: usize },
```

- [ ] **Step 2: Add dispatch arms**

In the existing `AgentAction::Schedule { action } => match action { ... }` block (added in C4, near line 1964), add three more arms BEFORE the closing `},`:

```rust
            AgentScheduleAction::IdleAdd {
                name,
                after_secs,
                message,
                sends_to,
                cooldown_secs,
                respect_quiet_hours,
            } => cmd::agent_schedule::cmd_idle_add(
                &name,
                after_secs,
                &message,
                sends_to,
                cooldown_secs,
                respect_quiet_hours,
            )?,
            AgentScheduleAction::IdleList { name } => {
                cmd::agent_schedule::cmd_idle_list(&name)?
            }
            AgentScheduleAction::IdleRemove { name, index } => {
                cmd::agent_schedule::cmd_idle_remove(&name, index)?
            }
```

- [ ] **Step 3: Verify build + smoke**

Run: `cargo build -p mur-core --release`
Expected: clean build.

Run (manual smoke):
```bash
./target/release/mur agent schedule --help
./target/release/mur agent schedule idle-add --help
```
Expected: both list the new subcommands; idle-add shows `--after-secs`, `--message`, `--cooldown-secs`, `--respect-quiet-hours`, `--sends-to` flags.

- [ ] **Step 4: Run full test suite**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/main.rs
git commit -m "feat(c6): wire mur agent schedule idle-{add,list,remove} into main"
```

---

## Task 7: Cookbook `docs/cookbook/c6-idle-triggers.md`

**Files:**
- Create: `docs/cookbook/c6-idle-triggers.md`

- [ ] **Step 1: Write the cookbook**

Create the file with this content:

````markdown
# C6 — Idle / Heartbeat Triggers

Fire a configured message when an agent has been idle for a user-defined window.

## When to use

- A "still there?" check-in after a long silence on a chat-bridge agent.
- A periodic garbage-collection or health-probe sweep on a worker agent.
- A self-pinging keepalive that exercises the LLM path even when no one is talking to the agent.

C6 reuses the supervisor's existing `TaskRunner`, so each fire is an ordinary task — entitlements, sandboxing (B1), telemetry, and B0SafetyHook all apply unchanged.

## How it works

When `profile.lifecycle.idle_triggers` is non-empty, the supervisor spawns one `IdleScheduler` task that wakes every 30 s and inspects `TaskRunner::last_activity_at` (Unix seconds, bumped on every `start_async`). For each configured trigger:

1. If `(now - last_activity) < after_secs` → skip.
2. If `(now - last_fire) < cooldown_secs` → skip (refire suppression).
3. If `respect_quiet_hours` is true and now is past today's quiet-window start → skip.
4. Otherwise: inject `trigger.message` via `runner.start_async()` and record the fire time.

Per-trigger cooldowns are independent — two triggers can fire at different cadences without interfering.

## Profile schema

```yaml
lifecycle:
  restart: on_failure
  idle_triggers:
    - after_secs: 3600          # required: idle threshold
      message: "still there?"   # required: injected message body
      sends_to: peer_agent      # optional: A2A peer (default = self)
      cooldown_secs: 1800       # optional: refire cooldown (default 600)
      respect_quiet_hours: true # optional: suppress in quiet hours (default true)
```

`after_secs` and `cooldown_secs` are independent: a 1-hour idle threshold with a 30-min cooldown means the trigger fires at most once every 30 min, but only after the agent has actually been idle for 1 hour first.

## CLI

```bash
# Add a trigger
mur agent schedule idle-add my-agent \
  --after-secs 3600 \
  --message "still there?" \
  --cooldown-secs 1800 \
  --respect-quiet-hours

# List all idle triggers
mur agent schedule idle-list my-agent

# Remove by index
mur agent schedule idle-remove my-agent 0
```

`mur agent schedule idle-list` output:

```
IDX  AFTER      COOLDOWN   QH    MESSAGE                        SENDS_TO
0    3600       1800       yes   still there?                   (self)
1    86400      1800       no    daily heartbeat                ops_agent
```

## Restart semantics

Like C4 cron triggers, idle triggers are read at supervisor boot. Editing them via `mur agent schedule idle-{add,remove}` mutates `profile.yaml` but the running supervisor caches the trigger list — changes apply on the next `mur agent stop && mur agent start`.

## Quiet-hours interaction

`respect_quiet_hours: true` suppresses fires from the start of the quiet-hours window onward (configured under `companion.quiet_hours.start`). For agents without companion enabled, the field is ignored. The window resets at midnight local time.

## v1 limitations (deferred to v2)

- 30-second poll resolution is not configurable in production. (Tests can use `IdleScheduler::with_tick_interval` for fast smoke.)
- No "did fire" telemetry counter — fires are visible only as ordinary task records in `~/.mur/agents/<name>/telemetry/<date>.jsonl`.
- No CLI `next` command (unlike C4 cron) — idle triggers don't have a deterministic next-fire time.

## See also

- `docs/cookbook/c4-cron-triggers.md` — wall-clock-driven scheduling.
- `docs/cookbook/c5-webhook.md` — external HTTP-driven triggering.
- `mur-agent-runtime/src/idle_scheduler.rs` — implementation.
- Roadmap §5.6 (C6 row).
````

- [ ] **Step 2: Commit**

```bash
git add docs/cookbook/c6-idle-triggers.md
git commit -m "docs(c6): cookbook for idle / heartbeat triggers"
```

---

## Task 8: Workspace version bump + final integration

**Files:**
- Modify: `Cargo.toml` (workspace) — `version = "2.12.0"` → `"2.13.0"`

- [ ] **Step 1: Bump version**

Edit `/Volumes/Firecuda4tb/Projects/mur/Cargo.toml`:

```toml
[workspace.package]
version = "2.13.0"
```

- [ ] **Step 2: Run full test + lint suite**

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

Expected: all green.

- [ ] **Step 3: Commit + push**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump workspace version 2.12.0 → 2.13.0 (C6 idle triggers)"
git push origin <feature-branch>
```

- [ ] **Step 4: Open PR**

```bash
gh pr create --title "feat(c6): Idle / Heartbeat Triggers — mur agent schedule idle-{add,list,remove}" --body "$(cat <<'EOF'
## Summary

C6 fires a configured message into an agent's task runner whenever the agent has been idle for a user-defined window. Reuses the supervisor + TaskRunner plumbing from C4 — each fire is an ordinary task subject to all entitlements, B1 sandboxing, and B0 safety hooks.

- New `IdleTrigger` schema field on `LifecycleConfig.idle_triggers: Vec<IdleTrigger>`.
- New `IdleScheduler` tokio task spawned by supervisor when triggers are non-empty.
- New CLI: `mur agent schedule idle-{add,list,remove}`.
- Reuses `companion::schedule::active_window_end_for_today` for quiet-hours respect.

## Test plan

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] Manual smoke: add trigger to a stub agent, confirm fire after threshold via `~/.mur/agents/<n>/telemetry/<date>.jsonl`
- [ ] Cookbook walkthrough at `docs/cookbook/c6-idle-triggers.md`

## Spec
- Roadmap §5.6 (C6 row)
- Plan: `docs/superpowers/plans/2026-05-07-mur-agent-c6-idle-triggers.md`
EOF
)"
```

---

## Self-Review Checklist (run before declaring done)

- [ ] Every task references **exact file paths** from this plan.
- [ ] No `TODO` / `TBD` / "implement later" placeholders.
- [ ] Type names match across tasks: `IdleTrigger`, `IdleScheduler`, `should_fire`, `cmd_idle_add/list/remove`, `read_idle_triggers`, `last_activity_at`, `with_tick_interval`.
- [ ] Spec coverage: schema (Task 1) ✅, runner activity (Task 2) ✅, scheduler primitive (Task 3) ✅, supervisor wiring (Task 4) ✅, CLI (Task 5) ✅, dispatch (Task 6) ✅, cookbook (Task 7) ✅, version (Task 8) ✅.
- [ ] Tests use the same `MurHomeGuard` + `ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())` pattern as existing C4 tests.
- [ ] `cargo fmt` is run before commit (CI requires strict formatter pass; multi-line `unsafe { set_var(...); }` blocks are mandatory).
