# mur Windows CI Hardening — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Land the `write_summary` `force` bypass that mirrors Phase 3.5's `write_rollup` fix, add an adversarial CLI regression test that locks the byte-equality-swallows-`--force` bug class, and introduce a crate-level `paths::mur_root` helper that Phase 2 of the hardening effort will use to sweep ~35 `dirs::home_dir()` direct callers.

**Architecture:** Three independent pieces, each small. (1) A new `mur-core/src/paths.rs` with a single `mur_root(override) -> PathBuf` primitive + 3 unit tests. (2) A `force: bool` parameter threaded through `write_summary` (signature change + one non-test caller update + update to 9 test call sites + 1 new unit test). (3) A new CLI integration test in `cli_conversations.rs` that drives `mur conversations compact` twice back-to-back with `--force` and asserts `.history/` gets an archived entry. Zero changes to existing Phase 3.5 or Phase 3.5.1 surface.

**Tech Stack:** Rust 2024 edition, tokio, chrono, tempfile, serde_json, existing mur tooling.

**Base directory:** `/Volumes/Firecuda4tb/Projects/mur/.worktrees/windows-ci-hardening-p1`. Branch: `fix/windows-ci-hardening-p1`. Spec: `docs/superpowers/specs/2026-04-23-mur-windows-ci-hardening-p1-design.md`.

---

## Task 1: `crate::paths::mur_root` helper

**Files:**
- Create: `mur-core/src/paths.rs`
- Modify: `mur-core/src/lib.rs` — add `pub mod paths;` in alphabetical order

### Step 1: Write the new module with its 3 unit tests

Create `mur-core/src/paths.rs` with the following contents (production code + tests in one file per Rust convention):

```rust
//! Crate-level path helpers.
//!
//! Use `mur_root` when you need the `.mur` data directory from code outside
//! `conversations/`. Respects `MUR_HOME` as an authoritative override — on
//! Windows, `dirs::home_dir()` calls `SHGetKnownFolderPath` and ignores
//! `HOME`/`USERPROFILE` env overrides, so tests that redirect via env vars
//! need this escape hatch.
//!
//! Semantics are identical to `conversations::paths::mur_root`; Phase 2 of
//! the Windows CI hardening effort will sweep the ~35
//! `dirs::home_dir().join(".mur")` direct callers in the crate to use this
//! one-line helper.

use std::path::PathBuf;

pub fn mur_root(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("MUR_HOME")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("no home dir").join(".mur")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mur_root_uses_explicit_override_first() {
        let p = mur_root(Some("/tmp/fake-mur"));
        assert_eq!(p, PathBuf::from("/tmp/fake-mur"));
    }

    #[test]
    fn mur_root_honors_mur_home_env_when_no_override() {
        // Serialize via the crate-local env-mutex so this test does not race
        // against `conversations` tests that also mutate MUR_HOME.
        let _g = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_HOME", "/tmp/via-env") };
        assert_eq!(mur_root(None), PathBuf::from("/tmp/via-env"));
        unsafe { std::env::remove_var("MUR_HOME") };
    }

    #[test]
    fn mur_root_falls_back_to_home_dir_when_env_empty() {
        let _g = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_HOME", "") };
        let p = mur_root(None);
        assert!(p.ends_with(".mur"), "expected .../.mur, got: {p:?}");
        unsafe { std::env::remove_var("MUR_HOME") };
    }
}
```

### Step 2: Register the module in `lib.rs`

In `mur-core/src/lib.rs`, insert `pub mod paths;` in alphabetical order — specifically between `pub mod llm;` and `pub mod retrieve;` (i.e. after line that declares `llm` and before the line that declares `retrieve`).

Current layout of the relevant lines in `lib.rs`:
```rust
pub mod llm;
pub mod retrieve;
```

After edit:
```rust
pub mod llm;
pub mod paths;
pub mod retrieve;
```

### Step 3: Run the tests

Run: `cargo test -p mur-core paths::tests --quiet -- --test-threads=1`

Expected:
```
test result: ok. 3 passed; 0 failed; 0 ignored
```

Use `--test-threads=1` to avoid env-var races with the conversations tests. Expect all 3 tests to pass.

Also run the full mur-core suite to confirm no regressions from adding the module:

Run: `cargo test -p mur-core`

Expected: all pass. Adding a new module with `#[cfg(test)] mod tests` cannot break other tests, but we verify.

### Step 4: fmt + clippy clean

Run: `cargo fmt -p mur-core && cargo clippy -p mur-core --all-targets -- -D warnings`

Expected: zero diff, zero warnings.

### Step 5: Commit

```bash
git add mur-core/src/paths.rs mur-core/src/lib.rs
git commit -m "feat: Windows CI hardening P1 Task 1 — crate::paths::mur_root helper"
```

---

## Task 2: `write_summary` force bypass

**Files:**
- Modify: `mur-core/src/conversations/summarize/writer.rs:49-144` (signature + byte-equality guard)
- Modify: `mur-core/src/conversations/summarize/writer.rs` test module at bottom — 1 new unit test + update existing call sites
- Modify: `mur-core/src/conversations/summarize/mod.rs:244` (one-line production call-site update)

### Step 1: Write the failing unit test

In `mur-core/src/conversations/summarize/writer.rs`, find the test module (starts near line 600). Locate the existing `dummy_doc(date: NaiveDate) -> SummaryDoc` helper at line ~605 and the existing `write_rollup_force_bypasses_idempotency` test (search for that name) as the template.

Append a new test, placed right after `write_rollup_force_bypasses_idempotency`:

```rust
    #[tokio::test]
    async fn write_summary_force_bypasses_idempotency() {
        // Windows CI Hardening Phase 1 — mirrors `write_rollup_force_bypasses_idempotency`.
        // Two consecutive writes with byte-identical bodies (same date, same
        // `generated_at` second) must NOT noop when force=true; must archive
        // the prior and rewrite. Guards the bug class Phase 3.5 fixed for
        // `write_rollup` from reappearing in `write_summary`.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let doc = dummy_doc(date);
        let _ = write_summary(&doc, vec![0.0; 16], vec![], false, Some(root))
            .await
            .unwrap();
        let r2 = write_summary(&doc, vec![0.0; 16], vec![], true, Some(root))
            .await
            .unwrap();
        assert!(!r2.noop, "force=true must NOT noop on identical content");
        assert!(r2.archived.is_some(), "force=true must archive the prior");
    }
```

### Step 2: Run the test to verify it fails

Run: `cargo test -p mur-core write_summary_force_bypasses_idempotency`

Expected: compile-fail — the new `force` argument position doesn't exist on `write_summary` yet. This is the correct red state.

### Step 3: Change the `write_summary` signature and add the `!force` guard

In `mur-core/src/conversations/summarize/writer.rs`, locate the signature near line 49. Replace:

```rust
pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    span_embeddings: Vec<Vec<f32>>,
    root_override: Option<&str>,
) -> Result<WriteResult> {
```

with:

```rust
pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    span_embeddings: Vec<Vec<f32>>,
    force: bool,
    root_override: Option<&str>,
) -> Result<WriteResult> {
```

Then in the body, locate the byte-equality noop block. Current code (near line 65):

```rust
    if prior_exists {
        let existing = std::fs::read_to_string(&md_path)?;
        if existing == new_body {
            return Ok(WriteResult {
                path: md_path,
                archived: None,
                noop: true,
            });
        }
```

Replace with:

```rust
    if prior_exists {
        let existing = std::fs::read_to_string(&md_path)?;
        // Byte-equality short-circuit is a best-effort optimization for the
        // "rerun with same inputs" path. `--force` bypasses it — users who
        // pass --force explicitly want a fresh archive + rewrite regardless
        // of whether the body happens to be byte-identical (e.g. two runs
        // in the same wall-clock second producing the same generated_at).
        // Mirrors the fix in `write_rollup` (Phase 3.5 post-review).
        if !force && existing == new_body {
            return Ok(WriteResult {
                path: md_path,
                archived: None,
                noop: true,
            });
        }
```

No other changes in the body. The archive + rewrite branch is unchanged.

### Step 4: Update the non-test call site in `summarize/mod.rs`

In `mur-core/src/conversations/summarize/mod.rs`, line 244 currently reads:

```rust
    match writer::write_summary(&doc, summary_embedding, span_embeddings, root_override).await {
```

Replace with:

```rust
    match writer::write_summary(&doc, summary_embedding, span_embeddings, force, root_override).await {
```

Verify `force` is in scope at this call site — `compact_day` takes `force: bool` as a parameter, so it should be. If it's not (the variable might be named differently in the enclosing fn), grep the enclosing function signature. The spec confirmed `compact_day` already has `force: bool`, so this should be a one-line drop-in.

### Step 5: Update the 9 existing test call sites in `writer.rs`

The test module in `writer.rs` has 9 call sites of `write_summary` that now need the new `false` arg between `span_embeddings` and `root_override`. They are at approximate lines 642, 662, 665, 679, 684, 699, 937, 963 (and one may be in a test helper). Use `grep -n "write_summary(" mur-core/src/conversations/summarize/writer.rs` to get the exact current line numbers, then update each one.

Example transformation — BEFORE:
```rust
        let r = write_summary(&doc, vec![0.0; 16], vec![], Some(root))
            .await
            .unwrap();
```

AFTER:
```rust
        let r = write_summary(&doc, vec![0.0; 16], vec![], false, Some(root))
            .await
            .unwrap();
```

All 9 existing test sites use `Some(root)` as the current last argument. The mechanical change is: insert `false,` immediately before `Some(root)` (preserving any `.await.unwrap()` suffix).

Double-check the 9 call sites include the `write_summary_force_bypasses_idempotency` test you added in Step 1. That test uses `false` and `true` values explicitly — don't reduce them to `false` during the bulk edit.

### Step 6: Compile-check and run

Run: `cargo build -p mur-core`

Expected: clean build. If the compiler complains about `missing argument force` anywhere you haven't covered, fix that call site (grep again if needed).

Run: `cargo test -p mur-core write_summary_force_bypasses_idempotency`

Expected: PASS.

Also run the broader summarize + writer tests to confirm no regressions:

Run: `cargo test -p mur-core summarize::`

Expected: all pass.

### Step 7: Full suite + fmt + clippy

Run:
```bash
cargo test -p mur-core
cargo fmt -p mur-core
cargo clippy -p mur-core --all-targets -- -D warnings
```

Expected: all tests pass, zero fmt diff, zero clippy warnings.

### Step 8: Commit

```bash
git add mur-core/src/conversations/summarize/writer.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "fix(summarize): Windows CI hardening P1 Task 2 — write_summary force bypass"
```

---

## Task 3: CLI adversarial regression test

**Files:**
- Modify: `mur-core/tests/cli_conversations.rs` — append 1 new test at the bottom

### Step 1: Locate the insertion point

Read the bottom of `mur-core/tests/cli_conversations.rs` and identify where the Phase 3.2.1 `mur_conversations_rollup_force_still_regenerates` test ends. The new test pairs with it — rollup-force for the rollup path, compact-force for the daily-summary path.

### Step 2: Write the adversarial integration test

Append to `mur-core/tests/cli_conversations.rs`:

```rust
/// Windows CI Hardening Phase 1 — adversarial regression guard for the
/// "same-wall-clock-second byte-equality swallows --force" bug class.
///
/// Phase 3.5 fixed this for `write_rollup` after it flaked on Windows;
/// Phase 1 of the hardening effort fixes the matching shape in
/// `write_summary` and locks the invariant with this test. Fails if any
/// future writer reintroduces the byte-equality noop short-circuit without
/// a `!force` guard.
///
/// Pairs with `mur_conversations_rollup_force_still_regenerates` (Phase 3.2.1).
#[test]
fn mur_conversations_compact_force_unconditionally_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    // Seed one raw JSONL line so compact has something to do.
    let raw = mur_home
        .join("conversations")
        .join("raw")
        .join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    let line = serde_json::json!({
        "v": 1,
        "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code",
        "conv": "c1",
        "role": "user",
        "content": {"t": "text", "v": "seed content for force-archive test"},
        "meta": {},
        "refs": []
    });
    std::fs::write(
        raw.join("cc_c1.jsonl"),
        serde_json::to_string(&line).unwrap() + "\n",
    )
    .unwrap();

    // First compact — produces .md under summary/<date>.md.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("first compact");
    assert!(
        out1.status.success(),
        "first compact failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Immediately re-compact with --force. Same wall-clock second is
    // possible on a fast runner. Pre-fix, the byte-equality short-circuit
    // in `write_summary` swallows --force silently. Post-fix, the !force
    // guard archives the prior md unconditionally.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact", "--force"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("second compact --force");
    assert!(
        out2.status.success(),
        "second compact --force failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Assertion: .history/ exists with ≥1 archived entry.
    let hist = mur_home
        .join("conversations")
        .join("summary")
        .join(".history");
    assert!(
        hist.exists(),
        ".history/ must exist after --force triggered an archive; \
         stdout of --force call:\n{}",
        String::from_utf8_lossy(&out2.stdout)
    );
    let archived = std::fs::read_dir(&hist)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert!(
        archived >= 1,
        "Phase 1 hardening: compact --force must unconditionally archive \
         the prior md even when the body is byte-identical. Found \
         {archived} archived files; expected ≥1. stdout:\n{}",
        String::from_utf8_lossy(&out2.stdout)
    );
}
```

### Step 3: Verify the test compiles and determine whether `mur conversations compact --force` works end-to-end

Run: `cargo build -p mur-core --tests`

Expected: clean build.

Run: `cargo test -p mur-core --test cli_conversations mur_conversations_compact_force_unconditionally_archives -- --test-threads=1`

Expected: PASS.

**If the test FAILS on the `.history/ must exist` assertion:** this indicates Task 2's `!force` guard didn't take effect end-to-end (most likely `compact_day` isn't threading `force` into `write_summary` correctly). Go back to Task 2 Step 4 and verify the call-site edit landed. Do NOT weaken the assertion to make the test pass.

**If the test FAILS on the first `compact` call:** verify `mur conversations compact` supports the `--force` CLI flag. Run `cargo run -p mur-core -- conversations compact --help` to confirm — it should be listed. If not, then `force` isn't plumbed from CLI to `compact_day` and Task 2's fix is incomplete. Escalate.

### Step 4: Run the full cli_conversations suite to confirm no regression

Run: `cargo test -p mur-core --test cli_conversations -- --test-threads=1`

Expected: all tests pass, including the existing Phase 3.2.1 `mur_conversations_rollup_force_still_regenerates`, Phase 3.5 `mur_ask_stage_1b_*`, Phase 3.5.1 `mur_ask_cli_*`, and this new test.

### Step 5: Full workspace suite

Run: `cargo test --workspace`

Expected: all pass.

### Step 6: fmt + clippy clean

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`

Expected: zero diff, zero warnings.

### Step 7: Commit

```bash
git add mur-core/tests/cli_conversations.rs
git commit -m "test: Windows CI hardening P1 Task 3 — adversarial compact --force archive test"
```

---

## Final Verification

- [x] **Step 1: Full suite + lint**

Run:
```bash
cargo fmt --check --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: zero format diff, zero clippy warnings, all tests pass — including:
- 3 new `paths::tests::*` unit tests
- `write_summary_force_bypasses_idempotency` unit test
- `mur_conversations_compact_force_unconditionally_archives` integration test
- All existing tests (Phase 3.5 rollup-force test still passing under existing fix)

- [x] **Step 2: Spec cross-check**

Re-read `docs/superpowers/specs/2026-04-23-mur-windows-ci-hardening-p1-design.md` §11 "Success criteria". Verify each item:

1. `write_summary_force_bypasses_idempotency` passes — ✅ Task 2.
2. `mur_conversations_compact_force_unconditionally_archives` passes — ✅ Task 3.
3. `cargo test --workspace` green across platforms — ✅ Final Verification Step 1.
4. `cargo clippy` clean — ✅ Final Verification Step 1.
5. `cargo fmt --check` clean — ✅ Final Verification Step 1.
6. `crate::paths::mur_root` resolvable from any `mur-core` module — ✅ Task 1 (registered in `lib.rs`).
7. Phase 2 follow-up issue/section in PR description — handled when opening the PR (not a code task).

- [x] **Step 3: Confirm zero regression on cold paths**

Run: `cargo test --workspace 2>&1 | grep -E "test result:|FAILED"`

Expected: zero `FAILED` lines. All `test result:` lines end with `0 failed`.

---

## Notes for the implementing agent

1. **Spec is source of truth.** When in doubt between plan and spec, spec wins. Flag the discrepancy back to the controller so the plan gets updated.
2. **Task 2's 9 test call sites is approximate** — use `grep -n "write_summary(" mur-core/src/conversations/summarize/writer.rs` to get the authoritative list for your branch state. The compiler will reject any missed site with a clear error — trust it.
3. **The force arg position matters** — `write_summary(doc, summary_vec, span_vec, force, root_override)`. Same position as `write_rollup(doc, vec, force, root_override)`, so the two `write_*` functions stay shape-aligned.
4. **Don't touch Pattern A callers in this PR.** The spec is explicit: Phase 2 of the hardening effort sweeps those. If you accidentally start editing `dirs::home_dir()` sites, stop and commit only Task 1/2/3 scope.
5. **Windows parity unverified locally** — macOS/Linux contributors cannot directly verify the Windows-specific behavior. Rely on CI. The test is designed to catch the bug class on any platform where two back-to-back compacts can share a wall-clock second, which is nearly always.
6. **`ENV_LOCK` is `pub(crate)`** — the Task 1 test reference `crate::conversations::ENV_LOCK` is verified reachable.
