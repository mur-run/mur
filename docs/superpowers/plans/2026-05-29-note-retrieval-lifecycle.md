# Note Retrieval → Lifecycle Promotion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make notes actually *evolve* — record a retrieval event when `mur notes search` surfaces a note, count it in the existing stats reducer, so the existing lifecycle sweep promotes the note through the maturity ladder (Draft → Emerging → …). This closes the loop that makes "managed lifecycle for personal notes" — the product's differentiator — real instead of inert.

**Architecture:** Reuse the existing trace → stats → sweep pipeline rather than build anything new. A note retrieval appends one JSONL line (`method: mur.note.retrieved`, `mur.skill.name`, `mur.skill.outcome: success`) to `~/.mur/traces/<date>.jsonl` — the same log `reindex_stats` already reads. `reindex_stats` is extended to count `mur.note.retrieved` lines as usage+success alongside `mur.skill.executed`. The already-wired `run_sweep` (`skill_lifecycle/sweep.rs`) then reduces stats through `next_state` and persists the promotion. No new state machine, no new reducer, no new sweep.

**Tech Stack:** Rust 2024, `serde_json` (already a mur-core dep), `chrono`, existing `mur_common::telemetry` constants, `mur_core::skill_stats::reindex::reindex_stats`, `mur_core::skill_lifecycle::sweep::run_sweep`, `mur_common::skill::stats::SkillStats`. No new dependencies.

**Depends on:** Plans 1, 2, A, and E (notes create+search) merged — Task 3 edits the `cmd_search` from Plan E, and the integration test calls `do_create`/`do_search` from Plan E.

**Out of scope (later plans):** recording retrievals from MCP/`show`/`list`; hybrid (vector) search; automatic periodic sweep scheduling; Pattern removal.

---

## Design notes (verified)

1. **Trace log is the source of truth; stats sidecars are caches** (`reindex.rs:1-2`). Therefore retrievals must be recorded into the trace log, not written straight to `SkillStats` (a direct write would be wiped on the next `reindex-stats`).
2. **`reindex_stats` filters by substring** (`reindex.rs:102`: `if !trimmed.contains("mur.skill.executed") { continue; }`) and reads `mur.skill.name` / `mur.skill.outcome` / `ts`. A retrieval line carrying those same keys is counted correctly once the substring filter also accepts `mur.note.retrieved`.
3. **A retrieval is a successful use** → emit `mur.skill.outcome: "success"` so `reindex_stats` increments both `usage_count` and `success_count`. With `PROMOTE_DRAFT_USES = 3` (`lifecycle.rs:26`), three retrievals make a note eligible for Emerging (count-only gate — no age/rate needed for Draft→Emerging).
4. **`transition_allowed` enforces `MIN_DWELL_HOURS = 24`** (`lifecycle.rs:12,154`): a promotion is blocked until 24h after `lifecycle_changed_at`. The integration test therefore sweeps with `now = Utc::now() + 2 days` so the dwell gate passes. (Production sweeps run later than note creation, so this is only a test concern.)
5. **`reindex_stats` is `async`** — tests that call it use `#[tokio::test]`.
6. **Recording is best-effort** — a failure to write the trace must not fail the user's search. `cmd_search` logs a `tracing::warn` and continues.
7. **Which notes count as "retrieved":** the notes actually returned by `do_search` (top-`limit`), not every candidate scored. This matches "surfaced to the user."

---

## File map

- **Modify:** `mur-common/src/telemetry.rs` — add `METHOD_NOTE_RETRIEVED`.
- **Modify:** `mur-core/src/cmd/notes_cmd.rs` — add `record_retrieval`; call it from `cmd_search`; integration test.
- **Modify:** `mur-core/src/skill_stats/reindex.rs` — count `mur.note.retrieved` lines; unit test.

---

## Task 1: `record_retrieval` — append a retrieval event to the trace log

**Files:**
- Modify: `mur-common/src/telemetry.rs` — add the method constant.
- Modify: `mur-core/src/cmd/notes_cmd.rs` — add `record_retrieval` + test.

- [ ] **Step 1: Add the telemetry constant**

In `mur-common/src/telemetry.rs`, after `pub const METHOD_SKILL_STEP_RESOLVED: ...` (line 61):

```rust
/// A `category: note` skill was surfaced to the user by a query. Counted by
/// `reindex_stats` as a usage + success so retrieval drives the note lifecycle.
pub const METHOD_NOTE_RETRIEVED: &str = "mur.note.retrieved";
```

- [ ] **Step 2: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `mur-core/src/cmd/notes_cmd.rs`:

```rust
#[test]
fn record_retrieval_appends_a_countable_trace_line() {
    use chrono::Utc;
    let tmp = tempdir().unwrap();
    let now = Utc::now();

    record_retrieval(tmp.path(), "my-note", now).unwrap();

    let path = tmp
        .path()
        .join("traces")
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("mur.note.retrieved"));

    let line = content.lines().next().unwrap();
    let val: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(val.get("mur.skill.name").and_then(|v| v.as_str()), Some("my-note"));
    assert_eq!(val.get("mur.skill.outcome").and_then(|v| v.as_str()), Some("success"));
}

#[test]
fn record_retrieval_appends_not_overwrites() {
    use chrono::Utc;
    let tmp = tempdir().unwrap();
    let now = Utc::now();
    record_retrieval(tmp.path(), "n", now).unwrap();
    record_retrieval(tmp.path(), "n", now).unwrap();
    let path = tmp.path().join("traces")
        .join(now.format("%Y-%m-%d").to_string()).with_extension("jsonl");
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.lines().count(), 2);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::record_retrieval_appends_a_countable_trace_line`
Expected: COMPILE ERROR — `cannot find function 'record_retrieval' in this scope`.

- [ ] **Step 4: Implement `record_retrieval`**

In `mur-core/src/cmd/notes_cmd.rs`, add near the top (after the existing `use` lines):

```rust
use chrono::{DateTime, Utc};
use mur_common::telemetry::METHOD_NOTE_RETRIEVED;
```

Then add the function (above the `#[cfg(test)]` block):

```rust
/// Append a retrieval event for `skill_name` to today's trace log so the stats
/// reducer (`reindex_stats`) counts it as a successful usage. The trace log is
/// the source of truth for stats, so retrievals are recorded here rather than
/// written directly to the stats sidecar (which `reindex-stats` would overwrite).
pub fn record_retrieval(mur_home: &Path, skill_name: &str, now: DateTime<Utc>) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    let line = serde_json::json!({
        "ts": now.to_rfc3339(),
        "method": METHOD_NOTE_RETRIEVED,
        "mur.skill.name": skill_name,
        "mur.skill.outcome": "success",
    });

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    use std::io::Write;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::record_retrieval`
Expected: both `record_retrieval_*` tests PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/telemetry.rs mur-core/src/cmd/notes_cmd.rs
git commit -m "feat(notes): record_retrieval appends mur.note.retrieved trace events

Trace log is the source of truth; retrievals are recorded there so the
existing stats reducer counts them. Best-effort append, never overwrites."
```

---

## Task 2: Count `mur.note.retrieved` in `reindex_stats`

**Files:**
- Modify: `mur-core/src/skill_stats/reindex.rs` — extend the event filter (line ~102); add a test.

- [ ] **Step 1: Write the failing test**

Append to the test module in `mur-core/src/skill_stats/reindex.rs` (if no `#[cfg(test)] mod tests` exists, add one with `use super::*;`):

```rust
#[tokio::test]
async fn reindex_counts_note_retrieval_events_as_usage_and_success() {
    use chrono::Utc;
    use mur_common::skill::stats::SkillStats;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let now = Utc::now();

    // A note skill must exist on disk for reindex to consider it.
    let dir = tmp.path().join("skills").join("my-note");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("skill.yaml"),
        "name: my-note\nversion: 1.0.0\npublisher: human:test\n\
         category: note\ndescription: d\ncontent:\n  abstract: a\n  note: b\n",
    )
    .unwrap();

    // Three retrieval lines in today's trace file.
    let traces_dir = tmp.path().join("traces");
    std::fs::create_dir_all(&traces_dir).unwrap();
    let trace_path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");
    let line = format!(
        "{{\"ts\":\"{}\",\"method\":\"mur.note.retrieved\",\
         \"mur.skill.name\":\"my-note\",\"mur.skill.outcome\":\"success\"}}",
        now.to_rfc3339()
    );
    std::fs::write(&trace_path, format!("{line}\n{line}\n{line}\n")).unwrap();

    reindex_stats(
        tmp.path(),
        ReindexOptions {
            skill_filter: Some("my-note".into()),
            since: None,
            days_back: 1,
        },
    )
    .await
    .unwrap();

    let stats = SkillStats::load(&SkillStats::path(tmp.path(), "my-note"))
        .unwrap()
        .unwrap();
    assert_eq!(stats.usage_count, 3);
    assert_eq!(stats.success_count, 3);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core skill_stats::reindex::tests::reindex_counts_note_retrieval_events_as_usage_and_success`
Expected: FAIL — `usage_count` is 0, because the substring filter rejects `mur.note.retrieved` lines.

- [ ] **Step 3: Extend the event filter**

In `mur-core/src/skill_stats/reindex.rs`, change the filter (line ~102):

```rust
                // Count skill executions and note retrievals; both carry
                // mur.skill.name + mur.skill.outcome.
                if !trimmed.contains("mur.skill.executed")
                    && !trimmed.contains("mur.note.retrieved")
                {
                    continue;
                }
```

No other change is needed: the existing code already reads `mur.skill.name`, `mur.skill.outcome` (== "success" → `success_count++`), and `ts` for timestamps, all of which a retrieval line provides.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core skill_stats::reindex::tests::reindex_counts_note_retrieval_events_as_usage_and_success`
Expected: PASS.

- [ ] **Step 5: Confirm existing reindex behavior is unchanged**

Run: `cargo test -p mur-core skill_stats::`
Expected: all existing reindex tests still PASS (the filter only *adds* an accepted event type).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skill_stats/reindex.rs
git commit -m "feat(stats): reindex_stats counts mur.note.retrieved as usage+success"
```

---

## Task 3: Record retrievals from `cmd_search`

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs` — `cmd_search` records each returned note.

- [ ] **Step 1: Update `cmd_search` to record retrievals (best-effort)**

Replace the `cmd_search` function body (from Plan E) so it records a retrieval for every note it prints:

```rust
/// Top-level `mur notes search` handler.
pub fn cmd_search(query: &str, limit: usize) -> Result<()> {
    let home = resolve_mur_home()?;
    let ranked = do_search(&home, query, limit)?;
    if ranked.is_empty() {
        println!("No notes match '{query}'.");
        return Ok(());
    }
    for (i, sp) in ranked.iter().enumerate() {
        println!(
            "{:>2}. {:<40} score={:.3}  {}",
            i + 1,
            sp.item.manifest.name,
            sp.score,
            sp.item.manifest.description
        );
    }

    // Record a retrieval for each surfaced note so it accrues lifecycle usage.
    // Best-effort: a trace-write failure must not fail the search.
    let now = Utc::now();
    for sp in &ranked {
        if let Err(e) = record_retrieval(&home, &sp.item.manifest.name, now) {
            tracing::warn!(note = %sp.item.manifest.name, error = %e, "record retrieval failed");
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Build and run the notes test surface**

Run: `cargo build -p mur-core && cargo test -p mur-core cmd::notes_cmd::`
Expected: clean build, all existing notes tests PASS. (`cmd_search`'s recording loop is covered end-to-end in Task 4; this step confirms no regression and that `Utc`/`record_retrieval` are in scope.)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "feat(notes): cmd_search records a retrieval for each surfaced note"
```

---

## Task 4: Integration test — retrievals promote Draft → Emerging

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs` — full-loop integration test.

- [ ] **Step 1: Write the test**

Append to the `#[cfg(test)] mod tests` block in `mur-core/src/cmd/notes_cmd.rs`:

```rust
#[tokio::test]
async fn three_retrievals_promote_a_note_from_draft_to_emerging() {
    use chrono::{Duration, Utc};
    use mur_common::skill::stats::{LifecycleState, SkillStats};
    use crate::skill_stats::reindex::{reindex_stats, ReindexOptions};
    use crate::skill_lifecycle::sweep::{run_sweep, SweepOptions};

    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "rust-errors", "Rust error handling", "# body\nanyhow").unwrap();

    // Surface the note three times.
    let now = Utc::now();
    for _ in 0..3 {
        record_retrieval(tmp.path(), "rust-errors", now).unwrap();
    }

    // Reduce the trace into stats.
    reindex_stats(
        tmp.path(),
        ReindexOptions { skill_filter: Some("rust-errors".into()), since: None, days_back: 1 },
    )
    .await
    .unwrap();

    let stats_path = SkillStats::path(tmp.path(), "rust-errors");
    let before = SkillStats::load(&stats_path).unwrap().unwrap();
    assert_eq!(before.usage_count, 3);
    assert_eq!(before.success_count, 3);
    assert_eq!(before.lifecycle_state, LifecycleState::Draft);

    // Sweep with a future `now` so the 24h MIN_DWELL_HOURS gate passes.
    run_sweep(
        tmp.path(),
        SweepOptions {
            filter: Some("rust-errors".into()),
            dry_run: false,
            now: now + Duration::days(2),
        },
    )
    .unwrap();

    let after = SkillStats::load(&stats_path).unwrap().unwrap();
    assert_eq!(
        after.lifecycle_state,
        LifecycleState::Emerging,
        "3 retrievals + dwell should promote Draft -> Emerging (PROMOTE_DRAFT_USES=3)"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::three_retrievals_promote_a_note_from_draft_to_emerging`
Expected: PASS — proof that the create → retrieve → reduce → sweep loop actually evolves a note's lifecycle.

If it fails at the final assertion with `Draft`, check: (a) the trace file is named with the same date used in `reindex` (both use `now`), (b) `days_back: 1` covers today, (c) the sweep `now` is ≥ 24h after `lifecycle_changed_at` (the `+ Duration::days(2)` ensures this).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "test(notes): end-to-end retrievals promote a note Draft -> Emerging"
```

---

## Task 5: Verification gate — full workspace and lints

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all pass. Net new tests: 2 (Task 1) + 1 (Task 2) + 1 (Task 4) = 4.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 3: Run `cargo fmt --check`**

Run: `cargo fmt --check`
Expected: clean. If not:

```bash
cargo fmt
git add -u
git commit --amend --no-edit
```

- [ ] **Step 4: Manual end-to-end smoke (optional but recommended)**

```bash
TMPHOME=$(mktemp -d)
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes create rust-errors \
  -d "Rust error handling" --body-file <(echo "# anyhow vs thiserror")
# Search three times to accrue retrievals
for i in 1 2 3; do MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes search "rust error"; done
# Reduce + sweep, then inspect lifecycle
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- skill reindex-stats rust-errors
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- skill stats rust-errors
```

Expected: `skill stats rust-errors` shows `usage_count: 3`. (Lifecycle promotion via the production `skill` sweep command depends on the 24h dwell, so it may still read `Draft` in a same-day manual run — that is correct behavior, not a bug. The 24h gate is exercised in the Task 4 automated test with a future `now`.)

If `MUR_HOME` is not honored by `resolve_mur_home`, consult `mur-core/src/cmd/agent/mod.rs:89` for the actual home-resolution contract.

- [ ] **Step 5: Final commit if cleanup was needed**

If Steps 2-3 required fixes, the amend handles it. Otherwise nothing extra.

---

## Done state

After this plan:

- `record_retrieval` appends a `mur.note.retrieved` event to `~/.mur/traces/<date>.jsonl`.
- `mur notes search` records a retrieval for every note it surfaces (best-effort).
- `reindex_stats` counts `mur.note.retrieved` as usage + success, alongside `mur.skill.executed`.
- The existing `run_sweep` promotes notes through the maturity ladder from accrued retrievals — verified end-to-end (Draft → Emerging after 3 retrievals + dwell).
- **The lifecycle differentiator is now real for notes:** a note's maturity reflects how often it is actually used, with zero new state-machine code — entirely through the existing trace → reduce → sweep pipeline.

**What this unlocks / pairs with (later plans):**
- `mur notes list` can display each note's `lifecycle_state` — surfacing the now-meaningful maturity.
- `mur notes show` should also call `record_retrieval` (a one-line addition once `show` exists).
- A scheduled/automatic sweep (cron or daemon tick) so promotion happens without a manual `skill sweep`.

**What this does NOT do:**
- No automatic sweep scheduling — promotion still requires `mur skill reindex-stats` + `mur skill sweep` (or their internal equivalents) to run. Wiring an automatic tick is a separate plan.
- No retrieval recording from MCP or `show`/`list` yet (those commands don't exist or aren't wired).
- No decay-driven demotion testing — covered by existing `skill_lifecycle` tests.
