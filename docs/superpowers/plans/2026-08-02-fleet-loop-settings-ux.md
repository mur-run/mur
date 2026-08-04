# Fleet Loop Settings UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Hub's fleet Settings panel ask questions a user can answer, and stop the write boundary from silently reinterpreting the values it accepts.

**Architecture:** A new `done_policy` module in `mur-core` turns `done_when` from a string into a three-way policy (router / queue-drained / marker), and the guarded loop gains a `QueueDrained` stop that fires before any LLM call. `cmd_fleet_set_loop` — the single write path for the `loop:` block — validates the five fail-open fields using the same parsers execution uses. The Hub replaces the `done_when` free-text box with a policy select, and gives the cron field preset shapes above it and Rust-computed fire times below it.

**Tech Stack:** Rust (edition 2024, `mur-core` / `mur-agent-runtime`), Tauri 2, React + TypeScript, vitest, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-08-02-fleet-loop-settings-ux-design.md`

**Branch:** `feat/fleet-loop-settings-ux` (already holds the spec at `69fd3083` and the interim numeric-input fix at `89bc8ef3`).

## Global Constraints

- **Rust edition 2024.** `let` chains are stable — `if let Some(x) = a && x > 0` is valid and preferred over nesting.
- **Run Rust tests with `cargo nextest run`, never `cargo test`.** Plain `cargo test --workspace` fails 7 tests spuriously in this repo.
- **`mur-core` needs environment to compile and to run.** Prefix every cargo
  command in this plan with:
  ```
  RUST_MIN_STACK=33554432 ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target
  ```
  Without `ORT_STRATEGY` the onnxruntime link fails; without `MUR_WEB_DIST` the
  dashboard embed fails. `RUST_MIN_STACK` is **required, not a fallback** —
  verified on this machine: without it, `cargo nextest run -p mur-core fleet`
  SIGABRTs on `cli::tests::cli_parses_fleet_create` and
  `cli::agent::tests::cli_action_parses_fleet_flag` from a pre-existing debug
  clap stack overflow. With it, all 285 pass. `CARGO_TARGET_DIR` points at the
  main checkout's already-warm 19G target because this volume has ~23G free and
  a second Tauri target would not fit; drop it if you are not working from a
  worktree.
- **No hardcoded values** (CLAUDE.md rule 1). Every literal that appears in more than one place, or that a reader would have to guess the meaning of, gets a named constant. This plan names them: `DONE_WHEN_QUEUE_EMPTY`, `CRON_PREVIEW_COUNT`, `CRON_PREVIEW_DEBOUNCE_MS`.
- **Single source file ≤ 800 lines** (CLAUDE.md rule 4). `mur-core/src/cmd/fleet/loop_run.rs` is already **1201 lines** — over the limit before this work starts. Task 1 exists so the new `done_when` vocabulary lands in its own module instead of growing it further. Task 2's additions to `loop_run.rs` — the `LoopStop` variant, its `outcome_label` arm, and the break site — are the permitted exception: they belong to the iteration loop and have nowhere else to live. Splitting the rest of `loop_run.rs` is a separate PR and out of scope here.

  **Amended 2026-08-03, after execution.** This section originally claimed "Task 1 removes roughly as much as Task 2 adds, so the file ends this work no larger than it started." That is false and should not be read as if it held: the file ended at **1281 lines, +80**. Task 1 removed ~26; Task 2's review correctly rejected the plan's judgement that an integration test for the new break "is not worth it" (the break fires ahead of every dial, so it needs no live agent), and the resulting `#[tokio::test]` coverage added ~85 lines the size budget never accounted for. The tests are right and stay. The exception is accepted in writing at its true cost, with the follow-up named rather than implied: **moving `loop_run.rs`'s `mod tests` into a `loop_run/tests.rs` child module is a pure-movement PR of its own**, per rule 4's own instruction to keep movement separate from behaviour changes. It is not folded into this branch.
- **Brand is uppercase `MUR`** in anything user-visible (CLAUDE.md rule 7). None of the new strings in this plan mention the brand, so this is a review check, not an action.
- **i18n key parity.** `mur-hub-gui/ui/src/i18n/types.ts` derives `TranslationKey` from `en.ts` and `Table = Record<TranslationKey, string>`. Any key added to or removed from `en.ts` must be added to or removed from `zh-TW.ts` in the same commit, or `tsc` fails.
- **Reply language.** Code, comments, commit messages and this plan are English. The `zh-TW.ts` values are Traditional Chinese (zh-TW), never Simplified.

---

## File Structure

**Create:**
- `mur-core/src/cmd/fleet/done_policy.rs` — the `done_when` vocabulary: the queue sentinel constant, `DonePolicy`, `done_policy()`, and `done_marker()` moved in from `loop_run.rs`. One responsibility: turning a stored `done_when` string into a decision. ~70 lines with tests.

**Modify:**
- `mur-core/src/cmd/fleet/mod.rs` — declare the new module.
- `mur-core/src/cmd/fleet/loop_run.rs` — drop `done_marker` (moved), import from the new module, add `LoopStop::QueueDrained` + its `outcome_label` arm + the break site.
- `mur-core/src/cmd/deep_research/ask.rs:164` — update the `done_marker` path.
- `mur-core/src/cmd/fleet/settings.rs` — `validate_loop_fields`, called from `cmd_fleet_set_loop`.
- `mur-hub-gui/src-tauri/Cargo.toml` — add the `mur-agent-runtime` path dependency (already in the build graph via `mur-core`; the direct dep costs no extra compilation).
- `mur-hub-gui/src-tauri/src/fleet.rs` — `cron_preview` command.
- `mur-hub-gui/src-tauri/src/lib.rs:712` — register it in `invoke_handler`.
- `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts` + `.test.ts` — policy parse/build, cron shape composition, revert the interim `marker:` gate.
- `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` — policy select, cron presets, fire-time preview.
- `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` — new keys, two removals.
- `CLAUDE.md` — the `done_when: marker:<TEXT>` sentence gains `queue-empty`.

---

### Task 1: Extract the `done_when` vocabulary into its own module

Pure code movement plus one new enum. No behavior changes — `loop_run.rs` must
behave identically after this task. This is separated from Task 2 because
`loop_run.rs` is already over the 800-line limit and CLAUDE.md rule 4 requires
movement to land before behavior changes.

**Files:**
- Create: `mur-core/src/cmd/fleet/done_policy.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs:14` (add `pub mod done_policy;` in alphabetical position, between `delete` and `export`)
- Modify: `mur-core/src/cmd/fleet/loop_run.rs:93-105` (remove `done_marker`), `:630` (call site), `:744-751` (its test)
- Modify: `mur-core/src/cmd/deep_research/ask.rs:164` (import path)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub const DONE_WHEN_QUEUE_EMPTY: &str` — the sentinel string `"queue-empty"`
  - `pub enum DonePolicy<'a> { Router, QueueEmpty, Marker(&'a str) }` — derives `Debug, Clone, Copy, PartialEq, Eq`
  - `pub fn done_policy(done_when: &str) -> DonePolicy<'_>`
  - `pub fn done_marker(done_when: &str) -> Option<&str>` — moved verbatim, same signature as before

- [ ] **Step 1: Create the module with the moved function and the new enum**

Create `mur-core/src/cmd/fleet/done_policy.rs`:

```rust
//! What a fleet's `done_when` string means.
//!
//! Three policies answer one question — "when is this fleet finished?" — and
//! the stored form is a single string so `fleet.yaml` keeps one field per
//! question. Parsing lives here rather than in `loop_run.rs` so the read side
//! (lenient: an unrecognised value means "ask the router", which keeps legacy
//! fleets loading) and the write side (strict: `settings.rs` rejects anything
//! outside the three forms) share one vocabulary.

/// The `done_when` sentinel selecting [`DonePolicy::QueueEmpty`].
pub const DONE_WHEN_QUEUE_EMPTY: &str = "queue-empty";

/// Prefix selecting [`DonePolicy::Marker`].
const MARKER_PREFIX: &str = "marker:";

/// How the guarded loop decides a fleet's goal is achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DonePolicy<'a> {
    /// Ask the router DONE/CONTINUE each iteration. The fallback: an empty
    /// criterion, or any legacy free-text one.
    Router,
    /// Stop as soon as an iteration finds no queued job. Deterministic and
    /// needs no cooperation from any agent.
    QueueEmpty,
    /// Converge when an agent emits this text as a whole line.
    Marker(&'a str),
}

/// Classify a stored `done_when`. Unrecognised values are [`DonePolicy::Router`]
/// rather than an error: `mur-common`'s own serde fixture carries
/// `done_when: 'all_tasks_done'`, and fleets written before this vocabulary
/// existed must keep loading.
pub fn done_policy(done_when: &str) -> DonePolicy<'_> {
    if let Some(marker) = done_marker(done_when) {
        return DonePolicy::Marker(marker);
    }
    if done_when.trim() == DONE_WHEN_QUEUE_EMPTY {
        return DonePolicy::QueueEmpty;
    }
    DonePolicy::Router
}

/// A structured `done_when` marker predicate: `marker:<TEXT>` means "converge
/// when a member emits `<TEXT>` as a sentinel (its own line) in the channel".
/// Returns the (trimmed, non-empty) marker text, or None for an empty /
/// non-`marker:` criterion (→ router fallback).
/// Machine-checkable convergence: deterministic and LLM-independent, vs. trusting
/// the router's free-text self-assessment.
pub fn done_marker(done_when: &str) -> Option<&str> {
    done_when
        .strip_prefix(MARKER_PREFIX)
        .map(str::trim)
        .filter(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_marker_parses_structured_criterion() {
        assert_eq!(done_marker("marker:FLEET_DONE"), Some("FLEET_DONE"));
        assert_eq!(done_marker("marker:  SHIPPED  "), Some("SHIPPED")); // trimmed
        assert_eq!(done_marker("marker:"), None); // empty
        assert_eq!(done_marker("marker:   "), None); // whitespace only
        assert_eq!(done_marker("all tasks closed"), None); // free text → router fallback
        assert_eq!(done_marker(""), None);
    }

    #[test]
    fn done_policy_maps_the_three_forms_and_treats_legacy_values_as_router() {
        assert_eq!(done_policy("marker:DONE"), DonePolicy::Marker("DONE"));
        assert_eq!(done_policy(DONE_WHEN_QUEUE_EMPTY), DonePolicy::QueueEmpty);
        assert_eq!(done_policy("  queue-empty  "), DonePolicy::QueueEmpty);
        assert_eq!(done_policy(""), DonePolicy::Router);
        // mur-common's serde fixture carries exactly this shape; it must keep
        // meaning "ask the router" rather than erroring or half-matching.
        assert_eq!(done_policy("all_tasks_done"), DonePolicy::Router);
    }
}
```

- [ ] **Step 2: Declare the module**

In `mur-core/src/cmd/fleet/mod.rs`, add between `pub mod delete;` (line 8) and
`pub mod export;` (line 9):

```rust
pub mod done_policy;
```

- [ ] **Step 3: Run the new tests to verify they pass**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core done_policy
```
Expected: 2 tests pass (`done_marker_parses_structured_criterion`,
`done_policy_maps_the_three_forms_and_treats_legacy_values_as_router`).

At this point `done_marker` exists in two places. The next step removes the old one.

- [ ] **Step 4: Delete the old `done_marker` and its test from `loop_run.rs`**

In `mur-core/src/cmd/fleet/loop_run.rs`, delete the whole `done_marker` function
together with its doc comment (the block beginning `/// A structured
\`done_when\` marker predicate:` and ending with the closing brace, around lines
93-105), and delete the `done_marker_parses_structured_criterion` test (around
lines 744-751).

Then add the import near the other `use super::` lines at the top of the file:

```rust
use super::done_policy::done_marker;
```

- [ ] **Step 5: Update the `deep_research` call site**

In `mur-core/src/cmd/deep_research/ask.rs:164`, change:

```rust
        .and_then(|l| crate::cmd::fleet::loop_run::done_marker(&l.done_when));
```

to:

```rust
        .and_then(|l| crate::cmd::fleet::done_policy::done_marker(&l.done_when));
```

- [ ] **Step 6: Verify the whole crate still compiles and its fleet tests pass**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core fleet
```
Expected: PASS, with no compile errors about `done_marker`. If a test binary
SIGABRTs on a stack overflow, that is a known pre-existing issue with debug clap
parsing, not this change — rerun with `RUST_MIN_STACK=33554432` prefixed.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/done_policy.rs \
        mur-core/src/cmd/fleet/mod.rs \
        mur-core/src/cmd/fleet/loop_run.rs \
        mur-core/src/cmd/deep_research/ask.rs
git commit -m "refactor(fleet): move done_when parsing into its own module

loop_run.rs is 1201 lines, already past the 800-line rule, so the new
done-policy vocabulary lands beside done_marker in a module of its own
rather than growing it further. Adds DonePolicy and the queue-empty
sentinel; no behavior change yet.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Stop the loop when a queue-drained fleet finds nothing queued

**Files:**
- Modify: `mur-core/src/cmd/fleet/loop_run.rs` — `LoopStop` enum (~line 39-56), `outcome_label` (~line 311-322), break site (immediately after the `iteration_goal` call, ~line 485)

**Interfaces:**
- Consumes: `super::done_policy::{done_policy, DonePolicy}` from Task 1.
- Produces: `LoopStop::QueueDrained`, whose `outcome_label` string is `"queue-drained"`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `mur-core/src/cmd/fleet/loop_run.rs`:

```rust
    #[test]
    fn queue_drained_outcome_has_its_own_label() {
        // The progress file's `outcome` is how a caller tells "finished because
        // there was nothing left to do" from "ran out of iterations".
        assert_eq!(outcome_label(LoopStop::QueueDrained), "queue-drained");
        assert_ne!(
            outcome_label(LoopStop::QueueDrained),
            outcome_label(LoopStop::MaxIterations)
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core queue_drained_outcome
```
Expected: FAIL to compile — `no variant named QueueDrained found for enum LoopStop`.

- [ ] **Step 3: Add the variant and its label**

In `mur-core/src/cmd/fleet/loop_run.rs`, add to the `LoopStop` enum after the
`CommanderKilled` variant:

```rust
    /// `done_when: queue-empty` and an iteration found no queued job — the
    /// fleet's work is done because there is none left.
    QueueDrained,
```

And add the matching arm to `outcome_label`, after the `CommanderKilled` arm:

```rust
        LoopStop::QueueDrained => "queue-drained",
```

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core queue_drained_outcome
```
Expected: PASS.

- [ ] **Step 5: Add the break site**

In `mur-core/src/cmd/fleet/loop_run.rs`, find this existing line inside the
iteration loop:

```rust
        let (iter_goal, mut active_job) = iteration_goal(mur_home, name, &fleet.goal)?;
```

Insert immediately after it:

```rust
        // `done_when: queue-empty` — a drained queue IS the completion
        // condition. Checked here, ahead of `plan_via_router` and every other
        // model call, so a cron tick that wakes to an empty queue costs nothing
        // rather than costing a full iteration. Stuck-detection cannot stand in
        // for this: a member replying "what should I run?" counts as progress,
        // so `stuck` resets and the loop runs to the iteration cap.
        if active_job.is_none()
            && let Some(lc) = fleet.loop_cfg.as_ref()
            && done_policy(&lc.done_when) == DonePolicy::QueueEmpty
        {
            println!("── fleet '{name}': job queue empty — nothing to do ──");
            break LoopStop::QueueDrained;
        }
```

Add the import beside the Task 1 one at the top of the file:

```rust
use super::done_policy::{DonePolicy, done_marker, done_policy};
```

(replacing the `use super::done_policy::done_marker;` line added in Task 1)

- [ ] **Step 6: Verify the crate compiles and the fleet suite passes**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core fleet
```
Expected: PASS.

Then check the lint gate, since a new match arm in a non-exhaustive position is
the usual failure here:

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo clippy -p mur-core -- -D warnings
```
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/loop_run.rs
git commit -m "feat(fleet): stop the loop when a queue-drained fleet has nothing queued

A cron-triggered worker fleet with an empty queue re-sends its standing
goal every iteration. Stuck-detection does not catch it — a member
replying 'what should I run?' is new agent activity, so the counter
resets and the loop runs to max_iterations plus one router-convergence
call per iteration, on every tick.

done_when: queue-empty makes the drained queue the completion condition.
The check sits ahead of every model call, so an empty tick is free.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Validate at the write boundary

**Files:**
- Modify: `mur-core/src/cmd/fleet/settings.rs` — new `validate_loop_fields`, called from `cmd_fleet_set_loop`; new tests

**Interfaces:**
- Consumes: `super::done_policy::{done_marker, DONE_WHEN_QUEUE_EMPTY}` (Task 1), `super::loop_run::parse_duration` (already `pub`), `mur_agent_runtime::scheduler::next_fire_after`.
- Produces: nothing consumed by later tasks — this task is terminal for the Rust side.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `mur-core/src/cmd/fleet/settings.rs`:

```rust
    #[test]
    fn validate_rejects_values_that_would_be_silently_reinterpreted() {
        // A calendar date parses as no duration at all, which means "no
        // deadline enforced" — the exact shape mur-common's fixture carries.
        assert!(validate_loop_fields(None, None, Some("2026-12-31"), None, None).is_err());
        // 0 is filtered out by effective_max_iterations and becomes the default.
        assert!(validate_loop_fields(None, Some(0), None, None, None).is_err());
        // Feb 31 never comes: valid syntax, no future firing, silent no-op.
        assert!(validate_loop_fields(Some("cron:0 9 31 2 *"), None, None, None, None).is_err());
        assert!(validate_loop_fields(Some("cron:not a cron"), None, None, None, None).is_err());
        assert!(validate_loop_fields(Some("interval:tuesday"), None, None, None, None).is_err());
        assert!(validate_loop_fields(Some("whenever"), None, None, None, None).is_err());
        // Missing the marker: prefix means router judgment, not marker matching.
        assert!(validate_loop_fields(None, None, None, None, Some("DONE")).is_err());
        // A negative budget is filtered out and becomes "no ceiling".
        assert!(validate_loop_fields(None, None, None, Some(-1.0), None).is_err());
    }

    #[test]
    fn validate_accepts_the_three_done_policies_and_real_schedules() {
        assert!(validate_loop_fields(None, None, None, None, Some("")).is_ok());
        assert!(validate_loop_fields(None, None, None, None, Some("queue-empty")).is_ok());
        assert!(validate_loop_fields(None, None, None, None, Some("marker:DONE")).is_ok());
        assert!(validate_loop_fields(Some("manual"), None, None, None, None).is_ok());
        assert!(validate_loop_fields(Some("interval:30m"), None, None, None, None).is_ok());
        assert!(validate_loop_fields(Some("cron:*/15 * * * *"), None, None, None, None).is_ok());
        assert!(validate_loop_fields(None, Some(1), Some("2h"), Some(0.0), None).is_ok());
        // An empty deadline is "no deadline", which is a legitimate value.
        assert!(validate_loop_fields(None, None, Some(""), None, None).is_ok());
    }

    #[test]
    fn set_loop_judges_only_the_fields_passed_in_this_call() {
        // A fleet already holding a bad deadline must still accept an unrelated
        // partial update. Validating the merged block instead of the arguments
        // would make one stale value block every future edit.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();

        let mut f = store::load_fleet(home, "dev").unwrap();
        f.loop_cfg = Some(FleetLoop {
            trigger: "manual".into(),
            max_iterations: 0,
            budget_usd: 0.0,
            deadline: "2026-12-31".into(),
            done_when: "all_tasks_done".into(),
        });
        store::save_fleet(home, &f).unwrap();

        cmd_fleet_set_loop(home, "dev", None, None, None, Some(2.0), None).unwrap();

        let l = store::load_fleet(home, "dev").unwrap().loop_cfg.unwrap();
        assert_eq!(l.budget_usd, 2.0, "the field we passed was written");
        assert_eq!(
            l.deadline, "2026-12-31",
            "a pre-existing bad value is left alone, not rejected"
        );
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core settings
```
Expected: FAIL to compile — `cannot find function validate_loop_fields`.

- [ ] **Step 3: Write the validator**

In `mur-core/src/cmd/fleet/settings.rs`, add above `cmd_fleet_set_loop`:

```rust
/// Reject a loop setting whose value would be silently reinterpreted as
/// something else.
///
/// Five fields are fail-open at execution time: an unparseable deadline means
/// "no deadline", `max_iterations: 0` means the default, an unfirable cron
/// means "never scheduled", a negative budget means "no ceiling", and a
/// `done_when` without the `marker:` prefix means "ask the router". Each of
/// those is a value that looks configured and does nothing.
///
/// Only the fields passed in THIS call are judged. A fleet already holding a
/// bad value must not have an unrelated partial update (`--budget-usd 2`)
/// rejected because of it — that would match neither the merge semantics
/// documented on `cmd_fleet_set_loop` nor the lenient read side in
/// `done_policy`. Strictness belongs at the write boundary, and this is it:
/// both `mur fleet set-loop` and the Hub's `fleet_set_loop` command land here.
///
/// Validation reuses the parsers execution uses (`parse_duration`,
/// `scheduler::next_fire_after`) so it cannot disagree with what will run.
fn validate_loop_fields(
    trigger: Option<&str>,
    max_iterations: Option<u32>,
    deadline: Option<&str>,
    budget_usd: Option<f64>,
    done_when: Option<&str>,
) -> Result<()> {
    if let Some(t) = trigger {
        let t = t.trim();
        if let Some(expr) = t.strip_prefix("cron:") {
            if mur_agent_runtime::scheduler::next_fire_after(expr, chrono::Local::now()).is_none() {
                bail!("cron expression {expr:?} is invalid or will never fire");
            }
        } else if let Some(dur) = t.strip_prefix("interval:") {
            if super::loop_run::parse_duration(dur).is_none() {
                bail!("interval {dur:?} must be a duration like 30s, 5m, 2h, 1d");
            }
        } else if t != "manual" {
            bail!("trigger must be manual, interval:<dur>, or cron:<5-field expression>");
        }
    }

    if let Some(n) = max_iterations
        && n == 0
    {
        bail!("max_iterations must be at least 1 (0 silently falls back to the default)");
    }

    if let Some(d) = deadline
        && !d.trim().is_empty()
        && super::loop_run::parse_duration(d.trim()).is_none()
    {
        bail!(
            "deadline {d:?} must be a duration relative to the loop start \
             (30s, 5m, 2h, 1d) — not a calendar date"
        );
    }

    if let Some(b) = budget_usd
        && b < 0.0
    {
        bail!("budget_usd must be zero or positive");
    }

    if let Some(dw) = done_when {
        let dw = dw.trim();
        let recognised = dw.is_empty()
            || dw == super::done_policy::DONE_WHEN_QUEUE_EMPTY
            || super::done_policy::done_marker(dw).is_some();
        if !recognised {
            bail!(
                "done_when must be empty (router decides), {}, or marker:<TEXT>",
                super::done_policy::DONE_WHEN_QUEUE_EMPTY
            );
        }
    }

    Ok(())
}
```

Change the import line at the top of the file from:

```rust
use anyhow::Result;
```

to:

```rust
use anyhow::{Result, bail};
```

And update the module doc comment on line 2, which still describes the field as
a marker:

```rust
//! iteration cap, deadline, done-when policy) without touching anything else.
```

- [ ] **Step 4: Call it from the write path**

In `cmd_fleet_set_loop`, insert as the first statement of the function body,
before `let mut fleet = store::load_fleet(mur_home, name)?;`:

```rust
    validate_loop_fields(
        trigger.as_deref(),
        max_iterations,
        deadline.as_deref(),
        budget_usd,
        done_when.as_deref(),
    )?;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run:
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core settings
```
Expected: PASS, including the two pre-existing merge-semantics tests.

Then the full fleet suite, because `cli_fleet.rs` drives `set-loop` end to end:

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core fleet
```
Expected: PASS. If a pre-existing test now fails because it passed a value this
validator rejects, that test was encoding the fail-open behavior — fix the test
to use a valid value, and say so in the commit message.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/fleet/settings.rs
git commit -m "feat(fleet): reject loop settings that would be silently reinterpreted

Five fields are fail-open: 'deadline: 2026-12-31' means no deadline,
'max_iterations: 0' means 8, an unfirable cron means never scheduled, a
negative budget means no ceiling, and a done_when without the marker:
prefix means router judgment. Each looks configured and does nothing.

Validated at the single write boundary both the CLI and the Hub pass
through, using the same parsers execution uses so the check cannot
disagree with what runs. Only fields passed in the call are judged, so a
stale bad value never blocks an unrelated edit, and reads stay lenient so
existing fleets keep loading.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Hub offers completion policies instead of marker strings

**Files:**
- Modify: `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts` — add `DonePolicyKind`, `DONE_WHEN_QUEUE_EMPTY`, `parseDonePolicy`, `buildDoneWhen`, `DONE_POLICY_HINT`; revert the interim `doneWhen` parameter on `settingsAreValid`
- Modify: `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts` — new tests; delete the two interim `marker:` tests
- Modify: `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` — state, save payload, the `done_when` row
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`, `mur-hub-gui/ui/src/i18n/zh-TW.ts`

**Interfaces:**
- Consumes: `DONE_WHEN_QUEUE_EMPTY` must equal the Rust constant from Task 1 (`"queue-empty"`).
- Produces:
  - `export type DonePolicyKind = "router" | "queue-empty" | "marker"`
  - `export function parseDonePolicy(doneWhen: string): DonePolicyKind`
  - `export function buildDoneWhen(kind: DonePolicyKind, loaded: string): string`
  - `export const DONE_POLICY_HINT: Record<DonePolicyKind, TranslationKey>`

- [ ] **Step 1: Write the failing tests**

In `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts`, first
**delete** the two tests added earlier in this branch —
`"rejects a done_when without the marker: prefix"` and
`"accepts a marker: done_when and an empty one"` — then add:

```ts
  it("maps a stored done_when to a policy, treating legacy criteria as router", () => {
    expect(parseDonePolicy("marker:RESEARCH_COMPLETE")).toBe("marker");
    expect(parseDonePolicy("queue-empty")).toBe("queue-empty");
    expect(parseDonePolicy("")).toBe("router");
    // Free-text criteria predate this vocabulary and mean "ask the router",
    // which is what the backend already does with them.
    expect(parseDonePolicy("all_tasks_done")).toBe("router");
    // A prefix with nothing after it is not a usable marker.
    expect(parseDonePolicy("marker:")).toBe("router");
  });

  it("writes an empty string for router, which is how the field gets cleared", () => {
    // `doneWhen.trim() || null` used to send null here, and the backend reads
    // null as "leave alone" -- so the Hub could not clear done_when at all.
    expect(buildDoneWhen("router", "marker:DONE")).toBe("");
    expect(buildDoneWhen("queue-empty", "")).toBe("queue-empty");
    // The Hub never authors a marker; it only preserves the loaded one.
    expect(buildDoneWhen("marker", "marker:RESEARCH_COMPLETE")).toBe("marker:RESEARCH_COMPLETE");
  });
```

Add `parseDonePolicy` and `buildDoneWhen` to the existing import from
`./fleetSettingsForm` at the top of the test file.

- [ ] **Step 2: Run them to verify they fail**

Run:
```bash
cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts
```
Expected: FAIL — `parseDonePolicy is not a function` (or a transform error on the missing export).

- [ ] **Step 3: Implement the helpers**

In `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts`, append:

```ts
/** Which completion policy the Settings select is showing. */
export type DonePolicyKind = "router" | "queue-empty" | "marker";

/** The `done_when` sentinel selecting the queue-drained policy. Must stay in
 *  step with mur-core's `DONE_WHEN_QUEUE_EMPTY`. */
export const DONE_WHEN_QUEUE_EMPTY = "queue-empty";

const MARKER_PREFIX = "marker:";

/**
 * Classify a stored `done_when`, mirroring mur-core's `done_policy`: anything
 * that is neither the queue sentinel nor a usable `marker:` value means "ask
 * the router" -- including legacy free-text criteria, which is exactly what the
 * backend does with them.
 */
export function parseDonePolicy(doneWhen: string): DonePolicyKind {
  const v = doneWhen.trim();
  if (v.startsWith(MARKER_PREFIX) && v.slice(MARKER_PREFIX.length).trim() !== "") return "marker";
  if (v === DONE_WHEN_QUEUE_EMPTY) return "queue-empty";
  return "router";
}

/**
 * The value to save for a chosen policy. `marker` returns the loaded expression
 * verbatim: the Hub never authors a marker, because it cannot supply the other
 * half of the contract -- something has to teach an agent to emit that text,
 * and that lives in the goal or a skill, not in this form.
 *
 * `router` returns "" rather than null on purpose. The backend treats a null as
 * "leave this field alone", so an explicit empty string is the only way to
 * clear a previously-set criterion.
 */
export function buildDoneWhen(kind: DonePolicyKind, loaded: string): string {
  if (kind === DONE_WHEN_QUEUE_EMPTY) return DONE_WHEN_QUEUE_EMPTY;
  if (kind === "marker") return loaded.trim();
  return "";
}

/** Which hint line explains the currently-selected policy. */
export const DONE_POLICY_HINT: Record<DonePolicyKind, TranslationKey> = {
  router: "fleet.settings.donePolicyHintRouter",
  "queue-empty": "fleet.settings.donePolicyHintQueueEmpty",
  marker: "fleet.settings.donePolicyHintMarker",
};
```

In the same file, revert `settingsAreValid` to its pre-branch signature by
removing the `doneWhen` parameter and the marker check — the free-text field it
guarded no longer exists:

```ts
export function settingsAreValid(
  trigKind: TriggerKind,
  trigValue: string,
  deadline: string
): boolean {
  if (trigKind === "interval" && !DURATION_RE.test(trigValue.trim())) return false;
  if (trigKind === "cron" && trigValue.trim() === "") return false;
  if (deadline.trim() !== "" && !DURATION_RE.test(deadline.trim())) return false;
  return true;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts
```
Expected: PASS.

- [ ] **Step 5: Add the i18n keys and remove the two that no longer apply**

In `mur-hub-gui/ui/src/i18n/en.ts`, **delete** these two lines:

```ts
  "fleet.settings.doneWhenHint": "marker:DONE (optional)",
  "fleet.settings.doneWhenHelp":
    "Must start with marker: — the member ends the loop by emitting that text on its own line. Leave empty to let the router decide each iteration.",
```

and add in their place:

```ts
  "fleet.settings.donePolicyRouter": "Router decides each iteration",
  "fleet.settings.donePolicyQueueEmpty": "Stop when the job queue is empty",
  "fleet.settings.donePolicyHintRouter":
    "Costs one model call per iteration and can misjudge. The iteration cap, deadline and budget still bound the loop.",
  "fleet.settings.donePolicyHintQueueEmpty":
    "Finishes as soon as an iteration finds nothing queued. Free and deterministic — the right choice for a fleet you feed with jobs.",
  "fleet.settings.donePolicyHintMarker":
    "Converges when a member emits this text on a line of its own. Set in fleet.yaml, alongside whatever teaches the agent to emit it.",
```

In `mur-hub-gui/ui/src/i18n/zh-TW.ts`, delete the same two keys and add:

```ts
  "fleet.settings.donePolicyRouter": "每輪由路由器判斷",
  "fleet.settings.donePolicyQueueEmpty": "佇列清空時結束",
  "fleet.settings.donePolicyHintRouter":
    "每輪多一次模型呼叫，而且可能誤判。迭代上限、截止時間與預算仍然框得住迴圈。",
  "fleet.settings.donePolicyHintQueueEmpty":
    "某一輪發現沒有排隊的工作就結束。零成本且確定，適合用工作餵養的機群。",
  "fleet.settings.donePolicyHintMarker":
    "成員單獨一行輸出這段文字即收斂。請在 fleet.yaml 設定，並在同處寫明是誰教成員輸出它。",
```

- [ ] **Step 6: Replace the `done_when` input with the policy select**

In `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx`:

Extend the import from `./fleetSettingsForm`:

```ts
import {
  parseTrigger,
  buildTrigger,
  settingsAreValid,
  modeBadgeLabel,
  loopDeadlineIsValid,
  parseDonePolicy,
  buildDoneWhen,
  DONE_POLICY_HINT,
  type TriggerKind,
  type DonePolicyKind,
} from "./fleetSettingsForm";
```

Replace the `doneWhen` state declaration:

```ts
  const [doneWhen, setDoneWhen] = useState(detail.loop_cfg?.done_when ?? "");
```

with:

```ts
  const loadedDoneWhen = detail.loop_cfg?.done_when ?? "";
  const loadedDonePolicy = parseDonePolicy(loadedDoneWhen);
  const [donePolicy, setDonePolicy] = useState<DonePolicyKind>(loadedDonePolicy);
```

In `handleSaveSettings`, change the guard and the payload field:

```ts
    if (!settingsAreValid(trigKind, trigValue, deadline)) return;
```

```ts
        doneWhen: buildDoneWhen(donePolicy, loadedDoneWhen),
```

Change the Save button's `disabled` expression back to the three-argument form:

```tsx
          disabled={busy !== null || !settingsAreValid(trigKind, trigValue, deadline)}
```

Replace the whole `done_when` row and the hint below it:

```tsx
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.doneWhen")}</label>
          <input
            value={doneWhen}
            onChange={(e) => setDoneWhen(e.target.value)}
            placeholder={t("fleet.settings.doneWhenHint")}
          />
        </div>
        <div className="fleet-settings__hint">{t("fleet.settings.doneWhenHelp")}</div>
```

with:

```tsx
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.doneWhen")}</label>
          <select
            value={donePolicy}
            onChange={(e) => setDonePolicy(e.target.value as DonePolicyKind)}
          >
            <option value="router">{t("fleet.settings.donePolicyRouter")}</option>
            <option value="queue-empty">{t("fleet.settings.donePolicyQueueEmpty")}</option>
            {/* Only offered when one is already set: the Hub preserves a marker
                but never authors one, because it cannot supply the half of the
                contract that teaches an agent to emit the text. */}
            {loadedDonePolicy === "marker" && (
              <option value="marker">{loadedDoneWhen.trim()}</option>
            )}
          </select>
        </div>
        <div className="fleet-settings__hint">{t(DONE_POLICY_HINT[donePolicy])}</div>
```

- [ ] **Step 7: Typecheck and run the whole UI suite**

Run:
```bash
cd mur-hub-gui/ui && npx tsc --noEmit && npx vitest run
```
Expected: no type errors (a missing or extra key in `zh-TW.ts` shows up here),
all tests pass.

- [ ] **Step 8: Commit**

```bash
git add mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts \
        mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts \
        mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx \
        mur-hub-gui/ui/src/i18n/en.ts \
        mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): ask which completion policy a fleet uses, not which string

The done_when box asked for a value whose valid contents depend on prose
the user wrote elsewhere -- something has to teach an agent to emit the
marker, and that lives in the goal or a skill, nowhere near the field. A
select of the three policies asks a question the user can answer.

A marker option appears only when one is already stored, so deep-research
keeps its RESEARCH_COMPLETE and the Hub never authors a contract it
cannot complete.

Also fixes clearing: 'doneWhen.trim() || null' sent null for an empty
string, and the backend reads null as leave-alone, so the field could
never be cleared from the Hub at all.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Cron presets above the expression, fire times below it

**Files:**
- Modify: `mur-hub-gui/src-tauri/Cargo.toml` — add `mur-agent-runtime` path dependency
- Modify: `mur-hub-gui/src-tauri/src/fleet.rs` — `cron_preview` command
- Modify: `mur-hub-gui/src-tauri/src/lib.rs:712` — register it
- Modify: `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts` + `.test.ts` — `CronShape`, `buildCronExpr`, preview constants
- Modify: `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` — shape select, time input, preview
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts`

**Interfaces:**
- Consumes: `mur_agent_runtime::scheduler::next_n_fires(expr: &str, count: usize) -> anyhow::Result<Vec<chrono::DateTime<Local>>>` — errors on an unparseable expression, returns an empty vec for one that parses but never fires.
- Produces:
  - Tauri command `cron_preview(expr: String, count: usize) -> Result<Vec<String>, String>`
  - `export type CronShape = "custom" | "hourly" | "daily" | "weekdays"`
  - `export function buildCronExpr(shape: CronShape, time: string): string`
  - `export const CRON_PREVIEW_COUNT: number`, `export const CRON_PREVIEW_DEBOUNCE_MS: number`

- [ ] **Step 1: Add the dependency**

In `mur-hub-gui/src-tauri/Cargo.toml`, add next to the existing
`mur-core = { path = "../../mur-core" }` line:

```toml
mur-agent-runtime = { path = "../../mur-agent-runtime" }
```

This crate is already in the build graph via `mur-core`, so the direct
dependency adds no compilation — it just avoids a pass-through wrapper in
`mur-core` that would exist only to re-export one function.

- [ ] **Step 2: Write the Tauri command**

In `mur-hub-gui/src-tauri/src/fleet.rs`, add after `fleet_set_loop`:

```rust
/// The next `count` fire times for a 5-field cron expression, formatted in the
/// machine's local time.
///
/// Deliberately routed through `mur_agent_runtime::scheduler` rather than a
/// JavaScript cron library: the daemon decides due-ness with this same parser,
/// and a preview that disagrees with the scheduler (on six-field padding, or
/// day-of-week numbering) is worse than no preview at all.
///
/// `Err` means the expression does not parse. `Ok(vec![])` means it parses but
/// will never fire again — two different problems, and the caller shows two
/// different messages.
#[tauri::command]
pub fn cron_preview(expr: String, count: usize) -> Result<Vec<String>, String> {
    let fires = mur_agent_runtime::scheduler::next_n_fires(expr.trim(), count)
        .map_err(|e| e.to_string())?;
    Ok(fires
        .iter()
        .map(|t| t.format("%-m/%-d %H:%M").to_string())
        .collect())
}
```

- [ ] **Step 3: Register it**

In `mur-hub-gui/src-tauri/src/lib.rs`, add to the `invoke_handler` list beside
`fleet::fleet_set_loop` (line 712):

```rust
            fleet::cron_preview,
```

- [ ] **Step 4: Verify the Rust side compiles**

Run:
```bash
cd mur-hub-gui/src-tauri && cargo check
```
Expected: no errors. If the UI `dist/` is missing, Tauri's build script fails —
create a stub `mur-hub-gui/ui/dist/index.html` (do not commit it) and retry.

- [ ] **Step 5: Write the failing test for expression composition**

In `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts`, add:

```ts
  it("composes a cron expression from a shape and a HH:MM time", () => {
    // Hourly uses the minute only -- the hour a user picked is meaningless for
    // "every hour", and silently keeping it would make 09:15 fire once a day.
    expect(buildCronExpr("hourly", "09:15")).toBe("15 * * * *");
    expect(buildCronExpr("daily", "09:05")).toBe("5 9 * * *");
    expect(buildCronExpr("weekdays", "18:00")).toBe("0 18 * * 1-5");
    // A native time input is empty until touched; midnight is the safe read.
    expect(buildCronExpr("daily", "")).toBe("0 0 * * *");
  });
```

Add `buildCronExpr` to the import from `./fleetSettingsForm`.

- [ ] **Step 6: Run it to verify it fails**

Run:
```bash
cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts
```
Expected: FAIL — `buildCronExpr is not a function`.

- [ ] **Step 7: Implement composition and the preview constants**

In `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts`, append:

```ts
/** Preset schedule shapes offered above the raw cron field. `custom` means the
 *  user is editing the expression directly and no preset applies. */
export type CronShape = "custom" | "hourly" | "daily" | "weekdays";

/** How many upcoming fire times to show under the cron field. */
export const CRON_PREVIEW_COUNT = 3;
/** Idle time before asking the backend to re-evaluate a typed expression. */
export const CRON_PREVIEW_DEBOUNCE_MS = 300;

/**
 * Compose a 5-field cron expression from a preset shape and the `HH:MM` string
 * a native <input type="time"> produces.
 *
 * Three shapes, not a full builder: these plus the existing `interval:<dur>`
 * trigger cover the schedules fleets actually use, and anything rarer is a
 * direct edit of the expression -- which stays visible and is verified by the
 * fire-time preview either way.
 */
export function buildCronExpr(shape: CronShape, time: string): string {
  const [h = "", m = ""] = time.split(":");
  const hour = Number(h) || 0;
  const minute = Number(m) || 0;
  if (shape === "hourly") return `${minute} * * * *`;
  if (shape === "weekdays") return `${minute} ${hour} * * 1-5`;
  return `${minute} ${hour} * * *`;
}
```

- [ ] **Step 8: Run the test to verify it passes**

Run:
```bash
cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts
```
Expected: PASS.

- [ ] **Step 9: Add the i18n keys**

In `mur-hub-gui/ui/src/i18n/en.ts`, add beside the other `fleet.settings.*` keys:

```ts
  "fleet.settings.cronShape": "Schedule",
  "fleet.settings.cronShapeCustom": "Custom expression",
  "fleet.settings.cronShapeHourly": "Every hour",
  "fleet.settings.cronShapeDaily": "Every day",
  "fleet.settings.cronShapeWeekdays": "Weekdays",
  "fleet.settings.cronNext": "Next",
  "fleet.settings.cronLocalTime": "local time",
  "fleet.settings.cronInvalid": "This is not a valid 5-field cron expression",
  "fleet.settings.cronNeverFires": "Valid, but this will never fire again",
```

In `mur-hub-gui/ui/src/i18n/zh-TW.ts`, add:

```ts
  "fleet.settings.cronShape": "排程樣式",
  "fleet.settings.cronShapeCustom": "自訂運算式",
  "fleet.settings.cronShapeHourly": "每小時",
  "fleet.settings.cronShapeDaily": "每天",
  "fleet.settings.cronShapeWeekdays": "平日",
  "fleet.settings.cronNext": "接下來",
  "fleet.settings.cronLocalTime": "本機時間",
  "fleet.settings.cronInvalid": "這不是有效的 5 欄位 cron 運算式",
  "fleet.settings.cronNeverFires": "運算式有效，但不會再觸發",
```

- [ ] **Step 10: Wire the presets and preview into the form**

In `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx`:

Change the React import to include `useEffect`:

```ts
import { useState, useEffect } from "react";
```

Extend the `./fleetSettingsForm` import with:

```ts
  buildCronExpr,
  CRON_PREVIEW_COUNT,
  CRON_PREVIEW_DEBOUNCE_MS,
  type CronShape,
```

Add state beside the other trigger state:

```ts
  const [cronShape, setCronShape] = useState<CronShape>("custom");
  const [cronTime, setCronTime] = useState("09:00");
  const [cronFires, setCronFires] = useState<string[] | null>(null);
  const [cronInvalid, setCronInvalid] = useState(false);
```

Add the debounced preview effect after that state:

```ts
  // Ask the backend what this expression will actually do. Debounced so typing
  // does not fire a command per keystroke; the cleanup cancels an in-flight
  // timer so only the latest value is ever evaluated.
  useEffect(() => {
    if (trigKind !== "cron" || trigValue.trim() === "") {
      setCronFires(null);
      setCronInvalid(false);
      return;
    }
    const timer = setTimeout(() => {
      invoke<string[]>("cron_preview", { expr: trigValue, count: CRON_PREVIEW_COUNT })
        .then((fires) => {
          setCronFires(fires);
          setCronInvalid(false);
        })
        .catch(() => {
          setCronFires(null);
          setCronInvalid(true);
        });
    }, CRON_PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [trigKind, trigValue]);
```

Add a helper above the `return (` of the component:

```ts
  function applyCronShape(shape: CronShape, time: string) {
    setCronShape(shape);
    setCronTime(time);
    if (shape !== "custom") setTrigValue(buildCronExpr(shape, time));
  }
```

Replace this existing block:

```tsx
        {trigKind === "interval" && !DURATION_RE.test(trigValue.trim()) && (
          <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
        )}
```

with the same warning followed by the cron preset row and preview:

```tsx
        {trigKind === "interval" && !DURATION_RE.test(trigValue.trim()) && (
          <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
        )}
        {trigKind === "cron" && (
          <div className="fleet-settings__row">
            <label>{t("fleet.settings.cronShape")}</label>
            <select
              value={cronShape}
              onChange={(e) => applyCronShape(e.target.value as CronShape, cronTime)}
            >
              <option value="custom">{t("fleet.settings.cronShapeCustom")}</option>
              <option value="hourly">{t("fleet.settings.cronShapeHourly")}</option>
              <option value="daily">{t("fleet.settings.cronShapeDaily")}</option>
              <option value="weekdays">{t("fleet.settings.cronShapeWeekdays")}</option>
            </select>
            {cronShape !== "custom" && (
              <input
                type="time"
                value={cronTime}
                onChange={(e) => applyCronShape(cronShape, e.target.value)}
              />
            )}
          </div>
        )}
        {trigKind === "cron" && cronInvalid && (
          <div className="fleet-settings__warning">{t("fleet.settings.cronInvalid")}</div>
        )}
        {trigKind === "cron" && cronFires !== null && cronFires.length === 0 && (
          <div className="fleet-settings__warning">{t("fleet.settings.cronNeverFires")}</div>
        )}
        {trigKind === "cron" && cronFires !== null && cronFires.length > 0 && (
          <div className="fleet-settings__hint">
            {t("fleet.settings.cronNext")}: {cronFires.join(" · ")} ({t("fleet.settings.cronLocalTime")})
          </div>
        )}
```

- [ ] **Step 11: Typecheck and run the whole UI suite**

Run:
```bash
cd mur-hub-gui/ui && npx tsc --noEmit && npx vitest run
```
Expected: no type errors, all tests pass.

- [ ] **Step 12: Update CLAUDE.md**

In `CLAUDE.md`, find the `mur fleet` bullet's sentence:

```
converges on `done_when: marker:<TEXT>` (own-line sentinel, not substring) or router DONE/CONTINUE.
```

Replace it with:

```
converges on `done_when: marker:<TEXT>` (own-line sentinel, not substring), `done_when: queue-empty` (stop once an iteration finds nothing queued), or router DONE/CONTINUE.
```

- [ ] **Step 13: Commit**

```bash
git add mur-hub-gui/src-tauri/Cargo.toml \
        mur-hub-gui/src-tauri/Cargo.lock \
        mur-hub-gui/src-tauri/src/fleet.rs \
        mur-hub-gui/src-tauri/src/lib.rs \
        mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts \
        mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts \
        mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx \
        mur-hub-gui/ui/src/i18n/en.ts \
        mur-hub-gui/ui/src/i18n/zh-TW.ts \
        CLAUDE.md
git commit -m "feat(hub): cron presets and a live fire-time preview

Choosing Cron handed the user a raw 5-field expression to type by hand,
with no way to tell a working one from one that never fires. A shape
select plus a native time input composes the three shapes fleets use;
the expression stays visible and editable as the source of truth.

Under it, the next three fire times from the same parser the daemon uses
to decide due-ness -- a JS cron library could disagree on six-field
padding or day-of-week numbering, and a preview that disagrees with the
scheduler is worse than none. An unparseable expression and one that
parses but never fires are different messages.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Verification before opening the PR

- [ ] Full Rust gate:
  ```bash
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
    cargo nextest run -p mur-core && \
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
    cargo clippy --workspace -- -D warnings && \
  cargo fmt --all --check
  ```
  A `-p mur-core` run can SIGABRT on ~7 `bin/mur` CLI-parse tests from a
  pre-existing debug-clap stack overflow; prefix `RUST_MIN_STACK=33554432` if so.

- [ ] Full UI gate:
  ```bash
  cd mur-hub-gui/ui && npx tsc --noEmit && npx vitest run && npx eslint .
  ```

- [ ] Hub Rust gate (CI runs clippy on `--lib` with a fresh compile, which has
  reddened PRs that passed locally):
  ```bash
  cd mur-hub-gui/src-tauri && cargo clippy --lib -- -D warnings
  ```

- [ ] Manual check in a built Hub `.app` (`build.sh` does **not** build the Hub —
  it needs `npm run build` then `cargo tauri build`): open a fleet's Settings,
  confirm the completion select shows two options for `builder` and three for
  `deep-research`, pick Cron + Every day and confirm the expression fills in and
  three fire times appear below it.
