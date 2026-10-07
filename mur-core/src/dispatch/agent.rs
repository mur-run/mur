use super::*;

pub(super) async fn run_agent(action: AgentAction) -> Result<()> {
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
        AgentAction::Start { name } => cmd::agent::cmd_start(&name)?,
        AgentAction::Stop { name } => cmd::agent::cmd_stop(&name)?,
        AgentAction::Restart {
            names,
            all,
            stale,
            dry_run,
        } => cmd::agent::cmd_restart(&names, all, stale, dry_run)?,
        AgentAction::Remove { name, purge, force } => cmd::agent::cmd_remove(&name, purge, force)?,
        AgentAction::Rename { old, new } => cmd::agent::cmd_rename(&old, &new)?,
        AgentAction::Fallback { name, model_refs } => {
            cmd::agent::model_resolve::cmd_agent_set_fallback(
                &cmd::agent::resolve_mur_home()?,
                &name,
                &model_refs,
            )?
        }
        AgentAction::Smart { name, state } => cmd::agent::model_resolve::cmd_agent_set_smart(
            &cmd::agent::resolve_mur_home()?,
            &name,
            &state,
        )?,
        AgentAction::Send {
            name,
            message,
            output_artifact_path,
        } => cmd::agent::cmd_send(&name, &message, output_artifact_path.as_deref())?,
        AgentAction::Card { name } => cmd::agent::cmd_card(&name)?,
        AgentAction::Dial {
            name,
            method,
            params,
        } => cmd::agent::cmd_dial(&name, &method, params.as_deref())?,
        AgentAction::Effort { name, level, clear } => cmd::agent::cmd_effort(&name, level, clear)?,
        AgentAction::Routing {
            name,
            limit,
            downgrades_only,
        } => cmd::agent::cmd_routing(&name, limit, downgrades_only)?,
        AgentAction::Who {
            can,
            skill,
            as_agent,
        } => cmd::agent::cmd_who(can, skill, as_agent)?,
        AgentAction::Cli {
            names,
            resume,
            auto: _,
            ask,
            skin,
            plain,
            budget_usd,
            auto_reads: _,
            no_auto_reads,
            fleet,
        } => {
            // Auto-approve is the default; `--ask` is the only way to start
            // ask-first. `--auto` is accepted for old scripts and means what
            // the default already means — and so does `--auto-reads`, whose
            // lane is now on unless `--no-auto-reads` turns it off.
            cmd::agent::cmd_cli(
                &names,
                resume,
                !ask,
                skin,
                plain,
                budget_usd,
                !no_auto_reads,
                fleet,
            )
            .await?
        }
        AgentAction::Pair { name } => cmd::agent_pair::cmd_pair(&name)?,
        AgentAction::Devices => cmd::agent_pair::cmd_devices()?,
        AgentAction::Unpair { fingerprint } => cmd::agent_pair::cmd_unpair(&fingerprint)?,
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
                no_probe,
                state_paths,
                publisher_name,
                publisher_homepage,
                publisher_registry_id,
            } => cmd::agent::cmd_mcp_add(
                &name,
                &server_id,
                &command,
                &args,
                &state_paths,
                cmd::agent::McpAddPin {
                    force,
                    no_probe,
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
                deep,
            } => {
                let code =
                    cmd::agent_mcp_pin::cmd_mcp_inspect(&name, server.as_deref(), probe, deep)?;
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
            AgentMcpAction::Vendor {
                name,
                server_id,
                version,
                force,
            } => cmd::agent_mcp_vendor::cmd_mcp_vendor(&name, &server_id, version, force)?,
            AgentMcpAction::Enable { name, server_id } => {
                cmd::agent::cmd_mcp_set_enabled(&name, &server_id, true)?
            }
            AgentMcpAction::Disable { name, server_id } => {
                cmd::agent::cmd_mcp_set_enabled(&name, &server_id, false)?
            }
            AgentMcpAction::SetNetwork {
                name,
                server_id,
                allow_hosts,
                allow_ports,
                deny_hosts,
                off,
                broad_audited,
                yes,
            } => cmd::agent::cmd_mcp_set_network(
                &name,
                &server_id,
                allow_hosts,
                allow_ports,
                deny_hosts,
                off,
                broad_audited,
                yes,
            )?,
            AgentMcpAction::Discover => cmd::agent::mcp_discover::cmd_mcp_discover()?,
            AgentMcpAction::Search { query } => {
                cmd::agent::mcp_registry::cmd_mcp_search(&query).await?
            }
            AgentMcpAction::RegistryAdd {
                name,
                server,
                force,
            } => cmd::agent::mcp_registry::cmd_mcp_registry_add(&name, &server, force).await?,
            AgentMcpAction::AddRemote {
                name,
                server_name,
                url,
                bearer_env,
                bearer_keychain,
            } => {
                let bearer = match (bearer_env, bearer_keychain) {
                    (Some(v), _) => Some(mur_common::secret::SecretRef::Env(v)),
                    (_, Some(sa)) => {
                        let (service, account) = sa.split_once('/').ok_or_else(|| {
                            anyhow::anyhow!("--bearer-keychain expects service/account")
                        })?;
                        Some(mur_common::secret::SecretRef::Keychain {
                            service: service.into(),
                            account: account.into(),
                        })
                    }
                    _ => None,
                };
                cmd::agent::mcp_add::cmd_mcp_add_remote(
                    &name,
                    &server_name,
                    &url,
                    bearer,
                    None,
                    None,
                )?
            }
            AgentMcpAction::Login { name, server } => {
                cmd::agent::mcp_login::cmd_mcp_login(&name, &server).await?
            }
        },
        AgentAction::Skill { action } => match action {
            AgentSkillAction::List { name } => cmd::agent::cmd_skill_list(&name)?,
            AgentSkillAction::Add { name, source } => {
                // A URL source is what `add-url` handles — route it there
                // instead of failing on a nonexistent local path.
                if source.starts_with("http://") || source.starts_with("https://") {
                    let ids =
                        cmd::agent::skill_remote::install_any_url(&name, &source, false).await?;
                    for id in &ids {
                        println!("Installed {id} onto '{name}'. Restart the agent to load it.");
                    }
                } else {
                    // Same confirmation as the URL branch above — a local-file
                    // install used to print nothing at all, so the user had no
                    // way to see the id it registered (which comes from the
                    // manifest name, not the filename).
                    let id = cmd::agent::cmd_skill_add(&name, &source)?;
                    println!("Installed {id} onto '{name}'. Restart the agent to load it.");
                }
            }
            AgentSkillAction::Remove { name, skill_id } => {
                cmd::agent::cmd_skill_remove(&name, &skill_id)?
            }
            AgentSkillAction::Convert { name, skill_id } => {
                cmd::agent::cmd_skill_convert(&name, &skill_id)?
            }
            AgentSkillAction::Show { name, skill_id } => {
                cmd::agent::cmd_skill_show(&name, &skill_id)?
            }
            AgentSkillAction::Enable { name, skill_id } => {
                cmd::agent::cmd_skill_set_enabled(&name, &skill_id, true)?
            }
            AgentSkillAction::Disable { name, skill_id } => {
                cmd::agent::cmd_skill_set_enabled(&name, &skill_id, false)?
            }
            AgentSkillAction::AddUrl { name, url, yes } => {
                let ids = cmd::agent::skill_remote::install_any_url(&name, &url, yes).await?;
                for id in &ids {
                    println!("Installed {id} onto '{name}'. Restart the agent to load it.");
                }
            }
            AgentSkillAction::RegistryAdd {
                name,
                skill,
                version,
                yes,
            } => {
                // Print consent summary before installing (best-effort; if
                // resolve_consent fails the real error surfaces from install).
                if let Ok(c) = cmd::agent::skill_registry_add::resolve_consent(
                    &cmd::agent::resolve_mur_home()?,
                    &skill,
                    version.as_deref(),
                ) {
                    println!("Skill:     {} v{}", c.name, c.version);
                    println!("Publisher: {}", c.publisher);
                    println!("Signature: {} [{}]", c.signature.status, c.signature.key_fp);
                    println!("Hash:      {}", c.hash);
                    println!("Trust:     {}", c.signer_trust);
                    if !c.mcp_requirements.is_empty() {
                        println!("MCP requirements: {}", c.mcp_requirements.join(", "));
                    }
                    if !c.findings.is_empty() {
                        println!("Findings:");
                        for f in &c.findings {
                            println!("  {f}");
                        }
                    }
                }
                let id = cmd::agent::skill_registry_add::cmd_skill_registry_add(
                    &name,
                    &skill,
                    version.as_deref(),
                    yes,
                )
                .await?;
                println!("Installed {id} onto '{name}' (Sandboxed). Restart the agent to load it.");
            }
            AgentSkillAction::InstallPack { agent, role, yes } => {
                let (installed, skipped) =
                    cmd::agent::skill_install_pack::cmd_skill_install_pack(&agent, &role, yes)
                        .await?;
                println!("installed: {:?}", installed);
                println!("skipped:   {:?}", skipped);
                if !installed.is_empty() {
                    println!("Restart '{agent}' to load the new skills.");
                }
            }
            AgentSkillAction::Search { name: _, query } => {
                let mur_home = cmd::agent::resolve_mur_home()?;
                let results =
                    cmd::agent::skill_registry_add::registry_search_for_agent(&mur_home, &query)?;
                if results.is_empty() {
                    println!("No registry skills found for '{query}'.");
                } else {
                    println!(
                        "{:25} {:10} {:20} {:10} SIGNED",
                        "NAME", "CATEGORY", "PUBLISHER", "LATEST"
                    );
                    for r in &results {
                        let sig = if r.signed_in_index { "yes" } else { "no" };
                        println!(
                            "{:25} {:10} {:20} {:10} {}",
                            r.name, r.category, r.publisher, r.latest, sig
                        );
                    }
                }
            }
            AgentSkillAction::TrustPublisher { key_fp, name } => {
                let mur_home = cmd::agent::resolve_mur_home()?;
                let mut kr =
                    mur_common::skill::publisher_trust::PublisherKeyring::load_or_seed(&mur_home)?;
                if kr.revoked.contains(&key_fp) {
                    anyhow::bail!("refusing to trust a revoked key: {key_fp}");
                }
                if kr.publishers.iter().any(|p| p.key_fp == key_fp) {
                    println!("already trusted: {key_fp}");
                } else {
                    kr.publishers
                        .push(mur_common::skill::publisher_trust::TrustedPublisher {
                            name: name.clone().unwrap_or_else(|| "user-trusted".to_string()),
                            key_fp: key_fp.clone(),
                            comment: "added via trust-publisher (TOFU)".to_string(),
                        });
                    kr.save(&mur_home)?;
                    println!("Trusted publisher key added: {key_fp}");
                }
            }
        },
        AgentAction::Addon { action } => match action {
            AgentAddonAction::Import {
                name,
                plugin_dir,
                plugin,
                force,
            } => cmd::agent::addon::cmd_addon_import(&name, &plugin_dir, plugin.as_deref(), force)?,
            AgentAddonAction::List { name } => cmd::agent::addon::cmd_addon_list(&name)?,
            AgentAddonAction::Enable { name, addon_id } => {
                cmd::agent::addon::cmd_addon_set_enabled(&name, &addon_id, true)?
            }
            AgentAddonAction::Disable { name, addon_id } => {
                cmd::agent::addon::cmd_addon_set_enabled(&name, &addon_id, false)?
            }
            AgentAddonAction::Remove { name, addon_id } => {
                cmd::agent::addon::cmd_addon_remove(&name, &addon_id)?
            }
            AgentAddonAction::DisableAll { name } => {
                cmd::agent::addon::cmd_addon_disable_all(&name)?
            }
            AgentAddonAction::Reimport {
                name,
                addon_id,
                from,
            } => cmd::agent::addon::cmd_addon_reimport(&name, &addon_id, from.as_deref())?,
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
            AgentPermAction::AllowPort { name, port } => {
                cmd::agent::cmd_perm_allow_port(&name, port)?
            }
            AgentPermAction::DenyPort { name, port } => {
                cmd::agent::cmd_perm_deny_port(&name, port)?
            }
            AgentPermAction::ListPorts { name } => cmd::agent::cmd_perm_list_ports(&name)?,
            AgentPermAction::ListPaths { name } => cmd::agent::cmd_perm_list_paths(&name)?,
            AgentPermAction::AllowRead { name, path } => {
                cmd::agent::cmd_perm_allow_read(&name, &path)?
            }
            AgentPermAction::AllowWrite { name, path } => {
                cmd::agent::cmd_perm_allow_write(&name, &path)?
            }
            AgentPermAction::DenyPath { name, path } => {
                cmd::agent::cmd_perm_deny_path(&name, &path)?
            }
            AgentPermAction::RemovePath { name, list, path } => {
                cmd::agent::cmd_perm_remove_path(&name, &list, &path)?
            }
            AgentPermAction::AllowSpawn { name, binary } => {
                cmd::agent::cmd_perm_allow_spawn(&name, &binary)?
            }
            AgentPermAction::DenySpawn { name, binary } => {
                cmd::agent::cmd_perm_deny_spawn(&name, &binary)?
            }
            AgentPermAction::AllowSpawnDir { name, dir } => {
                cmd::agent::cmd_perm_allow_spawn_dir(&name, &dir)?
            }
            AgentPermAction::DenySpawnDir { name, dir } => {
                cmd::agent::cmd_perm_deny_spawn_dir(&name, &dir)?
            }
            AgentPermAction::SetLimit { name, key, value } => {
                cmd::agent::cmd_perm_set_limit(&name, &key, value)?
            }
            AgentPermAction::Reseal { name } => cmd::agent::cmd_perm_reseal(&name)?,
            AgentPermAction::ToolAllow { name, pattern } => cmd::agent::cmd_perm_set_tool(
                &name,
                mur_common::agent::ToolPolicy::Allow,
                &pattern,
            )?,
            AgentPermAction::ToolAsk { name, pattern } => {
                cmd::agent::cmd_perm_set_tool(&name, mur_common::agent::ToolPolicy::Ask, &pattern)?
            }
            AgentPermAction::ToolDeny { name, pattern } => {
                cmd::agent::cmd_perm_set_tool(&name, mur_common::agent::ToolPolicy::Deny, &pattern)?
            }
            AgentPermAction::ToolClear { name, pattern } => {
                cmd::agent::cmd_perm_clear_tool(&name, &pattern)?
            }
            AgentPermAction::ToolList { name } => cmd::agent::cmd_perm_list_tools(&name)?,
        },
        AgentAction::Export { name, out, format } => {
            // Default the output path to `<name>.muragent` (or `.murpkg`) when -o/--out
            // is omitted, so the intuitive `mur agent export <name>` succeeds.
            let out = out.unwrap_or_else(|| {
                let ext = if format == "pkg" {
                    "murpkg"
                } else {
                    "muragent"
                };
                format!("{name}.{ext}")
            });
            cmd::agent::cmd_export(&name, &out, &format)?;
        }
        AgentAction::Install {
            path,
            model,
            as_name,
        } => {
            let (installed_name, fingerprint_hex) = cmd::agent::cmd_install(
                std::path::Path::new(&path),
                model.as_deref(),
                as_name.as_deref(),
            )?;
            // Symmetric with the fleet-import hook: best-effort, non-blocking
            // trusted-recipe install gated on the agent's signer being trusted
            // in the PublisherKeyring (not the bundle's own TOFU TrustStore).
            if let Ok(mur_home) = cmd::agent::resolve_mur_home()
                && let Ok(deps) = cmd::deps::aggregate_agent(&mur_home, &installed_name)
            {
                cmd::deps::install_trusted_recipes_at_import(
                    &mur_home,
                    &deps,
                    &fingerprint_hex,
                    &fingerprint_hex,
                    false,
                )
                .await;
            }
        }
        AgentAction::Uninstall { name, purge } => cmd::agent::cmd_uninstall(&name, purge)?,
        AgentAction::Inspect { path } => cmd::agent::cmd_inspect(std::path::Path::new(&path))?,
        AgentAction::Stats { name } => cmd::agent::cmd_stats(&name)?,
        AgentAction::Logs { name, tail } => cmd::agent::cmd_logs(&name, tail)?,
        AgentAction::Companion(args) => cmd::agent_companion::run(args).await?,
        AgentAction::Limits {
            name,
            json,
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
            if patch != cmd::limits_write::Patch::default() {
                cmd::limits_write::write_agent_limits(&name, &patch)?
            } else {
                let mur_home = cmd::agent::resolve_mur_home()?;
                let r = cmd::limits::report(&mur_home, &cmd::limits::Target::Agent(name.clone()))?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&cmd::limits::render_json(&r))?
                    );
                } else {
                    print!("{}", cmd::limits::render_human(&r));
                }
            }
        }
        AgentAction::Doctor { name, format, json } => match name {
            Some(name) => cmd::doctor::run_agent(&name, json)?,
            None => cmd::doctor::run(&format, json)?,
        },
        AgentAction::RuntimeDoctor { json, fix } => cmd::agent::cmd_doctor(json, fix)?,
        AgentAction::InstallDeps { name, program, yes } => {
            let mur_home = cmd::agent::resolve_mur_home()?;
            let deps = cmd::deps::aggregate_agent(&mur_home, &name)?;
            let lines = cmd::deps::doctor::build_report(&deps, &mur_home);
            cmd::deps::install::cmd_install_deps(&mur_home, &lines, program.as_deref(), yes)
                .await?;
        }
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
            VoiceAction::Download => cmd::agent_voice::cmd_voice_download(&name).await?,
        },
        AgentAction::Schedule { action } => match action {
            AgentScheduleAction::Add {
                name,
                cron,
                message,
                sends_to,
                once,
            } => {
                // The bound is the entry's own first firing — the scheduler
                // admits the firing its bound names and retires the one after
                // it. Same derivation as the `remind` tool, so `--once` and an
                // agent's own one-off proposal cannot disagree (#1119).
                let not_after = mur_agent_runtime::scheduler::one_off_bound(&cron, once);
                cmd::agent_schedule::cmd_schedule_add(&name, &cron, &message, sends_to, not_after)?
            }
            AgentScheduleAction::Proposals { name } => {
                cmd::agent_schedule::cmd_schedule_proposals(&name)?
            }
            AgentScheduleAction::Accept { name, id } => {
                cmd::agent_schedule::cmd_schedule_accept(&name, &id)?
            }
            AgentScheduleAction::Decline { name, id } => {
                cmd::agent_schedule::cmd_schedule_decline(&name, &id)?
            }
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
        AgentAction::Turn { action } => match action {
            crate::cli::agent::AgentTurnAction::List { name, json } => {
                cmd::agent::cmd_turn_list(&name, json)?
            }
            crate::cli::agent::AgentTurnAction::Undo {
                name,
                turn,
                dry_run,
                yes,
            } => cmd::agent::cmd_turn_undo(&name, &turn, dry_run, yes)?,
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
        AgentAction::Wizard {
            role,
            workspace,
            headless,
            no_llm,
            model_ref,
            no_eval,
        } => {
            cmd::agent::wizard::run(role, workspace, headless, no_llm, model_ref, no_eval).await?;
        }
    }
    Ok(())
}
