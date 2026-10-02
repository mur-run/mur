use super::*;

pub(super) async fn run_chat(action: ChatAction) -> Result<()> {
    match action {
        ChatAction::List { since, src } => cmd::conversations_cmd::cmd_chat_list(since, src)?,
        ChatAction::Show { date } => cmd::conversations_cmd::cmd_chat_show(date)?,
        ChatAction::Raw { date, conv } => cmd::conversations_cmd::cmd_chat_raw(date, conv)?,
        ChatAction::Search { query, limit, src } => {
            cmd::conversations_cmd::cmd_chat_search(query, limit, src).await?
        }
        ChatAction::Ask {
            question,
            src,
            since,
            until,
            k,
            model,
            min_score,
            json,
            no_escalate,
            debug_prompt,
            strict_citations,
            continue_flag,
            new_flag,
            show_session,
            no_summarize,
            summarize_model,
        } => {
            cmd::conversations_cmd::cmd_ask(cmd::conversations_cmd::AskArgs {
                question,
                src,
                since,
                until,
                k,
                model,
                min_score,
                json,
                no_escalate,
                debug_prompt,
                strict_citations,
                continue_flag,
                new_flag,
                show_session,
                no_summarize,
                summarize_model,
            })
            .await?
        }
        ChatAction::Pull => cmd::conversations_cmd::cmd_conversations_pull().await?,
        ChatAction::Cleanup => cmd::conversations_cmd::cmd_conversations_cleanup().await?,
        ChatAction::Reindex {
            raw_only,
            spans_only,
            rollups_only,
        } => {
            cmd::conversations_cmd::cmd_conversations_reindex(cmd::conversations_cmd::ReindexArgs {
                raw_only,
                spans_only,
                rollups_only,
            })
            .await?
        }
        ChatAction::Doctor => cmd::conversations_cmd::cmd_conversations_doctor().await?,
        ChatAction::Preflight => cmd::conversations_cmd::cmd_conversations_preflight().await?,
        ChatAction::Migrate {
            run,
            resume,
            discard_staging,
        } => {
            cmd::conversations_cmd::cmd_conversations_migrate(run, resume, discard_staging).await?
        }
        ChatAction::Rollback => cmd::conversations_cmd::cmd_conversations_rollback().await?,
        ChatAction::Compact {
            date,
            since,
            force,
            if_stale,
            max_days,
            extractive_only,
            debug_prompt,
            skip_rollups,
        } => {
            cmd::conversations_cmd::cmd_conversations_compact(cmd::conversations_cmd::CompactArgs {
                date,
                since,
                force,
                if_stale,
                max_days,
                extractive_only,
                debug_prompt,
                skip_rollups,
            })
            .await?
        }
        ChatAction::Rollup {
            week,
            month,
            all_missing,
            force,
            if_stale,
            max_weeks,
            max_months,
        } => {
            cmd::conversations_cmd::cmd_conversations_rollup(cmd::conversations_cmd::RollupArgs {
                week,
                month,
                all_missing,
                force,
                if_stale,
                max_weeks,
                max_months,
            })
            .await?
        }
        ChatAction::CostReport { since, json } => {
            cmd::conversations_cost_report::cmd_cost_report(&since, json, None).await?
        }
    }
    Ok(())
}

pub(super) async fn run_conversations(action: ConversationsAction) -> Result<()> {
    {
        eprintln!("# mur conversations: use `mur chat <subcommand>`");
        match action {
            ConversationsAction::Pull => cmd::conversations_cmd::cmd_conversations_pull().await?,
            ConversationsAction::Cleanup => {
                cmd::conversations_cmd::cmd_conversations_cleanup().await?
            }
            ConversationsAction::Reindex {
                raw_only,
                spans_only,
                rollups_only,
            } => {
                cmd::conversations_cmd::cmd_conversations_reindex(
                    cmd::conversations_cmd::ReindexArgs {
                        raw_only,
                        spans_only,
                        rollups_only,
                    },
                )
                .await?
            }
            ConversationsAction::Doctor => {
                cmd::conversations_cmd::cmd_conversations_doctor().await?
            }
            ConversationsAction::Preflight => {
                cmd::conversations_cmd::cmd_conversations_preflight().await?
            }
            ConversationsAction::Migrate {
                run,
                resume,
                discard_staging,
            } => {
                cmd::conversations_cmd::cmd_conversations_migrate(run, resume, discard_staging)
                    .await?
            }
            ConversationsAction::Rollback => {
                cmd::conversations_cmd::cmd_conversations_rollback().await?
            }
            ConversationsAction::Compact {
                date,
                since,
                force,
                if_stale,
                max_days,
                extractive_only,
                debug_prompt,
                skip_rollups,
            } => {
                cmd::conversations_cmd::cmd_conversations_compact(
                    cmd::conversations_cmd::CompactArgs {
                        date,
                        since,
                        force,
                        if_stale,
                        max_days,
                        extractive_only,
                        debug_prompt,
                        skip_rollups,
                    },
                )
                .await?
            }
            ConversationsAction::Rollup {
                week,
                month,
                all_missing,
                force,
                if_stale,
                max_weeks,
                max_months,
            } => {
                cmd::conversations_cmd::cmd_conversations_rollup(
                    cmd::conversations_cmd::RollupArgs {
                        week,
                        month,
                        all_missing,
                        force,
                        if_stale,
                        max_weeks,
                        max_months,
                    },
                )
                .await?
            }
            ConversationsAction::CostReport { since, json } => {
                cmd::conversations_cost_report::cmd_cost_report(&since, json, None).await?
            }
        }
    }
    Ok(())
}
