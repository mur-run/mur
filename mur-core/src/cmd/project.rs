use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::codebase::scanner::{expand_tilde, project_name_from_path, scan_project};
use crate::codebase::{
    BACKGROUND_CHUNK_THRESHOLD, CodebaseIndex, IndexLock, IndexProgress, IndexStatus,
    discover_all_indexes,
};
use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundMode {
    /// Auto-detect: background if chunks > BACKGROUND_CHUNK_THRESHOLD
    Auto,
    /// Force background execution
    ForceBackground,
    /// Force foreground execution
    ForceForeground,
}

// ─── Structured return types for tool/mcp consumption ───────────────

/// Structured result from project search — returned by do_project_search().
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectSearchResult {
    pub chunks: Vec<ProjectSearchChunk>,
    pub total_hits: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectSearchChunk {
    pub project: String,
    pub file: String,
    pub language: String,
    pub chunk_type: String,
    pub symbol: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
    pub score: f32,
}

/// Structured status for one project — returned by do_project_status().
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectStatusInfo {
    pub name: String,
    pub path: String,
    pub indexed: bool,
    pub chunks: Option<usize>,
    pub last_indexed: Option<String>,
    pub indexing_in_progress: bool,
    pub progress: Option<IndexProgressInfo>,
    /// Set when the index was built at a different vector width than the
    /// current config: (recorded, configured). Next `mur project index`
    /// rebuilds it automatically (#757).
    pub stale_dims: Option<(usize, usize)>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexProgressInfo {
    pub done_chunks: usize,
    pub total_chunks: usize,
    pub pct: f64,
    pub errors: usize,
}

// ─── Structured do_* functions ─────────────────────────────────────

/// Search all indexed projects (or a specific one) and return structured results.
pub async fn do_project_search(
    query: &str,
    project_filter: Option<&str>,
    limit: usize,
    all: bool,
) -> Result<ProjectSearchResult> {
    if query.trim().is_empty() {
        anyhow::bail!("query cannot be empty");
    }

    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let query_embedding = embed(query, &embed_config).await?;

    let indexes = discover_all_indexes();

    // Scope resolution: an explicit --project wins; otherwise default to the
    // current directory's project so results aren't polluted by unrelated repos.
    // `--all` searches every indexed project (the previous default behavior).
    let cwd_project = if project_filter.is_none() && !all {
        std::env::current_dir()
            .ok()
            .map(|p| p.canonicalize().unwrap_or(p))
            .map(|p| project_name_from_path(&p))
    } else {
        None
    };
    let effective_filter = project_filter.or(cwd_project.as_deref());

    let mut all_chunks: Vec<ProjectSearchChunk> = Vec::new();
    let mut matched_any_project = false;

    for discovered in &indexes {
        if let Some(filter) = effective_filter
            && discovered.name != *filter
        {
            continue;
        }
        matched_any_project = true;

        let project_path = discovered
            .project_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let index = CodebaseIndex::new(&discovered.name, &project_path);
        let chunks = index.search(&query_embedding, limit).await?;

        for c in &chunks {
            all_chunks.push(ProjectSearchChunk {
                project: discovered.name.clone(),
                file: c.file.clone(),
                language: c.language.clone(),
                chunk_type: c.chunk_type.clone(),
                symbol: c.symbol.clone(),
                content: c.content.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                score: c.score,
            });
        }
    }

    // If an explicit --project filter matched no indexed project at all, that is
    // a distinct condition from "project indexed but zero results".
    if !matched_any_project && let Some(name) = project_filter {
        anyhow::bail!(
            "project `{name}` is not indexed. Run `mur project index` in that project first."
        );
    }

    all_chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Git worktrees of the same repo share byte-identical files, so the same
    // chunk can appear once per worktree. Deduplicate by exact content, keeping
    // the first (highest-score) occurrence since we already sorted by score.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    all_chunks.retain(|c| seen.insert(c.content.clone()));

    // Duplicates are not meaningfully separate hits: count post-dedup.
    let total = all_chunks.len();
    all_chunks.truncate(limit);

    Ok(ProjectSearchResult {
        chunks: all_chunks,
        total_hits: total,
    })
}

/// Return structured status for a single project (default: current directory).
pub fn do_project_status(path: Option<&str>) -> Result<ProjectStatusInfo> {
    let project_path = match path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    let has_db = index.lance_path().exists();
    let stats = futures::executor::block_on(index.stats_async())?;

    let mut info = ProjectStatusInfo {
        name: project_name,
        path: project_path.display().to_string(),
        indexed: has_db,
        chunks: if has_db {
            Some(stats.chunks_created)
        } else {
            None
        },
        last_indexed: None,
        indexing_in_progress: false,
        progress: None,
        stale_dims: None,
    };

    // Stale-dims flag (#757): index built at a different vector width than
    // the current embedding config.
    if has_db
        && let Some(recorded) = index.recorded_dimensions()
        && let Ok(cfg) = crate::store::config::load_config()
        && recorded != cfg.embedding.dimensions
    {
        info.stale_dims = Some((recorded, cfg.embedding.dimensions));
    }

    // Check lock for background indexing
    let lock_path = index.lock_path();
    if lock_path.exists()
        && let Ok(data) = std::fs::read_to_string(&lock_path)
        && let Ok(lock) = serde_json::from_str::<IndexLock>(&data)
        && mur_common::lock_file::pid_alive(lock.pid)
    {
        info.indexing_in_progress = true;
        if let Some(prog) = index.read_progress() {
            let pct = if prog.total_chunks > 0 {
                (prog.done_chunks as f64 / prog.total_chunks as f64) * 100.0
            } else {
                0.0
            };
            info.progress = Some(IndexProgressInfo {
                done_chunks: prog.done_chunks,
                total_chunks: prog.total_chunks,
                pct,
                errors: prog.errors,
            });
        }
    }

    Ok(info)
}

/// List all indexed projects with summary info.
pub fn do_project_list() -> Result<Vec<ProjectStatusInfo>> {
    let indexes = discover_all_indexes();
    indexes
        .into_iter()
        .map(|idx| {
            let project_path = idx.project_path.as_deref().unwrap_or("");
            Ok(ProjectStatusInfo {
                name: idx.name,
                path: project_path.to_string(),
                indexed: true,
                chunks: None, // not computed for this view — None avoids falsely reporting 0
                last_indexed: idx.last_indexed,
                indexing_in_progress: false,
                progress: None,
                stale_dims: None, // not computed for the list view
            })
        })
        .collect()
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
            let estimated_chunks: usize = files
                .iter()
                .map(|f| {
                    let lines = f.content.lines().count();
                    if lines < 50 {
                        1
                    } else {
                        (lines / 60).max(1) + 1
                    }
                })
                .sum();
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

        let child = cmd
            .spawn()
            .with_context(|| "spawning background index worker")?;
        let pid = child.id();

        if !quiet {
            eprintln!("Indexing '{}' in background (PID: {}).", project_name, pid,);
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

pub async fn cmd_project_search(
    query: String,
    project_filter: Option<String>,
    limit: usize,
    json: bool,
    all: bool,
) -> Result<()> {
    let result = do_project_search(&query, project_filter.as_deref(), limit, all).await?;

    if result.chunks.is_empty() {
        if json {
            println!("[]");
        } else if project_filter.is_none() && !all {
            // Default scope is the current project; point users at --all.
            println!(
                "No matches in the current project. Use `--all` to search every indexed project, or `mur project index` to index this one."
            );
        } else {
            println!("No matches. Run `mur project index` first if the project isn't indexed.");
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&result.chunks)?);
    } else {
        for (i, c) in result.chunks.iter().enumerate() {
            println!(
                "{}. {}:{} ({}) lines {}-{} score={:.3}",
                i + 1,
                c.project,
                c.file,
                c.symbol.as_deref().unwrap_or(""),
                c.line_start,
                c.line_end,
                c.score
            );
            for line in c.content.lines().take(3) {
                println!("   {}", line);
            }
            println!();
        }
    }

    Ok(())
}

pub fn cmd_project_status(path: Option<String>) -> Result<()> {
    let info = do_project_status(path.as_deref())?;

    println!("Project: {}", info.name);
    println!("  Path: {}", info.path);

    if info.indexing_in_progress {
        if let Some(ref prog) = info.progress {
            println!(
                "  Status: indexing in background ({}/{} chunks, {:.0}%)",
                prog.done_chunks, prog.total_chunks, prog.pct
            );
            if prog.errors > 0 {
                println!("  Errors: {}", prog.errors);
            }
        } else {
            println!("  Status: indexing in background");
        }
        return Ok(());
    }

    println!("  Indexed: {}", if info.indexed { "yes" } else { "no" });
    if let Some(chunks) = info.chunks {
        println!("  Chunks: {}", chunks);
    }
    if let Some((recorded, configured)) = info.stale_dims {
        println!(
            "  ⚠ Index built at {recorded} dims but config is {configured} — run `mur project index` to rebuild."
        );
    }

    Ok(())
}

pub fn cmd_project_list() -> Result<()> {
    let projects = do_project_list()?;
    if projects.is_empty() {
        println!("No indexed projects.");
        return Ok(());
    }
    println!("Indexed projects:");
    for p in &projects {
        let last = p.last_indexed.as_deref().unwrap_or("(unknown)");
        println!("  {} — last indexed: {}", p.name, last);
        println!("    path: {}", p.path);
    }
    Ok(())
}

pub(crate) fn cmd_project_remove(path: Option<String>) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    if index.lance_path().exists() {
        index.delete_index()?;
        println!(
            "Removed index for '{}' at {}",
            project_name,
            project_path.display()
        );
        return Ok(());
    }

    // Fallback: scan all indexes for matching project_path (handles renamed dirs)
    let indexes = discover_all_indexes();
    let found = indexes.iter().find(|d| {
        d.project_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).canonicalize().ok())
            .map(|p| p == project_path)
            .unwrap_or(false)
    });

    match found {
        Some(entry) => {
            let fallback = CodebaseIndex::new(&entry.name, &project_path);
            fallback.delete_index()?;
            println!(
                "Removed index for '{}' at {}",
                entry.name,
                project_path.display()
            );
            Ok(())
        }
        None => {
            if indexes.is_empty() {
                anyhow::bail!(
                    "No index found for '{}'.\n  No projects are currently indexed. Run `mur project index` first.",
                    project_path.display()
                );
            }
            anyhow::bail!(
                "No index found for '{}'.\n  Indexed projects:\n{}",
                project_path.display(),
                indexes
                    .iter()
                    .map(|d| {
                        format!(
                            "    {}  ({})",
                            d.name,
                            d.project_path.as_deref().unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }
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

            // Parity with the foreground path: install the post-commit
            // auto-reindex hook on first successful index (idempotent).
            let _ = crate::codebase::ensure_git_hook(&project_path, true);

            // Reclaim disk from indexes whose worktree was removed.
            crate::codebase::prune_orphan_indexes();

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
