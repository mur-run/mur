use super::*;

pub(super) async fn run_session(action: SessionAction) -> Result<()> {
    match action {
        SessionAction::Start { source } => cmd::session::cmd_session_start(&source)?,
        SessionAction::Stop { analyze, reflect } => {
            cmd::session::cmd_session_stop(analyze, reflect).await?
        }
        SessionAction::Record {
            event_type,
            tool,
            content,
        } => cmd::session::cmd_session_record(&event_type, tool.as_deref(), &content)?,
        SessionAction::Status => cmd::session::cmd_session_status()?,
        SessionAction::List => cmd::session::cmd_session_list()?,
        SessionAction::Review { id } => cmd::session::cmd_session_review(&id)?,
        SessionAction::Show { id, last, json } => cmd::session::cmd_session_show(&id, last, json)?,
        SessionAction::Export {
            id,
            format,
            analyze,
            output,
        } => cmd::session::cmd_session_export(&id, &format, analyze, output).await?,
        SessionAction::Push { id, all } => {
            cmd::session::cmd_session_push(id.as_deref(), all).await?
        }
        SessionAction::In { source } => cmd::session::cmd_in(&source).await?,
        SessionAction::Out { action, force } => {
            cmd::session::cmd_out(action.as_deref(), force).await?
        }
        SessionAction::Discard => cmd::session::cmd_session_exit()?,
        SessionAction::Remove {
            id,
            all,
            force,
            dry_run,
        } => cmd::session::cmd_session_remove(id, all, force, dry_run)?,
        SessionAction::Gc => cmd::session::cmd_session_gc()?,
    }
    Ok(())
}
