use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use mur_common::action::{ActionEvent, TrashStatus};
use mur_core::action_pipeline::ledger::ActionLedger;
use mur_core::action_pipeline::{PendingStore, Pipeline};
use mur_trash::MoveOutcome;
use uuid::Uuid;

/// A `.partial` this old is a crashed cross-device copy, not one in flight.
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(60 * 60);

/// Scan all agent homes for action pipeline work:
/// 1. Expire stale pending items (past TTL)
/// 2. Execute deferred trash moves (cancel window elapsed, no Undo)
/// 3. Mark expired trash entries (past retention)
///
/// Called every 30 seconds from mur-daemon's main loop.
pub fn scan_all_agents(mur_home: &std::path::Path) -> Result<()> {
    let agents_dir = mur_home.join("agents");
    if !agents_dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent_home = entry.path();
        if let Err(e) = scan_one_agent(&agent_home) {
            tracing::warn!(
                agent = %entry.file_name().to_string_lossy(),
                error = %e,
                "action_tick: scan failed"
            );
        }
    }
    Ok(())
}

fn scan_one_agent(agent_home: &std::path::Path) -> Result<()> {
    let profile_path = agent_home.join("profile.yaml");
    if !profile_path.exists() {
        return Ok(());
    }
    let yaml = std::fs::read_to_string(&profile_path)?;
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml)?;
    let config = profile.action_pipeline;
    let pipeline = Pipeline::new(agent_home.to_path_buf(), config.clone());

    // 1. Expire stale pending items
    if let Ok(mut store) = PendingStore::new(&pipeline) {
        let expired = store.expire_stale().unwrap_or(0);
        if expired > 0 {
            tracing::info!(agent = %profile.name, expired, "expired stale pending items");
        }
    }

    // 2 & 3. Trash work — replay recent events and process
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 7);
    let now = Utc::now();

    // Execute deferred deletes (cancel window elapsed, no Undo)
    for entry in due_pending(&events, now) {
        execute_move_to_trash(&pipeline, entry)?;
    }

    // Mark expirations (past retention)
    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event
            && entry.status == TrashStatus::Retained
            && let Some(retention_until) = entry.retention_until
            && retention_until < now
        {
            mark_expired(&pipeline, &entry.id)?;
        }
    }

    match mur_trash::sweep_partials(&pipeline.trash_dir(), STALE_PARTIAL_AGE) {
        Ok(0) => {}
        Ok(n) => tracing::info!(agent = %profile.name, removed = n, "swept stale trash partials"),
        Err(e) => tracing::warn!(agent = %profile.name, error = %e, "trash partial sweep failed"),
    }

    Ok(())
}

/// Pending deletes whose cancel window has passed and that have not already
/// run or been cancelled. A `DeletionPending` stays in the ledger afterwards;
/// without the settled check every tick would try to move it again.
fn due_pending(
    events: &[ActionEvent],
    now: chrono::DateTime<Utc>,
) -> Vec<&mur_common::action::TrashEntry> {
    let settled = settled_ids(events);
    events
        .iter()
        .filter_map(|e| match e {
            ActionEvent::DeletionPending { entry }
                if entry.status == TrashStatus::PendingDelete
                    && entry.execute_at < now
                    && !settled.contains(&entry.id) =>
            {
                Some(entry)
            }
            _ => None,
        })
        .collect()
}

/// Entry ids whose pending delete already ran or was cancelled.
fn settled_ids(events: &[ActionEvent]) -> HashSet<Uuid> {
    events
        .iter()
        .filter_map(|e| match e {
            ActionEvent::TrashCreated { entry } => Some(entry.id),
            ActionEvent::DeletionCancelled { entry_id } => Some(*entry_id),
            _ => None,
        })
        .collect()
}

/// Move one pending delete into `<trash>/<entry-id>/<filename>` (D3) with the
/// shared cross-device mover (D4), then record it. Refusals and move failures
/// are logged and leave the entry pending; only a ledger failure is an error.
fn execute_move_to_trash(
    pipeline: &Pipeline,
    entry: &mur_common::action::TrashEntry,
) -> Result<()> {
    execute_move_to_trash_with(pipeline, entry, &mur_trash::rename_noreplace)
}

/// [`execute_move_to_trash`] with an injectable `rename`, so a test can force
/// EXDEV and pin the staging layout `sweep_partials` cleans (review #2164 P2).
/// Production passes [`mur_trash::rename_noreplace`]: the injected rename is
/// used for every step, including the final publish into the entry, so a
/// plain `std::fs::rename` here would silently overwrite (#2168).
fn execute_move_to_trash_with(
    pipeline: &Pipeline,
    entry: &mur_common::action::TrashEntry,
    rename: &dyn Fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> Result<()> {
    let path = &entry.original_path;
    let resolved = match mur_trash::check_target(path, &pipeline.agent_home) {
        Ok(Ok(p)) => p,
        Ok(Err(refusal)) => {
            tracing::warn!(path = %path.display(), %refusal, "pending delete refused");
            return Ok(());
        }
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "move to trash failed");
            return Ok(());
        }
    };
    let Some(trash_file) = mur_trash::entry_path(&pipeline.trash_dir(), entry.id, &resolved) else {
        return Ok(()); // check_target already refuses paths without a name
    };
    let entry_dir = trash_file.parent().map(std::path::Path::to_path_buf);

    // Staged under `<trash>/.staging/`: the only place the sweep looks, so a
    // crash mid-copy never strands a `.partial` inside the entry directory.
    let staging = mur_trash::staging_root(&pipeline.trash_dir());
    match mur_trash::move_with_staging(&resolved, &trash_file, &staging, rename) {
        Ok(MoveOutcome::Moved) => {}
        Ok(MoveOutcome::CopiedSourceKept(e)) => {
            // The copy is complete and restorable, so it is still recorded.
            tracing::warn!(path = %path.display(), error = %e, "trashed a copy; original not removed");
        }
        Err(e) => {
            if let Some(dir) = &entry_dir {
                let _ = std::fs::remove_dir(dir);
            }
            tracing::error!(path = %path.display(), error = %e, "move to trash failed");
            return Ok(());
        }
    }

    let mut updated = entry.clone();
    updated.trash_path = Some(trash_file);
    updated.retention_until = Some(
        Utc::now() + chrono::Duration::days(pipeline.config.deletion.trash_retention_days as i64),
    );
    updated.status = TrashStatus::Retained;

    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    ledger.append(&ActionEvent::TrashCreated { entry: updated })?;
    Ok(())
}

fn mark_expired(pipeline: &Pipeline, entry_id: &uuid::Uuid) -> Result<()> {
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    ledger.append(&ActionEvent::TrashExpired {
        entry_id: *entry_id,
    })?;
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "action_tick_tests.rs"]
mod tests;
