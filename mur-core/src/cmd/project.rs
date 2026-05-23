use anyhow::Result;
use std::path::PathBuf;

use crate::codebase::scanner::{expand_tilde, project_name_from_path};
use crate::codebase::{CodebaseIndex, discover_all_indexes};
use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};

pub(crate) async fn cmd_project_index(
    path: Option<String>,
    rebuild: bool,
    quiet: bool,
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

    let meta = index.lance_path().exists();
    let stats = index.stats_async().await?;

    println!("Project: {}", project_name);
    println!("  Path: {}", project_path.display());
    println!("  Indexed: {}", if meta { "yes" } else { "no" });
    if meta {
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
