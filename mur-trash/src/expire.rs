//! Retention expiry (#2174): permanently delete trash entries whose
//! `retention_until` has passed, once each.
//!
//! The trash layout on disk holds no per-entry metadata, so the ledger is the
//! only record of when an entry expires. Discovery is NOT a window ending
//! today (review #2176 P1): an entry whose creation day falls out of such a
//! window — a pre-fix backlog, a daemon offline for weeks, a delete that kept
//! failing — would never be retried. Instead a cursor file remembers the
//! earliest ledger day that still holds an outstanding entry; each pass
//! replays every day file from there, and a missing or unreadable cursor
//! replays the whole ledger. The cursor never moves later than
//! [`lookup_days`] before today, so the CLI's window is always covered too.
//! Nor does it move past a day file that could not be read (review #2176
//! P2): its records were not seen, so they are not known to be settled. When
//! the ledger directory itself cannot be listed the cursor is left untouched.
//! And an incomplete scan deletes and records nothing: the unread day may hold
//! the very `TrashExpired` that settles an entry, so acting on the partial
//! history would record it a second time.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use chrono::{DateTime, Local, NaiveDate, Utc};
use mur_common::action::{ActionEvent, DeletionConfig, TrashEntry, TrashStatus};
use mur_common::ledger::Ledger;
use uuid::Uuid;

use crate::layout::{ACTIONS_DIR_NAME, ledger_dir, trash_root};
use crate::safety::remove_within;

/// Days of ledger always replayed, even when retention is configured shorter.
pub const MIN_LOOKUP_DAYS: u32 = 30;
/// Slack past retention: covers day-file boundaries and a daemon that has not
/// yet expired an entry whose retention just ran out.
pub const LOOKUP_MARGIN_DAYS: u32 = 2;

/// File under `<agent_home>/actions/` holding the expiry cursor day.
pub const EXPIRY_CURSOR_FILE: &str = "trash-expiry.cursor";
const CURSOR_FORMAT: &str = "%Y-%m-%d";

/// `<agent_home>/actions/trash-expiry.cursor`.
pub fn expiry_cursor_path(agent_home: &Path) -> std::path::PathBuf {
    agent_home.join(ACTIONS_DIR_NAME).join(EXPIRY_CURSOR_FILE)
}

/// How far back the ledger must be replayed to see every entry still on disk.
pub fn lookup_days(deletion: &DeletionConfig) -> u32 {
    deletion
        .trash_retention_days
        .max(MIN_LOOKUP_DAYS)
        .saturating_add(LOOKUP_MARGIN_DAYS)
}

/// Bytes the trash holds, summed from the ledger rather than walked on disk.
///
/// Every byte in an entry directory was put there by a `TrashCreated` record
/// (status `Retained`) and leaves with a `TrashExpired`, `TrashRestored` or
/// `TrashPermDeleted`, so the open records are the trash's contents. Reading
/// the ledger costs one pass over its day files, not one `stat` per trashed
/// file. `.staging` leftovers from a crashed copy are not counted; the daemon
/// sweeps them.
///
/// Replay starts at the expiry cursor, not at a window ending today (review
/// #2177 P1): an open entry whose creation day is older than [`lookup_days`]
/// — a backlog, a delete that keeps failing, a retention since shortened —
/// still occupies the trash and must still count against the cap. The cursor
/// never passes an outstanding entry, and without one the whole ledger is
/// read, so nothing open is ever missed.
pub fn retained_bytes(agent_home: &Path) -> u64 {
    let events = Ledger::<ActionEvent>::scan_since(
        &ledger_dir(agent_home),
        read_cursor(&expiry_cursor_path(agent_home)),
    );
    let settled = settled_ids(events.iter().map(|(_, e)| e));
    outstanding(&events, &settled)
        .iter()
        .map(|(_, entry)| entry.file_size_bytes)
        .fold(0u64, u64::saturating_add)
}

/// What one expiry pass did.
#[derive(Debug, Default, PartialEq)]
pub struct ExpireReport {
    /// Entries deleted from disk (or already gone) and recorded `TrashExpired`.
    pub expired: Vec<Uuid>,
    /// Entries past retention that could not be deleted; retried next pass.
    pub failed: Vec<(Uuid, String)>,
}

/// Delete every retained entry of the agent at `agent_home` whose retention
/// ended before `now`, then record it as `TrashExpired`.
///
/// Deletion goes through [`remove_within`], so a link is removed as a link and
/// nothing outside the trash root is touched. An entry already expired,
/// restored or permanently deleted is skipped, so a second pass is a no-op.
/// The record is appended only after the files are gone; a failed delete is
/// reported and left for the next pass.
pub fn expire_due(
    agent_home: &Path,
    deletion: &DeletionConfig,
    now: DateTime<Utc>,
) -> anyhow::Result<ExpireReport> {
    let dir = ledger_dir(agent_home);
    let cursor_path = expiry_cursor_path(agent_home);
    let scan = Ledger::<ActionEvent>::scan_since(&dir, read_cursor(&cursor_path));
    let events = &scan.events;
    let today = now.with_timezone(&Local).date_naive();
    let floor = today - chrono::Duration::days(lookup_days(deletion).into());

    let settled = settled_ids(events.iter().map(|(_, e)| e));
    let outstanding = outstanding(events, &settled);
    let mut report = ExpireReport::default();
    let mut keep_from = floor;

    // An unread day may hold an entry's settlement, so on partial history an
    // "outstanding" entry may already be expired or restored: acting on it
    // would record it twice. Defer every side effect until a complete scan.
    let complete = scan.is_complete();
    if !complete {
        tracing::warn!(dir = %dir.display(), "ledger scan incomplete; trash expiry deferred");
    }
    let due: Vec<_> = outstanding
        .iter()
        .filter(|_| complete)
        .filter(|(_, entry)| entry.retention_until.is_some_and(|until| until < now))
        .collect();
    if !due.is_empty() {
        let root = trash_root(agent_home);
        let mut ledger = Ledger::<ActionEvent>::open(&dir)?;
        for (_, entry) in due {
            if let Some(path) = &entry.trash_path
                && let Err(e) = purge(&root, path)
            {
                report.failed.push((entry.id, e.to_string()));
                continue;
            }
            ledger.append(&ActionEvent::TrashExpired { entry_id: entry.id })?;
            report.expired.push(entry.id);
        }
    }
    // Everything not expired just now — still in retention, or failed — must
    // be seen again, so the cursor stays at or before its creation day.
    for (day, entry) in &outstanding {
        if !report.expired.contains(&entry.id) {
            keep_from = keep_from.min(*day);
        }
    }
    // A day that could not be read may hold outstanding entries we never saw.
    if let Some(first) = scan.unreadable_days.first() {
        keep_from = keep_from.min(*first);
    }
    if scan.listing_failed {
        // Whole days may be missing: no basis to move the cursor at all.
        tracing::warn!(dir = %dir.display(), "ledger listing incomplete; trash expiry cursor kept");
    } else if let Err(e) = write_cursor(&cursor_path, keep_from) {
        // Not fatal: a stale cursor only means a longer replay next pass.
        tracing::warn!(path = %cursor_path.display(), error = %e, "trash expiry cursor not saved");
    }
    Ok(report)
}

/// Unsettled retained entries, first creation record per id, with the day
/// file it was found in.
fn outstanding<'a>(
    events: &'a [(NaiveDate, ActionEvent)],
    settled: &HashSet<Uuid>,
) -> Vec<(NaiveDate, &'a TrashEntry)> {
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(|(day, e)| match e {
            ActionEvent::TrashCreated { entry }
                if entry.status == TrashStatus::Retained
                    && !settled.contains(&entry.id)
                    && seen.insert(entry.id) =>
            {
                Some((*day, entry))
            }
            _ => None,
        })
        .collect()
}

/// The saved cursor day; `None` (replay everything) when absent or unreadable.
fn read_cursor(path: &Path) -> Option<NaiveDate> {
    let text = std::fs::read_to_string(path).ok()?;
    NaiveDate::parse_from_str(text.trim(), CURSOR_FORMAT).ok()
}

/// Atomic write: temp file + rename, so a crash never leaves a torn cursor.
fn write_cursor(path: &Path, day: NaiveDate) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("cursor.tmp");
    std::fs::write(&tmp, format!("{}\n", day.format(CURSOR_FORMAT)))?;
    std::fs::rename(&tmp, path)
}

/// Entry ids that no longer own anything in the trash.
fn settled_ids<'a>(events: impl Iterator<Item = &'a ActionEvent>) -> HashSet<Uuid> {
    events
        .filter_map(|e| match e {
            ActionEvent::TrashExpired { entry_id }
            | ActionEvent::TrashRestored { entry_id }
            | ActionEvent::TrashPermDeleted { entry_id, .. } => Some(*entry_id),
            _ => None,
        })
        .collect()
}

/// Delete one trashed object and, once empty, its entry directory.
/// Already gone is fine: the entry still expires.
fn purge(root: &Path, path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    remove_within(root, path)?;
    if let Some(dir) = path.parent()
        && dir != root
        && dir.starts_with(root)
    {
        let _ = std::fs::remove_dir(dir); // only succeeds when empty
    }
    Ok(())
}
