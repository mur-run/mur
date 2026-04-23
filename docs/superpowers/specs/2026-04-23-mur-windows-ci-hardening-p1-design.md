# mur Windows CI Hardening — Phase 1 (Defensive + Groundwork)

**Status:** Design approved 2026-04-23. Ready for plan.
**Depends on:** Phase 3.5 shipped (merge `0ebab7d`) + Phase 3.5.1 shipped (merge `cdea3a2`).
**Branch:** `fix/windows-ci-hardening-p1`, worktree at `/Volumes/Firecuda4tb/Projects/mur/.worktrees/windows-ci-hardening-p1`.

---

## 1. Goal

Phase 3.5 uncovered two Windows-only bug shapes that both existed since Phase 2 but only surfaced under Phase 3.5's new test exposure:

1. **Pattern A** — `dirs::home_dir()` direct callers ignoring `MUR_HOME`. `SHGetKnownFolderPath` on Windows bypasses `HOME`/`USERPROFILE` env overrides that macOS and Linux respect, so integration tests setting `MUR_HOME + HOME + USERPROFILE` silently read the host's real `~/.mur/`. Fixed in Phase 3.5 for `store::config::config_path()` only; ~35 more callers remain across the crate.
2. **Pattern B** — Second-precision `generated_at` timestamps paired with byte-equality idempotency checks. Two fast back-to-back writes share the same second → same bytes → the idempotency short-circuit silently swallows `--force`. Fixed in Phase 3.5 for `write_rollup`; `write_summary` has the identical shape still unfixed.

**Phase 1 of the hardening effort** lands the remaining Pattern B fix, adds a test that guards the whole pattern class (not just the specific bug), and builds the crate-level infrastructure (`crate::paths::mur_root` helper) that Phase 2 will use to sweep the ~35 Pattern A callers. Phase 1 deliberately does NOT touch those callers — small PR, clear concern separation, revertable in isolation.

## 2. Non-goals

- **Sweeping Pattern A callers** — deferred to Phase 2 of the hardening effort (tracked in the PR description). This Phase 1 only provides the helper they'll use.
- **Touching C2 paths** — `~/Library/LaunchAgents` / `dirs::config_dir()` / systemd service dirs. These legitimately need platform-native paths; `dirs::` is correct there.
- **Schema changes** — no `AskConfig` / `RollupConfig` / `SummaryDoc` / `RollupDoc` changes.
- **Sub-second `generated_at` precision** — considered but rejected. Would change `.md` frontmatter serialization format, breaking byte-equality against existing user files (every existing `.md` would be re-archived on upgrade). `force` flag is the idempotency-preserving fix.
- **Re-pointing `store::config::config_path()` to the new helper** — Phase 3.5 already inlined MUR_HOME handling there; re-routing is non-essential churn and belongs to Phase 2's sweep.
- **CLI flag surface changes** — no new `mur` flags.

## 3. Architecture

Three independent components, each small:

```
mur-core/src/paths.rs           (new, ~15 LOC)
  └── pub fn mur_root(override_path: Option<&str>) -> PathBuf

mur-core/src/conversations/summarize/writer.rs  (modify)
  └── pub async fn write_summary(..., force: bool, ...)  // new param
      └── if !force && existing == new_body { ... noop ... }

mur-core/tests/cli_conversations.rs  (modify)
  └── fn mur_conversations_compact_force_unconditionally_archives
         (adversarial — locks the bug class)
```

No cross-cutting refactor. `paths::mur_root` is a new primitive; nothing is re-pointed at it in this phase. `write_summary`'s new `force` param is threaded one-call-deep through `compact_day` (already has `force: bool`). The integration test exercises the whole chain end-to-end through the CLI binary.

## 4. Locked design choices

| # | Question | Choice |
|---|---|---|
| D1 | Pattern B fix style — `force` flag vs. higher timestamp precision | **`force` flag.** Matches Phase 3.5's `write_rollup` precedent, keeps `.md` format byte-identical with existing on-disk files (no mass re-archive on upgrade). |
| D2 | Helper name + module placement | **`crate::paths::mur_root(override: Option<&str>)`** in a new `mur-core/src/paths.rs`. Mirrors `crate::conversations::paths::mur_root`'s existing signature exactly (drop-in replacement for Phase 2's sweep). |
| D3 | Compatibility of existing `conversations::paths::mur_root` | **Keep as-is for this PR.** Phase 2's sweep can decide whether to delete it (pointing callers at the new location) or leave it as a module-local re-export. Not Phase 1's concern. |
| D4 | Scope: Phase 1 alone, or bundle with Phase 2 sweep | **Phase 1 alone.** Reviewer surface separation — one PR per concern. |
| D5 | Regression test style — unit, integration, or both | **Both.** A unit test on `write_summary` asserts the force-bypass. An adversarial CLI integration test drives `mur conversations compact --force` end-to-end, assertions on `.history/` contents. The CLI one is the load-bearing guard for the bug class. |
| D6 | Integration test timing strategy | **Run two compacts back-to-back (no sleep).** The point is to catch the same-wall-clock-second race. If the test flakes under parallel test execution, add `--test-threads=1` hint in the PR description; don't add sleeps to mask the race. |

## 5. Pattern B — `write_summary` force bypass

### 5.1 Signature change

```rust
// mur-core/src/conversations/summarize/writer.rs

pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    span_embeddings: Vec<Vec<f32>>,
    force: bool,                          // ← new, positioned before root_override
    root_override: Option<&str>,
) -> Result<WriteResult> {
    // ... (existing head unchanged) ...

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
        archived = Some(archive_prior(&md_path, root_override)?);
        let retain = crate::store::config::load_config()
            .map(|c| c.conversations.compact.history_retain)
            .unwrap_or(5);
        let _ = prune_history(root_override, doc.date, retain);
        noop = false;
    } else {
        // ... (unchanged) ...
    }
    // ... (tail unchanged) ...
}
```

The `force` param goes between `span_embeddings` and `root_override` to match `write_rollup(doc, vec, force, root_override)` positional order.

### 5.2 Caller wiring

Exactly one non-test caller: `mur-core/src/conversations/summarize/mod.rs`. Grep produced one line:

```rust
match writer::write_summary(&doc, summary_embedding, span_embeddings, root_override).await {
```

Becomes:

```rust
match writer::write_summary(&doc, summary_embedding, span_embeddings, force, root_override).await {
```

`compact_day` already accepts `force: bool` so the variable name is already in scope at that call site.

### 5.3 Test callers

`write_summary` has ~6 test call sites in `writer.rs`. Each needs to add `false` (the no-force default) as the new arg. New `write_summary_force_bypasses_idempotency` test asserts the `true` path.

## 6. Regression test — adversarial pattern guard

### 6.1 New integration test

Placed at the bottom of `mur-core/tests/cli_conversations.rs`, alongside the Phase 3.2.1 `mur_conversations_rollup_force_still_regenerates` test:

```rust
/// Windows CI Hardening Phase 1 — adversarial regression guard for the
/// "same-wall-clock-second byte-equality swallows --force" bug class.
///
/// Phase 3.5 fixed the bug for `write_rollup` after it flaked on Windows;
/// Phase 1 of the hardening effort fixes the matching shape in
/// `write_summary` and locks the invariant with this test. Fails if any
/// future writer reintroduces the byte-equality noop short-circuit without
/// a `!force` guard.
#[test]
fn mur_conversations_compact_force_unconditionally_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d").to_string();

    // Seed one raw JSONL line.
    let raw = mur_home.join("conversations").join("raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    std::fs::write(
        raw.join("cc_c1.jsonl"),
        serde_json::to_string(&serde_json::json!({
            "v": 1, "ts": format!("{yesterday}T10:00:00Z"),
            "src": "claude-code", "conv": "c1", "role": "user",
            "content": {"t": "text", "v": "seed content for force-archive test"},
            "meta": {}, "refs": []
        })).unwrap() + "\n",
    ).unwrap();

    // First compact — produces .md.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path()).env("MUR_OLLAMA_MOCK", "1")
        .output().expect("first compact");
    assert!(
        out1.status.success(),
        "first compact failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Immediately re-compact with --force. Same wall-clock second is
    // possible → pre-fix, byte-equality would swallow --force. Post-fix,
    // the `!force` guard archives the prior md unconditionally.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact", "--force"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path()).env("MUR_OLLAMA_MOCK", "1")
        .output().expect("second compact --force");
    assert!(
        out2.status.success(),
        "second compact --force failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Assertion: .history/ exists with ≥1 archived entry.
    let hist = mur_home
        .join("conversations").join("summary").join(".history");
    assert!(
        hist.exists(),
        "history dir must exist after --force archive"
    );
    let archived = std::fs::read_dir(&hist).unwrap().filter_map(|e| e.ok()).count();
    assert!(
        archived >= 1,
        "Phase 1 hardening: compact --force must unconditionally archive the \
         prior md even when the body is byte-identical. Got {archived} \
         archived files. stdout of --force call:\n{}",
        String::from_utf8_lossy(&out2.stdout)
    );
}
```

### 6.2 Unit test

In `writer.rs` tests, alongside `write_rollup_force_bypasses_idempotency`:

```rust
#[tokio::test]
async fn write_summary_force_bypasses_idempotency() {
    // Mirrors write_rollup_force_bypasses_idempotency. Same wall-clock
    // second produces byte-identical bodies; `force=true` must archive + rewrite
    // anyway. `dummy_doc(date)` is the existing helper in this test module.
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

### 6.3 Test precedence commitment

The CLI integration test is the primary guard. If someone deletes the unit test but keeps the CLI test, the bug is still caught. If someone deletes the CLI test, the unit test still catches the direct regression but not a compact-pipeline regression. Both together cover the bug class robustly.

## 7. Crate-level `paths::mur_root` helper

### 7.1 New file

```rust
// mur-core/src/paths.rs

//! Crate-level path helpers.
//!
//! Use `mur_root` when you need the `.mur` data directory from code outside
//! `conversations/`. Respects `MUR_HOME` as an authoritative override — on
//! Windows, `dirs::home_dir()` calls `SHGetKnownFolderPath` and ignores
//! `HOME`/`USERPROFILE` env overrides, so tests that redirect via env vars
//! need this escape hatch.
//!
//! Semantics are identical to `conversations::paths::mur_root`; Phase 2 of
//! Windows CI hardening will sweep the ~35 `dirs::home_dir().join(".mur")`
//! direct callers in the crate to use this one-line helper.

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
        // Serialize via a module-local mutex to avoid clashing with other
        // env-var tests in the crate (see `conversations::ENV_LOCK`).
        let _g = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_HOME", "/tmp/via-env") };
        assert_eq!(mur_root(None), PathBuf::from("/tmp/via-env"));
        unsafe { std::env::remove_var("MUR_HOME") };
    }

    #[test]
    fn mur_root_falls_back_to_home_dir_when_env_empty() {
        let _g = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_HOME", "") };
        // Must be $HOME/.mur on the current host. On a CI runner the
        // concrete path is `/home/runner/.mur` etc — we just assert the
        // shape, not the literal path.
        let p = mur_root(None);
        assert!(p.ends_with(".mur"), "expected .../.mur, got: {p:?}");
        unsafe { std::env::remove_var("MUR_HOME") };
    }
}
```

### 7.2 `lib.rs` registration

`mur-core/src/lib.rs` grows one line, in alphabetical order between `inject` and `retrieve`:

```rust
pub mod paths;
```

### 7.3 Backward compatibility

`mur-core/src/conversations/paths.rs::mur_root` is untouched. Phase 2 will decide whether to make it a re-export of `crate::paths::mur_root` or sweep all conversations callers to the new location first and then delete the old one. Phase 1's commitment: no behavior change to existing callers.

## 8. Test matrix

| Test | Location | Asserts |
|---|---|---|
| `mur_root_uses_explicit_override_first` | `mur-core/src/paths.rs` | `Some(p)` short-circuits env + fallback |
| `mur_root_honors_mur_home_env_when_no_override` | `mur-core/src/paths.rs` | `MUR_HOME` set → returned |
| `mur_root_falls_back_to_home_dir_when_env_empty` | `mur-core/src/paths.rs` | Empty or missing `MUR_HOME` → `dirs::home_dir()/.mur` shape |
| `write_summary_force_bypasses_idempotency` | `mur-core/src/conversations/summarize/writer.rs` | `force=true` on byte-equal content archives + rewrites |
| `mur_conversations_compact_force_unconditionally_archives` | `mur-core/tests/cli_conversations.rs` | CLI `compact --force` produces `.history/` entry back-to-back |

Plus: existing `write_summary` test callers updated to pass `false` for the new `force` arg (mechanical — the compiler will guide).

## 9. File-change summary

| File | Change | LOC |
|---|---|---|
| `mur-core/src/paths.rs` | **new** — `mur_root` fn + 3 unit tests | +50 |
| `mur-core/src/lib.rs` | add `pub mod paths;` | +1 |
| `mur-core/src/conversations/summarize/writer.rs` | `write_summary` +`force` param; `!force` guard on byte-equality; +1 unit test; ~6 existing test call-sites updated | +25 |
| `mur-core/src/conversations/summarize/mod.rs` | `compact_day` threads `force` into `write_summary` call | +1 |
| `mur-core/tests/cli_conversations.rs` | `mur_conversations_compact_force_unconditionally_archives` | +60 |
| Spec doc (this file) | — | +spec |

Total production LOC: ~30. Test LOC: ~120. Total: ~150 LOC across 5 source files (+ spec + plan docs).

## 10. Error handling

- **`paths::mur_root`**: `dirs::home_dir()` returns `None` on exotic platforms without a home dir. The helper `.expect("no home dir")` — consistent with `conversations::paths::mur_root`'s current behavior. Panicking here is acceptable because the same panic exists in the non-test path today.
- **`write_summary` with `force=true`**: archive + rewrite paths unchanged. If archive fails (disk full, permission), error propagates as before. `force` only changes whether the byte-equality short-circuit fires; downstream I/O is identical.
- **Integration test timing**: if two `compact` invocations cross a wall-clock second boundary on a slow runner, the byte-equality check naturally produces different bodies (different `generated_at` second) and the test's `--force` still archives via the "content differs" path. The test is correct under both fast and slow runners; `--force` is the load-bearing guarantee.

## 11. Success criteria

1. `write_summary_force_bypasses_idempotency` passes — unit-level proof of `!force` guard.
2. `mur_conversations_compact_force_unconditionally_archives` passes — end-to-end proof that `compact --force` archives even under identical-body conditions.
3. `cargo test --workspace` green on macOS/Linux/Windows CI — no regression in Phase 3.5 `mur_conversations_rollup_force_still_regenerates` (still passing from the Phase 3.5 fix).
4. `cargo clippy --workspace --all-targets -- -D warnings` clean.
5. `cargo fmt --check --all` clean.
6. `crate::paths::mur_root` resolvable from any `mur-core` module (compile test — a subsequent Phase 2 commit will import it).
7. Phase 2 follow-up issue/section created in PR description listing all 35 C1 call sites.

## 12. PR 2 follow-up (tracked in PR description, not this phase)

Sweep the following files' `dirs::home_dir()` C1 callers to `crate::paths::mur_root`:

- `mur-core/src/cmd/session.rs` (7 sites — session recording paths)
- `mur-core/src/cmd/source_cmd.rs` (~10 sites — index, tantivy, excluding the `dirs::config_dir()` C2 site)
- `mur-core/src/cmd/sync_cmd.rs` (3 sites)
- `mur-core/src/cmd/system_schedule.rs` (~1 C1 site; `~/Library/LaunchAgents` + systemd paths are C2, leave alone)
- `mur-core/src/main.rs` (3 sites)
- `mur-core/src/cmd/{pattern,community_cmd,inject_cmd,reindex,server_cmd,conversations_cmd,misc}.rs` (~1-2 each)
- `mur-core/src/{auth,dashboard,extract_llm,interactive,verify}.rs` (1 each)

Total: ~35 sites. Purely mechanical replacement — suitable for subagent batch execution, one file per task.

---

_Spec approved for plan. Next: `docs/superpowers/plans/2026-04-23-mur-windows-ci-hardening-p1.md` via `superpowers:writing-plans`._
