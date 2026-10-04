use super::*;

pub(crate) fn cmd_session_start(source: &str) -> Result<()> {
    let session = session::start(source)?;
    eprintln!("Session started: {} (source: {})", &session.id[..8], source);
    Ok(())
}

pub(crate) async fn cmd_session_stop(_analyze: bool, _reflect: bool) -> Result<()> {
    match session::stop()? {
        Some(id) => {
            eprintln!("Session stopped: {}", &id[..8]);

            let recording_path = mur_common::home::mur_home()
                .join("session")
                .join("recordings")
                .join(format!("{}.jsonl", id));

            // Nudge hook: surface pending harvest proposals (replaces the
            // emergence/fingerprint miner — workflow-engine v2 P1a).
            {
                use crate::nudge::candidate::CandidateSource;
                let source = crate::nudge::HarvestProposalSource::default_source();
                if let Ok(nudge_candidates) = source.candidates(0)
                    && let Ok(surfaced) = record_nudges_for_candidates(&nudge_candidates)
                    && !surfaced.is_empty()
                {
                    eprintln!(
                        "💡 Noticed {} repeated workflow(s). Review with `mur suggest`.",
                        surfaced.len()
                    );
                    // Deliver to companion-enabled agents' inboxes.
                    let ledger_path = crate::nudge::NudgeLedger::default_path();
                    if let Ok(ledger) = crate::nudge::NudgeLedger::load(&ledger_path) {
                        let surfaced_cands: Vec<_> = surfaced
                            .iter()
                            .filter_map(|id| ledger.get(id).and_then(|r| r.candidate.clone()))
                            .collect();
                        if let Ok(n) = crate::nudge::companion::deliver_nudges_to_companions(
                            &crate::store::yaml::default_mur_dir(),
                            &surfaced_cands,
                            "en",
                        ) && n > 0
                        {
                            eprintln!(
                                "  📬 {n} nudge(s) sent to your companion (or run `mur suggest`)."
                            );
                        }
                    }
                }
            }

            // Auto-push to device sync if configured
            if let Ok(config) = crate::store::config::load_config()
                && config.sync.auto
                && config.sync.method != "local"
                && let Err(e) = super::super::sync_cmd::device_sync(
                    true,
                    super::super::sync_cmd::DeviceSyncDirection::Push,
                    None,
                )
                .await
            {
                eprintln!("  ⚠ Auto-push failed: {}", e);
            }

            // Interactive post-session menu (only in terminal)
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                // Show session summary
                let meta = session::load_meta_pub(&id);
                let event_count = if recording_path.exists() {
                    std::fs::read_to_string(&recording_path)
                        .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
                        .unwrap_or(0)
                } else {
                    0
                };

                eprintln!();
                eprintln!("Session summary:");
                eprintln!("  Events: {}", event_count);
                if let Some(ref m) = meta {
                    eprintln!(
                        "  Turns:  {} user, {} assistant",
                        m.user_turns, m.assistant_turns
                    );
                    if let (Some(stopped), Ok(start)) = (
                        &m.stopped_at,
                        chrono::DateTime::parse_from_rfc3339(&m.started_at),
                    ) && let Ok(end) = chrono::DateTime::parse_from_rfc3339(stopped)
                    {
                        let secs = end.signed_duration_since(start).num_seconds();
                        if secs >= 3600 {
                            eprintln!(
                                "  Duration: {}h {}m {}s",
                                secs / 3600,
                                (secs % 3600) / 60,
                                secs % 60
                            );
                        } else if secs >= 60 {
                            eprintln!("  Duration: {}m {}s", secs / 60, secs % 60);
                        } else {
                            eprintln!("  Duration: {}s", secs);
                        }
                    }
                }

                let items = &[
                    "🔍 Analyze — extract patterns with LLM (needs reasoning model)",
                    "📦 Export — save as markdown",
                    "⏭  Skip",
                ];

                if let Ok(choice) = dialoguer::Select::new()
                    .with_prompt("What next?")
                    .items(items)
                    .default(2)
                    .interact()
                {
                    let exe =
                        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("mur"));
                    match choice {
                        0 => {
                            if let Err(e) = analyze_session_to_draft(&id).await {
                                eprintln!("  ⚠ Analyze failed: {}", e);
                            }
                            open_review_url(&id);
                        }
                        1 => {
                            // Export: run `mur session export <id> --format markdown`
                            let status = std::process::Command::new(&exe)
                                .args(["session", "export", &id, "--format", "markdown"])
                                .status();
                            if let Ok(s) = status {
                                if !s.success() {
                                    eprintln!("  ⚠ session export exited with {}", s);
                                }
                            } else if let Err(e) = status {
                                eprintln!("  ⚠ Failed to run session export: {}", e);
                            }
                        }
                        _ => {
                            // Skip — do nothing
                        }
                    }
                }
            }
        }
        None => {
            eprintln!("No active session.");
        }
    }
    Ok(())
}

/// `mur in` — ambient mode: mark the session as important; manual mode: legacy
/// start-recording + inject context.
pub(crate) async fn cmd_in(source: &str) -> anyhow::Result<()> {
    let cfg = crate::store::config::load_config()?;
    if cfg.session.capture != "ambient" {
        // Legacy manual mode: identical to the old behavior.
        let session = crate::session::start(source)?;
        eprintln!("Session started: {} (source: {})", &session.id[..8], source);
        eprintln!(
            "  Use `mur session out` to stop and export, or `mur session discard` to discard."
        );

        // Inject context (equivalent to `mur context --quiet`)
        crate::cmd::context::cmd_context(
            None,
            false,
            false,
            2000,
            source.to_string(),
            false,
            vec![],
            true,
        )
        .await?;
        return Ok(());
    }

    // Ambient mode: recording is always on — `mur in` marks importance.
    let session_dir = crate::paths::mur_root(None).join("session");
    std::fs::create_dir_all(&session_dir)?;

    // Mark the most recent recording if it saw activity in the last 10 minutes;
    // otherwise leave a marker the next captured event consumes.
    let recent = crate::session::list_recordings()?.into_iter().find(|r| {
        r.modified
            .elapsed()
            .map(|e| e.as_secs() < 600)
            .unwrap_or(false)
    });
    match recent {
        Some(r) => {
            let meta = crate::session::update_marked(&r.id, true)?;
            eprintln!(
                "★ Session \"{}\" marked — the harvest gate will not skip it.",
                meta.title.as_deref().unwrap_or(&r.id[..8.min(r.id.len())])
            );
        }
        None => {
            std::fs::write(
                session_dir.join(crate::session::ambient::MARK_NEXT_FILE),
                "",
            )?;
            eprintln!(
                "★ Next session will be marked. (Recording is always on — see `mur session list`.)"
            );
        }
    }
    Ok(())
}

/// Retention GC over ambient recordings. Quiet by design — runs detached from
/// the session-start hook. Harvest scan is appended here in W2.
pub(crate) fn cmd_session_gc() -> anyhow::Result<()> {
    let cfg = crate::store::config::load_config()?;
    let recordings = crate::paths::mur_root(None)
        .join("session")
        .join("recordings");
    let removed = crate::session::gc_in_dir(&recordings, cfg.session.retention_days)?;
    if removed > 0 {
        eprintln!("session gc: removed {} expired recording(s)", removed);
    }
    if let Ok(report) = crate::harvest::scan()
        && report.proposed > 0
    {
        eprintln!("harvest: {} new workflow proposal(s)", report.proposed);
    }
    Ok(())
}
