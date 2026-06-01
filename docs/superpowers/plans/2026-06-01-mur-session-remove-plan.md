# `mur session remove` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `mur session remove <id>` and `mur session remove --all` CLI commands to delete local session recordings.

**Architecture:** Follows the existing `SessionAction` → `dispatch` → `cmd::session` pattern. Adds `remove_recording()` + `is_recording_synced()` to `session/mod.rs` as thin wrappers around existing `delete_recording()`, then wires them through `SessionAction::Remove` in `cli/actions.rs` and `dispatch.rs` into a new `cmd_session_remove()` in `cmd/session.rs`.

**Tech Stack:** Rust (edition 2024), clap (Subcommand derive), anyhow, std::io::IsTerminal

---

### Task 1: Add `remove_recording()` and `is_recording_synced()` to session module

**Files:**
- Modify: `mur-core/src/session/mod.rs` — add two new functions after `delete_recording()` (line ~300)
- Test: `mur-core/src/session/mod.rs` — add tests in the existing `mod tests` block

- [ ] **Step 1: Write the failing test for `is_recording_synced()`**

Add to the tests module (before the closing `}` of `mod tests`):

```rust
    #[test]
    fn test_is_recording_synced() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rec_dir = tmp.path().join("recordings");
        fs::create_dir_all(&rec_dir).unwrap();

        let synced_path = rec_dir.join("test-id.synced");
        assert!(!synced_path.exists());

        // Create .synced file — use the helper we'll add
        fs::write(&synced_path, "2026-06-01T00:00:00Z").unwrap();
        assert!(synced_path.exists());
    }
```

- [ ] **Step 2: Run test to verify it fails (new function not defined yet)**

Run: `cargo test -p mur-core test_is_recording_synced`
Expected: FAIL — the helper isn't called yet, but this test is really verifying the file operations work. It passes as-is since it only uses `fs`. Skip to the actual unit test.

Actually, the real test needs to call `is_recording_synced()`. Since the function uses hardcoded `recordings_dir()`, we need a testable wrapper. Write the test first:

```rust
    #[test]
    fn test_remove_recording_cleans_all_files() {
        use std::path::Path;

        let tmp = tempfile::TempDir::new().unwrap();
        let rec_dir = tmp.path().join("recordings");
        fs::create_dir_all(&rec_dir).unwrap();

        let id = "test-remove-123";
        // Create the three files
        fs::write(rec_dir.join(format!("{}.jsonl", id)), "line1\n").unwrap();
        fs::write(rec_dir.join(format!("{}.meta.json", id)), "{}").unwrap();
        fs::write(rec_dir.join(format!("{}.synced", id)), "2026-01-01").unwrap();

        assert!(rec_dir.join(format!("{}.jsonl", id)).exists());
        assert!(rec_dir.join(format!("{}.meta.json", id)).exists());
        assert!(rec_dir.join(format!("{}.synced", id)).exists());

        // Call the helper with explicit dir (will be remove_recording_in_dir)
        remove_recording_in_dir(&rec_dir, id).unwrap();

        assert!(!rec_dir.join(format!("{}.jsonl", id)).exists());
        assert!(!rec_dir.join(format!("{}.meta.json", id)).exists());
        assert!(!rec_dir.join(format!("{}.synced", id)).exists());
    }

    #[test]
    fn test_remove_recording_no_synced_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rec_dir = tmp.path().join("recordings");
        fs::create_dir_all(&rec_dir).unwrap();

        let id = "test-no-sync";
        fs::write(rec_dir.join(format!("{}.jsonl", id)), "line1\n").unwrap();
        fs::write(rec_dir.join(format!("{}.meta.json", id)), "{}").unwrap();

        // Should succeed even without .synced file
        remove_recording_in_dir(&rec_dir, id).unwrap();

        assert!(!rec_dir.join(format!("{}.jsonl", id)).exists());
        assert!(!rec_dir.join(format!("{}.meta.json", id)).exists());
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p mur-core test_remove_recording`
Expected: FAIL — `remove_recording_in_dir` not found

- [ ] **Step 4: Implement `remove_recording_in_dir()` and the public wrappers**

In `session/mod.rs`, add after `delete_recording()` (after line 301):

```rust
/// Remove a session's recording, metadata, and sync marker from a specific
/// recordings directory.  Used by `remove_recording()` and by tests.
fn remove_recording_in_dir(recordings_dir: &std::path::Path, id: &str) -> Result<()> {
    let jsonl = recordings_dir.join(format!("{}.jsonl", id));
    let meta = recordings_dir.join(format!("{}.meta.json", id));
    let synced = recordings_dir.join(format!("{}.synced", id));

    if jsonl.exists() {
        fs::remove_file(&jsonl)?;
    }
    if meta.exists() {
        fs::remove_file(&meta)?;
    }
    if synced.exists() {
        fs::remove_file(&synced)?;
    }
    Ok(())
}

/// Remove a session recording, metadata, and sync marker from the default
/// recordings directory.
pub(crate) fn remove_recording(id: &str) -> Result<()> {
    remove_recording_in_dir(&recordings_dir(), id)
}

/// Check if a recording has been synced to the cloud.
pub(crate) fn is_recording_synced(id: &str) -> bool {
    recordings_dir().join(format!("{}.synced", id)).exists()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-core test_remove_recording`
Expected: PASS (both tests)

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/session/mod.rs
git commit -m "feat(session): add remove_recording() and is_recording_synced()

- remove_recording() wraps delete_recording() + .synced marker cleanup
- is_recording_synced() checks .synced marker file existence
- remove_recording_in_dir() is the testable inner function

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Add `SessionAction::Remove` variant to CLI actions

**Files:**
- Modify: `mur-core/src/cli/actions.rs` — add variant after `Discard` (line 349)

- [ ] **Step 1: Add the variant**

In `cli/actions.rs`, add after the `Discard` variant (after line 349, before the closing `}`):

```rust
    /// Remove session recording(s)
    Remove {
        /// Session ID or prefix
        id: Option<String>,

        /// Remove all session recordings
        #[arg(long, conflicts_with = "id")]
        all: bool,

        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,

        /// Show what would be deleted without actually deleting
        #[arg(long, requires = "all")]
        dry_run: bool,
    },
```

- [ ] **Step 2: Verify it compiles (will fail at dispatch, which is expected)**

Run: `cargo check -p mur-core 2>&1 | head -20`
Expected: Non-exhaustive pattern error in `dispatch.rs` — we haven't added the match arm yet. This confirms the variant is recognized.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cli/actions.rs
git commit -m "feat(cli): add SessionAction::Remove variant

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Implement `cmd_session_remove()` in cmd/session.rs

**Files:**
- Modify: `mur-core/src/cmd/session.rs` — add function (e.g., after `cmd_session_list()` around line 937)

- [ ] **Step 1: Add the function**

Append at the end of `cmd/session.rs` (before the closing `}` of any existing impl block — actually the file has standalone functions, so append at the end before the last `}` if wrapping in a module, but this file is just a plain module with `pub(crate) fn` items):

Add after `cmd_session_list()` (after line 936):

```rust
pub(crate) fn cmd_session_remove(
    id: Option<String>,
    all: bool,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let active_id = crate::session::active_session_id().ok().flatten();

    if let Some(prefix) = id {
        // ── Single removal ──
        let full_id = session::find_recording_by_prefix(&prefix)?
            .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", prefix))?;

        // Guard: don't delete active session
        if let Some(ref active) = active_id
            && active == &full_id
        {
            anyhow::bail!(
                "Session {} is currently active. Use `mur session discard` to stop and delete it.",
                &full_id[..8]
            );
        }

        // Confirm unless --force (non-TTY without --force = error)
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if !force {
            if !is_tty {
                anyhow::bail!("--force required when not running interactively.");
            }
            eprint!("Delete session {}? [y/N]: ", &full_id[..8]);
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            if !buf.trim().eq_ignore_ascii_case("y") {
                eprintln!("Cancelled.");
                return Ok(());
            }
        }

        let was_synced = session::is_recording_synced(&full_id);
        session::remove_recording(&full_id)?;
        eprintln!("Session {} removed.", &full_id[..8]);
        if was_synced {
            eprintln!(
                "  \u{2139}\u{fe0f}  This session was synced to the cloud. Cloud copies are unaffected \
                 — use the dashboard to manage them."
            );
        }

    } else if all {
        // ── Bulk removal ──
        let recordings = session::list_recordings()?;
        if recordings.is_empty() {
            eprintln!("No session recordings found.");
            return Ok(());
        }

        // Filter out active session
        let (to_delete, skipped): (Vec<_>, Vec<_>) = recordings
            .into_iter()
            .partition(|r| active_id.as_ref() != Some(&r.id));

        if dry_run {
            eprintln!("Would delete {} session(s):", to_delete.len());
            for r in &to_delete {
                let ts: chrono::DateTime<chrono::Utc> = r.modified.into();
                eprintln!(
                    "  {} — {} events, {} bytes ({})",
                    &r.id[..8],
                    r.event_count,
                    r.file_size,
                    ts.format("%Y-%m-%d %H:%M"),
                );
            }
            if !skipped.is_empty() {
                eprintln!("  1 session skipped (active).");
            }
            return Ok(());
        }

        if to_delete.is_empty() {
            eprintln!("No sessions to delete (1 active).");
            return Ok(());
        }

        // Confirm
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if !force {
            if !is_tty {
                anyhow::bail!("--force required when not running interactively.");
            }
            eprint!("Delete {} session(s)? [y/N]: ", to_delete.len());
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            if !buf.trim().eq_ignore_ascii_case("y") {
                eprintln!("Cancelled.");
                return Ok(());
            }
        }

        let synced_count = to_delete
            .iter()
            .filter(|r| session::is_recording_synced(&r.id))
            .count();
        let mut deleted = 0usize;
        for r in &to_delete {
            if let Err(e) = session::remove_recording(&r.id) {
                eprintln!("  \u{26a0} Failed to delete {}: {}", &r.id[..8], e);
            } else {
                deleted += 1;
            }
        }

        eprintln!("Deleted {} session(s).", deleted);
        if !skipped.is_empty() {
            eprintln!("  1 session skipped (active).");
        }
        if synced_count > 0 {
            eprintln!();
            eprintln!(
                "  \u{2139}\u{fe0f}  {} session(s) were synced to the cloud. Cloud copies are unaffected.",
                synced_count,
            );
        }

    } else {
        anyhow::bail!("Specify a session ID or use --all.");
    }

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles (will fail at dispatch, still no match arm)**

Run: `cargo check -p mur-core 2>&1 | head -20`
Expected: Still non-exhaustive pattern error in `dispatch.rs`.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/session.rs
git commit -m "feat(session): add cmd_session_remove() function

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Wire dispatch and fix compilation

**Files:**
- Modify: `mur-core/src/dispatch.rs` — add match arm after `SessionAction::Discard` (line 225)

- [ ] **Step 1: Add the dispatch match arm**

In `dispatch.rs`, after `SessionAction::Discard => cmd::session::cmd_session_exit()?,` (line 225), add:

```rust
            SessionAction::Remove {
                id,
                all,
                force,
                dry_run,
            } => cmd::session::cmd_session_remove(id, all, force, dry_run)?,
```

- [ ] **Step 2: Build and verify**

Run: `cargo build -p mur-core`
Expected: PASS — compiles cleanly.

- [ ] **Step 3: Run all existing tests**

Run: `cargo test -p mur-core`
Expected: All tests pass (no regressions).

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p mur-core -- -D warnings`
Expected: No warnings.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/dispatch.rs
git commit -m "feat(dispatch): wire SessionAction::Remove to cmd_session_remove

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Build release and manual smoke test

**Files:**
- No code changes — verification only

- [ ] **Step 1: Build release binary**

```bash
./build.sh --install
```

- [ ] **Step 2: Verify `--help` shows the new subcommand**

Run: `mur session remove --help`
Expected: Shows usage with `<ID>`, `--all`, `--force`, `--dry-run` flags.

- [ ] **Step 3: Smoke test — single remove on non-existent ID**

Run: `mur session remove abc123`
Expected: `Error: No session found matching prefix 'abc123'`

- [ ] **Step 4: Smoke test — `--all` with no recordings**

Run: `mur session remove --all`
Expected: `No session recordings found.`

- [ ] **Step 5: Smoke test — `--all --dry-run` with no recordings**

Run: `mur session remove --all --dry-run`
Expected: `No session recordings found.`

- [ ] **Step 6: Smoke test — no ID and no `--all`**

Run: `mur session remove`
Expected: `Error: Specify a session ID or use --all.`

- [ ] **Step 7: Smoke test — `--all` with actual recordings**

Run:
```bash
mur in --source test
mur out
mur session remove --all --dry-run
```

Expected: Shows at least 1 session in the "Would delete" list.

- [ ] **Step 8: Smoke test — actual delete**

Run: `mur session remove --all --force`
Expected: `Deleted N session(s).` (no confirmation prompt).

- [ ] **Step 9: Verify recordings are gone**

Run: `ls ~/.mur/session/recordings/`
Expected: Directory is empty (or contains only the active session's `.jsonl`/`.meta.json` if one is active).

- [ ] **Step 10: Smoke test — active session protection**

Run:
```bash
mur in --source test
mur session remove --all --force
```

Expected: `1 session skipped (active).` (active session is not deleted).

Then: `mur out` to clean up.

- [ ] **Step 11: Commit (if any tweaks were needed)**

```bash
git add -A
git commit -m "chore: manual smoke test fixes for session remove"
```
