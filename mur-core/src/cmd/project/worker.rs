use anyhow::Result;
use std::path::PathBuf;

use crate::codebase::{CodebaseIndex, IndexProgress, IndexStatus};
use crate::store::config::load_config;
use crate::store::embedding::EmbeddingConfig;

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
