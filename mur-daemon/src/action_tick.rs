use anyhow::Result;
use chrono::Utc;
use mur_common::action::{ActionEvent, TrashStatus};
use mur_core::action_pipeline::ledger::ActionLedger;
use mur_core::action_pipeline::{PendingStore, Pipeline};

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

    for event in &events {
        match event {
            // Execute deferred deletes (cancel window elapsed, no Undo)
            ActionEvent::DeletionPending { entry } => {
                if entry.status == TrashStatus::PendingDelete
                    && entry.execute_at < now
                {
                    execute_move_to_trash(&pipeline, entry)?;
                }
            }
            // Mark expirations (past retention)
            ActionEvent::TrashCreated { entry } => {
                if entry.status == TrashStatus::Retained
                    && let Some(retention_until) = entry.retention_until
                    && retention_until < now
                {
                    mark_expired(&pipeline, &entry.id)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn execute_move_to_trash(
    pipeline: &Pipeline,
    entry: &mur_common::action::TrashEntry,
) -> Result<()> {
    let trash_dir = pipeline.trash_dir();
    std::fs::create_dir_all(&trash_dir)?;

    let ts = Utc::now().format("%Y%m%dT%H%M%S");
    let filename = entry
        .original_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let trash_subdir = trash_dir.join(format!("{ts}_{filename}"));
    std::fs::create_dir_all(&trash_subdir)?;
    let trash_file = trash_subdir.join(filename);

    // Try rename first
    let moved = match std::fs::rename(&entry.original_path, &trash_file) {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV — cross-filesystem, fallback to copy + unlink
            std::fs::copy(&entry.original_path, &trash_file)?;
            std::fs::remove_file(&entry.original_path)?;
            true
        }
        Err(e) => {
            tracing::error!(path=%entry.original_path.display(), error=%e, "move to trash failed");
            false
        }
    };

    if moved {
        let mut updated = entry.clone();
        updated.trash_path = Some(trash_file);
        updated.retention_until =
            Some(Utc::now() + chrono::Duration::days(
                pipeline.config.deletion.trash_retention_days as i64
            ));
        updated.status = TrashStatus::Retained;

        let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
        ledger.append(&ActionEvent::TrashCreated { entry: updated })?;
    }

    Ok(())
}

fn mark_expired(pipeline: &Pipeline, entry_id: &uuid::Uuid) -> Result<()> {
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    ledger.append(&ActionEvent::TrashExpired {
        entry_id: *entry_id,
    })?;
    Ok(())
}
