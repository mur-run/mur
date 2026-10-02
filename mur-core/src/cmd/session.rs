use anyhow::Result;
use std::collections::BTreeSet;

use crate::session;

mod export;
mod lifecycle;
mod out;
mod show;

#[allow(unused_imports)]
pub(crate) use export::*;
#[allow(unused_imports)]
pub(crate) use lifecycle::*;
#[allow(unused_imports)]
pub(crate) use out::*;
#[allow(unused_imports)]
pub(crate) use show::*;

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
