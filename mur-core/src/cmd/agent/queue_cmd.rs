use crate::action_pipeline::{Pipeline, TaskQueue};
use anyhow::{Context, Result, bail};
use uuid::Uuid;

use super::resolve_mur_home;

fn pipeline_for(name: &str) -> Result<(Pipeline, TaskQueue)> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let config = profile.action_pipeline.clone();
    let pipeline = Pipeline::new(agent_home, config);
    let queue = TaskQueue::new(&pipeline)?;
    Ok((pipeline, queue))
}

pub fn cmd_queue_list(name: &str) -> Result<()> {
    let (_pipeline, queue) = pipeline_for(name)?;
    let tasks = queue.all_tasks();
    if tasks.is_empty() {
        println!("no tasks for agent '{name}'");
        return Ok(());
    }
    println!("{:<36} {:<12} {:<20} ACTION", "ID", "STATE", "CREATED");
    for t in tasks {
        let state = match &t.state {
            mur_common::action::TaskState::Queued => "QUEUED",
            mur_common::action::TaskState::Running => "RUNNING",
            mur_common::action::TaskState::Paused => "PAUSED",
            mur_common::action::TaskState::Completed { .. } => "COMPLETED",
            mur_common::action::TaskState::Cancelled => "CANCELLED",
        };
        println!(
            "{:<36} {:<12} {:<20} {}",
            t.id,
            state,
            t.created_at.format("%Y-%m-%d %H:%M:%S"),
            t.action.label,
        );
    }
    Ok(())
}

pub fn cmd_queue_pause(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    queue.pause_task(task_id, "CLI pause".into())?;
    println!("paused task {id}");
    Ok(())
}

pub fn cmd_queue_resume(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    queue.resume_task(task_id)?;
    println!("resumed task {id}");
    Ok(())
}

pub fn cmd_queue_cancel(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    queue.cancel_task(task_id)?;
    println!("cancelled task {id}");
    Ok(())
}

pub fn cmd_queue_retry(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    let task = queue
        .get(task_id)
        .with_context(|| format!("task {id} not found"))?;
    let action = task.action.clone();
    let pending_id = task.pending_item_id;
    queue.enqueue(pending_id, action, task.timeout_seconds, vec![])?;
    println!("re-enqueued task {id}");
    Ok(())
}
