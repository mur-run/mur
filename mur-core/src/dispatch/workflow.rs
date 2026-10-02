use super::*;

pub(super) async fn run_workflow(action: WorkflowAction) -> Result<()> {
    match action {
        WorkflowAction::Run {
            query,
            fail_fast,
            prompt,
            yes,
            channel,
            channel_new,
        } => {
            cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt, yes, channel, channel_new)
                .await?
        }
        WorkflowAction::Suggest {
            create,
            accept,
            dismiss,
        } => cmd::workflow::cmd_suggest(create, accept.as_deref(), dismiss.as_deref())?,
        WorkflowAction::List => cmd::workflow::cmd_workflow_list()?,
        WorkflowAction::Schedule { action } => match action {
            ScheduleAction::List => cmd::workflow::cmd_schedule_list()?,
            ScheduleAction::Set { name, cron } => cmd::workflow::cmd_schedule_set(&name, &cron)?,
            ScheduleAction::Remove { name } => cmd::workflow::cmd_schedule_remove(&name)?,
            ScheduleAction::Enable { name } => cmd::workflow::cmd_schedule_enable(&name, true)?,
            ScheduleAction::Disable { name } => cmd::workflow::cmd_schedule_enable(&name, false)?,
        },
        WorkflowAction::Show { name, md } => cmd::workflow::cmd_workflow_show(&name, md)?,
        WorkflowAction::Search { query, limit } => {
            cmd::workflow::cmd_workflow_search(&query, limit).await?
        }
        WorkflowAction::New => cmd::workflow::cmd_workflow_new()?,
        WorkflowAction::Publish { name, team } => {
            cmd::workflow::cmd_workflow_publish(&name, &team)?
        }
        WorkflowAction::Delete {
            name,
            yes,
            local_only,
        } => cmd::workflow_delete::cmd_workflow_delete(&name, yes, local_only).await?,
        WorkflowAction::Install { name, from } => {
            cmd::workflow::cmd_workflow_install(&name, &from)?
        }
    }
    Ok(())
}
