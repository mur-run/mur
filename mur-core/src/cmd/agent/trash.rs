use crate::action_pipeline::Pipeline;
use crate::action_pipeline::ledger::ActionLedger;
use anyhow::{Context, Result, bail};
use mur_common::action::{ActionEvent, DeletionConfig, PermDeleteReason, TrashEntry};
use mur_trash::MoveOutcome;
use std::io;
use std::path::Path;
use uuid::Uuid;

use super::resolve_mur_home;

/// Shared with the daemon's expiry pass, so both see the same entries.
pub(super) use mur_trash::lookup_days;

pub fn cmd_trash_list(name: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let days = lookup_days(&pipeline.config.deletion);
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), days);
    let mut found = false;
    println!(
        "{:<36} {:<14} {:<40} ORIGINAL",
        "ID", "STATUS", "TRASH_PATH"
    );
    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event {
            found = true;
            let status = format!("{:?}", entry.status).to_lowercase();
            let trash_path = entry
                .trash_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "{:<36} {:<14} {:<40} {}",
                entry.id,
                status,
                trash_path,
                entry.original_path.display()
            );
        }
    }
    if !found {
        println!("trash is empty for agent '{name}'");
    }
    Ok(())
}

pub fn cmd_trash_restore(name: &str, id: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let entry_id = Uuid::parse_str(id).context("invalid entry ID")?;
    let entry = find_entry(&pipeline.ledger_dir(), &pipeline.config.deletion, entry_id)?;

    let outcome = restore_entry(&entry, mur_trash::move_across)?;
    // The object is back at its original path in both outcomes.
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    ledger.append(&ActionEvent::TrashRestored { entry_id })?;
    let trash_path = entry.trash_path.as_deref().unwrap_or(Path::new("?"));
    println!(
        "restored {} to {}",
        trash_path.display(),
        entry.original_path.display()
    );
    if let MoveOutcome::CopiedSourceKept(e) = outcome {
        eprintln!(
            "warning: restored, but the trash copy could not be removed: {e}\n  \
             remove it with: mur agent trash now {name} {id}"
        );
    }
    Ok(())
}

/// The `TrashCreated` record for `entry_id`, searched across the whole
/// retention window rather than a fixed 30 days.
fn find_entry(ledger_dir: &Path, deletion: &DeletionConfig, entry_id: Uuid) -> Result<TrashEntry> {
    ActionLedger::replay_days(ledger_dir, lookup_days(deletion))
        .into_iter()
        .find_map(|e| match e {
            ActionEvent::TrashCreated { entry } if entry.id == entry_id => Some(entry),
            _ => None,
        })
        .with_context(|| format!("trash entry {entry_id} not found"))
}

/// Move one trashed object back to its original path. Refuses to overwrite:
/// anything now at the original path (file, directory or dangling link) wins.
/// `mover` is [`mur_trash::move_across`] outside tests.
fn restore_entry(
    entry: &TrashEntry,
    mover: impl FnOnce(&Path, &Path) -> io::Result<MoveOutcome>,
) -> Result<MoveOutcome> {
    let id = entry.id;
    let trash_path = entry
        .trash_path
        .as_deref()
        .with_context(|| format!("entry {id} has no trash path"))?;
    if std::fs::symlink_metadata(trash_path).is_err() {
        bail!(
            "trash file not found for entry {id}: {}",
            trash_path.display()
        );
    }
    let original = &entry.original_path;
    // Early, friendly refusal. Not the guarantee: the mover's renames are
    // no-replace, so something created after this check still wins.
    if std::fs::symlink_metadata(original).is_ok() {
        bail!(not_overwriting(original));
    }
    match mover(trash_path, original) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => bail!(not_overwriting(original)),
        r => r.with_context(|| {
            format!(
                "cannot restore {} to {}",
                trash_path.display(),
                original.display()
            )
        }),
    }
}

fn not_overwriting(original: &Path) -> String {
    format!(
        "{} already exists; not overwriting it. Move or delete it, then restore again",
        original.display()
    )
}

pub fn cmd_trash_empty(name: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let events = ActionLedger::replay_days(
        &pipeline.ledger_dir(),
        lookup_days(&pipeline.config.deletion),
    );
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    let mut count = 0;

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event {
            if let Some(ref trash_path) = entry.trash_path {
                purge(&pipeline.trash_dir(), trash_path)?;
            }
            ledger.append(&ActionEvent::TrashPermDeleted {
                entry_id: entry.id,
                reason: PermDeleteReason::UserEmpty,
            })?;
            count += 1;
        }
    }
    println!("permanently deleted {count} trashed files");
    Ok(())
}

pub fn cmd_trash_now(name: &str, id: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let entry_id = Uuid::parse_str(id).context("invalid entry ID")?;
    let events = ActionLedger::replay_days(
        &pipeline.ledger_dir(),
        lookup_days(&pipeline.config.deletion),
    );

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event
            && entry.id == entry_id
        {
            if let Some(ref trash_path) = entry.trash_path {
                purge(&pipeline.trash_dir(), trash_path)?;
            }
            let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
            ledger.append(&ActionEvent::TrashPermDeleted {
                entry_id,
                reason: PermDeleteReason::UserNow,
            })?;
            println!("permanently deleted entry {id}");
            return Ok(());
        }
    }
    bail!("trash entry {id} not found");
}

/// Permanently delete one trashed object (D5): files, directories and links
/// alike, never following a link out of the trash and never the root itself.
/// The entry's own directory goes too once it is empty. Already gone is fine.
fn purge(trash_root: &Path, trash_path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(trash_path).is_err() {
        return Ok(());
    }
    mur_trash::remove_within(trash_root, trash_path)
        .with_context(|| format!("cannot delete {}", trash_path.display()))?;
    if let Some(dir) = trash_path.parent()
        && dir != trash_root
        && dir.starts_with(trash_root)
    {
        let _ = std::fs::remove_dir(dir); // only succeeds when empty
    }
    Ok(())
}

fn pipeline_for(name: &str) -> Result<Pipeline> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let config = profile.action_pipeline;
    Ok(Pipeline::new(agent_home, config))
}

#[cfg(all(test, unix))]
#[path = "trash_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "trash_restore_tests.rs"]
mod restore_tests;
