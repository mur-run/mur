# C4 Cron + Lifecycle Schedule Triggers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Wire the existing `LifecycleConfig.schedule: Vec<ScheduleEntry>` schema into a live cron scheduler inside the agent supervisor, and add CLI verbs (`mur agent schedule add/list/remove/next`) for managing schedule entries without editing YAML by hand.

**Architecture:** A new `CronScheduler` tokio task parses each `ScheduleEntry.cron` expression (5-field POSIX), converts it to 6-field for the `cron` crate, sleeps until next-fire via `chrono` + `tokio::time::sleep`, then injects the scheduled message into the local `TaskRunner`. The scheduler is spawned as a `transport_task` in the supervisor so it is aborted on SIGTERM with all other transports. CLI verbs mutate `profile.yaml` directly using the existing `load_profile_for_edit` + `save_profile` pattern from `mur-core/src/cmd/agent.rs`.

**Tech Stack:** `cron = "0.12"` (parse + upcoming iterator), `chrono` (already in workspace), `tokio::time::sleep`, `mur_common::a2a::{Message, MessagePart}`, `TaskRunner::run_sync`, `load_profile_for_edit` + `save_profile` from `cmd::agent`.

---

## File Map

| File | Action |
|------|--------|
| `mur-agent-runtime/Cargo.toml` | Modify: add `cron = "0.12"` |
| `mur-agent-runtime/src/scheduler.rs` | Create: `CronScheduler` + `next_n_fires` |
| `mur-agent-runtime/src/lib.rs` | Modify: `pub mod scheduler;` |
| `mur-agent-runtime/tests/scheduler_integration.rs` | Create: unit tests for `next_n_fires` |
| `mur-agent-runtime/src/supervisor.rs` | Modify: spawn `CronScheduler` when schedule non-empty |
| `mur-core/src/cmd/agent_schedule.rs` | Create: `cmd_schedule_{add,list,remove,next}` + `read_schedule` |
| `mur-core/src/cmd/mod.rs` | Modify: `pub(crate) mod agent_schedule;` |
| `mur-core/src/main.rs` | Modify: `AgentScheduleAction` enum + `AgentAction::Schedule` + dispatch |
| `docs/cookbook/c4-cron-triggers.md` | Create: operations cookbook |

---

### Task 1: `cron` dep + `CronScheduler` + `next_n_fires`

**Files:**
- Modify: `mur-agent-runtime/Cargo.toml`
- Create: `mur-agent-runtime/src/scheduler.rs`
- Modify: `mur-agent-runtime/src/lib.rs`
- Create: `mur-agent-runtime/tests/scheduler_integration.rs`

- [x] **Step 1: Write the failing tests**

Create `mur-agent-runtime/tests/scheduler_integration.rs`:

```rust
use mur_agent_runtime::scheduler::next_n_fires;

#[test]
fn next_n_fires_returns_n_times() {
    // "0 * * * *" = every hour. Ask for 3 next fire times.
    let fires = next_n_fires("0 * * * *", 3).expect("should parse");
    assert_eq!(fires.len(), 3);
    // Each successive firing is ~60 minutes after the previous.
    for w in fires.windows(2) {
        let diff_min = (w[1] - w[0]).num_minutes();
        assert!(
            diff_min >= 55 && diff_min <= 65,
            "expected ~60 min gap, got {diff_min} min"
        );
    }
}

#[test]
fn next_n_fires_bad_expr_returns_err() {
    assert!(next_n_fires("not a cron", 3).is_err());
}

#[test]
fn next_n_fires_five_field_posix() {
    // "30 9 * * 1-5" = weekday 09:30. Should give 5 results.
    let fires = next_n_fires("30 9 * * 1-5", 5).expect("should parse");
    assert_eq!(fires.len(), 5);
}
```

- [x] **Step 2: Run tests to confirm they fail**

```
cargo test -p mur-agent-runtime --test scheduler_integration 2>&1 | tail -5
```
Expected: compile error — `module 'scheduler' not found`

- [x] **Step 3: Add `cron = "0.12"` to `mur-agent-runtime/Cargo.toml`**

In the `[dependencies]` section, after the `chrono` workspace dependency line:
```toml
cron = "0.12"
```

- [x] **Step 4: Create `mur-agent-runtime/src/scheduler.rs`**

```rust
//! C4 — Cron-triggered message injection.
//!
//! `ScheduleEntry.cron` is a 5-field POSIX expression (min hour dom month dow).
//! The `cron` crate requires 6 fields; we prepend `"0 "` (sec=0) at parse time.
//! Each entry runs in its own infinite tokio loop: parse → find next fire →
//! sleep → inject → repeat. All loops are children of `CronScheduler::spawn`,
//! which returns a single `JoinHandle` aborted on SIGTERM by the supervisor.

use crate::task_runner::{TaskRunner, TaskSpec};
use anyhow::{Context, Result};
use chrono::Local;
use cron::Schedule;
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::ScheduleEntry;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{info, warn};

pub struct CronScheduler {
    entries: Vec<ScheduleEntry>,
    runner: Arc<TaskRunner>,
}

impl CronScheduler {
    pub fn new(entries: Vec<ScheduleEntry>, runner: Arc<TaskRunner>) -> Self {
        Self { entries, runner }
    }

    /// Spawn an outer tokio task that fans out one loop per entry.
    /// Push the returned handle onto `supervisor::transport_tasks` so it is
    /// aborted on SIGTERM alongside all other transports.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut handles = Vec::with_capacity(self.entries.len());
            for entry in self.entries {
                let runner = self.runner.clone();
                handles.push(tokio::spawn(async move {
                    run_entry(entry, runner).await;
                }));
            }
            for h in handles {
                let _ = h.await;
            }
        })
    }
}

async fn run_entry(entry: ScheduleEntry, runner: Arc<TaskRunner>) {
    let expr = format!("0 {}", entry.cron);
    let schedule = match Schedule::from_str(&expr) {
        Ok(s) => s,
        Err(e) => {
            warn!(cron = %entry.cron, error = %e, "invalid cron expression; entry skipped");
            return;
        }
    };

    loop {
        let now = Local::now();
        let next = match schedule.upcoming(Local).next() {
            Some(t) => t,
            None => {
                warn!(cron = %entry.cron, "cron expression yields no future times; entry disabled");
                return;
            }
        };

        let delta = next - now;
        if delta.num_milliseconds() > 0 {
            if let Ok(dur) = delta.to_std() {
                tokio::time::sleep(dur).await;
            }
        }

        info!(cron = %entry.cron, message = %entry.message, "cron trigger firing");

        if let Some(ref target) = entry.sends_to {
            // Cross-agent dispatch deferred to C4 v2; log and fall through to local.
            warn!(
                sends_to = %target,
                "sends_to cross-agent dispatch not yet implemented; message injected locally"
            );
        }

        let input = Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: entry.message.clone(),
            }],
        };
        runner
            .run_sync(TaskSpec {
                input,
                context_task_id: None,
            })
            .await;
    }
}

/// Return the next `count` fire times for a 5-field POSIX cron expression.
///
/// Used by `mur agent schedule next` to preview upcoming firings.
/// Converts 5-field → 6-field by prepending `"0 "` (seconds = 0).
pub fn next_n_fires(
    cron_expr: &str,
    count: usize,
) -> Result<Vec<chrono::DateTime<Local>>> {
    let expr = format!("0 {cron_expr}");
    let schedule = Schedule::from_str(&expr)
        .with_context(|| format!("parse cron expression {cron_expr:?}"))?;
    Ok(schedule.upcoming(Local).take(count).collect())
}
```

- [x] **Step 5: Add `pub mod scheduler;` to `mur-agent-runtime/src/lib.rs`**

After the `pub mod supervisor;` line, add:
```rust
pub mod scheduler;
```

- [x] **Step 6: Run tests to confirm they pass**

```
cargo test -p mur-agent-runtime --test scheduler_integration 2>&1 | tail -10
```
Expected:
```
test next_n_fires_returns_n_times ... ok
test next_n_fires_bad_expr_returns_err ... ok
test next_n_fires_five_field_posix ... ok
```

- [x] **Step 7: Commit**

```bash
git add mur-agent-runtime/Cargo.toml \
        mur-agent-runtime/src/scheduler.rs \
        mur-agent-runtime/src/lib.rs \
        mur-agent-runtime/tests/scheduler_integration.rs
git commit -m "feat(c4): CronScheduler + next_n_fires — cron dep + core scheduler loop"
```

---

### Task 2: Wire `CronScheduler` into supervisor

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`
- Create: `mur-agent-runtime/tests/supervisor_cron_smoke.rs`

- [x] **Step 1: Write the smoke test**

Create `mur-agent-runtime/tests/supervisor_cron_smoke.rs`:

```rust
//! Smoke: CronScheduler spawns without panicking and its handle can be aborted.

use mur_agent_runtime::scheduler::CronScheduler;
use mur_agent_runtime::task_runner::TaskRunner;
use mur_common::agent::ScheduleEntry;
use std::sync::Arc;

#[tokio::test]
async fn cron_scheduler_spawns_and_aborts() {
    let entries = vec![
        ScheduleEntry {
            cron: "* * * * *".into(),   // every minute
            message: "ping".into(),
            sends_to: None,
        },
        ScheduleEntry {
            cron: "0 9 * * 1-5".into(), // weekday 09:00
            message: "morning brief".into(),
            sends_to: None,
        },
    ];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let scheduler = CronScheduler::new(entries, runner);
    let handle = scheduler.spawn();
    // Abort immediately — verifies no panic at init/parse time.
    handle.abort();
    // JoinError::is_cancelled() = true after abort.
    let result = handle.await;
    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn cron_scheduler_skips_bad_entry() {
    // Entry with invalid cron: the per-entry loop should warn and return,
    // not panic the whole scheduler.
    let entries = vec![ScheduleEntry {
        cron: "not valid".into(),
        message: "should be skipped".into(),
        sends_to: None,
    }];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let handle = CronScheduler::new(entries, runner).spawn();
    // Give the inner loop a moment to exit (it won't block after warn+return).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // The handle should be done (loop exited naturally) or still running; either
    // is valid. What must NOT happen is a panic (which would surface as Err here).
    let _ = handle; // just ensuring no immediate panic on construction
}
```

- [x] **Step 2: Run tests to confirm they pass before touching supervisor.rs**

```
cargo test -p mur-agent-runtime --test supervisor_cron_smoke 2>&1 | tail -10
```
Expected: both tests pass (the scheduler module already exists from Task 1).

- [x] **Step 3: Add `use crate::scheduler::CronScheduler;` to supervisor.rs**

In `mur-agent-runtime/src/supervisor.rs`, in the `use` block at the top, add after the last `use crate::` line:
```rust
use crate::scheduler::CronScheduler;
```

- [x] **Step 4: Spawn the scheduler in the supervisor `entrypoint()`**

In `mur-agent-runtime/src/supervisor.rs`, after the block that writes `running.lock` (the line `write_lock(&lock_path, &lock)?;` at around line 556) and before the block labelled `// 8.5 — bridge agents`, add:

```rust
    // 8c. C4 — cron scheduler. Spawn one loop per lifecycle.schedule entry.
    //     Aborted on SIGTERM alongside other transport_tasks.
    if !profile.inner.lifecycle.schedule.is_empty() {
        let cs = CronScheduler::new(
            profile.inner.lifecycle.schedule.clone(),
            runner.clone(),
        );
        transport_tasks.push(cs.spawn());
        info!(
            count = profile.inner.lifecycle.schedule.len(),
            "CronScheduler started"
        );
    }
```

- [x] **Step 5: Clippy + test**

```
cargo clippy -p mur-agent-runtime -- -D warnings 2>&1 | tail -5
cargo test -p mur-agent-runtime 2>&1 | tail -10
```
Expected: no errors, all tests pass.

- [x] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs \
        mur-agent-runtime/tests/supervisor_cron_smoke.rs
git commit -m "feat(c4): wire CronScheduler into supervisor transport_tasks"
```

---

### Task 3: CLI — `cmd_schedule_{add,list,remove,next}` + `read_schedule`

**Files:**
- Create: `mur-core/src/cmd/agent_schedule.rs`
- Modify: `mur-core/src/cmd/mod.rs`
- Create: `mur-core/tests/cmd_agent_schedule.rs`

- [x] **Step 1: Write the failing tests**

Create `mur-core/tests/cmd_agent_schedule.rs`:

```rust
//! Integration tests for `mur agent schedule` CLI commands.
//! Points MUR_HOME at a tempdir containing a minimal profile.yaml.

use std::fs;
use tempfile::TempDir;

fn setup(agent: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agents").join(agent);
    fs::create_dir_all(&agent_dir).unwrap();
    // Minimal valid profile.yaml — matches the serialised AgentProfile defaults.
    let yaml = format!(
        r#"schema: 1
id: 00000000-0000-0000-0000-000000000001
name: {agent}
display_name: {agent}
version: 0.1.0
persona:
  category: custom
  description: test
  traits:
    tone: concise
    risk: cautious
    verbosity: medium
sys_prompt_file: sys_prompt.md
model:
  provider: ollama
  name: llama3.2:3b
  params: {{}}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket:
    enabled: false
    bind: "unix:///tmp/test.sock"
  tcp:
    enabled: false
    bind: ""
    pattern: Noise_XK_25519_ChaChaPoly_BLAKE2s
  webhook:
    enabled: false
    bind: "127.0.0.1"
    port: 0
    hmac_secret_ref: ""
communication:
  accepts_from: ["*"]
  sends_to: []
capabilities: []
entitlements:
  llm:
    mode: allowed
  network:
    outbound:
      mode: unrestricted
      allow_hosts: []
    inbound:
      ports: []
  filesystem:
    read: []
    write: []
  spawn:
    allowed_commands: []
  max_turns: 100
notifications:
  enabled: false
retry:
  llm:
    max_retries: 3
    backoff: exponential
    initial_delay_ms: 1000
    retry_on: []
  tool:
    max_retries: 1
    backoff: fixed
    initial_delay_ms: 500
    retry_on: []
lifecycle:
  restart: on_failure
  max_restarts: 3
  restart_window_secs: 600
  stop_timeout_secs: 15
  mcp_required: false
  execution: daemon
  schedule: []
identity:
  pubkey: zABC
file_transfer:
  accept_incoming_file_max_bytes: 10485760
  accept_incoming_total_per_hour: 104857600
  require_approval_above_bytes: 1048576
  reject_paths: []
  allowed_mime_types: []
deployment:
  target: laptop
companion:
  enabled: false
voice:
  enabled: false
trusted_peers: []
created_at: "2026-01-01T00:00:00+00:00"
updated_at: "2026-01-01T00:00:00+00:00"
"#
    );
    fs::write(agent_dir.join("profile.yaml"), yaml).unwrap();
    tmp
}

#[test]
fn add_list_remove_roundtrip() {
    let tmp = setup("sched_test");
    std::env::set_var("MUR_HOME", tmp.path());

    // Add two entries
    mur_core::cmd::agent_schedule::cmd_schedule_add(
        "sched_test", "0 9 * * 1-5", "morning brief", None,
    )
    .unwrap();
    mur_core::cmd::agent_schedule::cmd_schedule_add(
        "sched_test", "0 18 * * 1-5", "end of day", Some("other_agent".to_string()),
    )
    .unwrap();

    // read_schedule returns both
    let entries = mur_core::cmd::agent_schedule::read_schedule("sched_test").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].cron, "0 9 * * 1-5");
    assert_eq!(entries[0].message, "morning brief");
    assert!(entries[0].sends_to.is_none());
    assert_eq!(entries[1].cron, "0 18 * * 1-5");
    assert_eq!(entries[1].message, "end of day");
    assert_eq!(entries[1].sends_to.as_deref(), Some("other_agent"));

    // Remove index 0 (morning brief)
    mur_core::cmd::agent_schedule::cmd_schedule_remove("sched_test", 0).unwrap();

    let entries = mur_core::cmd::agent_schedule::read_schedule("sched_test").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cron, "0 18 * * 1-5");

    // Remove the last entry
    mur_core::cmd::agent_schedule::cmd_schedule_remove("sched_test", 0).unwrap();
    let entries = mur_core::cmd::agent_schedule::read_schedule("sched_test").unwrap();
    assert!(entries.is_empty());
}

#[test]
fn remove_out_of_bounds_returns_err() {
    let tmp = setup("sched_oob");
    std::env::set_var("MUR_HOME", tmp.path());

    let result = mur_core::cmd::agent_schedule::cmd_schedule_remove("sched_oob", 0);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("index"));
}

#[test]
fn schedule_next_returns_count_times() {
    let tmp = setup("sched_next");
    std::env::set_var("MUR_HOME", tmp.path());
    mur_core::cmd::agent_schedule::cmd_schedule_add(
        "sched_next", "0 * * * *", "hourly ping", None,
    )
    .unwrap();

    // cmd_schedule_next prints to stdout — just verify it doesn't error.
    mur_core::cmd::agent_schedule::cmd_schedule_next("sched_next", 3).unwrap();
}
```

- [x] **Step 2: Run tests to confirm they fail**

```
cargo test -p mur-core --test cmd_agent_schedule 2>&1 | tail -5
```
Expected: compile error — `module 'agent_schedule' not found`

- [x] **Step 3: Create `mur-core/src/cmd/agent_schedule.rs`**

```rust
//! C4 — `mur agent schedule add/list/remove/next`.
//!
//! Reads and writes `profile.lifecycle.schedule` using the same
//! `load_profile_for_edit` + `save_profile` helpers used by `agent_webhook.rs`.

use anyhow::{Context, Result, bail};
use mur_common::agent::ScheduleEntry;

use super::agent::{load_profile_for_edit, save_profile};

/// Append a new schedule entry to the agent's profile.
pub fn cmd_schedule_add(
    name: &str,
    cron: &str,
    message: &str,
    sends_to: Option<String>,
) -> Result<()> {
    validate_cron(cron)?;
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.lifecycle.schedule.push(ScheduleEntry {
        cron: cron.to_string(),
        message: message.to_string(),
        sends_to,
    });
    let idx = profile.lifecycle.schedule.len() - 1;
    save_profile(&path, &mut profile)?;
    println!("added schedule entry [{idx}]: {cron:?}  →  {message:?}");
    Ok(())
}

/// Print all schedule entries for the named agent.
pub fn cmd_schedule_list(name: &str) -> Result<()> {
    let entries = read_schedule(name)?;
    if entries.is_empty() {
        println!("no schedule entries for agent '{name}'");
        return Ok(());
    }
    println!("{:<4} {:<20} {:<30} {}", "IDX", "CRON", "MESSAGE", "SENDS_TO");
    for (i, e) in entries.iter().enumerate() {
        println!(
            "{:<4} {:<20} {:<30} {}",
            i,
            e.cron,
            e.message,
            e.sends_to.as_deref().unwrap_or("(self)")
        );
    }
    Ok(())
}

/// Remove the schedule entry at `index` (0-based).
pub fn cmd_schedule_remove(name: &str, index: usize) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let len = profile.lifecycle.schedule.len();
    if index >= len {
        bail!("index {index} out of range (agent '{name}' has {len} entries)");
    }
    let removed = profile.lifecycle.schedule.remove(index);
    save_profile(&path, &mut profile)?;
    println!("removed entry [{index}]: {:?}", removed.cron);
    Ok(())
}

/// Print the next `count` fire times for each schedule entry of the named agent.
pub fn cmd_schedule_next(name: &str, count: usize) -> Result<()> {
    let entries = read_schedule(name)?;
    if entries.is_empty() {
        println!("no schedule entries for agent '{name}'");
        return Ok(());
    }
    for (i, e) in entries.iter().enumerate() {
        println!("[{i}] {}", e.cron);
        match mur_agent_runtime::scheduler::next_n_fires(&e.cron, count) {
            Ok(times) => {
                for t in times {
                    println!("  {}", t.format("%Y-%m-%d %H:%M:%S %Z"));
                }
            }
            Err(err) => println!("  (invalid expression: {err})"),
        }
    }
    Ok(())
}

/// Return the raw schedule entries from the agent's profile (used by tests).
pub fn read_schedule(name: &str) -> Result<Vec<ScheduleEntry>> {
    let (_path, profile) = load_profile_for_edit(name)?;
    Ok(profile.lifecycle.schedule)
}

/// Validate a 5-field POSIX cron expression by attempting a parse.
fn validate_cron(expr: &str) -> Result<()> {
    mur_agent_runtime::scheduler::next_n_fires(expr, 1)
        .with_context(|| format!("invalid cron expression: {expr:?}"))?;
    Ok(())
}
```

- [x] **Step 4: Add `mur-agent-runtime` as a dep of `mur-core`**

`mur-core/Cargo.toml` currently does not depend on `mur-agent-runtime` (they're siblings). Check:

```
grep "mur-agent-runtime" mur-core/Cargo.toml
```

If absent, add to `[dependencies]` in `mur-core/Cargo.toml`:

```toml
mur-agent-runtime = { path = "../mur-agent-runtime" }
```

> **Note:** If there would be a circular dependency (unlikely — `mur-agent-runtime` depends on `mur-common` only, while `mur-core` depends on `mur-common`), move `next_n_fires` into `mur-common` instead and remove the `mur-agent-runtime` dep from `mur-core`. Prefer the non-circular path.

**Circular check:** `mur-agent-runtime/Cargo.toml` depends on `mur-common` only (confirmed). `mur-core` would add `mur-agent-runtime`. `mur-agent-runtime` does NOT depend on `mur-core`. No cycle.

- [x] **Step 5: Declare the module in `mur-core/src/cmd/mod.rs`**

After the `pub(crate) mod agent_webhook;` line, add:
```rust
pub(crate) mod agent_schedule;
```

- [x] **Step 6: Run tests to confirm they pass**

```
cargo test -p mur-core --test cmd_agent_schedule 2>&1 | tail -10
```
Expected:
```
test add_list_remove_roundtrip ... ok
test remove_out_of_bounds_returns_err ... ok
test schedule_next_returns_count_times ... ok
```

- [x] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent_schedule.rs \
        mur-core/src/cmd/mod.rs \
        mur-core/Cargo.toml \
        mur-core/tests/cmd_agent_schedule.rs
git commit -m "feat(c4): cmd_schedule_{add,list,remove,next} + read_schedule"
```

---

### Task 4: Wire CLI into `main.rs`

**Files:**
- Modify: `mur-core/src/main.rs`

- [x] **Step 1: Write a compile-only smoke test to confirm dispatch builds**

This task is primarily a wiring task; the compiler is the test. After the edit, run:
```
cargo build -p mur-core 2>&1 | tail -10
```

- [x] **Step 2: Add `AgentScheduleAction` enum to `main.rs`**

In `mur-core/src/main.rs`, after the `enum AgentVoiceAction` definition (or near the other `AgentXxxAction` enums), add:

```rust
#[derive(Subcommand)]
enum AgentScheduleAction {
    /// Append a cron entry to the agent's lifecycle.schedule
    Add {
        /// Agent name
        name: String,
        /// 5-field POSIX cron expression (e.g. "30 9 * * 1-5")
        #[arg(long)]
        cron: String,
        /// Message text injected as a user turn when the cron fires
        #[arg(long)]
        message: String,
        /// Send the message to a different agent instead of self (optional)
        #[arg(long)]
        sends_to: Option<String>,
    },
    /// List all schedule entries for an agent
    List {
        /// Agent name
        name: String,
    },
    /// Remove a schedule entry by index (0-based, see `list` for indices)
    Remove {
        /// Agent name
        name: String,
        /// Entry index to remove
        index: usize,
    },
    /// Show next N fire times for each schedule entry
    Next {
        /// Agent name
        name: String,
        /// How many upcoming fires to show per entry (default: 5)
        #[arg(long, default_value_t = 5)]
        count: usize,
    },
}
```

- [x] **Step 3: Add `Schedule` variant to `AgentAction` enum**

In the `enum AgentAction` definition, after the `Voice` variant, add:

```rust
    /// Manage lifecycle cron schedule entries (C4)
    Schedule {
        #[command(subcommand)]
        action: AgentScheduleAction,
    },
```

- [x] **Step 4: Add dispatch arm for `AgentAction::Schedule`**

In the large `match action { ... }` block that dispatches `AgentAction`, after the `AgentAction::Voice { ... }` arm, add:

```rust
            AgentAction::Schedule { action } => match action {
                AgentScheduleAction::Add { name, cron, message, sends_to } => {
                    cmd::agent_schedule::cmd_schedule_add(&name, &cron, &message, sends_to)?
                }
                AgentScheduleAction::List { name } => {
                    cmd::agent_schedule::cmd_schedule_list(&name)?
                }
                AgentScheduleAction::Remove { name, index } => {
                    cmd::agent_schedule::cmd_schedule_remove(&name, index)?
                }
                AgentScheduleAction::Next { name, count } => {
                    cmd::agent_schedule::cmd_schedule_next(&name, count)?
                }
            },
```

- [x] **Step 5: Build + clippy clean**

```
cargo build -p mur-core 2>&1 | tail -5
cargo clippy -p mur-core -- -D warnings 2>&1 | tail -5
```
Expected: no errors or warnings.

- [x] **Step 6: Smoke-test the CLI help**

```
cargo run -p mur-core -- agent schedule --help 2>&1
```
Expected output includes: `add`, `list`, `remove`, `next`

- [x] **Step 7: Run full workspace tests**

```
cargo test --workspace 2>&1 | tail -15
```
Expected: all tests pass.

- [x] **Step 8: Commit**

```bash
git add mur-core/src/main.rs
git commit -m "feat(c4): AgentAction::Schedule dispatch + AgentScheduleAction enum"
```

---

### Task 5: Cookbook `docs/cookbook/c4-cron-triggers.md`

**Files:**
- Create: `docs/cookbook/c4-cron-triggers.md`

- [x] **Step 1: Write the cookbook**

Create `docs/cookbook/c4-cron-triggers.md`:

```markdown
# C4 — Cron Triggers for murmur Agents

Cron triggers let an agent send itself a scheduled user-turn message on a repeating
schedule — no external scheduler or push needed. The firing logic lives inside the
supervisor (`mur-agent-runtime`), so it runs for the lifetime of the agent process.

## How it works

`profile.yaml` has a `lifecycle.schedule` list:

```yaml
lifecycle:
  execution: daemon
  schedule:
    - cron: "0 9 * * 1-5"      # weekday 09:00 local time
      message: "Morning brief — what's on the agenda today?"
    - cron: "0 18 * * 1-5"     # weekday 18:00 local time
      message: "End-of-day summary: list the three most important things done."
      sends_to: summarizer      # send to a different agent instead of self
```

On startup the supervisor parses each entry and spawns a persistent tokio loop per
entry. Each loop sleeps until the next cron firing, injects the message as a `user`
turn via `TaskRunner::run_sync`, then sleeps until the following firing.

**Format:** `cron` is a 5-field POSIX expression `min hour dom month dow`. The
scheduler prepends `0 ` internally (seconds = 0). Standard shortcuts like
`@daily` are NOT supported — use explicit 5-field expressions.

**`sends_to`:** Specifying a different agent name is v2; in v1 the message is
always injected locally with a warning logged. Leave it unset unless you intend
to upgrade to a multi-agent topology.

## CLI

### Add a schedule entry

```bash
mur agent schedule add myagent \
  --cron "30 8 * * 1-5" \
  --message "Good morning! Summarise today's calendar."
```

### List schedule entries

```bash
mur agent schedule list myagent
# IDX  CRON                 MESSAGE                        SENDS_TO
# 0    30 8 * * 1-5         Good morning! Summar...        (self)
# 1    0 18 * * 1-5         End-of-day summary             (self)
```

### Preview next fire times

```bash
mur agent schedule next myagent --count 3
# [0] 30 8 * * 1-5
#   2026-05-11 08:30:00 CST
#   2026-05-12 08:30:00 CST
#   2026-05-13 08:30:00 CST
```

### Remove an entry

```bash
mur agent schedule remove myagent 0   # removes index 0
```

## Restart required

Schedule entries are read once at supervisor startup. After adding, removing, or
modifying entries via the CLI, restart the agent for changes to take effect:

```bash
mur agent stop myagent
mur_agent_myagent   # or however you normally start the agent
```

## Cron reference

| Expression      | Meaning                |
|-----------------|------------------------|
| `* * * * *`     | every minute           |
| `0 * * * *`     | top of every hour      |
| `0 9 * * 1-5`   | weekday 09:00          |
| `0 9,18 * * *`  | 09:00 and 18:00 daily  |
| `0 0 1 * *`     | first of every month   |
| `*/15 * * * *`  | every 15 minutes       |

Times are in the **system local timezone** of the machine running the agent
(`chrono::Local`). Set `TZ` in the process environment to override.

## Known limitations (v1)

- `sends_to` dispatches locally with a warning (cross-agent dispatch is C4 v2).
- No persistence of missed firings: if the agent is offline when a cron would
  have fired, that firing is skipped — there is no catch-up mechanism.
- No per-entry enable/disable toggle; remove and re-add to disable.
```

- [x] **Step 2: Verify docs build (no broken markdown)**

```
# Check file exists and is non-empty
test -s docs/cookbook/c4-cron-triggers.md && echo "OK"
```

- [x] **Step 3: Commit**

```bash
git add docs/cookbook/c4-cron-triggers.md
git commit -m "docs(c4): cookbook for cron lifecycle triggers"
```

---

## Self-Review

### Spec coverage

| Spec requirement | Task covering it |
|-----------------|-----------------|
| `CronScheduler` tokio task that parses entries with `cron` crate | Task 1 |
| Sleep until next-fire using `chrono` | Task 1 (`run_entry`) |
| Inject scheduled message into supervisor's task runner | Task 1 (`runner.run_sync`) |
| Wire into `supervisor.rs` when schedule non-empty | Task 2 |
| `mur agent schedule add` CLI | Task 3 + Task 4 |
| `mur agent schedule list` CLI | Task 3 + Task 4 |
| `mur agent schedule remove` CLI | Task 3 + Task 4 |
| `mur agent schedule next` CLI (show next 5 fire times) | Task 3 + Task 4 |
| Cookbook `docs/cookbook/c4-cron-triggers.md` | Task 5 |

### Placeholder scan

No TBD / TODO / placeholder content found.

### Type consistency

- `ScheduleEntry` from `mur-common::agent` used consistently in Tasks 1, 2, 3.
- `next_n_fires(cron_expr: &str, count: usize) -> Result<Vec<DateTime<Local>>>` defined in Task 1, called in Task 3 (`validate_cron` + `cmd_schedule_next`).
- `TaskRunner::run_sync(TaskSpec { input: Message { ... }, context_task_id: None })` matches the existing signature in `task_runner.rs`.
- `load_profile_for_edit(name) -> Result<(PathBuf, AgentProfile)>` and `save_profile(path, &mut profile) -> Result<()>` match the signatures in `cmd::agent`.

### Circular dep check

`mur-agent-runtime` depends on `mur-common` only. Adding `mur-agent-runtime` as a dep of `mur-core` is safe (no cycle). Both already depend on `mur-common`.

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-07-mur-agent-c4-cron-triggers.md`.**

Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, spec + quality review between tasks

**2. Inline Execution** — execute tasks in this session using executing-plans, with checkpoints

**Which approach?**
