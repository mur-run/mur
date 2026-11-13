use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::codebase::scanner::{expand_tilde, project_name_from_path, scan_project};
use crate::codebase::{
    BACKGROUND_CHUNK_THRESHOLD, CodebaseIndex, IndexLock, IndexProgress, IndexStatus,
    discover_all_indexes,
};
use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};

// Public so `PROJECT_STATUS_SCHEMA_VERSION` and `ProjectStatusJson` stay
// reachable (as `cmd::project::status_json::*`) without a re-export the lib
// itself never uses — a re-export like that only compiles warning-free under
// an `#[allow(unused_imports)]`, and suppressing the warning hides the next
// genuinely dead import. The function is imported by value below; a module
// and a function may share a name, they live in different namespaces.
pub(crate) mod hook_report;
pub mod status_json;
use status_json::status_json;
mod wakeup;
pub(crate) mod worker;

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
    main_repo: bool,
    rebuild: bool,
    quiet: bool,
    bg_mode: BackgroundMode,
) -> Result<()> {
    let project_path = match (&path, main_repo) {
        (Some(p), _) => expand_tilde(p),
        // The post-commit hook runs with cwd inside whichever worktree committed.
        // Hooks are shared across linked worktrees, and what should refresh is the
        // main repo's index, not a per-worktree one — so resolve through git here
        // instead of baking a path into the hook, where a renamed directory would
        // strand it.
        (None, true) => {
            let cwd = std::env::current_dir()?;
            crate::codebase::scanner::main_repo_root(&cwd).ok_or_else(|| {
                anyhow::anyhow!(
                    "--main-repo: {} is not inside a git repository",
                    cwd.display()
                )
            })?
        }
        (None, false) => std::env::current_dir()?,
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
        // Check lock. The post-commit hook also fires inside sandboxed
        // agents, whose policy deliberately denies writes to the codebase
        // lance stores — that is expected, not a failure. The quiet (hook)
        // path queues a wake-up so the daemon reindexes outside the sandbox,
        // or skips with one line; never a bare EPERM.
        let acquired = match index.try_acquire_lock() {
            Ok(a) => a,
            Err(e) if quiet && is_permission_denied(&e) => {
                let home = crate::paths::mur_root(None);
                eprintln!("{}", wakeup::quiet_denied_line(&e, &project_name, &home));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if !acquired {
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
        index.hand_lock_to(pid)?;

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
    // #1672: states the install step cannot fix are reported here, once.
    if !quiet
        && let Some(notice) =
            hook_report::index_notice(&crate::codebase::hook_health(&project_path))
    {
        eprintln!("  {notice}");
    }
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

pub fn cmd_project_status(path: Option<String>, json: bool) -> Result<()> {
    let info = do_project_status(path.as_deref())?;

    if json {
        println!("{}", status_json(&info)?);
        return Ok(());
    }

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
    if info.indexed {
        match hook_report::check(std::path::Path::new(&info.path)) {
            Some(Ok(ok)) => println!("  Auto-index hook: {ok}"),
            Some(Err(problem)) => println!("  ⚠ Auto-index hook {problem}"),
            None => {}
        }
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

/// True when the error chain bottoms out in an `EPERM`/`EACCES` I/O error —
/// e.g. the sandbox denying the index lock write.
fn is_permission_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

#[cfg(test)]
mod permission_tests {
    use super::is_permission_denied;

    #[test]
    fn eperm_through_context_is_permission_denied() {
        // The platform's raw "denied" code, so the OS → ErrorKind mapping is
        // exercised: EPERM (1) is what a macOS Seatbelt write denial returns;
        // on Windows raw 1 is ERROR_INVALID_FUNCTION, so use
        // ERROR_ACCESS_DENIED (5) there.
        #[cfg(unix)]
        const DENIED: i32 = 1;
        #[cfg(windows)]
        const DENIED: i32 = 5;
        let e = anyhow::Error::new(std::io::Error::from_raw_os_error(DENIED))
            .context("writing index lock /x");
        assert!(is_permission_denied(&e));
    }

    #[test]
    fn other_io_errors_are_not_swallowed() {
        let e = anyhow::Error::new(std::io::Error::other("disk full"));
        assert!(!is_permission_denied(&e));
    }
}
