use super::*;

pub(super) async fn run_fleet(action: FleetAction) -> Result<()> {
    {
        let mur_home = crate::paths::mur_root(None);
        cmd::fleet::state_migrate::migrate_all(&mur_home);
        match action {
            FleetAction::Create {
                name,
                members,
                router,
                goal,
            } => {
                cmd::fleet::create::cmd_fleet_create(&mur_home, &name, members, router, goal, None)?
            }
            FleetAction::List { include_review } => {
                cmd::fleet::list::cmd_fleet_list(&mur_home, include_review)?
            }
            FleetAction::Show { name } => cmd::fleet::show::cmd_fleet_show(&mur_home, &name)?,
            FleetAction::Run {
                name,
                job,
                loop_flag,
                max_iterations,
                deadline,
                budget_usd,
                worktree,
                run_id,
                cwd,
                cwd_inferred,
            } => {
                if loop_flag {
                    if worktree {
                        anyhow::bail!(
                            "--worktree is not yet supported with --loop (the guarded-loop path has no worktree isolation)"
                        );
                    }
                    // job arg + --loop: enqueue the job first, then the loop drains it.
                    if let Some(text) = job {
                        cmd::fleet::jobs::enqueue_job(&mur_home, &name, &text, "cli")?;
                    }
                    // Same target as a one-shot run: `--cwd`, else the
                    // shell's directory. Gated once before the loop (#1607).
                    cmd::fleet::loop_run::cmd_fleet_run_loop(
                        &mur_home,
                        &name,
                        max_iterations,
                        deadline,
                        budget_usd,
                        run_id,
                        None,
                        Some(cmd::fleet::run::RunCwd {
                            path: cwd,
                            inferred: cwd_inferred,
                        }),
                    )
                    .await?
                } else {
                    cmd::fleet::run::cmd_fleet_run(
                        &mur_home,
                        &name,
                        job,
                        worktree,
                        run_id,
                        cmd::fleet::run::RunCwd {
                            path: cwd,
                            inferred: cwd_inferred,
                        },
                    )
                    .await?
                }
            }
            FleetAction::ReviewResume { name } => {
                tokio::task::spawn_blocking(move || {
                    cmd::fleet::review::session::cmd_fleet_review_resume(&mur_home, &name)
                })
                .await??
            }
            FleetAction::PruneReviews {
                older_than,
                include_paused,
                dry_run,
            } => cmd::fleet::review::prune::prune_reviews(
                &mur_home,
                &older_than,
                include_paused,
                dry_run,
                &mut std::io::stdout(),
                chrono::Utc::now(),
            )?,
            FleetAction::Review {
                main,
                reviewer,
                task,
                deadline,
                budget_usd,
            } => {
                let args = cmd::fleet::review::session::ReviewArgs {
                    main,
                    reviewer,
                    task,
                    deadline,
                    budget_usd,
                };
                // The driver blocks on A2A sockets and stdin; keep it off
                // the async runtime's worker threads.
                tokio::task::spawn_blocking(move || {
                    cmd::fleet::review::session::cmd_fleet_review(&mur_home, args)
                })
                .await??
            }
            FleetAction::Limits {
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
                    cmd::limits_write::write_fleet_limits(&mur_home, &name, &patch)?
                } else {
                    let r =
                        cmd::limits::report(&mur_home, &cmd::limits::Target::Fleet(name.clone()))?;
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
            FleetAction::Triage { days, json } => {
                cmd::fleet::triage_report::cmd_fleet_triage(&mur_home, days, json)?
            }
            FleetAction::SetLoop {
                name,
                trigger,
                max_iterations,
                deadline,
                budget_usd,
                done_when,
            } => cmd::fleet::settings::cmd_fleet_set_loop(
                &mur_home,
                &name,
                trigger,
                max_iterations,
                deadline,
                budget_usd,
                done_when,
            )?,
            FleetAction::Send { name, job } => {
                cmd::fleet::jobs::cmd_fleet_send(&mur_home, &name, &job)?
            }
            FleetAction::Jobs { name, all, since } => {
                cmd::fleet::jobs::cmd_fleet_jobs(&mur_home, &name, all, since.as_deref())?
            }
            FleetAction::Cancel { name, id, yes } => {
                cmd::fleet::jobs::cmd_fleet_cancel(&mur_home, &name, &id, yes)?
            }
            FleetAction::Stop { name } => cmd::fleet::control::cmd_fleet_stop(&mur_home, &name)?,
            FleetAction::Start { name } => cmd::fleet::control::cmd_fleet_start(&mur_home, &name)?,
            FleetAction::Export {
                name,
                with_members,
                out,
            } => cmd::fleet::export::cmd_fleet_export(
                &mur_home,
                &name,
                with_members,
                out,
                &chrono::Utc::now().to_rfc3339(),
            )?,
            FleetAction::Import {
                file,
                force,
                no_members,
                yes,
            } => {
                let (fleet_name, signer_fp, signature_verified) =
                    cmd::fleet::import::cmd_fleet_import(
                        &mur_home,
                        &file,
                        cmd::fleet::import::ImportOpts {
                            force,
                            no_members,
                            yes,
                        },
                    )?;
                // C1: the trusted-recipe install hook must run ONLY when the
                // bundle's signature was actually present AND verified — an
                // unsigned `--force` import must never reach the trust gate,
                // since its `signer_pubkey`/derived fp is attacker-controlled.
                if signature_verified {
                    // Phase 2: trusted-publisher recipe install (best-effort, non-blocking).
                    if let Ok(deps) = cmd::deps::aggregate_fleet(&mur_home, &fleet_name) {
                        cmd::deps::install_trusted_recipes_at_import(
                            &mur_home, &deps, &signer_fp, &signer_fp, yes,
                        )
                        .await;
                    }
                }
            }
            FleetAction::Delete { name, yes } => {
                cmd::fleet::delete::cmd_fleet_delete(&mur_home, &name, yes)?
            }
            FleetAction::Add { name, agents } => {
                cmd::fleet::roster::cmd_fleet_add(&mur_home, &name, agents)?
            }
            FleetAction::Remove { name, agents } => {
                cmd::fleet::roster::cmd_fleet_remove(&mur_home, &name, agents)?
            }
            FleetAction::Compare { name, unit } => {
                cmd::fleet::compare::cmd_fleet_compare(&mur_home, &name, unit.as_deref())?
            }
            FleetAction::Judge { name, stats } => {
                cmd::fleet::judge_cmd::cmd_fleet_judge(&mur_home, &name, stats)?
            }
            FleetAction::Cherry {
                name,
                auto,
                promote,
                target,
            } => cmd::fleet::cherry_cmd::cmd_fleet_cherry(
                &mur_home,
                &name,
                auto,
                promote,
                target.as_deref(),
            )?,
            FleetAction::PartitionPlan { name } => {
                cmd::fleet::partition_cmd::cmd_fleet_partition_plan(&mur_home, &name)?
            }
            FleetAction::Merge {
                name,
                promote,
                target,
            } => cmd::fleet::partition_cmd::cmd_fleet_merge(
                &mur_home,
                &name,
                promote,
                target.as_deref(),
            )?,
            FleetAction::MergeConcurrent {
                name,
                stats,
                promote,
                target,
            } => cmd::fleet::concurrent_cmd::cmd_fleet_merge_concurrent(
                &mur_home,
                &name,
                stats,
                promote,
                target.as_deref(),
            )?,
            FleetAction::Doctor { name } => {
                let deps = cmd::deps::aggregate_fleet(&mur_home, &name)?;
                let lines = cmd::deps::doctor::build_report(&deps, &mur_home);
                cmd::deps::doctor::print_report(&lines, &format!("mur fleet install-deps {name}"));
            }
            FleetAction::InstallDeps { name, program, yes } => {
                let deps = cmd::deps::aggregate_fleet(&mur_home, &name)?;
                let lines = cmd::deps::doctor::build_report(&deps, &mur_home);
                cmd::deps::install::cmd_install_deps(&mur_home, &lines, program.as_deref(), yes)
                    .await?;
            }
            FleetAction::Status { name } => {
                cmd::fleet::status::cmd_fleet_status(&mur_home, &name, &mut std::io::stdout())?
            }
        }
    }
    Ok(())
}
