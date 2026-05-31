use anyhow::{Context, Result, bail};
use mur_common::action::Action;
use crate::action_pipeline::{PendingStore, Pipeline};
use uuid::Uuid;

use super::resolve_mur_home;

pub fn cmd_pending_list(name: &str) -> Result<()> {
    let (_, store) = pending_store_for(name)?;
    let items = store.snapshot();
    if items.is_empty() {
        println!("no pending items for agent '{name}'");
        return Ok(());
    }
    println!("{:<36} {:<12} {:<8} FILES", "ID", "STATUS", "COUNT");
    for item in items {
        println!(
            "{:<36} {:<12} {:<8} {}",
            item.id,
            format!("{:?}", item.status).to_lowercase(),
            item.files.len(),
            item.files.iter().map(|f| f.path.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>().join(", "),
        );
    }
    Ok(())
}

pub fn cmd_pending_act(name: &str, id: &str, action_id: &str) -> Result<()> {
    let (_pipeline, mut store) = pending_store_for(name)?;
    let item_id = Uuid::parse_str(id).context("invalid item ID")?;

    // Find the action in agent's file_actions
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let action = profile
        .file_actions
        .iter()
        .find(|a| a.id == action_id)
        .map(|fa| Action {
            id: fa.id.clone(),
            label: fa.label_for(&profile.companion.locale).to_string(),
            user_prompt: None,
        })
        .or_else(|| {
            if action_id == "ask_me" {
                Some(Action {
                    id: "ask_me".into(),
                    label: "Ask me anything...".into(),
                    user_prompt: None,
                })
            } else {
                None
            }
        })
        .with_context(|| format!("action '{action_id}' not found in agent's file_actions"))?;

    store.select_action(item_id, action)?;
    println!("selected action '{action_id}' for item {id}");
    Ok(())
}

fn pending_store_for(name: &str) -> Result<(Pipeline, PendingStore)> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let config = profile.action_pipeline;
    let pipeline = Pipeline::new(agent_home, config);
    let store = PendingStore::new(&pipeline)?;
    Ok((pipeline, store))
}
