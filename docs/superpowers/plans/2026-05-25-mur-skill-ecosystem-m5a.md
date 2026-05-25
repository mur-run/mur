# M5a — Lifecycle Observability + Doctor (Read-Only) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the observability half of M5 — every installed skill gains a host-local `stats.json` sidecar that tracks usage counters, last-used timestamps, lifecycle state and decayed confidence. `mur skill doctor` reads those stats plus the manifest to surface health findings. **No skill manifest is mutated, no lifecycle transition is persisted, no auto-repair is performed.** All mutation lives in M5b.

**Spec mapping:** §9.1 lifecycle states (read-only computation), §9.2 decay (pure function), §9.3 doctor (`--fix` accepted but stubbed), §10.4 health dashboard (`mur skill info --metrics`), §14 M5 entries that do not require mutation.

**What M5a adds:**
- A `SkillStats` sidecar JSON per installed skill (`~/.mur/skills/<name>/stats.json`), updated off the hot path by a span subscriber that mirrors `telemetry_writer.rs`.
- Pure-function lifecycle + decay layer (`mur-common::skill::lifecycle`) — `next_state(...)`, `calculate_decay(...)`, `transition_allowed(...)`. No persistence side effects.
- Five CLI surfaces: `mur skill stats <name>`, `mur skill info <name> --metrics`, `mur skill pin/unpin`, `mur skill doctor [...] [--fix]`, `mur skill reindex-stats`.
- One new telemetry `Event::SkillExecuted` variant feeding the aggregator.

**What M5a does NOT do (deferred to M5b):**
- Persisting lifecycle transitions (the sweep that flips Draft→Emerging→Stable, auto-demote, auto-archive).
- `mur skill doctor --fix --apply` actually repairing anything (M5a accepts the flag for forward CLI stability but prints "fix mode requires M5b").
- Consolidation (dedup / contradiction / orphan).
- `mur skill sweep` command (the new M5b name for the lifecycle sweep).

**Tech Stack:** Rust 2024, existing `serde_json`, `tokio` (subscriber is async), `tracing`. Three new dependencies (all in active use elsewhere in the Rust ecosystem):
- `fd-lock` — cross-platform exclusive-lock RAII guard for the sidecar read-merge-write window. Pulled in via `mur-common` (the only crate that touches stats files).
- `globset` — for `mur skill doctor 'research-*'` and `mur skill sweep 'research-*'` glob matching. `mur-core` only.
- `supports-color` + `sysexits` — doctor output / exit-code conventions. `mur-core` only.

**Deployment assumption:** Single-host, single-user, single MUR_HOME. Multi-agent on the same host is in scope (the file lock handles two `mur_agent_*` runtimes racing on the same `stats.json`). NFS-mounted MUR_HOME is out of scope and documented.

---

## File Structure

**Create:**
- `mur-common/src/skill/stats.rs` — `SkillStats` struct, schema v1, serde, `load`/`save_atomic`/`merge_in_place` with `fd-lock`.
- `mur-common/src/skill/lifecycle.rs` — `LifecycleState` enum, `next_state`, `calculate_decay`, `transition_allowed`, half-life table, hysteresis constants.
- `mur-core/src/skill_stats/aggregator.rs` — span-subscriber + mpsc task that flushes counter deltas into sidecars on a 64-event / 2s tick.
- `mur-core/src/skill_stats/reindex.rs` — rebuild `stats.json` for one or all skills from the existing JSONL trace log.
- `mur-core/src/cmd/skill_stats.rs` — `cmd_stats`, `cmd_info_metrics`, `cmd_pin`, `cmd_unpin`, `cmd_reindex_stats`.
- `mur-core/src/cmd/skill_doctor.rs` — `cmd_doctor` (all checks read-only), severity enum, output formatters (text + json), exit-code derivation.
- `mur-core/tests/skill_stats_aggregator.rs` — end-to-end test: emit Events → flush → assert sidecar contents.
- `mur-core/tests/skill_doctor_findings.rs` — fixture-driven test of every doctor check.

**Modify:**
- `mur-common/src/skill/mod.rs` — `pub mod stats; pub mod lifecycle;` and re-exports.
- `mur-common/Cargo.toml` — add `fd-lock = "4"` (or current major).
- `mur-core/Cargo.toml` — add `globset`, `supports-color`, `sysexits`.
- `mur-core/src/lib.rs` — `pub mod skill_stats;`.
- `mur-core/src/main.rs` (or `cli.rs`) — wire the new subcommands.
- `mur-agent-runtime/src/telemetry_writer.rs` — add `Event::SkillExecuted { skill_name, skill_version, outcome, duration_ms, trace_id, task_id }` variant + `event_to_notification` arm.
- `mur-agent-runtime/src/runtime/...` — emit `SkillExecuted` on the path that already fires hooks for skill execution (M2 + M3c paths). Concrete file picked at Task 3 Step 1 after locating the existing call site.

**Do not modify:**
- `SkillManifest` / `Skill` structs in `mur-common::skill::manifest` — stats live entirely outside the signed manifest.
- DSSE / Ed25519 signing code — but Task 1 Step 4 adds a doc comment confirming `stats.json` is explicitly outside signature scope.

---

### Task 1 — `SkillStats` schema + atomic IO helpers

**Files:** `mur-common/src/skill/stats.rs` (new), `mur-common/src/skill/mod.rs`, `mur-common/Cargo.toml`.

- [ ] **Step 1: Define the schema**

```rust
//! Per-skill runtime statistics. **Not** signed, **not** part of the
//! publisher manifest. Lives at `<MUR_HOME>/skills/<name>/stats.json`
//! and is rebuildable from the JSONL trace log via
//! `mur skill reindex-stats`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const STATS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    #[default]
    Draft,
    Emerging,
    Stable,
    Canonical,
    Deprecated,
    Archived,
}

/// Sidecar stats for an installed skill. **Not part of the signed manifest.**
///
/// Schema evolution policy: additive only. New fields MUST be marked
/// `#[serde(default)]` so older `mur` builds reading newer files (and newer
/// builds reading older files) parse cleanly without migration. M6+ author
/// note: do not pre-reserve fields here without a producer — empty defaults
/// create semantic ambiguity. Add fields when their callers exist.
/// See `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m6-scoping.md` §4.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStats {
    pub schema_version: u32,
    pub skill_name: String,
    pub skill_version: String,
    /// SHA-256 of the manifest content at the time these stats were
    /// (re)initialised. A mismatch on load tells us the skill was
    /// reinstalled — see `reset_on_manifest_change()`.
    pub manifest_digest: String,

    pub lifecycle_state: LifecycleState,
    pub lifecycle_changed_at: DateTime<Utc>,
    pub pinned: bool,
    #[serde(default)]
    pub pinned_reason: String,

    pub usage_count: u64,
    pub success_count: u64,
    pub failure_count: u64,

    pub last_used_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub first_successful_use_at: Option<DateTime<Utc>>,

    /// Confidence at the moment of the most recent successful use (or
    /// most recent promotion — see `lifecycle::on_promotion`). Decay is
    /// computed *from this anchor*, never incrementally — keeps the
    /// value numerically stable and idempotent on read.
    pub anchor_confidence: f64,

    /// Watermark for incremental reindex — the trace timestamp that
    /// these stats have already absorbed. `mur skill reindex-stats`
    /// resumes from here.
    pub rebuilt_from_trace_through: Option<DateTime<Utc>>,
}

impl SkillStats {
    pub fn new(skill_name: &str, skill_version: &str, manifest_digest: &str, now: DateTime<Utc>) -> Self { /* … */ }
    pub fn path(mur_home: &Path, skill_name: &str) -> PathBuf { /* … */ }
}
```

Notes:
- `rolling_window` is **not** in the schema — computed on demand by `mur skill doctor` from the JSONL trace log. Decision made during M5 research (avoids read-modify-write of an embedded array on the hot path).
- `anchor_confidence` initialised to `1.0` for newly installed skills, *not* `0.5`. Decay alone brings it down; success boosts (M5b) bring it back up.
- `lifecycle_changed_at` initialised to install time even when state is `Draft` — used by the M5b sweep to enforce minimum dwell time.

- [ ] **Step 2: Atomic load / save with file lock**

```rust
use anyhow::{Context, Result};
use fd_lock::RwLock;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use tempfile::NamedTempFile;

impl SkillStats {
    /// Read the sidecar, or return `None` if absent. Lock-free — fine
    /// for read-mostly callers (doctor, info, stats). Concurrent writers
    /// going through `merge_in_place` will not corrupt the file because
    /// they hold the exclusive lock during the write window.
    pub fn load(path: &Path) -> Result<Option<Self>> { /* read + serde_json::from_str */ }

    /// Read-merge-write under an exclusive `fd-lock`. `merge_fn` is
    /// called with the loaded value (or the supplied default if none
    /// exists) and is responsible for applying the delta. The lock
    /// window is microseconds — counter increments only.
    pub fn merge_in_place(
        path: &Path,
        default: impl FnOnce() -> Self,
        merge_fn: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        // 1. Open (or create) the lockfile sentinel next to stats.json
        let lock_path = path.with_extension("lock");
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        let mut lock_file = RwLock::new(OpenOptions::new().create(true).write(true).read(true).open(&lock_path)?);
        let _guard = lock_file.write().context("acquire stats lock")?;

        // 2. Read current (or default)
        let mut stats = Self::load(path)?.unwrap_or_else(default);
        merge_fn(&mut stats)?;

        // 3. Temp-file + rename (POSIX & Windows atomic on same filesystem)
        let tmp = NamedTempFile::new_in(path.parent().unwrap())?;
        serde_json::to_writer_pretty(&tmp, &stats)?;
        tmp.persist(path).context("persist stats")?;
        Ok(())
    }
}
```

Key invariants documented inline:
- The lock is on `stats.lock`, **not** on `stats.json` itself — POSIX `flock(2)` on the data file would race with rename. Same pattern as `git/index.lock`.
- `NamedTempFile::persist` falls back to `rename(2)` (Unix) / `MoveFileEx` (Windows). Both atomic on the same filesystem.
- `parent().unwrap()` is safe — `SkillStats::path()` always returns a path with at least two segments.

- [ ] **Step 3: Reset-on-manifest-change**

```rust
impl SkillStats {
    /// Returns true if the loaded stats refer to a different manifest
    /// digest than the one currently installed. Callers (the aggregator
    /// and reindex) should `reset()` in that case rather than carry
    /// counters across an upgrade.
    pub fn is_stale(&self, current_digest: &str) -> bool {
        self.manifest_digest != current_digest
    }
}
```

Document the decision in the module rustdoc: a version bump resets `usage_count` / `success_count` / `failure_count` but **preserves** `pinned`, `first_successful_use_at`, and `lifecycle_state` (a Canonical skill bumping to 1.2.0 should not regress to Draft).

- [ ] **Step 4: Sign-scope doc comment**

Add a top-of-module comment:

```rust
//! ## Security
//!
//! `stats.json` is host-local mutable state and is **explicitly outside
//! the DSSE signature scope** (see §2.2 Layer 1 of the skill ecosystem
//! design). A skill's signature covers `skill.yaml` only. Stats can be
//! deleted or rebuilt (`mur skill reindex-stats`) without affecting
//! trust.
```

- [ ] **Step 5: Unit tests**

`mur-common/src/skill/stats.rs` `#[cfg(test)]`:
- `load` returns `Ok(None)` for missing path
- `merge_in_place` round-trips a counter increment
- Two concurrent `merge_in_place` calls from spawned threads both increments commit (file lock works) — use `std::thread::scope` so the test is `cargo test --workspace` safe
- `is_stale` returns true on digest mismatch, false otherwise
- Schema version 1 deserialises a known fixture string

- [ ] **Step 6: Build + commit**

```
cargo build --workspace
cargo test -p mur-common skill::stats
git add mur-common/Cargo.toml mur-common/src/skill/{stats.rs,mod.rs}
git commit -m "feat(skill): SkillStats schema + atomic IO with fd-lock"
```

---

### Task 2 — Lifecycle pure functions (`mur-common::skill::lifecycle`)

**Files:** `mur-common/src/skill/lifecycle.rs` (new), `mur-common/src/skill/mod.rs`.

The functions defined here are **pure** — they take inputs, return outputs, never touch disk. M5b's sweep will call them and persist; M5a's doctor will call them and only display.

- [ ] **Step 1: Half-life + hysteresis constants**

```rust
use chrono::{DateTime, Duration, Utc};
use crate::skill::stats::{LifecycleState, SkillStats};

pub const MIN_CONFIDENCE: f64 = 0.05;
pub const AUTO_ARCHIVE_CONFIDENCE: f64 = 0.10;
pub const AUTO_ARCHIVE_AGE_DAYS: i64 = 180;
pub const MIN_DWELL_HOURS: i64 = 24;

/// Half-life (days) for confidence decay, indexed by current state.
pub fn half_life_days(state: LifecycleState) -> f64 {
    match state {
        LifecycleState::Draft => 14.0,
        LifecycleState::Emerging => 90.0,
        LifecycleState::Stable => 365.0,
        LifecycleState::Canonical => 730.0,
        LifecycleState::Deprecated | LifecycleState::Archived => 365.0,
    }
}

/// Promotion thresholds — values that MUST be exceeded.
pub const PROMOTE_DRAFT_USES: u64 = 3;
pub const PROMOTE_EMERGING_USES: u64 = 10;
pub const PROMOTE_EMERGING_SUCCESS_RATE: f64 = 0.6;
pub const PROMOTE_EMERGING_AGE_DAYS: i64 = 7;
pub const PROMOTE_STABLE_USES: u64 = 30;
pub const PROMOTE_STABLE_SUCCESS_RATE: f64 = 0.8;
pub const PROMOTE_STABLE_AGE_DAYS: i64 = 30;

/// Demotion thresholds — values that MUST drop BELOW. Hysteresis: lower
/// than the symmetric promotion threshold to prevent flap.
pub const DEMOTE_EMERGING_USES: u64 = 8;             // promoted at 10, demoted at <8
pub const DEMOTE_EMERGING_SUCCESS_RATE: f64 = 0.55;  // promoted at 0.6, demoted at <0.55
pub const DEMOTE_STABLE_USES: u64 = 25;
pub const DEMOTE_STABLE_SUCCESS_RATE: f64 = 0.75;
pub const DEPRECATED_SUCCESS_RATE: f64 = 0.3;
pub const DEPRECATED_NO_SUCCESS_DAYS: i64 = 90;
```

- [ ] **Step 2: `calculate_decay`**

Exactly the function we agreed on. Keep the implementation small enough to inline:

```rust
pub fn calculate_decay(
    anchor_confidence: f64,
    last_success: Option<DateTime<Utc>>,
    half_life_days: f64,
    now: DateTime<Utc>,
) -> f64 {
    let conf = anchor_confidence.clamp(0.0, 1.0);
    if !conf.is_finite() || half_life_days <= 0.0 {
        return MIN_CONFIDENCE;
    }
    let last = match last_success {
        None => return MIN_CONFIDENCE,
        Some(t) => t.min(now),  // clock-skew defence
    };
    let days = (now - last).num_seconds() as f64 / 86_400.0;
    if days <= 0.0 {
        return conf;
    }
    (conf * 0.5_f64.powf(days / half_life_days)).max(MIN_CONFIDENCE)
}
```

- [ ] **Step 3: `next_state` — the transition predicate**

```rust
/// Compute what state the skill *should* be in given its current stats
/// and the current time. PURE — does not mutate. Idempotent: calling
/// this twice with the same inputs returns the same output.
///
/// Caller (M5b sweep, or M5a doctor preview) decides whether to
/// persist or merely display the result.
pub fn next_state(stats: &SkillStats, now: DateTime<Utc>) -> LifecycleState {
    // Manual override takes precedence over auto-demotion: pinned
    // skills cannot drop below their pinned tier. (Promotion is still
    // allowed — pin floors but does not ceil.)
    let current = stats.lifecycle_state;

    // Hard archive condition (overrides everything except pinned).
    if !stats.pinned {
        let decayed = calculate_decay(stats.anchor_confidence, stats.last_success_at, half_life_days(current), now);
        if let Some(first_ok) = stats.first_successful_use_at {
            let age_days = (now - first_ok).num_days();
            if decayed < AUTO_ARCHIVE_CONFIDENCE && age_days > AUTO_ARCHIVE_AGE_DAYS {
                return LifecycleState::Archived;
            }
        }
    }

    let success_rate = if stats.usage_count == 0 {
        0.0
    } else {
        stats.success_count as f64 / stats.usage_count as f64
    };
    let age_days = stats.first_successful_use_at
        .map(|t| (now - t).num_days())
        .unwrap_or(0);
    let no_success_days = stats.last_success_at
        .map(|t| (now - t).num_days())
        .unwrap_or(i64::MAX);

    // Deprecation predicate — applies from any non-Archived state.
    if !stats.pinned && current != LifecycleState::Archived
        && (success_rate < DEPRECATED_SUCCESS_RATE && stats.usage_count >= 5
            || no_success_days > DEPRECATED_NO_SUCCESS_DAYS)
    {
        return LifecycleState::Deprecated;
    }

    // Promotion ladder. Each rung requires the prior rung's criteria.
    let can_canonical = stats.pinned // canonical requires explicit human pin per spec §9.1
        && stats.success_count >= PROMOTE_STABLE_USES
        && success_rate >= PROMOTE_STABLE_SUCCESS_RATE
        && age_days >= PROMOTE_STABLE_AGE_DAYS;
    let can_stable = stats.success_count >= PROMOTE_EMERGING_USES
        && success_rate >= PROMOTE_EMERGING_SUCCESS_RATE
        && age_days >= PROMOTE_EMERGING_AGE_DAYS;
    let can_emerging = stats.success_count >= PROMOTE_DRAFT_USES;

    if can_canonical { LifecycleState::Canonical }
    else if can_stable { LifecycleState::Stable }
    else if can_emerging { LifecycleState::Emerging }
    else { LifecycleState::Draft }
}
```

- [ ] **Step 4: `transition_allowed` — dwell + hysteresis guard**

```rust
/// Returns true if the transition from `from` to `to` may be persisted
/// *right now*. Even when `next_state` says a transition is warranted,
/// this guard prevents:
///   - flap within MIN_DWELL_HOURS of the last transition
///   - downward transitions for pinned skills below their pinned tier
///   - hysteresis bounce around exact thresholds (the bands above
///     mostly handle this, but the guard catches edge cases)
pub fn transition_allowed(
    from: LifecycleState,
    to: LifecycleState,
    stats: &SkillStats,
    now: DateTime<Utc>,
) -> bool {
    if from == to { return false; }
    if stats.pinned && rank(to) < rank(from) {
        // pinned floor — refuse downward transitions
        return false;
    }
    let elapsed = now - stats.lifecycle_changed_at;
    if elapsed < Duration::hours(MIN_DWELL_HOURS) {
        return false;
    }
    true
}

fn rank(s: LifecycleState) -> u8 {
    match s {
        LifecycleState::Archived => 0,
        LifecycleState::Deprecated => 1,
        LifecycleState::Draft => 2,
        LifecycleState::Emerging => 3,
        LifecycleState::Stable => 4,
        LifecycleState::Canonical => 5,
    }
}
```

- [ ] **Step 5: `on_promotion` — the anchor-reset gotcha**

```rust
/// Called by the M5b sweep AFTER persisting a promotion. Resets the
/// confidence anchor so the new half-life applies from current, not
/// stale, confidence. Without this, a skill promoted from Draft to
/// Emerging would carry its already-decayed anchor under the longer
/// Emerging half-life and appear artificially fresh forever.
pub fn on_promotion(stats: &mut SkillStats, now: DateTime<Utc>) {
    let prior_half_life = half_life_days(stats.lifecycle_state); // already updated upstream — read the OLD
    // Anchor = currently observed decayed value, fresh moment-in-time.
    let decayed = calculate_decay(stats.anchor_confidence, stats.last_success_at, prior_half_life, now);
    stats.anchor_confidence = decayed;
    stats.lifecycle_changed_at = now;
}
```

Caller contract documented inline: "M5b's sweep MUST call this after writing the new `lifecycle_state` to disk." M5a never calls it.

- [ ] **Step 6: Unit tests** — `mur-common/src/skill/lifecycle.rs` `#[cfg(test)]`:
- decay floor honored at extreme age
- clock-skew (last_success > now) clamped, returns anchor unchanged
- next_state idempotent (call twice → same result)
- promotion through full Draft → Emerging → Stable → Canonical ladder with manufactured stats
- demotion: success_rate drops → returns Deprecated; recover above hysteresis → returns Stable
- pinned floor: skill at Canonical, pinned, with terrible metrics → next_state still ≥ pinned tier (NOTE: implementation today doesn't store pinned_tier — pin guards only against demotion *from current state*. Document this limitation; M5b can extend the schema with `pinned_at_state` if needed.)
- transition_allowed: dwell within 24h → false; identical from==to → false

- [ ] **Step 7: Build + commit**

```
cargo build --workspace
cargo test -p mur-common skill::lifecycle
git add mur-common/src/skill/{lifecycle.rs,mod.rs}
git commit -m "feat(skill): lifecycle + decay pure functions (M5a observability core)"
```

---

### Task 3 — `SkillExecuted` telemetry event + aggregator

**Files:** `mur-agent-runtime/src/telemetry_writer.rs`, `mur-agent-runtime/src/{call-site}.rs`, `mur-core/src/skill_stats/aggregator.rs` (new), `mur-core/src/skill_stats/mod.rs` (new), `mur-core/src/lib.rs`.

- [ ] **Step 1: Locate the skill-execute call site**

Grep for the existing skill fire path (search likely starts in `mur-agent-runtime/src/runtime/` — the M2 hook chain wiring is at `runtime/hook_chain.rs` and skill-fires already feed `LlmCall.fired_skills`). Confirm a single, well-defined point where one skill's execution finishes with a known outcome (success / failure / not-applicable).

If no such point exists (i.e., the M2 wiring only knows "this LLM call fired skill X" without per-skill outcome), the aggregator MUST infer outcomes from the LLM-call telemetry instead: emit one `SkillExecuted { outcome: NotEvaluated }` per name in `fired_skills`. Document the limitation; M5b's sweep / consolidate flow can revisit a finer-grained outcome signal.

- [ ] **Step 2: Add the event variant**

```rust
// mur-agent-runtime/src/telemetry_writer.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOutcome {
    Success,
    Failure,
    NotEvaluated,  // skill fired but outcome unknown (M5a fallback)
}

pub enum Event {
    // ... existing variants ...
    SkillExecuted {
        trace_id: String,
        task_id: String,
        skill_name: String,
        skill_version: String,
        manifest_digest: String,
        outcome: SkillOutcome,
        duration_ms: u64,
    },
}
```

Wire `event_to_notification` arm. Notification method: `"mur.skill.executed"`. Trace JSONL line ends up as one entry per skill execution.

- [ ] **Step 3: Emit `SkillExecuted` at the chosen call site**

Single new `tx.send(Event::SkillExecuted { ... }).await` (or `try_send` for non-blocking) at the call site from Step 1. Reuses the existing telemetry mpsc — does **not** introduce a second channel.

- [ ] **Step 4: Build the aggregator**

```rust
// mur-core/src/skill_stats/aggregator.rs

use chrono::Utc;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

pub const FLUSH_EVERY_EVENTS: usize = 64;
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct Delta {
    usage: u64,
    success: u64,
    failure: u64,
    last_used_at: Option<chrono::DateTime<Utc>>,
    last_success_at: Option<chrono::DateTime<Utc>>,
    first_success_seen: Option<chrono::DateTime<Utc>>,
    manifest_digest: String,
    skill_version: String,
}

pub struct StatsAggregator {
    mur_home: PathBuf,
    deltas: Arc<Mutex<HashMap<String, Delta>>>, // key = skill_name
}

impl StatsAggregator {
    pub fn spawn(mur_home: PathBuf, mut rx: mpsc::Receiver<StatsEvent>) -> Self {
        let deltas: Arc<Mutex<HashMap<String, Delta>>> = Arc::default();
        let deltas_clone = Arc::clone(&deltas);
        let mur_home_clone = mur_home.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(FLUSH_EVERY);
            let mut event_budget = FLUSH_EVERY_EVENTS;
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        flush(&mur_home_clone, &deltas_clone).await;
                    }
                    Some(ev) = rx.recv() => {
                        merge_one(&deltas_clone, ev).await;
                        event_budget = event_budget.saturating_sub(1);
                        if event_budget == 0 {
                            flush(&mur_home_clone, &deltas_clone).await;
                            event_budget = FLUSH_EVERY_EVENTS;
                        }
                    }
                    else => break,  // channel closed → drain + exit
                }
            }
            flush(&mur_home_clone, &deltas_clone).await;
        });
        Self { mur_home, deltas }
    }
}
```

`StatsEvent` is the aggregator's local DTO converted from `Event::SkillExecuted` by a thin adapter — we deliberately do not make `mur-core` depend on the runtime's `Event` enum (the adapter lives wherever the runtime spawns the aggregator).

`flush()` iterates the dirty map, calls `SkillStats::merge_in_place(...)` for each — counters are commutative so per-skill flush ordering is irrelevant, and the file lock per skill keeps multi-agent safety.

- [ ] **Step 5: Wire `SkillExecuted` → aggregator**

In the runtime startup path that already spawns `TelemetryWriter` (currently in `mur-agent-runtime/src/runtime/...`), subscribe a second receiver to the existing notification stream filtered on `mur.skill.executed`, parse it back into `StatsEvent`, and send to the aggregator. **Or** — and this is cleaner — `TelemetryWriter::sender()` already returns a clone-able `mpsc::Sender<Event>`; build a fan-out task that forwards `Event::SkillExecuted` to the aggregator without going through JSON.

Pick fan-out (cheaper, no JSON parse on the inbound side). Document the choice inline.

- [ ] **Step 6: Tests**

`mur-core/tests/skill_stats_aggregator.rs`:
- Spawn aggregator with a temp MUR_HOME containing one fake installed skill (just `skills/foo/skill.yaml` is enough).
- Send 12 `StatsEvent`s mixing success/failure outcomes.
- Sleep `FLUSH_EVERY + 1s`.
- Assert `stats.json` exists with `usage_count = 12`, correct `success_count` / `failure_count`, latest `last_used_at`.
- Crash test: drop the aggregator handle before flush — assert no panic.

- [ ] **Step 7: Build + commit**

```
cargo build --workspace
cargo test -p mur-core skill_stats_aggregator
cargo test -p mur-agent-runtime
git commit -am "feat(skill): SkillExecuted telemetry event + stats aggregator (M5a)"
```

---

### Task 4 — `mur skill reindex-stats` (rebuild from traces)

**Files:** `mur-core/src/skill_stats/reindex.rs` (new), `mur-core/src/cmd/skill_stats.rs` (new), `mur-core/src/cli.rs` or wherever the subcommands enum lives.

Justification: stats sidecars are a cache; when they're missing, corrupt, or behind, the JSONL trace log is the source of truth. This mirrors the existing `mur reindex` (LanceDB rebuild from YAML) pattern.

- [ ] **Step 1: Implement reindex**

```rust
// mur-core/src/skill_stats/reindex.rs

pub struct ReindexOptions {
    pub skill_filter: Option<String>,  // exact name or glob
    pub since: Option<DateTime<Utc>>,  // default: read sidecar watermark
    pub days_back: u32,                 // default: 30
}

pub async fn reindex_stats(mur_home: &Path, opts: ReindexOptions) -> Result<ReindexReport> {
    // 1. Enumerate installed skills (use list_installed())
    // 2. For each matching skill, decide start watermark
    // 3. Scan ~/.mur/traces/*.jsonl (newest day backwards by `days_back`)
    // 4. For each line with method == "mur.skill.executed" and skill_name matches, fold into a fresh SkillStats default
    // 5. SkillStats::merge_in_place to write the rebuilt counters
    // 6. Return report
    todo!()
}
```

Trace parsing reuses `mur-agent-runtime::telemetry_writer::event_to_notification` shape (the JSONL writer's exact output schema).

- [ ] **Step 2: CLI wiring**

```rust
// mur-core/src/cmd/skill_stats.rs

pub async fn cmd_reindex_stats(home: &Path, skill_filter: Option<&str>, days_back: u32) -> Result<()> {
    let report = reindex_stats(home, ReindexOptions {
        skill_filter: skill_filter.map(str::to_string),
        since: None,
        days_back,
    }).await?;
    println!("Reindexed {} skill(s) from {} trace line(s)", report.skills_touched, report.lines_consumed);
    Ok(())
}

pub fn cmd_stats(home: &Path, name: &str) -> Result<()> {
    let path = SkillStats::path(home, name);
    match SkillStats::load(&path)? {
        Some(s) => println!("{}", serde_json::to_string_pretty(&s)?),
        None => println!("no stats for skill '{}' — run `mur skill reindex-stats {}`", name, name),
    }
    Ok(())
}

pub fn cmd_pin(home: &Path, name: &str, reason: Option<&str>) -> Result<()> { /* ... */ }
pub fn cmd_unpin(home: &Path, name: &str) -> Result<()> { /* ... */ }
```

`cmd_pin` / `cmd_unpin` are the **only** M5a CLI paths that intentionally mutate `stats.json` (everything else goes through the aggregator). They use `SkillStats::merge_in_place` directly with a tiny merge_fn that flips `pinned` + writes `pinned_reason`. Document why pin is allowed to skip the aggregator: it's a human decision, not a telemetry signal.

- [ ] **Step 3: Tests**

`mur-core/tests/skill_stats_reindex.rs`:
- Set up MUR_HOME with two installed skills and a synthetic `~/.mur/traces/2026-05-25.jsonl` with 20 mixed `mur.skill.executed` lines for both skills.
- Run reindex.
- Assert both sidecars contain the expected counter totals.
- Assert idempotent: re-run with same args, sidecars unchanged (watermark prevents double-counting).

- [ ] **Step 4: Build + commit**

```
cargo build --workspace
cargo test -p mur-core skill_stats
git commit -am "feat(skill): mur skill {stats,pin,unpin,reindex-stats}"
```

---

### Task 5 — `mur skill doctor` (read-only + `--fix` stub)

**Files:** `mur-core/src/cmd/skill_doctor.rs` (new), `mur-core/Cargo.toml`, CLI wiring.

- [ ] **Step 1: Check trait + registry**

```rust
// mur-core/src/cmd/skill_doctor.rs

pub enum Severity { Ok, Warn, Fail, Unknown }

pub struct Finding {
    pub check_id: &'static str,
    pub category: &'static str,
    pub severity: Severity,
    pub skill_name: String,
    pub message: String,
    pub remediation: Option<String>,
    pub fixable: bool,
}

pub trait Check {
    fn id(&self) -> &'static str;
    fn category(&self) -> &'static str;
    fn run(&self, ctx: &DoctorCtx) -> Vec<Finding>;
}

pub struct DoctorCtx<'a> {
    pub home: &'a Path,
    pub skills: &'a [InstalledSkillView],  // pre-loaded manifest + stats per skill
    pub now: DateTime<Utc>,
}
```

- [ ] **Step 2: Implement the four M5a checks**

| check_id | severity ladder |
|---|---|
| `tool-availability` | Fail if a `requires` MCP tool is not in the agent's trust capability list; Unknown if capability list unreadable |
| `dependency-freshness` | Fail if a required skill is missing; Warn if installed but outside constraint window; Unknown if registry unreachable for "is there a newer version" check |
| `execution-recency` | Ok if `last_success_at` within 30d; Warn 30–90d; Fail (i.e. proposed-archive) >90d; Unknown if stats sidecar missing |
| `failure-rate` | Ok if `success_rate >= 0.9` over last 10 executions; Warn 0.7–0.9; Fail <0.7. Compute "last 10" by tailing the day's JSONL trace for `mur.skill.executed` lines (cheap — bounded scan) |

`api-drift` is registered but stubbed: returns a single `Severity::Unknown` finding with message "deferred to M6 (LLM-driven analysis)".

- [ ] **Step 3: Output formatters + exit code**

```rust
pub enum DoctorFormat { Text, Json }

pub fn format(findings: &[Finding], fmt: DoctorFormat, color: bool, writer: &mut dyn Write) -> Result<()> { /* … */ }

pub fn exit_code(findings: &[Finding], strict: bool) -> i32 {
    use Severity::*;
    let any_fail = findings.iter().any(|f| matches!(f.severity, Fail));
    let any_warn = findings.iter().any(|f| matches!(f.severity, Warn));
    if any_fail { 1 }
    else if strict && any_warn { 1 }
    else { 0 }
}
```

Text format uses `[OK]` / `[!]` / `[X]` / `[?]` symbols. ASCII fallback when `supports_color::on_cached(supports_color::Stream::Stdout).is_none()`. JSON format uses the documented schema (codex-doctor shape) with `schema_version: 1`.

- [ ] **Step 4: `--fix` stub**

Accept `--fix` and `--apply` flags so M5b's CLI extension does not break callers. In M5a both are no-ops:

```rust
if opts.fix {
    eprintln!("warning: --fix is accepted but not yet implemented (requires M5b). Showing findings only.");
}
if opts.apply {
    eprintln!("warning: --apply requires --fix and M5b's repair engine. Showing findings only.");
}
```

- [ ] **Step 5: Selectors**

- No positional / no `--all` → defaults to all installed skills (researched, surveyed against brew/flutter/npm doctor).
- Positional name(s) → exact match.
- Positional containing `*` or `?` → fed to `globset` (requires `--check` to be glob-aware too? No — `--check` is enum-bounded, no glob.).
- `--check tools,deps,recency,failure-rate,api-drift` → subset.
- `--json` → JSON output (suppresses progress chatter to stdout).
- `--strict` → CI-friendly mode where warnings exit 1.

- [ ] **Step 6: Telemetry**

Emit one `tracing::info_span!("mur.skill.doctor.run", checks = ?check_ids, scope = ?scope)` and per-finding `tracing::info_span!("mur.skill.doctor.check", check = id, severity = ?sev, skill = name)`. Reuses the M4 telemetry plumbing already wired through the runtime. The CLI side simply uses `tracing` — the runtime exporters do not need to subscribe (the CLI typically runs in-process without the runtime, so spans only show up in `RUST_LOG=debug` for now). Document this clearly.

- [ ] **Step 7: Tests**

`mur-core/tests/skill_doctor_findings.rs`:
- Tool-availability fixture: skill requires `mcp:nonexistent` → exactly one Fail.
- Dependency-freshness fixture: skill requires `base@^1.2.0` but `base@1.0.0` is installed → exactly one Warn.
- Execution-recency fixture: stats with `last_success_at` set to 100 days ago → Fail.
- Failure-rate fixture: 10 synthetic trace lines, 4 successes → Warn (success_rate 0.4 → Fail per ladder; verify boundary).
- Exit-code matrix: all Ok → 0; one Warn, no strict → 0; one Warn, strict → 1; one Fail → 1.
- JSON output round-trips through serde_json::Value.

- [ ] **Step 8: Build + commit**

```
cargo build --workspace
cargo test -p mur-core skill_doctor
git commit -am "feat(skill): mur skill doctor — read-only health checks (M5a)"
```

---

### Task 6 — `mur skill info --metrics`

**Files:** `mur-core/src/cmd/skill_cmd.rs` (modify existing `cmd_info`).

The existing `mur skill info <name>` prints manifest + signature info. M5a adds a `--metrics` flag that appends the stats + decayed confidence + current vs proposed lifecycle state to that output.

- [ ] **Step 1: Extend `cmd_info` signature**

```rust
pub fn cmd_info(name: &str, full: bool, metrics: bool) -> Result<()> { /* … */ }
```

- [ ] **Step 2: Render block**

```
Metrics:
  state:       Emerging  (proposed: Stable — promotion eligible after sweep)
  pinned:      no
  usage:       42 (success 38 / failure 4 / rate 90%)
  confidence:  0.78  (anchor 1.00, decayed over 14d)
  last used:   2026-05-23 (2 days ago)
  first ok:    2026-05-11 (14 days ago)
```

"proposed" comes from calling `lifecycle::next_state(stats, now)` — pure, no side effect.

- [ ] **Step 3: Tests + commit**

Snapshot test for the rendered block. Commit:

```
git commit -am "feat(skill): mur skill info --metrics"
```

---

## Out of scope — deferred to M5b

All of these explicitly stay out of M5a:

1. **`mur skill sweep`** — the persisting lifecycle sweep that calls `next_state` + `transition_allowed` + `on_promotion` + writes `stats.json` for every skill in one pass. M5a defines the predicate; M5b ships the driver.
2. **`mur skill doctor --fix --apply`** — actually executing repairs (reinstalling deps, deleting stale skills). M5a accepts the flags for forward CLI stability.
3. **`mur skill consolidate`** — dedup, contradiction, orphan, gap detection. M5a's "rebuild from traces" path proves the trace log is queryable; consolidation in M5b builds on that.
4. **`mur skill archive <name>`** — operator-driven archive (vs the auto-archive condition baked into `next_state`). Auto-archive in M5a is computed read-only; the command lands in M5b alongside the sweep.
5. **LanceDB skill vector index** — would replace Jaccard-on-tokens for consolidation dedup. Deferred to M6.
6. **LLM-driven `api-drift` check** — registered as Unknown stub in M5a; the actual implementation needs trace clustering + LLM analysis. M6.

If any of these become blocking before M5b is scheduled, slot them in as Task 7+; each is independent.

---

## Self-Review

**Spec coverage (§ refers to `docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md`):**

| Spec § | Requirement | M5a coverage |
|---|---|---|
| §9.1 | Lifecycle state machine | Pure predicate (`next_state`) — display only; persist deferred to M5b |
| §9.2 | Confidence decay | Pure function (`calculate_decay`) used by doctor + `info --metrics`; anchor stored in stats; on-promotion reset documented for M5b |
| §9.3 | `mur skill doctor` | Four read-only checks shipped; `api-drift` stubbed; `--fix` accepts flag but is a no-op printing a forward-pointer |
| §9.4 | Consolidation | **Deferred to M5b** |
| §10.4 | Health dashboard | `mur skill stats`, `mur skill info --metrics` |
| §14 M5 | `mur skill evolve` (lifecycle sweep) | **Renamed to `mur skill sweep` per M5b design — see Out of scope** |

**Storage decision audit:**
- Sidecar JSON at `~/.mur/skills/<name>/stats.json` — Homebrew INSTALL_RECEIPT.json pattern.
- Manifest digest reference inside stats — detects reinstall/upgrade, resets counters but preserves pin + first-success + state.
- fd-lock on `stats.lock` (not on `stats.json`) — git/index.lock pattern; avoids POSIX `flock` racing with `rename`.
- Sidecar treated as cache → `mur skill reindex-stats` rebuilds from JSONL traces.

**Concurrency story:**
- Hot path: skill execution → mpsc::send (non-blocking try_send acceptable; we accept drop-on-full for telemetry consistent with existing `telemetry_writer.rs:328`) → aggregator task → batched flush.
- File lock window: microseconds (read serde_json + merge in-memory + temp-rename).
- Two `mur` processes racing on the same sidecar: lock serialises them; both increments commit. CRDT property (counter sums commute; max timestamp) makes the rare lost-lock case lose at most one increment.

**Numerical stability:**
- Decay is anchor-based (`anchor * 0.5^(dt/h)`) not incremental. f64 `powf` is monotonic; no drift across calls.
- Floor at MIN_CONFIDENCE = 0.05 prevents underflow-to-zero.
- All timestamps UTC; `last_success.min(now)` clamps clock skew.

**Compile-blocker scan:**
- New crate deps (`fd-lock`, `globset`, `supports-color`, `sysexits`) are pure-Rust with no system requirements.
- The new `Event::SkillExecuted` variant is additive — exhaustive matches in `event_to_notification` must add the arm; Task 3 Step 2 covers it.
- `SkillStats` lives in `mur-common` (no async, no tokio) so it can be linked from both the CLI and the runtime aggregator.

**Behavior regression scan:**
- `mur skill list` gains an extra column (`state`) when stats exist; absent stats render as `Draft` (default). Verify the column is added conditionally so existing snapshot tests do not break, or update snapshots.
- `mur skill info <name>` without `--metrics` is byte-identical to today. With `--metrics` it appends a block.
- The new `Event::SkillExecuted` adds one line per skill execution to today's trace JSONL. Disk growth proportional to skill use — same magnitude as existing `LlmCall` lines. Document in a release note for users monitoring `~/.mur/traces/` size.

**Atomic-write guarantee preserved:** Every write to `stats.json` goes through `merge_in_place` → `NamedTempFile::persist` (POSIX rename / Windows MoveFileEx). No direct `File::write_all` to `stats.json` anywhere.

**Test coverage:**
- Unit: lifecycle predicates, decay math, schema serde — `mur-common`.
- Integration: aggregator end-to-end, reindex end-to-end, doctor per-check, exit-code matrix — `mur-core/tests/`.
- **Not covered**: NFS-mounted MUR_HOME (out of scope), Windows-specific lock semantics on shared volumes (defer until a Windows user reports), pathological trace files >10 GB (we tail the day file; size is bounded by daily rotation in `telemetry_writer.rs`).

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m5a.md`.

Suggested branch: `feat/skill-ecosystem-m5a`, branched from current `main` (M4b is merged, commit `2034508`).

Two execution options:
1. **Linear (`superpowers:executing-plans`)** — work the tasks in order, commit at the end of each task. Roughly 6 commits.
2. **Subagent-driven (`superpowers:subagent-driven-development`)** — Task 1 (sidecar IO) and Task 2 (lifecycle predicates) are fully independent and can be parallelised. Task 3+ depends on Task 1. Task 5 depends on Task 2 + Task 3 (for the failure-rate check that tails traces).

Recommended: subagent-driven for Tasks 1+2 in parallel, then linear for 3 → 4 → 5 → 6.
