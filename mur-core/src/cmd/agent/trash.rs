use crate::action_pipeline::Pipeline;
use crate::action_pipeline::ledger::ActionLedger;
use anyhow::{Context, Result, bail};
use mur_common::action::{ActionEvent, PermDeleteReason};
use uuid::Uuid;

use super::resolve_mur_home;

pub fn cmd_trash_list(name: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);
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
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event
            && entry.id == entry_id
        {
            if let Some(ref trash_path) = entry.trash_path
                && trash_path.exists()
            {
                std::fs::rename(trash_path, &entry.original_path)?;
                let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
                ledger.append(&ActionEvent::TrashRestored { entry_id })?;
                println!(
                    "restored {} to {}",
                    trash_path.display(),
                    entry.original_path.display()
                );
                return Ok(());
            }
            bail!("trash file not found for entry {id}");
        }
    }
    bail!("trash entry {id} not found");
}

pub fn cmd_trash_empty(name: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    let mut count = 0;

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event {
            if let Some(ref trash_path) = entry.trash_path
                && trash_path.exists()
            {
                std::fs::remove_file(trash_path)?;
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
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event
            && entry.id == entry_id
        {
            if let Some(ref trash_path) = entry.trash_path
                && trash_path.exists()
            {
                std::fs::remove_file(trash_path)?;
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
