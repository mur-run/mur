use anyhow::Result;
use std::collections::BTreeSet;

use crate::session;

/// Analyze a session with the LLM workflow extractor and persist the result as
/// a draft workflow. Replaces the dead `mur learn extract` spawn (the `learn`
/// subcommand never existed — workflow-engine v2 P1a cleanup).
async fn analyze_session_to_draft(id: &str) -> Result<String> {
    let events = session::read_events(id)?;
    let extracted = if crate::extract::has_llm_config() {
        match crate::extract::extract_workflow_llm(id, &events).await {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  ⚠ LLM extraction failed ({e}); using logic-only extraction.");
                crate::extract::extract_workflow(id, &events)
            }
        }
    } else {
        crate::extract::extract_workflow(id, &events)
    };
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    let name = extracted.workflow.name.clone();
    if !store.exists(&name) {
        store.save(&extracted.workflow)?;
        eprintln!("  ✓ Draft workflow saved: {} (run: mur run {})", name, name);
    } else {
        eprintln!("  ✓ Workflow `{}` already exists — left unchanged.", name);
    }
    Ok(name)
}

/// Show the session review URL and auto-open in browser.
fn open_review_url(session_id: &str) {
    let mut local_running = std::net::TcpStream::connect("127.0.0.1:3847").is_ok();

    if !local_running {
        // Auto-start `mur serve` in the background
        if let Ok(exe) = std::env::current_exe() {
            match std::process::Command::new(exe)
                .args(["serve"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => {
                    // Wait briefly for the server to start
                    for _ in 0..10 {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        if std::net::TcpStream::connect("127.0.0.1:3847").is_ok() {
                            local_running = true;
                            break;
                        }
                    }
                }
                Err(e) => tracing::debug!("Failed to start mur serve: {e}"),
            }
        }
    }

    let url = format!("http://localhost:3847/#/sessions/{}/review", session_id);

    eprintln!();
    eprintln!("📊 Review: {}", url);

    if local_running {
        // Try open::that first, fall back to platform `open` command (macOS)
        // open::that can fail silently in subprocess/non-TTY contexts
        if let Err(e) = open::that(&url) {
            tracing::debug!("open::that failed: {e}, trying platform fallback");
            let cmd = if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(target_os = "windows") {
                "cmd"
            } else {
                "xdg-open"
            };
            let mut proc = std::process::Command::new(cmd);
            if cfg!(target_os = "windows") {
                proc.args(["/C", "start", "", &url]);
            } else {
                proc.arg(&url);
            }
            let _ = proc
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    } else {
        eprintln!("   (run `mur serve` first to open the dashboard)");
    }
}

pub(crate) fn cmd_session_start(source: &str) -> Result<()> {
    let session = session::start(source)?;
    eprintln!("Session started: {} (source: {})", &session.id[..8], source);
    Ok(())
}

pub(crate) async fn cmd_session_stop(_analyze: bool, _reflect: bool) -> Result<()> {
    match session::stop()? {
        Some(id) => {
            eprintln!("Session stopped: {}", &id[..8]);

            let recording_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
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
                && let Err(e) = super::sync_cmd::device_sync(
                    true,
                    super::sync_cmd::DeviceSyncDirection::Push,
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

/// `mur out` — stop session + post-session menu
///
/// - TTY mode: shows dialoguer interactive menu
/// - Non-TTY mode (LLM): outputs structured text for the LLM to present
/// - `--action <name>`: directly executes the chosen action (for LLM second call)
pub(crate) async fn cmd_out(action: Option<&str>, force: bool) -> anyhow::Result<()> {
    // Back-compat: explicit action keeps the old behavior verbatim.
    if let Some(action) = action {
        return cmd_out_execute(action, force).await;
    }

    // Legacy manual mode: stop the active session first (old `mur out` contract),
    // keeping auto-push for the stopped session.
    if let Ok(Some(id)) = crate::session::stop() {
        eprintln!("■ Stopped session {}", &id[..8.min(id.len())]);

        if let Ok(config) = crate::store::config::load_config()
            && config.sync.auto
            && config.sync.method != "local"
            && let Err(e) =
                super::sync_cmd::device_sync(true, super::sync_cmd::DeviceSyncDirection::Push, None)
                    .await
        {
            eprintln!("  ⚠ Auto-push failed: {}", e);
        }
    }

    // Harvest: scan now (synchronous — the user asked), then review.
    let _ = crate::harvest::scan();
    let inbox = crate::harvest::proposal::inbox_dir();
    let pending = crate::harvest::proposal::pending_in_dir(&inbox)?;

    if pending.is_empty() {
        eprintln!("✓ Nothing to harvest — no pending workflow proposals.");
        eprintln!("  (Recording is always on; see `mur session list`.)");
        return Ok(());
    }

    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if !is_tty {
        eprintln!("◆ {} pending workflow proposal(s):", pending.len());
        for p in &pending {
            eprintln!(
                "  {}  \"{}\" — {} steps{}",
                &p.id[..8.min(p.id.len())],
                p.title,
                p.steps.len(),
                p.similar_to
                    .as_deref()
                    .map(|s| format!(" (≈ existing `{}`)", s))
                    .unwrap_or_default()
            );
        }
        eprintln!(
            "Run `mur session out` in a terminal to review, or `mur session out --action analyze` for LLM analysis."
        );
        return Ok(());
    }

    for p in pending {
        eprintln!();
        eprintln!(
            "◆ \"{}\"  ({} events · {}m)",
            p.title,
            p.event_count,
            p.duration_secs / 60
        );
        for (i, s) in p.steps.iter().enumerate().take(8) {
            eprintln!("    {}. {}", i + 1, s);
        }
        if p.steps.len() > 8 {
            eprintln!("    … {} more", p.steps.len() - 8);
        }
        if let Some(similar) = &p.similar_to {
            eprintln!(
                "  ⚠ near-duplicate of existing `{}` — consider merging instead",
                similar
            );
        }

        let items = &["✓ Accept as draft workflow", "⏭ Skip", "✗ Quit review"];
        let choice = dialoguer::Select::new()
            .with_prompt(format!("Save as `{}`?", p.suggested_name))
            .items(items)
            .default(0)
            .interact()
            .unwrap_or(2);
        match choice {
            0 => {
                let skill_name = accept_proposal_as_skill(&p).await?;
                crate::harvest::proposal::set_status_in_dir(
                    &inbox,
                    &p.id,
                    crate::harvest::proposal::ProposalStatus::Accepted,
                )?;
                mark_harvested(&p.id);
                eprintln!(
                    "  ✓ Skill saved as `{}` — edit: ~/.mur/skills/{}/skill.yaml · run: mur run {}",
                    skill_name, skill_name, skill_name
                );
            }
            1 => {
                crate::harvest::proposal::set_status_in_dir(
                    &inbox,
                    &p.id,
                    crate::harvest::proposal::ProposalStatus::Dismissed,
                )?;
                mark_harvested(&p.id);
            }
            _ => break,
        }
    }
    Ok(())
}

/// Stamp `harvested_at` so retention GC may reclaim the recording.
fn mark_harvested(id: &str) {
    if let Some(mut meta) = crate::session::load_meta_pub(id) {
        meta.harvested_at = Some(chrono::Utc::now().to_rfc3339());
        let recordings = crate::paths::mur_root(None)
            .join("session")
            .join("recordings");
        if let Ok(json) = serde_json::to_string_pretty(&meta) {
            let _ = std::fs::write(recordings.join(format!("{}.meta.json", id)), json);
        }
    }
}

/// Accept a harvest proposal as a `category: Workflow` skill (P5a).
///
/// Runs LLM extraction (fallback: logic-only) and writes
/// `~/.mur/skills/<name>/skill.yaml` with `provenance: Llm`.
/// Returns the skill name that was written.
async fn accept_proposal_as_skill(
    proposal: &crate::harvest::proposal::Proposal,
) -> anyhow::Result<String> {
    use mur_common::skill::{
        SkillManifest,
        manifest::{Content, Procedure, ProcedureStep, Visibility},
        store::{global_skill_dir, write_to_dir},
        types::{Category, Priority, Provenance},
    };

    let id = &proposal.id;
    let events = session::read_events(id).unwrap_or_default();

    let extracted = if !events.is_empty() && crate::extract::has_llm_config() {
        match crate::extract::extract_workflow_llm(id, &events).await {
            Ok(e) => {
                eprintln!("  ◆ LLM extracted workflow from {} events.", events.len());
                e
            }
            Err(e) => {
                eprintln!("  ⚠ LLM extraction failed ({e}); using logic-only extraction.");
                crate::extract::extract_workflow(id, &events)
            }
        }
    } else {
        crate::extract::extract_workflow(id, &events)
    };

    let wf = &extracted.workflow;
    let name = crate::harvest::proposal::suggest_name(&wf.base.name);

    let mur_home = crate::paths::mur_root(None);
    let skill_dir = global_skill_dir(&mur_home, &name);
    if skill_dir.join("skill.yaml").exists() {
        eprintln!("  ✓ Skill `{}` already exists — left unchanged.", name);
        return Ok(name);
    }

    // Convert sequential workflow steps to a linear DAG chain.
    let steps: Vec<ProcedureStep> = wf
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| ProcedureStep {
            description: s.description.clone(),
            id: Some(format!("step{i}")),
            depends_on: if i == 0 {
                vec![]
            } else {
                vec![format!("step{}", i - 1)]
            },
            command: s.command.clone(),
            tool: s.tool.clone(),
            ..Default::default()
        })
        .collect();

    let abstract_text = if wf.trigger.is_empty() {
        wf.base.description.clone()
    } else {
        format!("{}. Trigger: {}", wf.base.description, wf.trigger)
    };

    let mut tags = vec!["harvested".to_string()];
    for t in &wf.tools {
        let t_lower = t.to_lowercase();
        if !tags.contains(&t_lower) {
            tags.push(t_lower);
        }
    }

    let manifest = SkillManifest {
        name: name.clone(),
        version: "1.0.0".to_string(),
        publisher: "local".to_string(),
        description: wf.base.description.clone(),
        category: Category::Workflow,
        provenance: Provenance::Llm,
        hosts: vec![],
        // Project-local scope: a skill harvested from a repo session is stamped
        // scope: Project so it injects only in that repo (matches injection's
        // active_project = current repo root). Non-repo sessions → User (global).
        scope: if proposal.project.is_some() {
            mur_common::skill::manifest::SkillScope::Project
        } else {
            Default::default()
        },
        visibility: Visibility::default(),
        fleet: None,
        team: None,
        governance: None,
        project: proposal.project.clone(),
        content: Content {
            r#abstract: abstract_text,
            procedure: Some(Procedure {
                variables: wf.variables.clone(),
                steps,
            }),
            context: None,
            command: None,
            note: None,
        },
        requires: vec![],
        tags,
        triggers: vec![],
        priority: Priority::Normal,
        evolution_log: vec![],
        transfer_chain: vec![],
        mcp_requirements: vec![],
        updated_at: chrono::Utc::now(),
    };

    std::fs::create_dir_all(&skill_dir)?;
    write_to_dir(&skill_dir, &manifest)?;
    Ok(name)
}

/// Check if a session has enough substance to warrant LLM analysis.
///
/// Returns `(worth_it, reason)` — skips only when ALL thresholds are below minimum.
fn session_worth_analyzing(
    recording_path: &std::path::Path,
    meta: Option<&crate::session::SessionMeta>,
) -> (bool, String) {
    let content = match std::fs::read_to_string(recording_path) {
        Ok(c) => c,
        Err(_) => return (false, "recording file not found".to_string()),
    };

    let noise_patterns = [
        "mur session",
        "mur sync",
        "mur context",
        "mur inject",
        "/mur:in",
        "/mur:out",
        "/mur-in",
        "/mur-out",
        "[stop:",
        "turn_end",
    ];

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let non_noise_count = lines
        .iter()
        .filter(|l| {
            let lower = l.to_lowercase();
            !noise_patterns.iter().any(|n| lower.contains(n))
        })
        .count();

    let tool_call_count = lines.iter().filter(|l| l.contains("\"tool_call\"")).count();

    let user_turns = meta.map(|m| m.user_turns).unwrap_or(0);
    let duration_secs = meta
        .and_then(|m| {
            let start = chrono::DateTime::parse_from_rfc3339(&m.started_at).ok()?;
            let end = chrono::DateTime::parse_from_rfc3339(m.stopped_at.as_ref()?).ok()?;
            Some(end.signed_duration_since(start).num_seconds())
        })
        .unwrap_or(0);

    // Skip only when ALL thresholds are below minimum (conservative)
    if non_noise_count < 5 && user_turns < 2 && duration_secs < 120 && tool_call_count == 0 {
        return (
            false,
            format!(
                "{} events, {} turns, {}s",
                non_noise_count, user_turns, duration_secs
            ),
        );
    }

    (true, String::new())
}

/// Execute a specific post-session action (called via `mur out --action <name>`)
async fn cmd_out_execute(action: &str, force: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("mur"));

    // Ensure the active session is stopped first — `mur session out` (no action)
    // stops it before the menu, and analyze/export operate on the most recent
    // *stopped* session, so a direct `--action` call must stop it too (idempotent:
    // a no-op when the prior no-action call already stopped it).
    if let Ok(Some(id)) = crate::session::stop() {
        eprintln!("■ Stopped session {}", &id[..8.min(id.len())]);
    }

    match action {
        "analyze" => {
            // Find the most recent stopped session
            let recordings = crate::session::list_recordings()?;
            let recent = recordings
                .iter()
                .find(|r| r.meta.as_ref().is_some_and(|m| m.stopped_at.is_some()));

            match recent {
                Some(r) => {
                    let recording_path = dirs::home_dir()
                        .expect("no home dir")
                        .join(".mur")
                        .join("session")
                        .join("recordings")
                        .join(format!("{}.jsonl", &r.id));

                    // Check if session is worth analyzing
                    if !force {
                        let meta = r.meta.as_ref();
                        let (worth_it, reason) = session_worth_analyzing(&recording_path, meta);
                        if !worth_it {
                            eprintln!("Session too short for LLM analysis ({}).", reason);
                            eprintln!("Use --force to analyze anyway.");
                            open_review_url(&r.id);
                            return Ok(());
                        }
                    }

                    if let Err(e) = analyze_session_to_draft(&r.id).await {
                        eprintln!("  ⚠ Analyze failed: {}", e);
                    }

                    open_review_url(&r.id);
                }
                None => eprintln!("No stopped session found."),
            }
        }
        "export" => {
            let recordings = crate::session::list_recordings()?;
            let recent = recordings
                .iter()
                .find(|r| r.meta.as_ref().is_some_and(|m| m.stopped_at.is_some()));

            match recent {
                Some(r) => {
                    let status = std::process::Command::new(&exe)
                        .args(["session", "export", &r.id, "--format", "markdown"])
                        .status()?;
                    if !status.success() {
                        eprintln!("  ⚠ session export exited with {}", status);
                    }
                }
                None => eprintln!("No stopped session found."),
            }
        }
        "skip" => {
            eprintln!("Done.");
        }
        _ => {
            anyhow::bail!("Unknown action '{}'. Use: analyze, export, skip", action);
        }
    }

    Ok(())
}

pub(crate) fn cmd_session_record(
    event_type: &str,
    tool: Option<&str>,
    content: &str,
) -> Result<()> {
    // Validate event type
    match event_type {
        "user" | "assistant" | "tool_call" | "tool_result" => {}
        _ => anyhow::bail!(
            "Invalid event type '{}'. Use: user, assistant, tool_call, tool_result",
            event_type
        ),
    }

    if !session::record(event_type, tool, content)? {
        // No active session — silently succeed (hooks shouldn't fail)
        return Ok(());
    }

    // Route to conversations pipeline if enabled. Best-effort: never fail the
    // hook on conversations errors — the legacy recording already succeeded.
    if crate::conversations::is_enabled().unwrap_or(false)
        && let Ok(Some(session_id)) = crate::session::active_session_id()
        && let Ok(msg) = crate::conversations::ingest::claude_code::event_to_message(
            event_type,
            tool,
            content,
            &session_id,
        )
        && let Ok(mut pipeline) = crate::conversations::ingest::pipeline::Pipeline::new(None)
    {
        let _ = pipeline.run(vec![msg]);
    }
    Ok(())
}

pub(crate) fn cmd_session_exit() -> Result<()> {
    match session::stop()? {
        Some(id) => {
            let recordings_dir = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("session")
                .join("recordings");
            let recording = recordings_dir.join(format!("{}.jsonl", id));
            let meta = recordings_dir.join(format!("{}.meta.json", id));
            if recording.exists() {
                let _ = std::fs::remove_file(&recording);
            }
            if meta.exists() {
                let _ = std::fs::remove_file(&meta);
            }
            eprintln!("Session stopped: {} (recording deleted)", &id[..8]);
        }
        None => {
            eprintln!("No active session.");
        }
    }
    Ok(())
}

pub(crate) fn cmd_session_status() -> Result<()> {
    match session::get_active()? {
        Some(session) => {
            println!("Active session: {}", session.id);
            println!("  Started: {}", session.started_at);
            println!("  Source:  {}", session.source);

            // Count events in the recording
            let recording_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("session")
                .join("recordings")
                .join(format!("{}.jsonl", session.id));

            if recording_path.exists() {
                let content = std::fs::read_to_string(&recording_path).unwrap_or_default();
                let count = content.lines().filter(|l| !l.trim().is_empty()).count();
                println!("  Events:  {}", count);
            }
        }
        None => {
            println!("No active session.");
        }
    }
    Ok(())
}

pub(crate) fn cmd_session_review(id_prefix: &str) -> Result<()> {
    let full_id = session::find_recording_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", id_prefix))?;

    let port = 3847u16;
    let url = format!("http://localhost:{}/#/sessions/{}/review", port, full_id);

    // Check if server is already running
    let server_running = std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok();

    if !server_running {
        eprintln!("Starting server on port {}...", port);
        // Start server in the background
        let exe = std::env::current_exe().unwrap_or_else(|_| "mur".into());
        std::process::Command::new(exe)
            .args(["serve", "--port", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
        // Brief wait for server to bind
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    eprintln!(
        "Opening session {} in browser...",
        &full_id[..8.min(full_id.len())]
    );

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", &url])
            .spawn();
    }

    Ok(())
}

pub(crate) fn cmd_session_show(id_prefix: &str, last: Option<usize>, json: bool) -> Result<()> {
    let full_id = session::find_recording_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", id_prefix))?;

    let meta = session::load_meta_pub(&full_id);
    let events = session::read_events(&full_id)?;

    if json {
        #[derive(serde::Serialize)]
        struct SessionJson {
            id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            meta: Option<session::SessionMeta>,
            events: Vec<session::SessionEvent>,
        }

        let display_events = if let Some(n) = last {
            events
                .into_iter()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        } else {
            events
        };

        let output = SessionJson {
            id: full_id,
            meta,
            events: display_events,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // Pretty-print header
    let short_id = if full_id.len() > 8 {
        &full_id[..8]
    } else {
        &full_id
    };
    println!("Session {}", short_id);
    println!("{}", "─".repeat(60));

    if let Some(m) = meta {
        println!("  ID:      {}", m.id);
        println!("  Source:  {}", m.source);
        if let Some(ref title) = m.title {
            println!("  Title:   {}", title);
        }
        println!("  Started: {}", m.started_at);
        if let Some(ref stopped) = m.stopped_at {
            println!("  Stopped: {}", stopped);
            // Calculate duration
            if let (Ok(start), Ok(end)) = (
                chrono::DateTime::parse_from_rfc3339(&m.started_at),
                chrono::DateTime::parse_from_rfc3339(stopped),
            ) {
                let dur = end.signed_duration_since(start);
                let secs = dur.num_seconds();
                if secs >= 3600 {
                    println!(
                        "  Duration: {}h {}m {}s",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60
                    );
                } else if secs >= 60 {
                    println!("  Duration: {}m {}s", secs / 60, secs % 60);
                } else {
                    println!("  Duration: {}s", secs);
                }
            }
        } else {
            println!("  Stopped: (still active)");
        }
        println!(
            "  Turns:   {} user, {} assistant",
            m.user_turns, m.assistant_turns
        );
        if !m.tools_used.is_empty() {
            println!("  Tools:   {}", m.tools_used.join(", "));
        }
    } else {
        println!("  ID: {}", full_id);
        println!("  (no metadata file found)");
    }

    println!();

    // Determine events to display
    let display_events: Vec<&session::SessionEvent> = if let Some(n) = last {
        events
            .iter()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        events.iter().collect()
    };

    if display_events.is_empty() {
        println!("  (no events)");
        return Ok(());
    }

    // Use first event timestamp as baseline for relative time
    let base_ts = events.first().map(|e| e.timestamp).unwrap_or(0);

    // Detect if stdout is a terminal (for content truncation)
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());

    println!("Events ({}):", display_events.len());
    println!();

    for event in &display_events {
        let rel_ms = event.timestamp.saturating_sub(base_ts);
        let rel_secs = rel_ms / 1000;
        let time_str = if rel_secs >= 3600 {
            format!(
                "+{}:{:02}:{:02}",
                rel_secs / 3600,
                (rel_secs % 3600) / 60,
                rel_secs % 60
            )
        } else {
            format!("+{:02}:{:02}", rel_secs / 60, rel_secs % 60)
        };

        let (icon, label) = match event.event_type.as_str() {
            "user" => ("👤", "user".to_string()),
            "assistant" => ("🤖", "assistant".to_string()),
            "tool_call" => (
                "🔧",
                if let Some(ref t) = event.tool {
                    format!("tool_call({})", t)
                } else {
                    "tool_call".to_string()
                },
            ),
            "tool_result" => ("📋", "tool_result".to_string()),
            _ => ("  ", event.event_type.clone()),
        };

        let content = if is_tty && event.content.len() > 200 {
            format!("{}…", &event.content[..200])
        } else {
            event.content.clone()
        };

        // Replace newlines with spaces for single-line display
        let content = content.replace('\n', " ").replace('\r', "");

        println!("  {} {} {} {}", time_str, icon, label, content);
    }

    Ok(())
}

pub(crate) fn cmd_session_list() -> Result<()> {
    let recordings = session::list_recordings()?;

    if recordings.is_empty() {
        println!("No session recordings found.");
        return Ok(());
    }

    println!("Session recordings ({}):\n", recordings.len());
    for r in &recordings {
        let time: chrono::DateTime<chrono::Utc> = r.modified.into();
        let short_id = if r.id.len() > 8 { &r.id[..8] } else { &r.id };
        println!(
            "  {} — {} events, {} bytes ({})",
            short_id,
            r.event_count,
            r.file_size,
            time.format("%Y-%m-%d %H:%M"),
        );
    }
    Ok(())
}

pub(crate) async fn cmd_session_export(
    id_prefix: &str,
    format: &str,
    analyze: bool,
    output: Option<String>,
) -> Result<()> {
    let full_id = session::find_recording_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", id_prefix))?;

    let meta = session::load_meta_pub(&full_id);
    let events = session::read_events(&full_id)?;

    let result = match format {
        "json" => export_json(&full_id, &meta, &events)?,
        "markdown" => export_markdown(&full_id, &meta, &events)?,
        "skill" => {
            if crate::extract::has_llm_config() {
                eprintln!("Using LLM-enhanced extraction (Haiku)...");
                export_skill_llm(&full_id, &events).await?
            } else {
                export_skill(&full_id, &meta, &events)?
            }
        }
        _ => anyhow::bail!("Unknown format '{}'. Use: json, markdown, skill", format),
    };

    if analyze && let Err(e) = analyze_session_to_draft(&full_id).await {
        eprintln!("  ⚠ Analyze failed: {}", e);
    }

    if let Some(path) = output {
        std::fs::write(&path, &result)?;
        eprintln!("Exported to {}", path);
    } else {
        print!("{}", result);
    }

    Ok(())
}

fn export_json(
    id: &str,
    meta: &Option<session::SessionMeta>,
    events: &[session::SessionEvent],
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct SessionExport {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<session::SessionMeta>,
        events: Vec<session::SessionEvent>,
    }

    let output = SessionExport {
        id: id.to_string(),
        meta: meta.clone(),
        events: events.to_vec(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

fn export_markdown(
    id: &str,
    meta: &Option<session::SessionMeta>,
    events: &[session::SessionEvent],
) -> Result<String> {
    let mut out = String::new();

    // Header
    let title = meta.as_ref().and_then(|m| m.title.as_deref()).unwrap_or(id);
    out.push_str(&format!("# Session: {}\n\n", title));

    if let Some(m) = meta {
        out.push_str(&format!("- **Source:** {}\n", m.source));
        out.push_str(&format!(
            "- **Started:** {}\n",
            format_timestamp_human(&m.started_at)
        ));

        if let Some(stopped) = &m.stopped_at
            && let (Ok(start), Ok(end)) = (
                chrono::DateTime::parse_from_rfc3339(&m.started_at),
                chrono::DateTime::parse_from_rfc3339(stopped),
            )
        {
            let dur = end.signed_duration_since(start);
            let secs = dur.num_seconds();
            let duration_str = if secs >= 3600 {
                format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
            } else if secs >= 60 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}s", secs)
            };
            out.push_str(&format!("- **Duration:** {}\n", duration_str));
        }

        if !m.tools_used.is_empty() {
            out.push_str(&format!("- **Tools:** {}\n", m.tools_used.join(", ")));
        }
    }

    out.push_str("\n## Timeline\n\n");

    for event in events {
        let ts = format_epoch_ms(event.timestamp);
        let (icon, label) = match event.event_type.as_str() {
            "user" => ("\u{1f464}", "User"),
            "assistant" => ("\u{1f916}", "Assistant"),
            "tool_call" => {
                if let Some(ref t) = event.tool {
                    ("\u{1f527}", t.as_str())
                } else {
                    ("\u{1f527}", "Tool")
                }
            }
            "tool_result" => ("\u{1f4cb}", "Tool Result"),
            _ => ("", event.event_type.as_str()),
        };

        let heading = if event.event_type == "tool_call" {
            if let Some(ref t) = event.tool {
                format!("### {} Tool: {} ({})\n\n", icon, t, ts)
            } else {
                format!("### {} Tool ({})\n\n", icon, ts)
            }
        } else {
            format!("### {} {} ({})\n\n", icon, label, ts)
        };

        out.push_str(&heading);
        out.push_str(&event.content);
        out.push_str("\n\n");
    }

    Ok(out)
}

fn export_skill(
    id: &str,
    meta: &Option<session::SessionMeta>,
    events: &[session::SessionEvent],
) -> Result<String> {
    let title = meta
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .unwrap_or("Untitled workflow");

    // Derive a slug name from the title
    let name: String = title
        .chars()
        .take(40)
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let name = if name.is_empty() {
        "session-workflow".to_string()
    } else {
        name
    };

    // Collect tool_call events as steps
    let mut steps = Vec::new();
    let mut tools_used = BTreeSet::new();
    let mut order = 1u32;

    for event in events {
        if event.event_type == "tool_call" {
            let tool_name = event.tool.clone().unwrap_or_else(|| "unknown".to_string());
            tools_used.insert(tool_name.clone());

            // Truncate long content for the description
            let desc: String = event.content.chars().take(120).collect();

            steps.push(format!(
                "  - order: {}\n    description: \"{}\"\n    tool: \"{}\"",
                order,
                desc.replace('\"', "\\\"").replace('\n', " "),
                tool_name,
            ));
            order += 1;
        }
    }

    let mut out = String::new();
    out.push_str(&format!("name: \"{}\"\n", name));
    out.push_str(&format!(
        "description: \"Workflow extracted from session {}\"\n",
        &id[..8.min(id.len())]
    ));
    out.push_str("tier: session\n");
    out.push_str("importance: 0.5\n");
    out.push_str("confidence: 0.3\n");
    out.push_str(&format!(
        "tags: [\"extracted\", \"session\"{}]\n",
        if tools_used.is_empty() {
            String::new()
        } else {
            format!(
                ", {}",
                tools_used
                    .iter()
                    .map(|t| format!("\"{}\"", t.to_lowercase()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    ));

    if steps.is_empty() {
        out.push_str("steps: []\n");
    } else {
        out.push_str("steps:\n");
        for step in &steps {
            out.push_str(step);
            out.push('\n');
        }
    }

    out.push_str(&format!(
        "tools: [{}]\n",
        tools_used
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!("source_sessions: [\"{}\"]\n", id));
    out.push_str("trigger: \"\"\n");

    Ok(out)
}

/// LLM-enhanced skill export using extract_workflow_llm.
async fn export_skill_llm(id: &str, events: &[session::SessionEvent]) -> Result<String> {
    let extracted = crate::extract::extract_workflow_llm(id, events).await?;
    let w = &extracted.workflow;

    let mut out = String::new();
    out.push_str(&format!("name: \"{}\"\n", w.base.name));
    out.push_str(&format!(
        "description: \"{}\"\n",
        w.base.description.replace('\"', "\\\"")
    ));
    out.push_str("tier: session\n");
    out.push_str("importance: 0.5\n");
    out.push_str("confidence: 0.5\n");

    // Tags
    let mut tags = vec![
        "extracted".to_string(),
        "session".to_string(),
        "llm-enhanced".to_string(),
    ];
    for t in &w.tools {
        tags.push(t.to_lowercase());
    }
    out.push_str(&format!(
        "tags: [{}]\n",
        tags.iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Steps
    if w.steps.is_empty() {
        out.push_str("steps: []\n");
    } else {
        out.push_str("steps:\n");
        for step in &w.steps {
            out.push_str(&format!(
                "  - order: {}\n    description: \"{}\"\n",
                step.order,
                step.description.replace('\"', "\\\"").replace('\n', " "),
            ));
            if let Some(ref tool) = step.tool {
                out.push_str(&format!("    tool: \"{}\"\n", tool));
            }
        }
    }

    out.push_str(&format!(
        "tools: [{}]\n",
        w.tools
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Variables
    if !w.variables.is_empty() {
        out.push_str("variables:\n");
        for var in &w.variables {
            out.push_str(&format!(
                "  - name: \"{}\"\n    description: \"{}\"\n",
                var.name,
                var.description
                    .as_deref()
                    .unwrap_or("")
                    .replace('\"', "\\\""),
            ));
            if let Some(ref dv) = var.default {
                out.push_str(&format!("    default: \"{}\"\n", dv));
            }
        }
    }

    out.push_str(&format!("source_sessions: [\"{}\"]\n", id));
    out.push_str(&format!(
        "trigger: \"{}\"\n",
        w.trigger.replace('\"', "\\\"")
    ));

    Ok(out)
}

pub(crate) async fn cmd_session_push(id_prefix: Option<&str>, all: bool) -> Result<()> {
    let config = crate::store::config::load_config()?;
    let server_url = &config.server.url;
    let token = match crate::auth::load_tokens() {
        Some(t) => t.access_token,
        None => {
            eprintln!("Not authenticated. Run `mur auth login` first.");
            return Ok(());
        }
    };

    if all {
        let pushed = session::cloud::push_unsynced(server_url, &token, false).await?;
        if pushed == 0 {
            eprintln!("All sessions already synced.");
        } else {
            eprintln!();
            eprintln!("📊 Review: https://dashboard.mur.run/#/sessions");
        }
    } else if let Some(prefix) = id_prefix {
        let full_id = session::find_recording_by_prefix(prefix)?
            .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", prefix))?;

        if session::cloud::push_session(server_url, &token, &full_id, false).await? {
            eprintln!();
            eprintln!(
                "📊 Review: https://dashboard.mur.run/#/sessions/{}/review",
                full_id
            );
        }
    } else {
        // No ID and no --all: push the most recent stopped session
        let recordings = session::list_recordings()?;
        let recent = recordings
            .iter()
            .find(|r| r.meta.as_ref().is_some_and(|m| m.stopped_at.is_some()));

        match recent {
            Some(r) => {
                if session::cloud::push_session(server_url, &token, &r.id, false).await? {
                    eprintln!();
                    eprintln!(
                        "📊 Review: https://dashboard.mur.run/#/sessions/{}/review",
                        r.id
                    );
                } else {
                    eprintln!("Session already synced or skipped.");
                }
            }
            None => {
                eprintln!("No stopped sessions to push.");
            }
        }
    }

    Ok(())
}

/// Format an RFC 3339 timestamp into a short human-readable form.
fn format_timestamp_human(rfc3339: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        rfc3339.to_string()
    }
}

/// Format epoch milliseconds into HH:MM:SS.
fn format_epoch_ms(epoch_ms: u64) -> String {
    let secs = epoch_ms / 1000;
    let hours = (secs / 3600) % 24;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", hours, mins, s)
}

/// Filter candidates through the nudge ledger and mark the actionable ones
/// Surfaced. Returns the ids that were surfaced (for the CLI hint).
pub(crate) fn record_nudges_for_candidates(
    candidates: &[crate::nudge::WorkflowCandidate],
) -> anyhow::Result<Vec<String>> {
    let config_path = crate::store::yaml::default_mur_dir().join("config.yaml");
    let cfg = mur_common::config::Config::load_or_default(&config_path);
    if !cfg.nudge.enabled || candidates.is_empty() {
        return Ok(vec![]);
    }
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    let now = chrono::Utc::now();
    let actionable = ledger.filter_actionable(candidates, now, cfg.nudge.daily_cap);
    crate::nudge::NudgeEmitter::emit_pending(&mut ledger, &actionable, now);
    ledger.save(&path)?;
    Ok(actionable.into_iter().map(|c| c.id).collect())
}

pub(crate) fn cmd_session_remove(
    id: Option<String>,
    all: bool,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let active_id = crate::session::active_session_id().ok().flatten();

    if let Some(prefix) = id {
        // ── Single removal ──
        let full_id = session::find_recording_by_prefix(&prefix)?
            .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", prefix))?;

        // Guard: don't delete active session
        if let Some(ref active) = active_id
            && active == &full_id
        {
            anyhow::bail!(
                "Session {} is currently active. Use `mur session discard` to stop and delete it.",
                &full_id[..8]
            );
        }

        // Confirm unless --force (non-TTY without --force = error)
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if !force {
            if !is_tty {
                anyhow::bail!("--force required when not running interactively.");
            }
            eprint!("Delete session {}? [y/N]: ", &full_id[..8]);
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            if !buf.trim().eq_ignore_ascii_case("y") {
                eprintln!("Cancelled.");
                return Ok(());
            }
        }

        let was_synced = session::is_recording_synced(&full_id);
        session::remove_recording(&full_id)?;
        eprintln!("Session {} removed.", &full_id[..8]);
        if was_synced {
            eprintln!(
                "  \u{2139}\u{fe0f}  This session was synced to the cloud. Cloud copies are unaffected \
                 — use the dashboard to manage them."
            );
        }
    } else if all {
        // ── Bulk removal ──
        let recordings = session::list_recordings()?;
        if recordings.is_empty() {
            eprintln!("No session recordings found.");
            return Ok(());
        }

        // Filter out active session
        let (to_delete, skipped): (Vec<_>, Vec<_>) = recordings
            .into_iter()
            .partition(|r| active_id.as_ref() != Some(&r.id));

        if dry_run {
            eprintln!("Would delete {} session(s):", to_delete.len());
            for r in &to_delete {
                let ts: chrono::DateTime<chrono::Utc> = r.modified.into();
                eprintln!(
                    "  {} — {} events, {} bytes ({})",
                    &r.id[..8],
                    r.event_count,
                    r.file_size,
                    ts.format("%Y-%m-%d %H:%M"),
                );
            }
            if !skipped.is_empty() {
                eprintln!("  1 session skipped (active).");
            }
            return Ok(());
        }

        if to_delete.is_empty() {
            eprintln!("No sessions to delete (1 active).");
            return Ok(());
        }

        // Confirm
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if !force {
            if !is_tty {
                anyhow::bail!("--force required when not running interactively.");
            }
            eprint!("Delete {} session(s)? [y/N]: ", to_delete.len());
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            if !buf.trim().eq_ignore_ascii_case("y") {
                eprintln!("Cancelled.");
                return Ok(());
            }
        }

        let synced_count = to_delete
            .iter()
            .filter(|r| session::is_recording_synced(&r.id))
            .count();
        let mut deleted = 0usize;
        for r in &to_delete {
            if let Err(e) = session::remove_recording(&r.id) {
                eprintln!("  \u{26a0} Failed to delete {}: {}", &r.id[..8], e);
            } else {
                deleted += 1;
            }
        }

        eprintln!("Deleted {} session(s).", deleted);
        if !skipped.is_empty() {
            eprintln!("  1 session skipped (active).");
        }
        if synced_count > 0 {
            eprintln!();
            eprintln!(
                "  \u{2139}\u{fe0f}  {} session(s) were synced to the cloud. Cloud copies are unaffected.",
                synced_count,
            );
        }
    } else {
        anyhow::bail!("Specify a session ID or use --all.");
    }

    Ok(())
}
