//! Command dispatch — `Cli` → `cmd::*` handler. Extracted from `main.rs`'s
//! `async_main` body. One arm per top-level `Commands` variant; almost every
//! arm is a thin delegate into `cmd::*`. Keep new branches small — heavy
//! logic belongs in `cmd::<feature>`.

use anyhow::{Context, Result};
use clap::CommandFactory;

use crate::cli::{
    AgentAction, AgentEvalAction, AgentHooksAction, AgentMcpAction, AgentPendingAction,
    AgentPermAction, AgentPromptAction, AgentQueueAction, AgentScheduleAction, AgentSecretAction,
    AgentSkillAction, AgentTrashAction, AgentWebhookAction, AuthAction, ChatAction, Cli, Commands,
    ConversationsAction, DaemonAction, DeployAction, DraftsAction, EvalAction, ExchangeAction,
    HookEvent, InternalsAction, MurmurdAction, ProjectAction, ScheduleAction, SessionAction,
    SleepAction, SyncAction, TeamAction, VoiceAction, WorkflowAction,
};
use crate::store::config as store_config;
use crate::{cmd, dashboard, team, verify};

/// Resolve an optional --team arg, falling back to config's default team.
fn resolve_team_arg(arg: Option<String>) -> Result<String> {
    if let Some(t) = arg {
        return Ok(t);
    }
    let cfg = store_config::load_config()?;
    cfg.sync.team_id.ok_or_else(|| {
        anyhow::anyhow!(
            "No team specified. Pass --team <slug> or run `mur team use <slug>` to set a default."
        )
    })
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        // Deprecated: use `mur notes search`
        Commands::Search {
            query,
            source: _,
            result_type: _,
            only_sources: _,
            only_patterns: _,
            limit,
            json: _,
        } => {
            eprintln!("# mur search: use `mur notes search`");
            cmd::notes_cmd::cmd_search(&query, limit)?
        }
        Commands::Stats => cmd::misc::cmd_stats()?,
        Commands::Doctor => cmd::misc::cmd_doctor()?,

        Commands::Sync {
            quiet,
            project,
            team,
            action,
        } => {
            if let Some(action) = action {
                match action {
                    SyncAction::Status => cmd::sync_cmd::run_status()?,
                    SyncAction::Fleet {
                        direction,
                        force_local,
                    } => {
                        let direction = direction.unwrap_or(crate::cli::FleetSyncDir::Both);
                        let device_sync_dir = match direction {
                            crate::cli::FleetSyncDir::Pull => {
                                cmd::sync_cmd::DeviceSyncDirection::Pull
                            }
                            crate::cli::FleetSyncDir::Push => {
                                cmd::sync_cmd::DeviceSyncDirection::Push
                            }
                            crate::cli::FleetSyncDir::Both => {
                                cmd::sync_cmd::DeviceSyncDirection::Both
                            }
                        };
                        cmd::fleet_sync::fleet_sync_cmd(device_sync_dir, force_local).await?
                    }
                }
            } else {
                cmd::sync_cmd::cmd_sync(quiet, project, team.as_deref()).await?;
            }
        }
        // Deprecated: use `mur hook inject`
        Commands::Inject { query, project: _ } => {
            eprintln!("# mur inject: use `mur hook inject`");
            cmd::inject_cmd::cmd_inject(&query).await?
        }
        Commands::Hook { event } => match event {
            HookEvent::Prompt { tool } => cmd::hook::cmd_hook_prompt(&tool).await?,
            HookEvent::Tool { tool } => cmd::hook::cmd_hook_tool(&tool).await?,
            HookEvent::Stop { tool } => cmd::hook::cmd_hook_stop(&tool).await?,
            HookEvent::SessionStart { tool } => cmd::hook::cmd_hook_session_start(&tool).await?,
            HookEvent::Stats => cmd::hook::cmd_hook_stats()?,
            HookEvent::Inject { query } => cmd::inject_cmd::cmd_inject(&query).await?,
            HookEvent::Context {
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
        },
        // Deprecated: use `mur daemon`
        Commands::Murmurd { action } => {
            eprintln!("# mur murmurd: use `mur daemon`");
            match action {
                MurmurdAction::Start { detach } => cmd::murmurd::cmd_murmurd_start(detach)?,
                MurmurdAction::Stop => cmd::murmurd::cmd_murmurd_stop()?,
                MurmurdAction::Status => cmd::murmurd::cmd_murmurd_status()?,
            }
        }
        // Deprecated: use `mur workflow run`
        Commands::Run {
            query,
            fail_fast,
            prompt,
        } => {
            eprintln!("# mur run: use `mur workflow run`");
            cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt).await?
        }
        Commands::Workflow { action } => match action {
            WorkflowAction::Run {
                query,
                fail_fast,
                prompt,
            } => cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt).await?,
            WorkflowAction::Suggest {
                create,
                accept,
                dismiss,
            } => cmd::workflow::cmd_suggest(create, accept.as_deref(), dismiss.as_deref())?,
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
        // Deprecated: use `mur internals reindex`
        Commands::Reindex { bootstrap } => {
            eprintln!("# mur reindex: use `mur internals reindex`");
            if bootstrap {
                cmd::reindex::cmd_reindex_bootstrap()?;
            } else {
                cmd::reindex::cmd_reindex().await?;
            }
        }
        Commands::Update { check } => cmd::update::cmd_update(check)?,
        // Deprecated: use `mur workflow suggest`
        Commands::Suggest {
            create,
            accept,
            dismiss,
        } => {
            eprintln!("# mur suggest: use `mur workflow suggest`");
            cmd::workflow::cmd_suggest(create, accept.as_deref(), dismiss.as_deref())?
        }
        // Deprecated: use `mur hook context`
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
            eprintln!("# mur context: use `mur hook context`");
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
        },
        Commands::Dashboard => {
            dashboard::render_dashboard()?;
        }
        Commands::Team { action } => match action {
            TeamAction::List { team } => match team {
                Some(t) => {
                    let client = reqwest::Client::new();
                    let team_id = team::resolve_team_id(&client, &t).await?;
                    cmd::team_cmd::cmd_team_list(&team_id).await?
                }
                None => cmd::team_cmd::cmd_team_list_mine().await?,
            },
            TeamAction::Use { team } => cmd::team_cmd::cmd_team_use(&team).await?,
            TeamAction::Share { name, team } => {
                let slug = resolve_team_arg(team)?;
                let client = reqwest::Client::new();
                let team_id = team::resolve_team_id(&client, &slug).await?;
                cmd::team_cmd::cmd_team_share(&name, &team_id).await?
            }
            TeamAction::Sync { team } => {
                let slug = resolve_team_arg(team)?;
                let client = reqwest::Client::new();
                let team_id = team::resolve_team_id(&client, &slug).await?;
                cmd::team_cmd::cmd_team_sync(&team_id).await?
            }
        },
        Commands::Init {
            hooks,
            refresh_discovery,
        } => cmd::init::cmd_init(hooks, refresh_discovery)?,
        // Deprecated: use `mur daemon serve`
        Commands::Serve {
            port,
            open,
            readonly,
        } => {
            eprintln!("# mur serve: use `mur daemon serve`");
            cmd::server_cmd::cmd_serve(port, open, readonly).await?
        }
        Commands::Model(args) => cmd::model::run(args)?,
        Commands::Agent { action } => run_agent(action).await?,
        Commands::Skill { action } => match action {
            crate::cli::SkillAction::Validate {
                path,
                warnings_only,
            } => cmd::skill_cmd::cmd_validate(&path, warnings_only)?,
            crate::cli::SkillAction::Fmt { path, to, write } => {
                cmd::skill_cmd::cmd_fmt(&path, to.as_deref(), write)?
            }
            crate::cli::SkillAction::List => cmd::skill_cmd::cmd_list()?,
            crate::cli::SkillAction::Show { name } => cmd::skill_cmd::cmd_show(&name)?,
            crate::cli::SkillAction::Remove { name } => cmd::skill_cmd::cmd_remove(&name)?,
            crate::cli::SkillAction::Search { query, local } => {
                cmd::skill_cmd::cmd_search(&query, local)?
            }
            crate::cli::SkillAction::Info {
                name,
                full,
                metrics,
            } => cmd::skill_cmd::cmd_info(&name, full, metrics)?,
            crate::cli::SkillAction::Audit { name } => cmd::skill_cmd::cmd_audit(&name)?,
            crate::cli::SkillAction::Trust { name, level } => {
                cmd::skill_cmd::cmd_trust(&name, &level)?
            }
            crate::cli::SkillAction::Install { source } => {
                cmd::skill_install::cmd_install_cli(&source)?
            }
            crate::cli::SkillAction::Publish { path } => cmd::skill_publish::cmd_publish(&path)?,
            crate::cli::SkillAction::Update { name } => cmd::skill_install::cmd_update_cli(&name)?,
            crate::cli::SkillAction::Deps { name } => cmd::skill_deps::cmd_deps_cli(&name)?,
            crate::cli::SkillAction::Generate {
                from_session,
                name,
                model,
                dry_run,
                parallel,
            } => {
                cmd::skill_generate::cmd_generate_cli(cmd::skill_generate::GenerateOptions {
                    session_id: from_session,
                    name,
                    model_override: model,
                    dry_run,
                    max_parallel: parallel,
                })
                .await?
            }
            crate::cli::SkillAction::Suggest {
                max_sessions,
                threshold,
            } => {
                let home = cmd::agent::resolve_mur_home()?;
                cmd::skill_suggest::cmd_suggest(
                    &home,
                    cmd::skill_suggest::SuggestOptions {
                        max_sessions,
                        threshold,
                    },
                )?
            }
            crate::cli::SkillAction::Evolve {
                name,
                dry_run,
                max_iterations,
            } => {
                let home = cmd::agent::resolve_mur_home()?;
                cmd::skill_evolve::cmd_evolve(
                    &home,
                    cmd::skill_evolve::EvolveOptions {
                        skill_name: name,
                        dry_run,
                        max_iterations,
                    },
                )
                .await?
            }
            crate::cli::SkillAction::Stats {
                name,
                all_agents,
                json,
            } => {
                let home = cmd::agent::resolve_mur_home()?;
                if all_agents {
                    let rows = crate::cross_agent::stats_agg::aggregate_skill_stats(&home, &name)?;
                    if json {
                        serde_json::to_writer_pretty(std::io::stdout(), &rows)?;
                        println!();
                    } else if rows.is_empty() {
                        println!("No stats found for '{}' on any agent.", name);
                    } else {
                        println!(
                            "{:<24} {:>8} {:>8} {:>8}  {:<10}  LAST USED",
                            "AGENT", "USES", "OK", "FAIL", "LIFECYCLE",
                        );
                        for r in &rows {
                            println!(
                                "{:<24} {:>8} {:>8} {:>8}  {:<10}  {}",
                                r.agent,
                                r.usage_count,
                                r.success_count,
                                r.failure_count,
                                r.lifecycle,
                                r.last_used_at
                                    .map(|d| d.to_rfc3339())
                                    .unwrap_or_else(|| "-".into()),
                            );
                        }
                        let total_uses: u64 = rows.iter().map(|r| r.usage_count).sum();
                        let total_ok: u64 = rows.iter().map(|r| r.success_count).sum();
                        let success_rate = if total_uses > 0 {
                            total_ok as f64 / total_uses as f64
                        } else {
                            0.0
                        };
                        println!(
                            "\nPopulation: {} agents, {} uses, {:.1}% success",
                            rows.len(),
                            total_uses,
                            success_rate * 100.0,
                        );
                    }
                } else {
                    cmd::skill_stats::cmd_stats(&name)?;
                }
            }
            crate::cli::SkillAction::Pin { name, reason } => {
                cmd::skill_stats::cmd_pin(&name, reason.as_deref())?
            }
            crate::cli::SkillAction::Unpin { name } => cmd::skill_stats::cmd_unpin(&name)?,
            crate::cli::SkillAction::ReindexStats { name, days_back } => {
                cmd::skill_stats::cmd_reindex_stats(name.as_deref(), days_back).await?
            }
            crate::cli::SkillAction::Doctor {
                names,
                check,
                json,
                strict,
                fix,
                apply,
                llm,
                llm_status,
            } => cmd::skill_doctor::cmd_doctor(
                &names, &check, json, strict, fix, apply, llm, llm_status,
            )?,
            crate::cli::SkillAction::Sweep { name, dry_run } => {
                cmd::skill_sweep::cmd_sweep(name.as_deref(), dry_run)?
            }
            crate::cli::SkillAction::Curate { name } => cmd::skill_curate::cmd_curate(&name)?,
            crate::cli::SkillAction::ReindexVec { name, prune } => {
                let home = cmd::agent::resolve_mur_home()?;
                cmd::skill_reindex_vec::cmd_reindex_vec(&home, name.as_deref(), prune).await?
            }
            crate::cli::SkillAction::Archive { name, reason } => {
                cmd::skill_archive::cmd_archive(&name, reason.as_deref())?
            }
            crate::cli::SkillAction::Consolidate {
                dry_run,
                apply,
                method,
                llm_adjudicate,
                cross_agent,
            } => {
                let home = cmd::agent::resolve_mur_home()?;
                if cross_agent {
                    let cross_method = match method {
                        crate::cli::skill::Method::Jaccard => {
                            crate::cross_agent::consolidate::CrossAgentMethod::Jaccard
                        }
                        crate::cli::skill::Method::Vector => {
                            crate::cross_agent::consolidate::CrossAgentMethod::Vector
                        }
                        crate::cli::skill::Method::Both => {
                            crate::cross_agent::consolidate::CrossAgentMethod::Both
                        }
                    };
                    let report = match &cross_method {
                        crate::cross_agent::consolidate::CrossAgentMethod::Jaccard => {
                            crate::cross_agent::consolidate::run_consolidate_cross_agent(
                                &home,
                                apply && !dry_run,
                            )?
                        }
                        _ => {
                            let cfg = mur_common::config::Config::load_or_default(
                                &home.join("config.yaml"),
                            );
                            let embed_config =
                                crate::store::embedding::EmbeddingConfig::from_config(&cfg);
                            let index_dir = home.join("lance");
                            let store =
                                crate::store::vector::factory::get_vector_store(&cfg, &index_dir)
                                    .await
                                    .context("opening vector store")?;
                            crate::cross_agent::consolidate::run_consolidate_cross_agent_with_method(
                                &home,
                                apply && !dry_run,
                                cross_method,
                                &embed_config,
                                &*store,
                            )
                            .await?
                        }
                    };
                    let mode = if apply && !dry_run {
                        "Applied"
                    } else {
                        "Dry-run"
                    };
                    println!(
                        "Cross-agent consolidation report ({mode}): {} duplicate(s)",
                        report.duplicates.len(),
                    );
                    for d in &report.duplicates {
                        println!(
                            "  Duplicate: {}:{} ≈ {}:{} (sim={:.3}, src={}, keeper={}:{})",
                            d.a_agent,
                            d.a_skill,
                            d.b_agent,
                            d.b_skill,
                            d.similarity,
                            serde_json::to_string(&d.similarity_source).unwrap_or_default(),
                            d.keeper_agent,
                            d.keeper_skill,
                        );
                    }
                } else {
                    let method = match method {
                        crate::cli::skill::Method::Jaccard => {
                            crate::skill_consolidate::ConsolidateMethod::Jaccard
                        }
                        crate::cli::skill::Method::Vector => {
                            crate::skill_consolidate::ConsolidateMethod::Vector
                        }
                        crate::cli::skill::Method::Both => {
                            crate::skill_consolidate::ConsolidateMethod::Both
                        }
                    };
                    cmd::skill_consolidate::cmd_consolidate(
                        &home,
                        dry_run,
                        apply,
                        method,
                        llm_adjudicate,
                    )
                    .await?
                }
            }
            crate::cli::SkillAction::Recombine {
                a,
                b,
                strategy,
                name,
                dry_run,
                agent,
                json,
            } => {
                use crate::cross_agent::recombine::RecombineStrategy;
                let home = cmd::agent::resolve_mur_home()?;
                let strategy = match strategy {
                    crate::cli::skill::RecombineStrategyArg::Union => RecombineStrategy::Union,
                    crate::cli::skill::RecombineStrategyArg::Intersection => {
                        RecombineStrategy::Intersection
                    }
                    crate::cli::skill::RecombineStrategyArg::Llm => RecombineStrategy::Llm,
                };
                let code = cmd::skill_recombine::cmd_recombine(
                    &home, &a, &b, strategy, name, dry_run, agent, json,
                )
                .await;
                if code != 0 {
                    std::process::exit(code);
                }
            }
            crate::cli::SkillAction::Credit { name, agent, json } => {
                let home = cmd::agent::resolve_mur_home()?;
                let agent_name = agent.unwrap_or_else(|| {
                    cmd::skill_install::caller_agent_name(&home)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "(global)".into())
                });
                cmd::skill_credit::cmd_credit(&home, &agent_name, &name, json)?
            }
            crate::cli::SkillAction::Exchange { action } => match action {
                ExchangeAction::Import { file } => cmd::misc::cmd_exchange_import(&file)?,
                ExchangeAction::ImportAll => cmd::misc::cmd_exchange_import_all()?,
                ExchangeAction::Export { name, dir } => cmd::misc::cmd_exchange_export(&name, dir)?,
            },
            crate::cli::SkillAction::Drafts { action } => match action {
                DraftsAction::List { since } => cmd::drafts::cmd_drafts_list(since).await?,
                DraftsAction::Show { id } => cmd::drafts::cmd_drafts_show(&id).await?,
                DraftsAction::Accept { id, as_tier } => {
                    cmd::drafts::cmd_drafts_accept(&id, as_tier.as_deref()).await?
                }
                DraftsAction::Reject { id, reason } => {
                    cmd::drafts::cmd_drafts_reject(&id, reason.as_deref()).await?
                }
            },
            crate::cli::SkillAction::Eval { action } => match action {
                EvalAction::Run { suite, format } => {
                    let code = cmd::eval::cmd_eval_run(&suite, &format)?;
                    std::process::exit(code);
                }
            },
            crate::cli::SkillAction::Intent(action) => {
                let home = cmd::agent::resolve_mur_home()?;
                let agent_name = cmd::skill_install::caller_agent_name(&home)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "(global)".into());
                match action {
                    crate::cli::IntentAction::Canonicalise { dry_run, json } => {
                        cmd::skill_intent::cmd_intent_canonicalise(
                            &home,
                            &agent_name,
                            dry_run,
                            json,
                        )?
                    }
                    crate::cli::IntentAction::Show { json } => {
                        cmd::skill_intent::cmd_intent_show(&home, json)?
                    }
                }
            }
        },
        Commands::Notes { action } => match action {
            crate::cli::notes::NotesAction::Create {
                name,
                description,
                body_file,
            } => cmd::notes_cmd::cmd_create(&name, &description, body_file.as_deref())?,
            crate::cli::notes::NotesAction::Search { query, limit } => {
                cmd::notes_cmd::cmd_search(&query, limit)?
            }
            crate::cli::notes::NotesAction::List { maturity, limit } => {
                cmd::notes_cmd::cmd_list(maturity.as_deref(), limit)?
            }
            crate::cli::notes::NotesAction::Show { name } => cmd::notes_cmd::cmd_show(&name)?,
        },
        // Deprecated: use `mur skill exchange`
        Commands::Exchange { action } => {
            eprintln!("# mur exchange: use `mur skill exchange`");
            match action {
                ExchangeAction::Import { file } => cmd::misc::cmd_exchange_import(&file)?,
                ExchangeAction::ImportAll => cmd::misc::cmd_exchange_import_all()?,
                ExchangeAction::Export { name, dir } => cmd::misc::cmd_exchange_export(&name, dir)?,
            }
        }
        Commands::Verify { file, all } => {
            // Initialize known commands from the clap tree so verify doesn't
            // need a hardcoded list.
            let clap_cmd = Cli::command();
            let known = verify::collect_commands_from_clap(&clap_cmd);
            verify::set_known_commands(known);
            cmd::verify::cmd_verify(file.as_deref(), all)?
        }
        // Deprecated: use `mur session in`
        Commands::In { source } => {
            eprintln!("# mur in: use `mur session in`");
            cmd::session::cmd_in(&source).await?
        }
        // Deprecated: use `mur session out`
        Commands::Out { action, force } => {
            eprintln!("# mur out: use `mur session out`");
            cmd::session::cmd_out(action.as_deref(), force).await?
        }
        Commands::Push { dry_run } => {
            let config = crate::store::config::load_config()?;
            cmd::sync_cmd::run_push(&config.server.url, dry_run).await?;
        }
        Commands::Fetch { dry_run } => {
            let config = crate::store::config::load_config()?;
            cmd::sync_cmd::run_fetch(&config.server.url, dry_run).await?;
        }
        // Deprecated: use `mur skill drafts`
        Commands::Drafts { action } => {
            eprintln!("# mur drafts: use `mur skill drafts`");
            match action {
                DraftsAction::List { since } => cmd::drafts::cmd_drafts_list(since).await?,
                DraftsAction::Show { id } => cmd::drafts::cmd_drafts_show(&id).await?,
                DraftsAction::Accept { id, as_tier } => {
                    cmd::drafts::cmd_drafts_accept(&id, as_tier.as_deref()).await?
                }
                DraftsAction::Reject { id, reason } => {
                    cmd::drafts::cmd_drafts_reject(&id, reason.as_deref()).await?
                }
            }
        }
        // Deprecated: use `mur session discard`
        Commands::Exit | Commands::Quit => {
            eprintln!("# mur exit/quit: use `mur session discard`");
            cmd::session::cmd_session_exit()?
        }
        Commands::Chat { action } => match action {
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
                cmd::conversations_cmd::cmd_conversations_reindex(
                    cmd::conversations_cmd::ReindexArgs {
                        raw_only,
                        spans_only,
                        rollups_only,
                    },
                )
                .await?
            }
            ChatAction::Doctor => cmd::conversations_cmd::cmd_conversations_doctor().await?,
            ChatAction::Preflight => cmd::conversations_cmd::cmd_conversations_preflight().await?,
            ChatAction::Migrate {
                run,
                resume,
                discard_staging,
            } => {
                cmd::conversations_cmd::cmd_conversations_migrate(run, resume, discard_staging)
                    .await?
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
            ChatAction::Rollup {
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
            ChatAction::CostReport { since, json } => {
                cmd::conversations_cost_report::cmd_cost_report(&since, json, None).await?
            }
        },
        // Deprecated: use `mur chat <subcommand>`
        Commands::Conversations { action } => {
            eprintln!("# mur conversations: use `mur chat <subcommand>`");
            match action {
                ConversationsAction::Pull => {
                    cmd::conversations_cmd::cmd_conversations_pull().await?
                }
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
        // Deprecated: use `mur chat ask`
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
            InternalsAction::Reindex { bootstrap } => {
                if bootstrap {
                    cmd::reindex::cmd_reindex_bootstrap()?;
                } else {
                    cmd::reindex::cmd_reindex().await?;
                }
            }
            InternalsAction::RebuildIndex { layer } => cmd::internals::cmd_rebuild_index(&layer)?,
            InternalsAction::Git { layer, args } => {
                cmd::internals::cmd_internals_git(&layer, &args)?
            }
        },
        // Deprecated: use `mur skill eval`
        Commands::Eval { action } => {
            eprintln!("# mur eval: use `mur skill eval`");
            match action {
                EvalAction::Run { suite, format } => {
                    let code = cmd::eval::cmd_eval_run(&suite, &format)?;
                    std::process::exit(code);
                }
            }
        }
        // Deprecated: use `mur daemon sleep`
        Commands::Sleep { action } => {
            eprintln!("# mur sleep: use `mur daemon sleep`");
            match action {
                SleepAction::Enable => cmd::sleep::cmd_sleep_enable()?,
                SleepAction::Disable => cmd::sleep::cmd_sleep_disable()?,
                SleepAction::Status => cmd::sleep::cmd_sleep_status()?,
            }
        }
        Commands::Project { action } => match action {
            ProjectAction::Index {
                path,
                rebuild,
                quiet,
                background,
                foreground,
            } => {
                let mode = match (background, foreground) {
                    (true, _) => cmd::project::BackgroundMode::ForceBackground,
                    (_, true) => cmd::project::BackgroundMode::ForceForeground,
                    (false, false) => cmd::project::BackgroundMode::Auto,
                };
                cmd::project::cmd_project_index(path, rebuild, quiet, mode).await?
            }
            ProjectAction::IndexWorker {
                project_name,
                project_path,
                rebuild,
            } => {
                cmd::project::cmd_project_index_worker(&project_name, &project_path, rebuild)
                    .await?
            }
            ProjectAction::Search {
                query,
                project,
                limit,
                json,
            } => cmd::project::cmd_project_search(query, project, limit, json).await?,
            ProjectAction::Status { path } => cmd::project::cmd_project_status(path)?,
            ProjectAction::List => cmd::project::cmd_project_list()?,
            ProjectAction::Remove { path } => cmd::project::cmd_project_remove(path)?,
        },
        Commands::Auth { action } => match action {
            AuthAction::Login => cmd::misc::cmd_login().await?,
            AuthAction::Logout => cmd::misc::cmd_logout()?,
        },
        Commands::Daemon { action } => match action {
            DaemonAction::Start { detach } => cmd::murmurd::cmd_murmurd_start(detach)?,
            DaemonAction::Stop => cmd::murmurd::cmd_murmurd_stop()?,
            DaemonAction::Status => cmd::murmurd::cmd_murmurd_status()?,
            DaemonAction::Serve {
                port,
                open,
                readonly,
            } => cmd::server_cmd::cmd_serve(port, open, readonly).await?,
            DaemonAction::Sleep { action } => match action {
                SleepAction::Enable => cmd::sleep::cmd_sleep_enable()?,
                SleepAction::Disable => cmd::sleep::cmd_sleep_disable()?,
                SleepAction::Status => cmd::sleep::cmd_sleep_status()?,
            },
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
            name, out, format, ..
        } => {
            cmd::agent::cmd_export(&name, &out, &format)?;
        }
        AgentAction::Install { path, model } => {
            cmd::agent::cmd_install(std::path::Path::new(&path), model.as_deref())?
        }
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
            AgentScheduleAction::PropagateInit {
                name,
                after_secs,
                cooldown_secs,
            } => cmd::agent_schedule::cmd_propagate_init(&name, after_secs, cooldown_secs)?,
        },
        AgentAction::Hooks { action } => match action {
            AgentHooksAction::Show { name, json } => cmd::agent_hooks::cmd_hooks_show(&name, json)?,
        },
        AgentAction::MigrateToHub => cmd::agent::cmd_migrate_to_hub()?,
        AgentAction::Peers { json } => {
            cmd::agent::cmd_peers(&cmd::agent::resolve_mur_home()?, json)?
        }
        AgentAction::Propagate {
            name,
            dry_run,
            max,
            min_fitness,
            min_samples,
            json,
        } => {
            let home = cmd::agent::resolve_mur_home()?;
            cmd::agent_propagate::cmd_propagate(
                &home,
                &name,
                dry_run,
                max,
                min_fitness,
                min_samples,
                json,
            )?
        }
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
        AgentAction::Pending { name, action } => match action {
            Some(AgentPendingAction::List) | None => cmd::agent::cmd_pending_list(&name)?,
            Some(AgentPendingAction::Act { id, action_id }) => {
                cmd::agent::cmd_pending_act(&name, &id, &action_id)?
            }
        },
        AgentAction::Trash { name, action } => match action {
            AgentTrashAction::List => cmd::agent::cmd_trash_list(&name)?,
            AgentTrashAction::Restore { id } => cmd::agent::cmd_trash_restore(&name, &id)?,
            AgentTrashAction::Empty => cmd::agent::cmd_trash_empty(&name)?,
            AgentTrashAction::Now { id } => cmd::agent::cmd_trash_now(&name, &id)?,
        },
        AgentAction::Queue { name, action } => match action {
            AgentQueueAction::List => cmd::agent::cmd_queue_list(&name)?,
            AgentQueueAction::Pause { id } => cmd::agent::cmd_queue_pause(&name, &id)?,
            AgentQueueAction::Resume { id } => cmd::agent::cmd_queue_resume(&name, &id)?,
            AgentQueueAction::Cancel { id } => cmd::agent::cmd_queue_cancel(&name, &id)?,
            AgentQueueAction::Retry { id } => cmd::agent::cmd_queue_retry(&name, &id)?,
        },
    }
    Ok(())
}
