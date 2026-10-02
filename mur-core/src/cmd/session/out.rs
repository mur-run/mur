use super::*;

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
            && let Err(e) = super::super::sync_cmd::device_sync(
                true,
                super::super::sync_cmd::DeviceSyncDirection::Push,
                None,
            )
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
        // A silent gate is indistinguishable from a broken one. Say what has
        // been banked so "no proposals" reads as "nothing has repeated yet"
        // rather than "recurrence is dead" (#783).
        let idx = crate::harvest::recurrence::load(&crate::harvest::recurrence::path_for(&inbox));
        if let Some(top) = idx.entries.iter().map(|e| e.count).max() {
            let need = crate::store::config::load_config()
                .map(|c| c.harvest.min_occurrences)
                .unwrap_or(0);
            eprintln!(
                "  {} session shape(s) banked, most-repeated {}× — a proposal needs {}×.",
                idx.entries.len(),
                top,
                need
            );
        }
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
            "◆ \"{}\"  ({} events · {}m{})",
            p.title,
            p.event_count,
            p.duration_secs / 60,
            // Why this proposal exists at all — a one-off never gets here (#783).
            if p.occurrences > 1 {
                format!(" · seen {}×", p.occurrences)
            } else {
                String::new()
            }
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

    review_memory_proposals()?;
    Ok(())
}

/// Memory-proposal review lane (federation P2c): what agents remembered and
/// proposed for team-wide visibility. Accept = the note becomes GLOBAL (all
/// agents' loaders see it); dismiss = the proposal is consumed while the
/// capturing agent keeps its local copy.
fn review_memory_proposals() -> Result<()> {
    let home = crate::paths::mur_root(None);
    let pending = crate::harvest::memory_proposal::pending(&home)?;
    if pending.is_empty() {
        return Ok(());
    }
    eprintln!("\n{} memory proposal(s) from agents:", pending.len());
    for p in &pending {
        let kind = mur_common::skill::lifecycle::note_kind(&p.proposal.manifest)
            .map(|k| format!("{k:?}").to_lowercase())
            .unwrap_or_default();
        // The human is the gate — show what the signature actually proves.
        let sig_note = match &p.sig_status {
            crate::harvest::memory_proposal::ProposalSigStatus::Verified => "✓ signed",
            crate::harvest::memory_proposal::ProposalSigStatus::Unsigned => "unsigned",
            crate::harvest::memory_proposal::ProposalSigStatus::Invalid(_) => "✗ INVALID SIGNATURE",
        };
        eprintln!(
            "\n  [{}] {} ({kind}, {sig_note}) — {}",
            p.proposal.agent, p.proposal.manifest.name, p.proposal.manifest.description
        );
        if let Some(body) = &p.proposal.manifest.content.note {
            for line in body.lines().take(3) {
                eprintln!("    {line}");
            }
        }
        let items = &[
            "✓ Accept — make it a shared note (visible to all agents)",
            "⏭ Skip",
            "✗ Dismiss — drop the proposal (agent keeps its local copy)",
        ];
        let choice = dialoguer::Select::new()
            .with_prompt(format!("Share `{}`?", p.proposal.manifest.name))
            .items(items)
            .default(1)
            .interact()
            .unwrap_or(1);
        match choice {
            0 => match crate::harvest::memory_proposal::accept(&home, p) {
                Ok(name) => eprintln!("  ✓ shared as `{name}` — ~/.mur/skills/{name}/skill.yaml"),
                Err(e) => eprintln!("  ✗ {e}"),
            },
            2 => {
                if let Err(e) = crate::harvest::memory_proposal::dismiss(p) {
                    eprintln!("  ✗ {e}");
                }
            }
            _ => {}
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
        origin: None,
        origin_version: None,
        origin_hash: None,
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
        requires_programs: vec![],
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
                        .join(format!("{}.jsonl", r.id));

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
            // Say what actually happened. "Done." reads like the inbox was
            // dealt with; skip touches nothing, and the session-start hook
            // brings the same proposals back tomorrow.
            let inbox = crate::harvest::proposal::inbox_dir();
            match crate::harvest::proposal::pending_in_dir(&inbox) {
                Ok(p) if !p.is_empty() => eprintln!(
                    "Skipped — {} proposal(s) still pending. `mur out --action reject` dismisses them.",
                    p.len()
                ),
                _ => eprintln!("Done."),
            }
        }
        "reject" => {
            let inbox = crate::harvest::proposal::inbox_dir();
            let dismissed = crate::harvest::proposal::dismiss_pending_in_dir(&inbox)?;
            if dismissed.is_empty() {
                eprintln!("No pending proposals to reject.");
            } else {
                eprintln!("Dismissed {} proposal(s):", dismissed.len());
                for (id, title) in &dismissed {
                    let short: String = id.chars().take(8).collect();
                    let line = title.lines().next().unwrap_or("").trim();
                    eprintln!("  {short}  {line}");
                }
            }
        }
        _ => {
            anyhow::bail!(
                "Unknown action '{}'. Use: analyze, export, skip, reject",
                action
            );
        }
    }

    Ok(())
}
