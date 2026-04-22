# mur Conversations Phase 3.2.1 — `--src` Rollup Fix + `--if-stale` No-op Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close two Phase 3.2 known-gap bugs: `--src` silently dropping rollup hits, and `--if-stale` behaving as a silent alias for `--force`. No schema changes, no migration.

**Architecture:** Fix 1 changes `gather_hits` in `ask/retrieve.rs` to pass `None` (not `primary_src`) to the layer=3/4 k-NN searches — rollups surface based purely on embedding relevance. Fix 2 un-ORs `--if-stale` from `force` inside `cmd_conversations_rollup`, keeping the flag as a documented no-op for backward compat. Help text updated in `main.rs` for both flags.

**Tech Stack:** Rust 2024 · no new Cargo dependencies · pure mechanical changes.

**Spec:** `docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-2-1-design.md` (commit `135583c`).
**Depends on:** Phase 3.4 shipped (`958b48c`).

---

## File Structure

**Modify (3 files across 2 fixes):**

```
mur-core/src/conversations/ask/retrieve.rs      Fix 1: layer=3/4 search calls pass None; update inline comment; + 1 new test
mur-core/src/main.rs                            Fix 1: --src help text; Fix 2: --if-stale help text
mur-core/src/cmd/conversations_cmd.rs           Fix 2: un-OR --if-stale from force (2 sites)
mur-core/tests/cli_conversations.rs             Fix 2: + 2 new integration tests
```

No new files. No new Cargo dependencies. No LanceDB schema changes. No migration.

---

## Task Overview (3 tasks, all haiku)

| # | Task | Model | Depends on |
|---|------|-------|------------|
| 1 | Fix 1: `--src` rollup inclusion + 1 unit test | haiku | — |
| 2 | Fix 2: `--if-stale` no-op + 2 integration tests | haiku | — |
| 3 | Golden-path sanity check (17/17 still green) | haiku | 1, 2 |

Tasks 1 and 2 are independent — they touch disjoint code paths and could technically run in parallel, but the subagent-driven-development skill serializes them.

---

## Task 1: `--src` rollup inclusion

**Files:**
- Modify: `mur-core/src/conversations/ask/retrieve.rs` (lines 42-46 inline comment; lines 57-62 k-NN call args; new test appended to `#[cfg(test)] mod tests`)
- Modify: `mur-core/src/main.rs` (lines 352-354 `--src` help text inside `Commands::Ask`)

### 1a. Failing test — rollup hits surface despite `--src` filter

- [ ] **Step 1: Failing test** — append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/ask/retrieve.rs`, after the existing `gather_hits_escalates_to_layer_0_when_all_upper_empty` test (around line 721):

```rust
    #[tokio::test]
    async fn gather_hits_rollup_surfaces_despite_src_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();

        // Seed one layer=2 cc span + one layer=3 rollup ("week" synthetic source).
        let s = make_msg("c_span", "span text");
        idx.upsert_with_layer(&[(s, vec![0.7; 16], 2)]).await.unwrap();
        idx.upsert_rollup_row(crate::conversations::index::RollupRow {
            id: "wk_2026-W16_L3_0",
            ts: 0,
            source: "week",
            conv_id: "week:2026-W16",
            layer: 3,
            content: "week narrative",
            vector: &vec![0.7; 16],
        })
        .await
        .unwrap();

        // Query with --src cc filter active (would exclude layer=3 rows pre-3.2.1).
        let args = RetrieveArgs {
            query_embedding: vec![0.7; 16],
            filters: &Filters {
                source: vec![Source::ClaudeCode],
                since: None,
                until: None,
                min_score: 0.0,
            },
            k_summary: 8,
            k_raw: 4,
            escalation_threshold: 0.3,
            mmr_threshold: 0.95,
            no_escalate: false,
            max_context_tokens: 6000,
            root_override: Some(root),
        };
        let hits = gather_hits(args).await.unwrap();
        let layers: Vec<i8> = hits.iter().map(|h| h.layer).collect();
        assert!(
            layers.contains(&2),
            "cc layer=2 span must survive source filter; layers: {layers:?}"
        );
        assert!(
            layers.contains(&3),
            "layer=3 rollup must surface despite --src filter (Phase 3.2.1); layers: {layers:?}"
        );
    }
```

- [ ] **Step 2: Run — must fail** (current `gather_hits` passes `primary_src` to the layer=3 search, so the k-NN predicate `source = 'cc'` excludes the week rollup row whose source is `'week'`):

```
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-2-1
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::retrieve::tests::gather_hits_rollup_surfaces_despite_src_filter
```

Expected: test runs, the assertion `layers.contains(&3)` fails (layer=3 not in results — the rollup was filtered out at the DB level).

### 1b. Implement Fix 1 — layer=3/4 searches pass `None`

- [ ] **Step 3: Edit `gather_hits` in `mur-core/src/conversations/ask/retrieve.rs`**

Current body around lines 57-62 (the layer=3 and layer=4 k-NN calls):

```rust
    let l3 = idx
        .search(&args.query_embedding, k_each, primary_src, Some(3))
        .await?;
    let l4 = idx
        .search(&args.query_embedding, k_each, primary_src, Some(4))
        .await?;
```

Change to:

```rust
    // Phase 3.2.1: rollup rows are multi-source aggregates by construction
    // (built from day summaries across all enabled sources). The --src filter
    // applies only to day-level content (layers 0/1/2); rollups surface based
    // purely on embedding relevance. Pass None so the LanceDB predicate
    // doesn't exclude them via source-column mismatch.
    let l3 = idx
        .search(&args.query_embedding, k_each, None, Some(3))
        .await?;
    let l4 = idx
        .search(&args.query_embedding, k_each, None, Some(4))
        .await?;
```

Also replace the inline Phase 3.2 doc comment (currently lines 42-46) with the Phase 3.2.1 version:

```rust
    let dims = args.query_embedding.len() as i32;
    let idx = ConversationIndex::open(dims, args.root_override).await?;
    // Note: --src filtering via `primary_src` applies only to day-level
    // content (layers 0/1/2). Layer=3/4 rollup rows are multi-source
    // aggregates and always surface based on relevance — see the l3/l4
    // search calls below which pass None instead of `primary_src`.
    // (Phase 3.2.1 fix — prior Phase 3.2 behavior silently dropped all
    // rollup hits under --src; see docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-2-1-design.md §5.)
    let primary_src = args.filters.source.first().copied();
```

- [ ] **Step 4: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::retrieve::tests::gather_hits_rollup_surfaces_despite_src_filter
```

Also confirm the existing `gather_hits_collapsed_tree_returns_hits_from_multiple_layers` test (line 646) still passes — it doesn't pass a source filter, so its behavior is unaffected:

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::retrieve::tests::gather_hits_
```

All `gather_hits_*` tests (4 total after this task) must pass.

### 1c. Update `--src` CLI help text

- [ ] **Step 5: Edit `mur-core/src/main.rs`** — the `Commands::Ask::src` field is currently (around line 352-358):

```rust
        /// Filter results to a specific source (e.g. "cc", "cursor").
        /// Phase 3.2 note: source filtering does NOT apply to weekly/monthly
        /// rollup hits (layer=3/4) — rollup rows use synthetic source strings
        /// ("week"/"month") and are always excluded when --src is passed.
        #[arg(long)]
        src: Option<String>,
```

Replace the Phase 3.2 note with the Phase 3.2.1 semantic:

```rust
        /// Filter results to a specific source (e.g. "cc", "cursor").
        /// Phase 3.2.1: the source filter applies ONLY to day-level content
        /// (layers 0/1/2). Weekly/monthly rollup hits (layers 3/4) are
        /// multi-source aggregates and always surface based on relevance,
        /// regardless of --src.
        #[arg(long)]
        src: Option<String>,
```

### 1d. Lint + scope-check + commit

- [ ] **Step 6: Full-suite sanity + lint**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

- [ ] **Step 7: Verify scope + commit**

Run `git status`. Expected modified files: ONLY
- `mur-core/src/conversations/ask/retrieve.rs`
- `mur-core/src/main.rs`

If anything else is dirty, `git checkout -- <that file>` to discard.

```
git add mur-core/src/conversations/ask/retrieve.rs mur-core/src/main.rs
git commit -m "$(cat <<'EOF'
fix(core): --src filter no longer drops layer=3/4 rollup hits (Phase 3.2.1)

Phase 3.2 introduced layer=3 (week) and layer=4 (month) rollup rows
with synthetic source strings ("week"/"month") that don't match any
real Source::file_prefix. The --src filter passes primary_src into
every k-NN search as a LanceDB predicate `source = '<prefix>'`, so
rollup rows were silently excluded at the DB level — a user asking
"what did I ship in March?" with --src cc got zero rollup hits.

Fix: layer=3 and layer=4 k-NN calls in gather_hits now pass None
instead of primary_src. Rollups are multi-source aggregates by
construction (built from day summaries across all enabled sources) —
filtering them by a single source doesn't match user intent or the
data structure. Layer=0/1/2 continue to honor --src as before.

Rationale for NOT storing a sources_mask (Q1 option b in the spec):
  - Schema change + migration + reindex path for an ambiguous
    semantic (a rollup built from 1% cc content — include or not?).
  - Deferred permanently unless measurement shows users want it.

Help text on --src updated to document the new semantic.

1 new test: gather_hits_rollup_surfaces_despite_src_filter — seeds a
layer=2 cc span + layer=3 rollup, queries with --src cc, asserts both
layers appear in results.

Plan: Task 1 of docs/superpowers/plans/2026-04-22-mur-conversations-phase-3-2-1.md
Spec: §5

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `--if-stale` no-op on rollup subcommand

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (un-OR `--if-stale` from force in 2 sites)
- Modify: `mur-core/src/main.rs` (lines 814-816 `--if-stale` help text inside `ConversationsAction::Rollup`)
- Modify: `mur-core/tests/cli_conversations.rs` (append 2 integration tests)

### 2a. Failing tests — `--if-stale` is a no-op; `--force` still regenerates

- [ ] **Step 1: Failing tests** — append to the END of `mur-core/tests/cli_conversations.rs`:

```rust
/// Phase 3.2.1: `--if-stale` on `mur conversations rollup --week` must NOT
/// force regeneration. The default behavior (force=false) already triggers
/// the sha-based idempotency check inside rollup_week — running the same
/// rollup twice with --if-stale should produce zero .history/ archive
/// entries (no archive happens when input_content_sha matches existing md).
#[test]
fn mur_conversations_rollup_if_stale_is_idempotent_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // Seed 7 day summaries for 2026-W16 (Apr 13-19).
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    for d in 13..=19 {
        std::fs::write(
            summary_dir.join(format!("2026-04-{d:02}.md")),
            format!(
                "---\n\
                 schema: 1\n\
                 date: 2026-04-{d:02}\n\
                 generated_at: 2026-04-{d:02}T03:00:00Z\n\
                 generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
                 duration_ms: 50\n\
                 conv_count: 1\n\
                 msg_count: 1\n\
                 sources: [cc]\n\
                 pattern_refs: []\n\
                 keywords: []\n\
                 links:\n  prev: null\n  next: null\n\
                 warnings: []\n\
                 input_content_sha: {d}sha\n\
                 ---\n\n\
                 ## Extractive spans\n\n\
                 [1] _{{cc/c1 @L1}}_:\n> day {d} span\n\n\
                 ## Abstractive narrative\n\n\
                 Narrative for day {d}.\n"
            ),
        )
        .unwrap();
    }

    // Populate layer=2 spans via --spans-only reindex (rollup needs them).
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(
        out.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // First rollup invocation.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "rollup", "--week", "2026-W16", "--if-stale"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("first rollup");
    assert!(
        out1.status.success(),
        "first rollup failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Second invocation — same --if-stale flag. Must NOT trigger regeneration.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "rollup", "--week", "2026-W16", "--if-stale"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("second rollup");
    assert!(
        out2.status.success(),
        "second rollup failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Key assertion: the .history/ archive dir is empty (or does not exist).
    // Each regeneration archives the prior md file — if --if-stale forced
    // regen (pre-3.2.1 bug), we'd see 1 archived file. After 3.2.1, zero.
    let hist = mur_home
        .join("conversations")
        .join("summary")
        .join("weekly")
        .join(".history");
    let archived = if hist.exists() {
        std::fs::read_dir(&hist)
            .unwrap()
            .filter_map(|e| e.ok())
            .count()
    } else {
        0
    };
    assert_eq!(
        archived, 0,
        "Phase 3.2.1: --if-stale must not force regen. Found {archived} archived files; expected 0. \
         stdout of 2nd call: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

/// Phase 3.2.1: --force MUST still regenerate unconditionally, even when the
/// content is fresh. This test verifies we didn't break --force while
/// fixing --if-stale.
#[test]
fn mur_conversations_rollup_force_still_regenerates() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // Same 7-day seed as above.
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    for d in 13..=19 {
        std::fs::write(
            summary_dir.join(format!("2026-04-{d:02}.md")),
            format!(
                "---\n\
                 schema: 1\n\
                 date: 2026-04-{d:02}\n\
                 generated_at: 2026-04-{d:02}T03:00:00Z\n\
                 generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
                 duration_ms: 50\n\
                 conv_count: 1\n\
                 msg_count: 1\n\
                 sources: [cc]\n\
                 pattern_refs: []\n\
                 keywords: []\n\
                 links:\n  prev: null\n  next: null\n\
                 warnings: []\n\
                 input_content_sha: {d}sha\n\
                 ---\n\n\
                 ## Extractive spans\n\n\
                 [1] _{{cc/c1 @L1}}_:\n> day {d} span\n\n\
                 ## Abstractive narrative\n\n\
                 Narrative for day {d}.\n"
            ),
        )
        .unwrap();
    }
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");

    // First rollup.
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "rollup", "--week", "2026-W16"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("first rollup");

    // Second rollup with --force — must archive the prior md.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "rollup", "--week", "2026-W16", "--force"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("second rollup --force");
    assert!(
        out2.status.success(),
        "second rollup --force failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Verify --force triggered a .history/ entry.
    let hist = mur_home
        .join("conversations")
        .join("summary")
        .join("weekly")
        .join(".history");
    assert!(
        hist.exists(),
        ".history/ must exist after --force triggered an archive"
    );
    let archived = std::fs::read_dir(&hist)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert!(
        archived >= 1,
        "Phase 3.2.1: --force must still regenerate. Found {archived} archived files; expected ≥1. \
         stdout of --force call: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}
```

- [ ] **Step 2: Run — one must fail, one may pass or fail**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_conversations_rollup_if_stale_is_idempotent_noop
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_conversations_rollup_force_still_regenerates
```

Expected:
- `mur_conversations_rollup_if_stale_is_idempotent_noop` → **FAILS** (current bug: `--if-stale` sets `force=true`, the second call archives the first's md, `archived` is 1, test's `assert_eq!(archived, 0)` fails).
- `mur_conversations_rollup_force_still_regenerates` → probably PASSES today (current behavior is correct for `--force`; this test is a regression guard for Task 2's edit).

### 2b. Implement Fix 2 — un-OR `--if-stale` from force

- [ ] **Step 3: Edit `mur-core/src/cmd/conversations_cmd.rs`**

The `cmd_conversations_rollup` handler has two sites around lines 869-877:

```rust
    if let Some(w) = args.week {
        let force = args.force || args.if_stale;
        let r = rollup_week(&w, force, &rollup_cfg, None).await?;
        println!("{}: {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        return Ok(());
    }
    if let Some(m) = args.month {
        let force = args.force || args.if_stale;
        let r = rollup_month(&m, force, &rollup_cfg, None).await?;
        println!("{}: {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        return Ok(());
    }
```

Change both `let force = ...` lines to:

```rust
    if let Some(w) = args.week {
        // Phase 3.2.1: --if-stale is a no-op; the default (force=false)
        // already triggers the sha-based idempotency check inside
        // rollup_week. Flag retained for backward-compat with scripts.
        let force = args.force;
        let r = rollup_week(&w, force, &rollup_cfg, None).await?;
        println!("{}: {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        return Ok(());
    }
    if let Some(m) = args.month {
        // Phase 3.2.1: --if-stale is a no-op; the default (force=false)
        // already triggers the sha-based idempotency check inside
        // rollup_month. Flag retained for backward-compat with scripts.
        let force = args.force;
        let r = rollup_month(&m, force, &rollup_cfg, None).await?;
        println!("{}: {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        return Ok(());
    }
```

**IMPORTANT:** Do NOT touch the third `let force = args.force || args.if_stale;` site at line ~1011. That one is in `cmd_conversations_compact` (the compact subcommand, not rollup) and has correct semantics — `compact_day`'s force semantics differ from rollup's. Only the two sites inside `cmd_conversations_rollup` are wrong.

### 2c. Update `--if-stale` CLI help text

- [ ] **Step 4: Edit `mur-core/src/main.rs`** — the `ConversationsAction::Rollup::if_stale` field is currently (around line 814-816):

```rust
        /// Only regenerate when source content hash changed.
        #[arg(long)]
        if_stale: bool,
```

Replace with:

```rust
        /// Phase 3.2.1: no-op retained for backward compatibility. The
        /// default (omitting --force) already regenerates only when the
        /// source content hash has changed via the internal idempotency
        /// check. Use --force to regenerate unconditionally.
        #[arg(long)]
        if_stale: bool,
```

### 2d. Run tests + lint + commit

- [ ] **Step 5: Run both new tests — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_conversations_rollup_if_stale_is_idempotent_noop
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_conversations_rollup_force_still_regenerates
```

Both pass.

- [ ] **Step 6: Full workspace + lint**

```
MUR_OLLAMA_MOCK=1 cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

- [ ] **Step 7: Verify scope + commit**

Run `git status`. Expected modified files: ONLY
- `mur-core/src/cmd/conversations_cmd.rs`
- `mur-core/src/main.rs`
- `mur-core/tests/cli_conversations.rs`

If anything else is dirty (e.g., `retrieve.rs` still shows up because the worktree state wasn't clean between tasks), `git checkout -- <file>` — Task 1 should have been committed before this task.

```
git add mur-core/src/cmd/conversations_cmd.rs mur-core/src/main.rs mur-core/tests/cli_conversations.rs
git commit -m "$(cat <<'EOF'
fix(core): --if-stale on rollup subcommand is now a no-op (Phase 3.2.1)

Phase 3.2's cmd_conversations_rollup handler did:
  let force = args.force || args.if_stale;

This made --if-stale a silent alias for --force — always regenerating
even when the content was fresh. But --if-stale semantically means
"regenerate only if stale," which is already the default behavior
(force=false triggers the sha-based idempotency check inside
rollup_week / rollup_month).

Fix: un-OR --if-stale from force in both the --week and --month
branches. Flag retained for backward compatibility with scripts that
pass it — its semantic is now the default, so no behavior changes
when the flag is present.

Leaves the third `force = args.force || args.if_stale` site inside
cmd_conversations_compact untouched — compact_day's force semantics
differ from rollup's and the OR is correct there.

Help text on --if-stale updated to document the no-op behavior.

2 new integration tests:
  - mur_conversations_rollup_if_stale_is_idempotent_noop: 2x rollup
    runs with --if-stale produce zero .history/ archive entries.
  - mur_conversations_rollup_force_still_regenerates: --force still
    bypasses the sha check and archives the prior md (regression
    guard for this fix).

Plan: Task 2 of docs/superpowers/plans/2026-04-22-mur-conversations-phase-3-2-1.md
Spec: §6

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Golden-path sanity check

**Files:**
- None modified. This is a verification-only task that proves the Phase 3.3 golden path's 17 steps still pass after both fixes.

### 3a. Build binary + run golden path

- [ ] **Step 1: Build**

```
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-2-1
cargo build -p mur-core --bin mur 2>&1 | tail -3
```

Expected: `Finished` with no warnings.

- [ ] **Step 2: Run golden path**

```
./scripts/golden-path-conversations.sh 2>&1 | tail -10
```

Expected final line verbatim: `=== ALL 17 STEPS GREEN ===`.

If any step fails:
- Step 14 (rollup layer=3/4 hits surface in ask) — check if it now ALSO surfaces layer=2 hits differently because of Fix 1. The Phase 3.3 golden-path Step 14 doesn't pass `--src`, so Fix 1 shouldn't affect it. If it fails, STOP and investigate.
- Steps involving rollup idempotency — Fix 2 changes behavior when the golden path passes `--if-stale`. Phase 3.3 golden-path doesn't pass that flag either. Shouldn't affect.

If everything passes, the fixes are pure additions with no regressions.

### 3b. Final workspace + lint sweep

- [ ] **Step 3: Full workspace + lint**

```
MUR_OLLAMA_MOCK=1 cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected test counts (relative to Phase 3.4 baseline):
- mur-common: 109 (unchanged).
- mur-core lib: +1 (new `gather_hits_rollup_surfaces_despite_src_filter`).
- mur-core integration (cli_conversations): +2 (`mur_conversations_rollup_if_stale_is_idempotent_noop` + `mur_conversations_rollup_force_still_regenerates`).

All green.

### 3c. No commit — or empty-doc commit

- [ ] **Step 4: Decide on commit**

This task is verification-only. No files were modified. Options:

(a) **No commit.** The task is a gate between implementation and PR-opening; its success is recorded in the PR description.

(b) **Empty-doc commit** (only if you want a visible milestone in git history):

```
git commit --allow-empty -m "$(cat <<'EOF'
chore(core): Phase 3.2.1 verified — 17-step golden path + full workspace green

Verification gate between implementation and PR:
  - `./scripts/golden-path-conversations.sh` prints
    `=== ALL 17 STEPS GREEN ===`
  - `cargo test --workspace` green
  - `cargo clippy --workspace --all-targets -- -D warnings` clean
  - `cargo fmt --check` clean

Fix 1 (--src rollup inclusion) and Fix 2 (--if-stale no-op) both
land as pure additions with no regressions on existing test
assertions.

Plan: Task 3 of docs/superpowers/plans/2026-04-22-mur-conversations-phase-3-2-1.md

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**Recommendation:** Option (a) — skip the empty commit. The PR description serves as the verification record. Git history stays clean with exactly one commit per fix.

---

## 🏁 End of Phase 3.2.1

After Task 3, open one PR (`fix/conversations-phase-3-2-1` → `main`), wait for CI green + reviewer approval, then ship. Two commits total (Task 1 + Task 2) plus the pre-existing spec + plan commits.

**Post-merge checklist:**
- Update the Phase 3.2 shipped memory file to note the gaps are now closed (`--src` + `--if-stale`).
- Update memory index with a new `project_conversations_phase_3_2_1_shipped.md` entry.
