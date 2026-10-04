//! Command dispatch — `Cli` → `cmd::*` handler. Extracted from `main.rs`'s
//! `async_main` body. One arm per top-level `Commands` variant; almost every
//! arm is a thin delegate into `cmd::*`. Keep new branches small — heavy
//! logic belongs in `cmd::<feature>`.

mod agent;
mod browser;
mod chat;
mod deep_research;
mod fleet;
mod session;
mod skill;
mod workflow;

use agent::run_agent;
use browser::run_browser;
use chat::{run_chat, run_conversations};
use deep_research::run_deep_research;
use fleet::run_fleet;
use session::run_session;
use skill::run_skill;
use workflow::run_workflow;

use anyhow::{Context, Result};
use clap::CommandFactory;

use crate::cli::{
    AgentAction, AgentAddonAction, AgentEvalAction, AgentHooksAction, AgentMcpAction,
    AgentPendingAction, AgentPermAction, AgentPromptAction, AgentQueueAction, AgentScheduleAction,
    AgentSecretAction, AgentSkillAction, AgentTrashAction, AgentWebhookAction, AuthAction,
    BrowserAction, CapabilityAction, ChannelAction, ChatAction, Cli, CodeNavAction,
    CommanderAction, Commands, ConversationsAction, DaemonAction, DeepResearchAction, DeployAction,
    DraftsAction, EvalAction, ExchangeAction, FleetAction, HookEvent, InternalsAction,
    MurmurdAction, OfficialAction, OpenAction, ProjectAction, ScheduleAction, SessionAction,
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
        Commands::Doctor { fix } => cmd::misc::cmd_doctor(fix)?,
        Commands::Limits {
            name,
            json,
            global,
            deadline,
            stuck,
            cost_usd,
            unset,
        } => {
            let patch = cmd::limits_write::Patch {
                deadline,
                stuck,
                cost_usd,
                unset,
            };
            let editing = patch != cmd::limits_write::Patch::default();
            let home = cmd::agent::resolve_mur_home()?;
            match (global, name) {
                (true, _) if editing => {
                    cmd::limits_write::upsert_global_limits(&home.join("config.yaml"), &patch)?
                }
                (true, _) => {
                    let cfg =
                        mur_common::config::Config::load_or_default(&home.join("config.yaml"));
                    print!("{}", cmd::limits_write::render_limits_block(&cfg.limits));
                }
                (false, Some(n)) if editing => match cmd::limits::detect_target(&home, &n)? {
                    cmd::limits::Target::Fleet(f) => {
                        cmd::limits_write::write_fleet_limits(&home, &f, &patch)?
                    }
                    cmd::limits::Target::Agent(a) => {
                        cmd::limits_write::write_agent_limits(&a, &patch)?
                    }
                    // `detect_target` resolves a name to a fleet or an agent
                    // only — `Global` is reached solely via the `--global`
                    // flag, handled in the arms above.
                    cmd::limits::Target::Global => {
                        unreachable!("detect_target never resolves a name to Global")
                    }
                },
                (false, Some(n)) => cmd::limits::cmd_limits(&n, json)?,
                (false, None) => anyhow::bail!("give a fleet or agent name, or --global"),
            }
        }

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
                MurmurdAction::Restart => cmd::murmurd::cmd_murmurd_restart()?,
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
            cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt, false, None, false).await?
        }
        Commands::Workflow { action } => run_workflow(action).await?,
        Commands::Channel { action } => match action {
            ChannelAction::Approve {
                channel_id,
                hitl_id,
                deny,
                reason,
            } => {
                cmd::channel::approve(&channel_id, &hitl_id, deny, reason)?;
            }
            ChannelAction::BackfillPurpose { apply, limit } => {
                let home = cmd::agent::resolve_mur_home()?;
                cmd::channel::backfill_purpose(&home, apply, limit)?;
            }
        },
        Commands::Job { action } => {
            let mur_home = crate::paths::mur_root(None);
            crate::cmd::job::run(&mur_home, action)?
        }
        Commands::Monitor { action } => {
            let mur_home = crate::paths::mur_root(None);
            cmd::monitor::run(&mur_home, action)?
        }
        // Deprecated: use `mur internals reindex`
        Commands::Reindex { bootstrap } => {
            eprintln!("# mur reindex: use `mur internals reindex`");
            if bootstrap {
                cmd::reindex::cmd_reindex_bootstrap()?;
            } else {
                cmd::reindex::cmd_reindex().await?;
            }
        }
        Commands::Update {
            check,
            restart_agents,
        } => {
            // `update::run` uses `reqwest::blocking`, whose internal runtime panics
            // when dropped inside this async context. Run it on a blocking thread
            // (no entered runtime there). Manual installs (InstallSource::Other)
            // reach the network path, so this must not panic for them.
            tokio::task::spawn_blocking(move || cmd::update::cmd_update(check, restart_agents))
                .await
                .context("update task panicked")??
        }
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
        Commands::Session { action } => run_session(action).await?,
        Commands::Dashboard => {
            dashboard::render_dashboard()?;
        }
        Commands::Fleet { action } => run_fleet(action).await?,
        Commands::Capability { action } => match action {
            CapabilityAction::List { agent } => {
                cmd::capability::cmd_capability_list(agent.as_deref())?
            }
            CapabilityAction::Show { name } => cmd::capability::cmd_capability_show(&name)?,
            CapabilityAction::Install { name, agent, yes } => {
                cmd::capability::cmd_capability_install(&name, &agent, yes)?
            }
            CapabilityAction::Remove { name, agent } => {
                cmd::capability::cmd_capability_remove(&name, &agent)?
            }
        },
        Commands::Commander { action } => {
            let mur_home = crate::paths::mur_root(None);
            match action {
                CommanderAction::Pin { pubkey, force } => {
                    cmd::commander::cmd_commander_pin(&mur_home, &pubkey, force)?
                }
                CommanderAction::Status => cmd::commander::cmd_commander_status(&mur_home)?,
                CommanderAction::Directive {
                    fleet,
                    kind,
                    budget_usd,
                } => {
                    // CLI uses "budget-ceiling"; map to the internal "budget_ceiling".
                    let k = if kind == "budget-ceiling" {
                        "budget_ceiling"
                    } else {
                        &kind
                    };
                    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                    cmd::commander::cmd_commander_directive(
                        &mur_home, &fleet, k, budget_usd, now_ms,
                    )?
                }
            }
        }
        Commands::Browser { action } => run_browser(action).await?,
        Commands::CodeNav { action } => match action {
            CodeNavAction::Setup {
                agent,
                with_serena,
                project,
                no_ast_grep,
                lsp,
                yes,
            } => {
                use std::io::IsTerminal;
                let consent =
                    cmd::browser::setup::Consent::new(std::io::stdin().is_terminal(), yes);
                let args = cmd::code_nav::setup::Args {
                    agent,
                    project,
                    flags: cmd::code_nav::plan::Flags {
                        with_serena,
                        no_ast_grep,
                        lsp,
                    },
                };
                // The installers use blocking HTTP and `Command::status`;
                // a blocking client inside the async runtime panics on drop.
                tokio::task::spawn_blocking(move || {
                    cmd::code_nav::setup::run(
                        args,
                        consent,
                        &mut std::io::stdin().lock(),
                        &mut std::io::stdout(),
                    )
                })
                .await
                .context("code-nav setup task")??
            }
        },
        Commands::DeepResearch {
            action,
            question,
            run_id,
        } => run_deep_research(action, question, run_id).await?,
        Commands::Official { action } => match action {
            OfficialAction::List => cmd::official::cmd_official_list().await?,
            OfficialAction::Install {
                id,
                model_policy,
                model_ref,
                fallback,
            } => {
                cmd::official::cmd_official_install(&id, model_policy, model_ref, fallback).await?
            }
        },
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
        Commands::Model(args) => cmd::model::run(args).await?,
        Commands::Migrate { patterns } => {
            if patterns {
                cmd::migrate_patterns::cmd_migrate_patterns()?;
            } else {
                eprintln!("Nothing to do. Try: mur migrate --patterns");
            }
        }
        Commands::Agent { action } => run_agent(action).await?,
        Commands::Skill { action } => run_skill(action).await?,
        Commands::Notes { action } => match action {
            crate::cli::notes::NotesAction::Create {
                name,
                description,
                body_file,
                kind,
            } => cmd::notes_cmd::cmd_create(&name, &description, body_file.as_deref(), &kind)?,
            crate::cli::notes::NotesAction::Search { query, limit } => {
                cmd::notes_cmd::cmd_search(&query, limit)?
            }
            crate::cli::notes::NotesAction::Remove { name, agent, yes } => {
                cmd::notes_cmd::cmd_remove(&name, agent.as_deref(), yes)?
            }
            crate::cli::notes::NotesAction::List {
                maturity,
                limit,
                agent,
            } => cmd::notes_cmd::cmd_list(maturity.as_deref(), limit, agent.as_deref())?,
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
        Commands::Open {
            action,
            json,
            all,
            check,
        } => {
            let home = crate::paths::mur_root(None);
            let cfg_path = home.join("config.yaml");
            match action {
                Some(OpenAction::Add { title, agent, next }) => {
                    let id = crate::open_items::reported::report(
                        &home,
                        &agent,
                        &title,
                        next.as_deref(),
                    )?;
                    println!("Recorded (reported by {agent}): {id}");
                }
                Some(OpenAction::Done { id }) => {
                    use crate::open_items::reported::Resolution;
                    match crate::open_items::reported::resolve(&home, &id)? {
                        Resolution::Closed { id, title } => println!("Resolved {id} — {title}"),
                        Resolution::Unmatched => {
                            // Still logged: the item may be on another machine
                            // and not synced yet. But it closed nothing here.
                            eprintln!(
                                "No open item matches '{id}' (by id or title); recorded anyway in case it syncs in later. Ids are shown by `mur open`."
                            );
                            std::process::exit(1);
                        }
                    }
                }
                Some(OpenAction::Mute { origin }) => {
                    let mut cfg = mur_common::config::Config::load_or_default(&cfg_path);
                    // Record even when nothing currently carries it — a source
                    // can be legitimately empty today — but say so, so a typo
                    // surfaces here rather than as a silent no-op later.
                    let mut seen: Vec<String> = crate::open_items::collect(&home)
                        .into_iter()
                        .map(|i| i.origin)
                        .collect();
                    if !seen.contains(&origin) {
                        seen.sort();
                        seen.dedup();
                        eprintln!(
                            "warning: nothing currently has origin '{origin}' (in use: {})",
                            if seen.is_empty() {
                                "none".to_string()
                            } else {
                                seen.join(", ")
                            }
                        );
                    }
                    if !cfg.open_items.muted.contains(&origin) {
                        cfg.open_items.muted.push(origin.clone());
                        cfg.open_items.muted.sort();
                        crate::store::config::save_config_at(&cfg_path, &cfg)?;
                    }
                    println!("Muted {origin}");
                }
                Some(OpenAction::Unmute { origin }) => {
                    let mut cfg = mur_common::config::Config::load_or_default(&cfg_path);
                    cfg.open_items.muted.retain(|m| m != &origin);
                    crate::store::config::save_config_at(&cfg_path, &cfg)?;
                    // Unmuting something that was not muted is not an error:
                    // the requested end state holds either way.
                    println!("Unmuted {origin}");
                }
                None => {
                    let items = crate::open_items::collect(&home);
                    // Fail toward showing: an unreadable config yields no
                    // mutes, never a quiet, confident, incomplete list.
                    let configured = mur_common::config::Config::load_or_default(&cfg_path)
                        .open_items
                        .muted;
                    // `--all` suspends the policy for this render. It does not
                    // change what the policy *is*, which is why only the human
                    // half below consults it.
                    let effective = if all { Vec::new() } else { configured.clone() };
                    let (visible, matched) =
                        crate::open_items::partition(items.clone(), &effective);
                    if json {
                        // Display policy never truncates the machine-readable
                        // form: consumers get every item plus the configured
                        // mute set and apply their own policy. Emitting only
                        // the visible half would make a fully muted list
                        // indistinguishable from an empty one, and reporting
                        // only the mutes that matched today would make a
                        // consumer's view of the policy depend on what happens
                        // to be outstanding.
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "items": items,
                                "muted": configured,
                            }))?
                        );
                    } else {
                        let (visible, coverage) = if check {
                            let (v, c) = crate::open_items::probe::annotate(
                                visible,
                                &std::env::current_dir()?,
                            );
                            (v, c.line())
                        } else {
                            (visible, None)
                        };
                        let (fresh, stale) =
                            crate::open_items::split_stale(visible, chrono::Utc::now());
                        // `--all` suspends this policy too: stale items come
                        // back into the list rather than being counted in a
                        // footer. Oldest last, which the split already gives.
                        let (shown, hidden) = if all {
                            ([fresh, stale].concat(), 0)
                        } else {
                            let n = stale.len();
                            (fresh, n)
                        };
                        print!("{}", crate::open_items::render(&shown, &matched, hidden));
                        if let Some(line) = coverage {
                            println!("\n{line}");
                        }
                    }
                }
            }
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
        Commands::Chat { action } => run_chat(action).await?,
        // Deprecated: use `mur chat <subcommand>`
        Commands::Conversations { action } => run_conversations(action).await?,
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
            InternalsAction::MigrateChannels => {
                let home = crate::paths::mur_root(None);
                let n = cmd::agent::channel_import::migrate_all(&home)?;
                println!("✅ imported {n} CLI session(s) into channels");
            }
            InternalsAction::ScheduleStatus { agent } => {
                let home = crate::paths::mur_root(None);
                let st = crate::schedule_status::schedule_status(&home, agent.as_deref());
                println!("{}", serde_json::to_string_pretty(&st)?);
            }
            InternalsAction::Recommend { cwd, limit } => {
                let recs = crate::recommend::recommend_for_cwd(std::path::Path::new(&cwd), limit);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "recommendations": recs }))?
                );
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
                main_repo,
            } => {
                let mode = match (background, foreground) {
                    (true, _) => cmd::project::BackgroundMode::ForceBackground,
                    (_, true) => cmd::project::BackgroundMode::ForceForeground,
                    (false, false) => cmd::project::BackgroundMode::Auto,
                };
                cmd::project::cmd_project_index(path, main_repo, rebuild, quiet, mode).await?
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
                all,
            } => cmd::project::cmd_project_search(query, project, limit, json, all).await?,
            ProjectAction::Status { path, json } => cmd::project::cmd_project_status(path, json)?,
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
            DaemonAction::Restart => cmd::murmurd::cmd_murmurd_restart()?,
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
        Commands::Compress { file, query } => {
            cmd::compress::do_compress(file.as_deref(), query.as_deref())?
        }
        Commands::Retrieve { hash, query } => cmd::compress::do_retrieve(&hash, query.as_deref())?,
    }

    Ok(())
}
