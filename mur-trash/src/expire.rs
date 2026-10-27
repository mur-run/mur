//! Retention expiry (#2174): permanently delete trash entries whose
//! `retention_until` has passed, once each.
//!
//! The trash layout on disk holds no per-entry metadata, so the ledger is the
//! only record of when an entry expires. It is replayed over
//! [`lookup_days`] — the same window `trash list|restore|empty|now` use — so
//! an entry visible to the CLI is also visible to expiry.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use chrono::{DateTime, Utc};
use mur_common::action::{ActionEvent, DeletionConfig, TrashEntry, TrashStatus};
use mur_common::ledger::Ledger;
use uuid::Uuid;

use crate::layout::{ledger_dir, trash_root};
use crate::safety::remove_within;

/// Days of ledger always replayed, even when retention is configured shorter.
pub const MIN_LOOKUP_DAYS: u32 = 30;
/// Slack past retention: covers day-file boundaries and a daemon that has not
/// yet expired an entry whose retention just ran out.
pub const LOOKUP_MARGIN_DAYS: u32 = 2;

/// How far back the ledger must be replayed to see every entry still on disk.
pub fn lookup_days(deletion: &DeletionConfig) -> u32 {
    deletion
        .trash_retention_days
        .max(MIN_LOOKUP_DAYS)
        .saturating_add(LOOKUP_MARGIN_DAYS)
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
    let events: Vec<ActionEvent> = Ledger::<ActionEvent>::scan_days(&dir, lookup_days(deletion))
        .into_iter()
        .filter_map(Result::ok)
        .collect();
    let due = due_expiries(&events, now);
    let mut report = ExpireReport::default();
    if due.is_empty() {
        return Ok(report);
    }

    let root = trash_root(agent_home);
    let mut ledger = Ledger::<ActionEvent>::open(&dir)?;
    for entry in due {
        if let Some(path) = &entry.trash_path
            && let Err(e) = purge(&root, path)
        {
            report.failed.push((entry.id, e.to_string()));
            continue;
        }
        ledger.append(&ActionEvent::TrashExpired { entry_id: entry.id })?;
        report.expired.push(entry.id);
    }
    Ok(report)
}

/// Retained entries past `retention_until` that nothing has settled yet.
fn due_expiries(events: &[ActionEvent], now: DateTime<Utc>) -> Vec<&TrashEntry> {
    let settled = settled_ids(events);
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(|e| match e {
            ActionEvent::TrashCreated { entry }
                if entry.status == TrashStatus::Retained
                    && entry.retention_until.is_some_and(|until| until < now)
                    && !settled.contains(&entry.id)
                    && seen.insert(entry.id) =>
            {
                Some(entry)
            }
            _ => None,
        })
        .collect()
}

/// Entry ids that no longer own anything in the trash.
fn settled_ids(events: &[ActionEvent]) -> HashSet<Uuid> {
    events
        .iter()
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
