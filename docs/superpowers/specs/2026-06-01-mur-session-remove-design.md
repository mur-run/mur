# `mur session remove` — Remove session recordings

**Date:** 2026-06-01
**Status:** Draft
**Target:** mur binary (`mur-core` crate)

## Summary

Add `mur session remove <session-id>` and `mur session remove --all` to delete local
session recordings. Removes `.jsonl` recording + `.meta.json` metadata + `.synced`
cloud marker files. Does NOT touch fingerprints (shared single file), conversations
(independent pipeline with own retention), or cloud copies (server has no delete API
today — inform the user if a session was previously synced).

## Command Interface

```
mur session remove [<id>]    Delete one session by ID prefix
mur session remove --all      Delete all sessions
```

| Flag | Description |
|------|-------------|
| `<id>` | Session ID or prefix (8+ chars, same as `show`/`export`/`review`) |
| `--all` | Remove all recordings (mutually exclusive with `<id>`) |
| `--force`, `-f` | Skip confirmation prompt |
| `--dry-run` | Show what would be deleted without actually deleting (only with `--all`) |

## Behavior

### Active Session Protection

If the target session is currently active (recording in progress via `mur in`),
refuse and tell the user to `mur session discard` first.

For `--all`, skip the active session (deleting the rest) and print a warning.

### Single Removal (`remove <id>`)

1. Resolve ID prefix via `find_recording_by_prefix()` (existing function)
2. Check it's not the active session
3. Confirm interactively unless `--force` (or non-TTY):
   ```
   Delete session abc12345? [y/N]:
   ```
4. Delete `.jsonl` + `.meta.json` + `.synced` files via `delete_recording()` (existing) + extra `.synced` cleanup
5. If the session was synced to cloud, print a note:
   ```
   Session abc12345 removed locally.
   ⓘ  This session was synced to the cloud. Cloud copies are unaffected — use the dashboard to manage them.
   ```

### Bulk Removal (`remove --all`)

1. `list_recordings()` to get all sessions
2. Skip the active session (warn)
3. `--dry-run` shows what would be deleted:
   ```
   Would delete 5 session(s):
     abc12345 — 42 events, 15 KiB (2026-05-30)
     def67890 — 128 events, 48 KiB (2026-05-28)
     ...
   ```
4. Without `--dry-run`, confirm unless `--force`:
   ```
   Delete 5 session(s)? [y/N]:
   ```
5. Delete each recording (best-effort: a single failure doesn't stop the rest)
6. Report summary: `Deleted 5 session(s), 1 skipped (active).`

### Non-TTY Safety

If stdin is not a terminal and `--force` was not passed, error out instead of
prompting or auto-confirming. This prevents accidental deletion from hooks, cron
jobs, or scripts:

```
Error: --force required when not running interactively.
```

The `--force` flag is always required for non-interactive use — both for single
`remove <id>` and `remove --all`.

## Files Removed

| File | Path | When |
|------|------|------|
| Recording | `~/.mur/session/recordings/{id}.jsonl` | Always |
| Metadata | `~/.mur/session/recordings/{id}.meta.json` | Always (if exists) |
| Synced marker | `~/.mur/session/recordings/{id}.synced` | Always (if exists) |

`delete_recording()` already handles `.jsonl` + `.meta.json`. Extend it or add a small wrapper to also remove `.synced`.

## Implementation

### Files to modify (mur-core crate)

| File | Change |
|------|--------|
| `src/cli/actions.rs` | Add `Remove { id, all, force, dry_run }` variant to `SessionAction` |
| `src/dispatch.rs` | Match `SessionAction::Remove` → `cmd_session_remove(...)` |
| `src/cmd/session.rs` | New `cmd_session_remove()` function |
| `src/session/mod.rs` | Modify `delete_recording()` to also remove `.synced`, or add `remove_recording()` wrapper |

### `SessionAction::Remove` (clap)

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

### `cmd_session_remove()`

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

        // Confirm (unless --force or non-TTY)
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
                "  ⓘ  This session was synced to the cloud. Cloud copies are unaffected \
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

        let synced_count = to_delete.iter().filter(|r| session::is_recording_synced(&r.id)).count();
        let mut deleted = 0usize;
        for r in &to_delete {
            if let Err(e) = session::remove_recording(&r.id) {
                eprintln!("  ⚠ Failed to delete {}: {}", &r.id[..8], e);
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
                "  ⓘ  {} session(s) were synced to the cloud. Cloud copies are unaffected.",
                synced_count,
            );
        }

    } else {
        anyhow::bail!("Specify a session ID or use --all.");
    }

    Ok(())
}
```

### `session::remove_recording()` (new wrapper in `session/mod.rs`)

```rust
/// Remove a session recording, metadata, and sync marker.
pub(crate) fn remove_recording(id: &str) -> Result<()> {
    // Delete .jsonl + .meta.json (existing)
    delete_recording(id)?;

    // Delete .synced marker if present
    let synced = recordings_dir().join(format!("{}.synced", id));
    if synced.exists() {
        std::fs::remove_file(&synced)?;
    }
    Ok(())
}

/// Check if a recording has been synced to the cloud.
pub(crate) fn is_recording_synced(id: &str) -> bool {
    recordings_dir().join(format!("{}.synced", id)).exists()
}
```

## Error Cases

| Condition | Behavior |
|-----------|----------|
| No session matches ID prefix | `Error: No session found matching prefix 'abc'` |
| Trying to delete active session | `Error: Session abc12345 is currently active. Use mur session discard to stop and delete it.` |
| No ID and no `--all` | `Error: Specify a session ID or use --all.` |
| `--all` with no recordings | `No session recordings found.` (exit 0 — not an error) |
| Non-TTY without `--force` | `Error: --force required when not running interactively.` |
| Permission denied on filesystem | Propagate IO error |
| Partial failure during `--all` | Print warning, continue deleting others, report count |

## Testing

- Unit test for `remove_recording()` — create temp `.jsonl` + `.meta.json` + `.synced`, call remove, verify all three gone
- Unit test for `is_recording_synced()` — create `.synced`, check, remove, check
- Unit test for active session guard — `active_session_id()` returns Some, remove should bail
- Manual test: `mur in`, `mur out`, `mur session list`, `mur session remove <id>`, verify with `mur session list`
- Manual test: `mur session remove --all --dry-run`, then `--all --force`
- Confirm prompt test: run in TTY without `--force`, verify prompt; test `--force` skips it
