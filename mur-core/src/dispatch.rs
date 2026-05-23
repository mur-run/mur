//! Command dispatch — `Cli` → `cmd::*` handler. Extracted from `main.rs`'s
//! `async_main` body. One arm per top-level `Commands` variant; almost every
//! arm is a thin delegate into `cmd::*`. Keep new branches small — heavy
//! logic belongs in `cmd::<feature>`.

use anyhow::Result;
use clap::CommandFactory;

use crate::cli::{
    AgentAction, AgentEvalAction, AgentHooksAction, AgentMcpAction, AgentPermAction,
    AgentPromptAction, AgentScheduleAction, AgentSecretAction, AgentSkillAction,
    AgentWebhookAction, ChatAction, Cli, Commands, CommunityAction, ConversationsAction,
    DeployAction, DraftsAction, EvalAction, EvolveAction, ExchangeAction, FeedbackAction,
    GepAction, HookEvent, InternalsAction, LearnAction, MurmurdAction, PackAction, PatternAction,
    ProjectAction, ScheduleAction, SessionAction, SleepAction, SyncAction, TeamAction, VoiceAction,
    WorkflowAction,
};
use crate::{cmd, dashboard, verify};

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::New { diagram } => cmd::pattern::cmd_new(diagram)?,
        Commands::Search {
            query,
            source,
            result_type,
            only_sources,
            only_patterns,
            limit,
            json,
        } => {
            cmd::search::cmd_search_unified(
                query,
                source,
                result_type,
                only_sources,
                only_patterns,
                limit,
                json,
            )
            .await?
        }
        Commands::Stats => cmd::misc::cmd_stats()?,
        Commands::Doctor => cmd::misc::cmd_doctor()?,
        Commands::Pin { name } => cmd::pattern::cmd_set_lifecycle(&name, "pin")?,
        Commands::Mute { name } => cmd::pattern::cmd_set_lifecycle(&name, "mute")?,
        Commands::Boost { name, amount } => cmd::pattern::cmd_boost(&name, amount)?,
        Commands::Feedback { action } => match action {
            FeedbackAction::Helpful { name } => cmd::pattern::cmd_feedback(&name, true)?,
            FeedbackAction::Unhelpful { name } => cmd::pattern::cmd_feedback(&name, false)?,
            FeedbackAction::Auto { file, dry_run } => {
                cmd::pattern::cmd_feedback_auto(file, dry_run)?
            }
        },
        Commands::Gc { auto } => cmd::misc::cmd_gc(auto)?,

        Commands::Learn { action } => match action {
            LearnAction::Extract {
                file,
                fingerprint,
                llm,
            } => {
                cmd::learn::cmd_learn_extract(file, fingerprint, llm).await?;
            }
            LearnAction::Cross {
                min_projects,
                dry_run,
            } => {
                cmd::learn::cmd_learn_cross(min_projects, dry_run)?;
            }
        },
        Commands::Sync {
            quiet,
            project,
            action,
        } => {
            if let Some(action) = action {
                match action {
                    SyncAction::Status => cmd::sync_cmd::run_status()?,
                }
            } else {
                cmd::sync_cmd::cmd_sync(quiet, project).await?;
            }
        }
        Commands::Inject { query, project: _ } => cmd::inject_cmd::cmd_inject(&query).await?,
        Commands::Hook { event } => match event {
            HookEvent::Prompt { tool } => cmd::hook::cmd_hook_prompt(&tool).await?,
            HookEvent::Tool { tool } => cmd::hook::cmd_hook_tool(&tool).await?,
            HookEvent::Stop { tool } => cmd::hook::cmd_hook_stop(&tool).await?,
            HookEvent::SessionStart { tool } => cmd::hook::cmd_hook_session_start(&tool).await?,
            HookEvent::Stats => cmd::hook::cmd_hook_stats()?,
        },
        Commands::Murmurd { action } => match action {
            MurmurdAction::Start { detach } => cmd::murmurd::cmd_murmurd_start(detach)?,
            MurmurdAction::Stop => cmd::murmurd::cmd_murmurd_stop()?,
            MurmurdAction::Status => cmd::murmurd::cmd_murmurd_status()?,
        },
        Commands::Run {
            query,
            fail_fast,
            prompt,
        } => cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt).await?,
        Commands::Pattern { action } => match action {
            PatternAction::Show { name } => cmd::pattern::cmd_pattern_show(&name)?,
            PatternAction::History { name } => cmd::pattern_history::cmd_pattern_history(&name)?,
            PatternAction::Diff { name, v1, v2 } => {
                cmd::pattern_history::cmd_pattern_diff(&name, v1, v2)?
            }
            PatternAction::Rollback { name, to } => {
                cmd::pattern_history::cmd_pattern_rollback(&name, to)?
            }
        },
        Commands::Workflow { action } => match action {
            WorkflowAction::List => cmd::workflow::cmd_workflow_list()?,
            WorkflowAction::Schedule { action } => match action {
                ScheduleAction::List => cmd::workflow::cmd_schedule_list()?,
                ScheduleAction::Set { name, cron } => {
                    cmd::workflow::cmd_schedule_set(&name, &cron)?
                }
                ScheduleAction::Remove { name } => cmd::workflow::cmd_schedule_remove(&name)?,
                ScheduleAction::Enable { name } => cmd::workflow::cmd_schedule_enable(&name, true)?,
                ScheduleAction::Disable { name } => {
                    cmd::workflow::cmd_schedule_enable(&name, false)?
                }
            },
            WorkflowAction::Show { name, md } => cmd::workflow::cmd_workflow_show(&name, md)?,
            WorkflowAction::Search { query, limit } => {
                cmd::workflow::cmd_workflow_search(&query, limit).await?
            }
            WorkflowAction::New => cmd::workflow::cmd_workflow_new()?,
            WorkflowAction::Publish { name, team } => {
                cmd::workflow::cmd_workflow_publish(&name, &team)?
            }
            WorkflowAction::Install { name, from } => {
                cmd::workflow::cmd_workflow_install(&name, &from)?
            }
        },
        Commands::Reindex { bootstrap } => {
            if bootstrap {
                cmd::reindex::cmd_reindex_bootstrap()?;
            } else {
                cmd::reindex::cmd_reindex().await?;
            }
        }
        Commands::Update { check } => cmd::update::cmd_update(check)?,
        Commands::Promote { name, tier } => cmd::pattern::cmd_promote(&name, &tier)?,
        Commands::Deprecate { name } => cmd::pattern::cmd_deprecate(&name)?,
        Commands::Links { name } => cmd::pattern::cmd_links(&name)?,
        Commands::Evolve {
            dry_run,
            force,
            consolidate,
            action,
        } => {
            if let Some(action) = action {
                match action {
                    EvolveAction::Compose { create } => {
                        cmd::evolve_cmd::cmd_evolve_compose(create)?
                    }
                    EvolveAction::Cooccurrence { min } => {
                        cmd::evolve_cmd::cmd_evolve_cooccurrence(min)?
                    }
                }
            } else if consolidate {
                cmd::evolve_cmd::cmd_consolidate(dry_run)?;
            } else {
                cmd::evolve_cmd::cmd_evolve(dry_run, force)?;
            }
        }
        Commands::Gep { action } => match action {
            GepAction::Evolve => cmd::community_cmd::cmd_gep_evolve()?,
            GepAction::Status => cmd::community_cmd::cmd_gep_status()?,
        },
        Commands::Emerge { threshold, dry_run } => cmd::learn::cmd_emerge(threshold, dry_run)?,
        Commands::Suggest { create } => cmd::workflow::cmd_suggest(create)?,
        Commands::Context {
            quiet,
            compact,
            query,
            file,
            budget,
            source,
            json,
            scope,
        } => {
            cmd::context::cmd_context(query, compact, file, budget, source, json, scope, quiet)
                .await?
        }
        Commands::Session { action } => match action {
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
            SessionAction::Show { id, last, json } => {
                cmd::session::cmd_session_show(&id, last, json)?
            }
            SessionAction::Export {
                id,
                format,
                analyze,
                output,
            } => cmd::session::cmd_session_export(&id, &format, analyze, output).await?,
            SessionAction::Push { id, all } => {
                cmd::session::cmd_session_push(id.as_deref(), all).await?
            }
            SessionAction::Reflect { dry_run } => {
                cmd::session::cmd_session_reflect(dry_run).await?
            }
        },
        Commands::Dashboard => {
            dashboard::render_dashboard()?;
        }
        Commands::Community { action } => match action {
            CommunityAction::Publish { name } => {
                cmd::community_cmd::cmd_community_publish(&name).await?
            }
            CommunityAction::Fetch { id } => cmd::community_cmd::cmd_community_fetch(&id).await?,
            CommunityAction::Search { query } => {
                cmd::community_cmd::cmd_community_search(&query).await?
            }
            CommunityAction::List { sort } => cmd::community_cmd::cmd_community_list(&sort).await?,
            CommunityAction::Star { id } => cmd::community_cmd::cmd_community_star(&id).await?,
            CommunityAction::Report {
                name,
                effectiveness,
                sessions,
            } => cmd::community_cmd::cmd_community_report(&name, effectiveness, sessions).await?,
            CommunityAction::Packs => cmd::community_cmd::cmd_community_packs().await?,
            CommunityAction::Pack { action } => match action {
                PackAction::Install { id } => {
                    cmd::community_cmd::cmd_community_pack_install(&id).await?
                }
                PackAction::Show { id } => cmd::community_cmd::cmd_community_pack_show(&id).await?,
            },
        },
        Commands::Team { action } => match action {
            TeamAction::List { team } => cmd::community_cmd::cmd_team_list(&team).await?,
            TeamAction::Share { name, team } => {
                cmd::community_cmd::cmd_team_share(&name, &team).await?
            }
            TeamAction::Sync { team } => cmd::community_cmd::cmd_team_sync(&team).await?,
        },
        Commands::Login => cmd::misc::cmd_login().await?,
        Commands::Logout => cmd::misc::cmd_logout()?,
        Commands::Init {
            hooks,
            refresh_discovery,
        } => cmd::init::cmd_init(hooks, refresh_discovery)?,
        Commands::Serve {
            port,
            open,
            readonly,
        } => cmd::server_cmd::cmd_serve(port, open, readonly).await?,
        Commands::Why { name } => cmd::inject_cmd::cmd_why(&name)?,
        Commands::Edit { name, quick } => cmd::pattern::cmd_edit(&name, quick)?,
        Commands::Model(args) => cmd::model::run(args)?,
        Commands::Agent { action } => run_agent(action).await?,
        Commands::Exchange { action } => match action {
            ExchangeAction::Import { file } => cmd::misc::cmd_exchange_import(&file)?,
            ExchangeAction::ImportAll => cmd::misc::cmd_exchange_import_all()?,
            ExchangeAction::Export { name, dir } => cmd::misc::cmd_exchange_export(&name, dir)?,
        },
        Commands::Verify { file, all } => {
            // Initialize known commands from the clap tree so verify doesn't
            // need a hardcoded list.
            let clap_cmd = Cli::command();
            let known = verify::collect_commands_from_clap(&clap_cmd);
            verify::set_known_commands(known);
            cmd::verify::cmd_verify(file.as_deref(), all)?
        }
        Commands::Import { file, dry_run } => cmd::misc::cmd_import(file, dry_run)?,
        Commands::In { source } => cmd::session::cmd_in(&source).await?,
        Commands::Out { action, force } => cmd::session::cmd_out(action.as_deref(), force).await?,
        Commands::Push { dry_run } => {
            let config = crate::store::config::load_config()?;
            cmd::sync_cmd::run_push(&config.server.url, dry_run).await?;
        }
        Commands::Fetch { dry_run } => {
            let config = crate::store::config::load_config()?;
            cmd::sync_cmd::run_fetch(&config.server.url, dry_run).await?;
        }
        Commands::Drafts { action } => match action {
            DraftsAction::List { since } => cmd::drafts::cmd_drafts_list(since).await?,
            DraftsAction::Show { id } => cmd::drafts::cmd_drafts_show(&id).await?,
            DraftsAction::Accept { id, as_tier } => {
                cmd::drafts::cmd_drafts_accept(&id, as_tier.as_deref()).await?
            }
            DraftsAction::Reject { id, reason } => {
                cmd::drafts::cmd_drafts_reject(&id, reason.as_deref()).await?
            }
        },
        Commands::Exit | Commands::Quit => cmd::session::cmd_session_exit()?,
        Commands::Chat { action } => match action {
            ChatAction::List { since, src } => cmd::conversations_cmd::cmd_chat_list(since, src)?,
            ChatAction::Show { date } => cmd::conversations_cmd::cmd_chat_show(date)?,
            ChatAction::Raw { date, conv } => cmd::conversations_cmd::cmd_chat_raw(date, conv)?,
            ChatAction::Search { query, limit, src } => {
                cmd::conversations_cmd::cmd_chat_search(query, limit, src).await?
            }
        },
        Commands::Conversations { action } => match action {
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
        },
        Commands::Deploy { action } => match action {
            DeployAction::Up {
                build,
                detach,
                file,
            } => cmd::deploy::cmd_deploy_up(file.as_deref(), build, detach)?,
            DeployAction::Down { volumes, file } => {
                cmd::deploy::cmd_deploy_down(file.as_deref(), volumes)?
            }
            DeployAction::Status { file } => cmd::deploy::cmd_deploy_status(file.as_deref())?,
            DeployAction::Logs {
                service,
                follow,
                file,
            } => cmd::deploy::cmd_deploy_logs(file.as_deref(), service.as_deref(), follow)?,
            DeployAction::Build { file } => cmd::deploy::cmd_deploy_build(file.as_deref())?,
        },
        Commands::Ask {
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
        #[cfg(feature = "sources")]
        Commands::Source { cmd } => crate::cmd::source_cmd::handle(cmd).await?,
        Commands::Internals { action } => match action {
            InternalsAction::RebuildIndex { layer } => cmd::internals::cmd_rebuild_index(&layer)?,
            InternalsAction::Git { layer, args } => {
                cmd::internals::cmd_internals_git(&layer, &args)?
            }
        },
        Commands::Eval { action } => match action {
            EvalAction::Run { suite, format } => {
                let code = cmd::eval::cmd_eval_run(&suite, &format)?;
                std::process::exit(code);
            }
        },
        Commands::Sleep { action } => match action {
            SleepAction::Enable => cmd::sleep::cmd_sleep_enable()?,
            SleepAction::Disable => cmd::sleep::cmd_sleep_disable()?,
            SleepAction::Status => cmd::sleep::cmd_sleep_status()?,
        },
        Commands::Project { action } => match action {
            ProjectAction::Index {
                path,
                rebuild,
                quiet,
            } => cmd::project::cmd_project_index(path, rebuild, quiet).await?,
            ProjectAction::Search {
                query,
                project,
                limit,
                json,
            } => cmd::project::cmd_project_search(query, project, limit, json).await?,
            ProjectAction::Status { path } => cmd::project::cmd_project_status(path).await?,
            ProjectAction::List => cmd::project::cmd_project_list()?,
        },
    }

    Ok(())
}

async fn run_agent(action: AgentAction) -> Result<()> {
    match action {
        AgentAction::Create {
            name,
            no_interactive,
            display_name,
            model,
            provider,
        } => cmd::agent::cmd_create(&name, no_interactive, display_name, model, provider)?,
        AgentAction::List { json } => cmd::agent::cmd_list(json)?,
        AgentAction::Status { name } => cmd::agent::cmd_status(&name)?,
        AgentAction::Stop { name } => cmd::agent::cmd_stop(&name)?,
        AgentAction::Remove { name, purge, force } => cmd::agent::cmd_remove(&name, purge, force)?,
        AgentAction::Rename { old, new } => cmd::agent::cmd_rename(&old, &new)?,
        AgentAction::Send { name, message } => cmd::agent::cmd_send(&name, &message)?,
        AgentAction::Card { name } => cmd::agent::cmd_card(&name)?,
        AgentAction::Rekey {
            name,
            reason,
            yes,
            emergency,
        } => cmd::agent_rekey::cmd_rekey(&name, &reason, yes, emergency)?,
        AgentAction::RekeyStatus { name, json } => cmd::agent_rekey::cmd_rekey_status(&name, json)?,
        AgentAction::InstallService { name, dry_run } => {
            cmd::agent::cmd_install_service(&name, dry_run)?
        }
        AgentAction::Prompt { action } => match action {
            AgentPromptAction::Show { name } => cmd::agent::cmd_prompt_show(&name)?,
            AgentPromptAction::Edit { name } => cmd::agent::cmd_prompt_edit(&name)?,
            AgentPromptAction::Set {
                name,
                content,
                file,
            } => cmd::agent::cmd_prompt_set(&name, content.as_deref(), file.as_deref())?,
        },
        AgentAction::Mcp { action } => match action {
            AgentMcpAction::List { name } => cmd::agent::cmd_mcp_list(&name)?,
            AgentMcpAction::Add {
                name,
                server_id,
                command,
                args,
                force,
                publisher_name,
                publisher_homepage,
                publisher_registry_id,
            } => cmd::agent::cmd_mcp_add(
                &name,
                &server_id,
                &command,
                &args,
                cmd::agent::McpAddPin {
                    force,
                    publisher_name,
                    publisher_homepage,
                    publisher_registry_id,
                },
            )?,
            AgentMcpAction::Remove { name, server_id } => {
                cmd::agent::cmd_mcp_remove(&name, &server_id)?
            }
            AgentMcpAction::Rename { name, old, new } => {
                cmd::agent::cmd_mcp_rename(&name, &old, &new)?
            }
            AgentMcpAction::Inspect {
                name,
                server,
                probe,
            } => {
                let code = cmd::agent_mcp_pin::cmd_mcp_inspect(&name, server.as_deref(), probe)?;
                if code != 0 {
                    std::process::exit(code);
                }
            }
            AgentMcpAction::Pin {
                name,
                server_id,
                force,
                no_probe,
                publisher_name,
                publisher_homepage,
                publisher_registry_id,
            } => cmd::agent_mcp_pin::cmd_mcp_pin(
                &name,
                &server_id,
                force,
                no_probe,
                publisher_name,
                publisher_homepage,
                publisher_registry_id,
            )?,
        },
        AgentAction::Skill { action } => match action {
            AgentSkillAction::List { name } => cmd::agent::cmd_skill_list(&name)?,
            AgentSkillAction::Add { name, source } => cmd::agent::cmd_skill_add(&name, &source)?,
            AgentSkillAction::Remove { name, skill_id } => {
                cmd::agent::cmd_skill_remove(&name, &skill_id)?
            }
            AgentSkillAction::Show { name, skill_id } => {
                cmd::agent::cmd_skill_show(&name, &skill_id)?
            }
        },
        AgentAction::Perm { action } => match action {
            AgentPermAction::Show { name, section } => {
                cmd::agent::cmd_perm_show(&name, section.as_deref())?
            }
            AgentPermAction::SetMode { name, key, value } => {
                cmd::agent::cmd_perm_set_mode(&name, &key, &value)?
            }
            AgentPermAction::AllowHost { name, glob } => {
                cmd::agent::cmd_perm_allow_host(&name, &glob)?
            }
            AgentPermAction::DenyHost { name, glob } => {
                cmd::agent::cmd_perm_deny_host(&name, &glob)?
            }
            AgentPermAction::ListHosts { name } => cmd::agent::cmd_perm_list_hosts(&name)?,
            AgentPermAction::AllowRead { name, path } => {
                cmd::agent::cmd_perm_allow_read(&name, &path)?
            }
            AgentPermAction::AllowWrite { name, path } => {
                cmd::agent::cmd_perm_allow_write(&name, &path)?
            }
            AgentPermAction::DenyPath { name, path } => {
                cmd::agent::cmd_perm_deny_path(&name, &path)?
            }
            AgentPermAction::AllowSpawn { name, binary } => {
                cmd::agent::cmd_perm_allow_spawn(&name, &binary)?
            }
            AgentPermAction::DenySpawn { name, binary } => {
                cmd::agent::cmd_perm_deny_spawn(&name, &binary)?
            }
            AgentPermAction::SetLimit { name, key, value } => {
                cmd::agent::cmd_perm_set_limit(&name, &key, value)?
            }
        },
        AgentAction::Export {
            name,
            out,
            format,
            theme,
            icon,
            clone_identity,
            skip_notarize,
        } => {
            if format == "gui" {
                use std::path::PathBuf;
                let mur_home = mur_core::paths::mur_root(None);
                let agent_home = mur_home.join("agents").join(&name);
                if !agent_home.exists() {
                    anyhow::bail!("agent '{name}' not found at {}", agent_home.display());
                }
                let opts = cmd::agent_export_gui::ExportGuiOptions {
                    agent_name: name.clone(),
                    agent_home,
                    out: PathBuf::from(&out),
                    theme,
                    icon: icon.map(PathBuf::from),
                    clone_identity,
                    skip_notarize,
                };
                cmd::agent_export_gui::run(opts)?;
            } else {
                cmd::agent::cmd_export(&name, &out, &format)?;
            }
        }
        AgentAction::Install { path } => cmd::agent::cmd_install(std::path::Path::new(&path))?,
        AgentAction::Uninstall { name, purge } => cmd::agent::cmd_uninstall(&name, purge)?,
        AgentAction::Inspect { path } => cmd::agent::cmd_inspect(std::path::Path::new(&path))?,
        AgentAction::Stats { name } => cmd::agent::cmd_stats(&name)?,
        AgentAction::Logs { name, tail } => cmd::agent::cmd_logs(&name, tail)?,
        AgentAction::Companion(args) => cmd::agent_companion::run(args).await?,
        AgentAction::Doctor { format, json } => cmd::doctor::run(&format, json)?,
        AgentAction::Secret { agent, action } => match action {
            AgentSecretAction::Set { key, value } => {
                cmd::agent::cmd_secret_set(&agent, &key, value.as_deref()).await?
            }
            AgentSecretAction::List => cmd::agent::cmd_secret_list(&agent).await?,
            AgentSecretAction::Delete { key } => {
                cmd::agent::cmd_secret_delete(&agent, &key).await?
            }
        },
        AgentAction::Eval { action } => match action {
            AgentEvalAction::Report { jsonl, out } => {
                let code = cmd::agent_eval::cmd_eval_report(&jsonl, out.as_deref())?;
                if code != 0 {
                    std::process::exit(code);
                }
            }
        },
        AgentAction::Webhook { agent, action } => match action {
            AgentWebhookAction::Enable { bind, port } => {
                cmd::agent_webhook::cmd_webhook_enable(&agent, bind, port)?
            }
            AgentWebhookAction::Disable => cmd::agent_webhook::cmd_webhook_disable(&agent)?,
            AgentWebhookAction::Show => cmd::agent_webhook::cmd_webhook_show(&agent)?,
            AgentWebhookAction::SecretSet { value } => {
                cmd::agent_webhook::cmd_webhook_secret_set(&agent, value.as_deref()).await?
            }
        },
        AgentAction::Voice { name, action } => match action {
            VoiceAction::Enable { voice_id } => {
                cmd::agent_voice::cmd_voice_enable(&name, voice_id.as_deref())?
            }
            VoiceAction::Disable => cmd::agent_voice::cmd_voice_disable(&name)?,
            VoiceAction::Download => {
                anyhow::bail!("voice download not yet implemented; will ship in D1 Task 3");
            }
        },
        AgentAction::Schedule { action } => match action {
            AgentScheduleAction::Add {
                name,
                cron,
                message,
                sends_to,
            } => cmd::agent_schedule::cmd_schedule_add(&name, &cron, &message, sends_to)?,
            AgentScheduleAction::List { name } => cmd::agent_schedule::cmd_schedule_list(&name)?,
            AgentScheduleAction::Remove { name, index } => {
                cmd::agent_schedule::cmd_schedule_remove(&name, index)?
            }
            AgentScheduleAction::Next { name, count } => {
                cmd::agent_schedule::cmd_schedule_next(&name, count)?
            }
            AgentScheduleAction::IdleAdd {
                name,
                after_secs,
                message,
                sends_to,
                cooldown_secs,
                respect_quiet_hours,
            } => cmd::agent_schedule::cmd_idle_add(
                &name,
                after_secs,
                &message,
                sends_to,
                cooldown_secs,
                respect_quiet_hours,
            )?,
            AgentScheduleAction::IdleList { name } => cmd::agent_schedule::cmd_idle_list(&name)?,
            AgentScheduleAction::IdleRemove { name, index } => {
                cmd::agent_schedule::cmd_idle_remove(&name, index)?
            }
        },
        AgentAction::Hooks { action } => match action {
            AgentHooksAction::Show { name, json } => cmd::agent_hooks::cmd_hooks_show(&name, json)?,
        },
        AgentAction::MigrateToHub => cmd::agent::cmd_migrate_to_hub()?,
        AgentAction::History { name } => cmd::agent_history::cmd_agent_history(&name)?,
        AgentAction::Rollback { name, to } => cmd::agent_history::cmd_agent_rollback(&name, to)?,
        AgentAction::Snapshot { action } => match action {
            crate::cli::agent::SnapshotAction::Pull { name, dry_run } => {
                cmd::agent::cmd_snapshot_pull(&name, dry_run)?
            }
            crate::cli::agent::SnapshotAction::Show { name } => {
                cmd::agent::cmd_snapshot_show(&name)?
            }
        },
        AgentAction::Reconnect { name } => cmd::agent::cmd_agent_reconnect(&name)?,
        AgentAction::Apply { file } => cmd::agent::cmd_agent_apply(&file)?,
    }
    Ok(())
}
