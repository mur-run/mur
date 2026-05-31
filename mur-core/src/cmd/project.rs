use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::codebase::scanner::{expand_tilde, project_name_from_path, scan_project};
use crate::codebase::{
    CodebaseIndex, IndexProgress, IndexStatus, IndexLock, BACKGROUND_CHUNK_THRESHOLD,
    discover_all_indexes,
};
use crate::store::config::load_config;
use crate::store::embedding::{embed, EmbeddingConfig};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundMode {
    /// Auto-detect: background if chunks > BACKGROUND_CHUNK_THRESHOLD
    Auto,
    /// Force background execution
    ForceBackground,
    /// Force foreground execution
    ForceForeground,
}

pub(crate) async fn cmd_project_index(
    path: Option<String>,
    rebuild: bool,
    quiet: bool,
    bg_mode: BackgroundMode,
) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);

    if !project_path.join(".git").exists() {
        anyhow::bail!(
            "Not a git repository: {}\n  mur project index requires a git repo",
            project_path.display()
        );
    }

    let project_name = project_name_from_path(&project_path);
    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index = CodebaseIndex::new(&project_name, &project_path);

    // Determine whether to run in background
    let run_in_background = match bg_mode {
        BackgroundMode::ForceBackground => true,
        BackgroundMode::ForceForeground => false,
        BackgroundMode::Auto => {
            // Quick scan to estimate chunk count
            let files = scan_project(&project_path);
            let estimated_chunks: usize = files.iter().map(|f| {
                let lines = f.content.lines().count();
                if lines < 50 { 1 } else { (lines / 60).max(1) + 1 }
            }).sum();
            estimated_chunks > BACKGROUND_CHUNK_THRESHOLD
        }
    };

    if run_in_background {
        // Check lock
        if !index.try_acquire_lock()? {
            eprintln!(
                "Indexing already in progress for '{}' (lock held by another process).",
                project_name
            );
            eprintln!("  Check status: mur project status");
            return Ok(());
        }

        // Spawn worker subprocess
        let mur_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("mur"));
        let mut cmd = std::process::Command::new(&mur_bin);
        cmd.args([
            "project",
            "index-worker",
            &project_name,
            &project_path.display().to_string(),
        ]);
        if rebuild {
            cmd.arg("--rebuild");
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.stdin(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0); // Detach from parent process group
        }

        let child = cmd.spawn().with_context(|| "spawning background index worker")?;
        let pid = child.id();

        if !quiet {
            eprintln!(
                "Indexing '{}' in background (PID: {}).",
                project_name, pid,
            );
            eprintln!("  Check progress: mur project status");
        }

        // Overwrite lock with the child's pid
        let lock = IndexLock {
            pid,
            project_name: project_name.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        let lock_path = index.lock_path();
        std::fs::write(&lock_path, serde_json::to_string(&lock)?)?;

        // Write initial progress
        index.write_progress(&IndexProgress {
            status: IndexStatus::Running,
            total_chunks: 0,
            done_chunks: 0,
            errors: 0,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            error_message: None,
        })?;

        return Ok(());
    }

    // ── Foreground path (existing behavior) ──
    if !quiet {
        eprintln!("Indexing {} ({})...", project_name, project_path.display());
    }

    let stats = index
        .build(&embed_config, rebuild, |done, total| {
            if !quiet && total > 0 {
                eprint!("\r  Embedding {}/{} chunks...", done, total);
            }
        })
        .await?;

    if !quiet {
        eprintln!();
        eprintln!(
            "  {} files, {} chunks, {} changed, {} skipped ({:.1}s)",
            stats.files_indexed,
            stats.chunks_created,
            stats.files_changed,
            stats.files_skipped,
            stats.duration_ms as f64 / 1000.0
        );
    }

    crate::codebase::ensure_git_hook(&project_path, quiet)?;
    Ok(())
}

pub(crate) async fn cmd_project_search(
    query: String,
    project_filter: Option<String>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let query_embedding = embed(&query, &embed_config).await?;

    let indexes = discover_all_indexes();
    if indexes.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No indexed projects found. Run `mur project index` first.");
        }
        return Ok(());
    }

    let mut all_results: Vec<serde_json::Value> = Vec::new();

    for discovered in &indexes {
        if let Some(ref filter) = project_filter
            && discovered.name != *filter
        {
            continue;
        }

        let project_path = discovered
            .project_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_default();
        let index = CodebaseIndex::new(&discovered.name, &project_path);
        let chunks = index.search(&query_embedding, limit).await?;

        for c in &chunks {
            all_results.push(serde_json::json!({
                "project": discovered.name,
                "file": c.file,
                "language": c.language,
                "chunk_type": c.chunk_type,
                "symbol": c.symbol,
                "content": c.content,
                "line_start": c.line_start,
                "line_end": c.line_end,
                "score": c.score,
            }));
        }
    }

    all_results.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    if json {
        println!("{}", serde_json::to_string_pretty(&all_results)?);
    } else {
        for (i, result) in all_results.iter().enumerate() {
            let file = result["file"].as_str().unwrap_or("");
            let project = result["project"].as_str().unwrap_or("");
            let symbol = result["symbol"].as_str().unwrap_or("");
            let score = result["score"].as_f64().unwrap_or(0.0);
            let content = result["content"].as_str().unwrap_or("");
            let line_start = result["line_start"].as_u64().unwrap_or(0);
            let line_end = result["line_end"].as_u64().unwrap_or(0);

            println!(
                "{}. {}:{} ({}) lines {}-{} score={:.3}",
                i + 1,
                project,
                file,
                symbol,
                line_start,
                line_end,
                score
            );
            for line in content.lines().take(3) {
                println!("   {}", line);
            }
            println!();
        }
    }

    Ok(())
}

pub(crate) async fn cmd_project_status(path: Option<String>) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    let has_db = index.lance_path().exists();
    let stats = index.stats_async().await?;

    println!("Project: {}", project_name);
    println!("  Path: {}", project_path.display());

    // Check if a background index is running
    let lock_path = index.lock_path();
    if lock_path.exists() {
        if let Ok(data) = std::fs::read_to_string(&lock_path) {
            if let Ok(lock) = serde_json::from_str::<IndexLock>(&data) {
                if mur_common::lock_file::pid_alive(lock.pid) {
                    // Live background process — show progress
                    if let Some(progress) = index.read_progress() {
                        let pct = if progress.total_chunks > 0 {
                            (progress.done_chunks as f64 / progress.total_chunks as f64) * 100.0
                        } else {
                            0.0
                        };
                        println!("  Status: indexing in background (PID: {})", lock.pid);
                        println!("  Progress: {}/{} chunks ({:.0}%)",
                            progress.done_chunks, progress.total_chunks, pct);
                        if progress.errors > 0 {
                            println!("  Errors: {}", progress.errors);
                        }
                    } else {
                        println!("  Status: indexing in background (PID: {})", lock.pid);
                    }
                    return Ok(());
                } else {
                    // Stale lock — process died
                    if let Some(progress) = index.read_progress() {
                        match progress.status {
                            IndexStatus::Done => {
                                println!("  Last index: completed");
                            }
                            IndexStatus::Error => {
                                println!("  Last index: failed");
                                if let Some(ref msg) = progress.error_message {
                                    println!("  Error: {}", msg);
                                }
                            }
                            IndexStatus::Running => {
                                println!("  Last index: interrupted (stale lock, PID {} no longer alive)", lock.pid);
                            }
                        }
                    } else {
                        println!("  Last index: unknown (stale lock)");
                    }
                }
            }
        }
    }

    println!("  Indexed: {}", if has_db { "yes" } else { "no" });
    if has_db {
        println!("  Chunks: {}", stats.chunks_created);
    }

    Ok(())
}

pub(crate) fn cmd_project_list() -> Result<()> {
    let indexes = discover_all_indexes();
    if indexes.is_empty() {
        println!("No indexed projects.");
        return Ok(());
    }
    println!("Indexed projects:");
    for idx in &indexes {
        let path_display = idx.project_path.as_deref().unwrap_or("(unknown)");
        let last = idx.last_indexed.as_deref().unwrap_or("(unknown)");
        println!(
            "  {} — {} files, last indexed: {}",
            idx.name, idx.file_count, last
        );
        println!("    path: {}", path_display);
    }
    Ok(())
}

/// Internal: runs the actual indexing work when spawned in background mode.
pub(crate) async fn cmd_project_index_worker(
    project_name: &str,
    project_path_str: &str,
    rebuild: bool,
) -> Result<()> {
    let project_path = PathBuf::from(project_path_str);
    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index = CodebaseIndex::new(project_name, &project_path);

    // Build with progress callbacks that write to the progress file
    let build_result = index
        .build(&embed_config, rebuild, |done, total| {
            let progress = IndexProgress {
                status: IndexStatus::Running,
                total_chunks: total,
                done_chunks: done,
                errors: 0,
                started_at: String::new(),
                finished_at: None,
                error_message: None,
            };
            let _ = index.write_progress(&progress);
        })
        .await;

    match build_result {
        Ok(stats) => {
            let progress = IndexProgress {
                status: IndexStatus::Done,
                total_chunks: stats.chunks_created,
                done_chunks: stats.chunks_created,
                errors: 0,
                started_at: String::new(),
                finished_at: Some(chrono::Utc::now().to_rfc3339()),
                error_message: None,
            };
            let _ = index.write_progress(&progress);
            index.release_lock();

            // Desktop notification
            send_notification(
                &format!("mur: {} indexed", project_name),
                &format!(
                    "{} files, {} chunks in {:.1}s",
                    stats.files_indexed,
                    stats.chunks_created,
                    stats.duration_ms as f64 / 1000.0
                ),
            );
        }
        Err(e) => {
            let progress = IndexProgress {
                status: IndexStatus::Error,
                total_chunks: 0,
                done_chunks: 0,
                errors: 1,
                started_at: String::new(),
                finished_at: Some(chrono::Utc::now().to_rfc3339()),
                error_message: Some(e.to_string()),
            };
            let _ = index.write_progress(&progress);
            index.release_lock();

            send_notification(
                &format!("mur: {} index failed", project_name),
                &format!("Error: {:.100}", e),
            );
        }
    }

    Ok(())
}

/// Send a desktop notification. macOS uses osascript; Linux uses notify-send if available.
fn send_notification(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            message.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Fallback: terminal bell + stderr
        eprintln!("\n\x07{}: {}", title, message);
        // Try notify-send on Linux
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("notify-send")
                .args([title, message])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}
