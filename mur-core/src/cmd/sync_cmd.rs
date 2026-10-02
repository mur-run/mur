use anyhow::{Context, Result};

use crate::capture;
use crate::inject;
use crate::store::yaml::YamlStore;

mod device;
mod skill_install;
mod transport;

pub use device::DeviceSyncDirection;
pub(crate) use device::device_sync;
pub(crate) use skill_install::ensure_mur_skill;
use transport::*;

pub(crate) async fn cmd_sync(quiet: bool, project_aware: bool, team: Option<&str>) -> Result<()> {
    use inject::sync::{default_targets, generate_sync_content_from_items, write_sync_file};

    // ─── Heartbeat: register device activity ──────────────────
    crate::auth::heartbeat();

    // ─── Device sync first (cloud or git) ─────────────────────
    // Failures warn but don't block tool sync
    if let Err(e) = device_sync(quiet, DeviceSyncDirection::Both, team).await
        && !quiet
    {
        eprintln!("  ⚠ Device sync error: {}", e);
    }

    // Ensure built-in skills are installed BEFORE loading the corpus — on a
    // pristine home the corpus is empty and the early return below would
    // otherwise skip installation forever (issue #593).
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("HOME directory not found"))?;
    let mur_dir = mur_common::trust::mur_home();
    let skill_installed = ensure_mur_skill(&home, &mur_dir)?;
    if !quiet && skill_installed {
        println!("  🎓 MUR skill installed/updated for AI tools");
    }

    // Skills are the sync content source (workflow-engine v2 P1b).
    let candidates =
        crate::retrieve::skill_candidates::load_skill_candidates(&mur_dir.join("skills"), &mur_dir)
            .unwrap_or_default();

    if candidates.is_empty() {
        if !quiet {
            println!("No skills to sync.");
        }
        return Ok(());
    }

    // Get current working directory for project-scoped sync
    let cwd = std::env::current_dir()?;
    let targets = default_targets();

    // Build project-aware query when --project is set. Resolve git worktrees to
    // the main repo name so sync scopes per-repo, consistent with the index.
    let project_name = crate::codebase::scanner::project_name_from_path(&cwd);

    let sync_query = if project_aware {
        build_project_sync_query(&cwd, &project_name)
    } else {
        project_name.clone()
    };
    let active_scope = crate::retrieve::skill_candidates::ActiveScope::detect();

    for target in &targets {
        let target_path = cwd.join(&target.file);

        // Only write to files that already exist on disk
        if !target_path.exists() {
            continue;
        }

        let top = inject::sync::select_sync_skills(
            candidates.clone(),
            &sync_query,
            &active_scope,
            target.max_patterns,
        );

        if top.is_empty() {
            continue;
        }

        let content = generate_sync_content_from_items(&top, &target.format);
        write_sync_file(&target_path, &content, &target.format)?;
        if !quiet {
            println!(
                "  {} — wrote {} skills to {}",
                target.name,
                top.len(),
                target_path.display()
            );
        }
    }

    // ─── Auto-reindex if dirty ───────────────────────────────
    let index_dirty = is_index_dirty(&home);
    if index_dirty {
        if !quiet {
            println!("  🔄 Index outdated — reindexing...");
        }
        match crate::cmd::reindex::cmd_reindex().await {
            Ok(()) => {}
            Err(e) => {
                if !quiet {
                    eprintln!(
                        "  ⚠ Reindex skipped: {} (run `mur reindex` manually or start Ollama)",
                        e
                    );
                }
            }
        }
    } else if !quiet {
        println!("  ✅ Index up to date");
    }

    // ─── Ensure default templates exist ──────────────────────
    ensure_default_templates(&home, quiet)?;

    if !quiet {
        println!("Sync complete.");
    }
    Ok(())
}

/// Bootstrap default template files if they don't exist.
fn ensure_default_templates(home: &std::path::Path, quiet: bool) -> Result<()> {
    let templates_dir = home.join(".mur").join("templates");
    let extract_prompt = templates_dir.join("extract-prompt.md");

    if !extract_prompt.exists() {
        std::fs::create_dir_all(&templates_dir)?;
        std::fs::write(&extract_prompt, crate::cmd::learn::DEFAULT_EXTRACT_PROMPT)?;
        if !quiet {
            println!(
                "  📝 Created default extraction template: {}",
                extract_prompt.display()
            );
        }
    }

    Ok(())
}

/// Check if the LanceDB index is stale compared to pattern/workflow YAML files.
fn is_index_dirty(home: &std::path::Path) -> bool {
    let mur_dir = home.join(".mur");
    let index_dir = mur_dir.join("index");

    // No index → dirty
    if !index_dir.exists() {
        return true;
    }

    // Get index mtime (use the directory mtime as proxy)
    let index_mtime = match std::fs::metadata(&index_dir).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };

    // Check all YAML files in patterns/ and workflows/
    let dirs_to_check = [mur_dir.join("patterns"), mur_dir.join("workflows")];

    for dir in &dirs_to_check {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                    && let Ok(meta) = std::fs::metadata(&path)
                    && let Ok(mtime) = meta.modified()
                    && mtime > index_mtime
                {
                    return true;
                }
            }
        }
    }

    false
}

// ─── Phase-1 memory-sync CLI helpers ─────────────────────────────────────────

/// Execute `mur push [--dry-run]`.
pub(crate) async fn run_push(server_url: &str, dry_run: bool) -> anyhow::Result<()> {
    let outbox = crate::sync::Outbox::default_location()?;
    let pending_paths = outbox.list_pending()?;

    if pending_paths.is_empty() {
        println!("outbox empty, nothing to push");
        return Ok(());
    }

    // Parse all pending signals; collect (path, signal) pairs, drop bad YAML with warning.
    let mut to_send: Vec<(std::path::PathBuf, mur_common::Signal)> =
        Vec::with_capacity(pending_paths.len());
    for p in pending_paths {
        let yaml = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  skip unreadable {}: {e}", p.display());
                continue;
            }
        };
        match serde_yaml::from_str::<mur_common::Signal>(&yaml) {
            Ok(sig) => to_send.push((p, sig)),
            Err(e) => eprintln!("  skip bad YAML {}: {e}", p.display()),
        }
    }

    if dry_run {
        println!("[dry-run] would push {} signal(s)", to_send.len());
        for (p, s) in &to_send {
            println!("  - {} ({})", s.id, p.display());
        }
        return Ok(());
    }

    let tokens = crate::auth::load_tokens()
        .ok_or_else(|| anyhow::anyhow!("not logged in (run `mur auth login`)"))?;
    let client = crate::sync::SyncClient::new(server_url, &tokens.access_token)?;

    let signals: Vec<mur_common::Signal> = to_send.iter().map(|(_, s)| s.clone()).collect();
    let resp = client.push_batch(&signals).await.context("push_batch")?;

    // Move accepted signal files to .flushed/.
    for (path, sig) in &to_send {
        if resp.accepted.iter().any(|a| *a == sig.id.to_string()) {
            outbox.mark_flushed(path)?;
        }
    }

    println!(
        "pushed: {} accepted, {} rejected",
        resp.accepted.len(),
        resp.rejected.len()
    );
    for r in &resp.rejected {
        println!("  rejected {}: {}", r.id, r.reason);
    }
    Ok(())
}

/// Execute `mur fetch [--dry-run]`.
pub(crate) async fn run_fetch(server_url: &str, dry_run: bool) -> anyhow::Result<()> {
    let cursor_store = crate::sync::CursorStore::default_location()?;
    let cursor = cursor_store.load()?;

    if dry_run {
        println!(
            "[dry-run] would fetch since={:?}",
            cursor.last_signal_id.as_deref()
        );
        return Ok(());
    }

    let tokens = crate::auth::load_tokens()
        .ok_or_else(|| anyhow::anyhow!("not logged in (run `mur auth login`)"))?;
    let client = crate::sync::SyncClient::new(server_url, &tokens.access_token)?;

    let resp = client
        .fetch_pending(cursor.last_signal_id.as_deref())
        .await
        .context("fetch_pending")?;

    let inbox = crate::sync::Inbox::default_location()?;
    let mut ids_to_ack: Vec<String> = Vec::with_capacity(resp.signals.len());
    for s in &resp.signals {
        inbox.receive(s)?;
        ids_to_ack.push(s.id.to_string());
    }

    let store = YamlStore::default_store()?;
    let report = inbox.apply_all(&store)?;

    println!(
        "fetched: {} signal(s) — applied {}, skipped {}, errors {}",
        resp.signals.len(),
        report.applied,
        report.skipped,
        report.errors.len()
    );
    for e in &report.errors {
        eprintln!("  error: {e}");
    }

    if !ids_to_ack.is_empty() {
        client.ack(&ids_to_ack).await.context("ack")?;
    }

    cursor_store.save(&crate::sync::FetchCursor {
        last_signal_id: resp.next_cursor,
        last_fetched_at: Some(chrono::Utc::now()),
    })?;
    Ok(())
}

/// Execute `mur sync status`.
pub(crate) fn run_status() -> anyhow::Result<()> {
    let outbox = crate::sync::Outbox::default_location()?;
    let cursor_store = crate::sync::CursorStore::default_location()?;

    let outbox_pending = outbox.list_pending()?.len();

    // Count inbox pending files directly — Inbox doesn't expose a list API.
    let inbox_dir = dirs::home_dir()
        .map(|h| h.join(".mur/inbox"))
        .unwrap_or_default();
    let inbox_pending = if inbox_dir.exists() {
        std::fs::read_dir(&inbox_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().extension().and_then(|s| s.to_str()) == Some("yaml")
                            && !e
                                .path()
                                .file_name()
                                .and_then(|s| s.to_str())
                                .is_some_and(|n| n.starts_with('.'))
                    })
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    let cursor = cursor_store.load()?;

    println!("sync status");
    println!("  outbox pending: {outbox_pending}");
    println!("  inbox pending:  {inbox_pending}");
    match cursor.last_fetched_at {
        Some(t) => println!("  last fetch:     {t}"),
        None => println!("  last fetch:     never"),
    }
    Ok(())
}

/// Build a richer query for project-aware sync by detecting language and git context.
pub(crate) fn build_project_sync_query(cwd: &std::path::Path, project_name: &str) -> String {
    let mut parts = vec![project_name.to_string()];

    // Detect language
    if let Some(lang) = capture::starter::detect_language_name(cwd) {
        parts.push(lang);
    }

    // Try git remote for extra context
    if let Ok(output) = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(name) = remote.rsplit('/').next() {
            let name = name.trim_end_matches(".git");
            if name != project_name {
                parts.push(name.to_string());
            }
        }
    }

    parts.join(" ")
}

#[cfg(test)]
mod sync_status_tests;

#[cfg(test)]
mod sync_skill_tests;

#[cfg(test)]
mod builtin_skill_tests;

#[cfg(test)]
mod dev_skill_trigger_tests;

#[cfg(test)]
mod deep_research_skill_tests;

#[cfg(test)]
mod gitattributes_tests;

#[cfg(test)]
mod never_shadow_tests;
