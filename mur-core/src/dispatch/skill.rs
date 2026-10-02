use super::*;

pub(super) async fn run_skill(action: crate::cli::SkillAction) -> Result<()> {
    match action {
        crate::cli::SkillAction::New {
            name,
            category,
            dir,
            agent,
            force,
        } => cmd::skill_cmd::cmd_new(cmd::skill_cmd::NewOptions {
            name,
            category,
            dir,
            agent,
            force,
        })?,
        crate::cli::SkillAction::Edit { name, agent, dir } => {
            cmd::skill_cmd::cmd_edit(&name, agent.as_deref(), dir.as_deref())?
        }
        crate::cli::SkillAction::Validate {
            path,
            warnings_only,
        } => cmd::skill_cmd::cmd_validate(&path, warnings_only)?,
        crate::cli::SkillAction::Schema { out } => cmd::skill_cmd::cmd_schema(out.as_deref())?,
        crate::cli::SkillAction::Fmt { path, to, write } => {
            cmd::skill_cmd::cmd_fmt(&path, to.as_deref(), write)?
        }
        crate::cli::SkillAction::List => cmd::skill_cmd::cmd_list()?,
        crate::cli::SkillAction::Show { name } => cmd::skill_cmd::cmd_show(&name)?,
        crate::cli::SkillAction::Remove { name } => cmd::skill_cmd::cmd_remove(&name)?,
        crate::cli::SkillAction::Scope {
            name,
            fleet,
            project,
            team,
            user,
        } => cmd::skill_cmd::cmd_scope(&name, fleet, project, team, user)?,
        crate::cli::SkillAction::Search { query, local } => {
            cmd::skill_cmd::cmd_search(&query, local)?
        }
        crate::cli::SkillAction::Info {
            name,
            full,
            metrics,
        } => cmd::skill_cmd::cmd_info(&name, full, metrics)?,
        crate::cli::SkillAction::Audit { name } => cmd::skill_cmd::cmd_audit(&name)?,
        crate::cli::SkillAction::Trust { name, level } => cmd::skill_cmd::cmd_trust(&name, &level)?,
        crate::cli::SkillAction::Install { source } => {
            cmd::skill_install::cmd_install_cli(&source)?
        }
        crate::cli::SkillAction::Publish { path } => cmd::skill_publish::cmd_publish(&path)?,
        crate::cli::SkillAction::RegistryIndex { dir, check } => {
            let path = std::path::Path::new(&dir);
            if check {
                cmd::skill_registry_index::check_index(path)?;
                println!("✓ index.yaml is authoritative");
            } else {
                let idx = cmd::skill_registry_index::build_registry_index(path)?;
                let yaml = idx
                    .to_yaml()
                    .map_err(|e| anyhow::anyhow!("serialize index: {e}"))?;
                std::fs::write(path.join("index.yaml"), &yaml)?;
                println!("✓ regenerated index.yaml ({} skills)", idx.skills.len());
            }
        }
        crate::cli::SkillAction::Update { name } => cmd::skill_install::cmd_update_cli(&name)?,
        crate::cli::SkillAction::Upgrade { check, json } => {
            cmd::skill_upgrade_cmd::cmd_upgrade_cli(check, json)?
        }
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
                        let cfg =
                            mur_common::config::Config::load_or_default(&home.join("config.yaml"));
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
                        .await
                        .map_err(|e| {
                            crate::store::vector::unreadable::hinted(
                                e,
                                crate::store::vector::unreadable::hint::SKILLS,
                            )
                        })?
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
                    cmd::skill_intent::cmd_intent_canonicalise(&home, &agent_name, dry_run, json)?
                }
                crate::cli::IntentAction::Show { json } => {
                    cmd::skill_intent::cmd_intent_show(&home, json)?
                }
            }
        }
    }
    Ok(())
}
