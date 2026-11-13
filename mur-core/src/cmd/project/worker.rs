use anyhow::Result;
use std::path::PathBuf;

use crate::codebase::{CodebaseIndex, IndexProgress, IndexStatus};
use crate::store::config::load_config;
use crate::store::embedding::EmbeddingConfig;

/// Internal: runs the actual indexing work when spawned in background mode.
///
/// `no_notify` skips the desktop notification on both the success and failure
/// arms. `no_hook` skips `ensure_git_hook`: `core.hooksPath` is agent-writable
/// repo config, so a daemon-spawned worker must not write `post-commit` into
/// a directory the agent picked. CLI-spawned workers pass neither.
pub(crate) async fn cmd_project_index_worker(
    project_name: &str,
    project_path_str: &str,
    rebuild: bool,
    no_notify: bool,
    no_hook: bool,
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
            index.release_lock_if_held_by(std::process::id());

            // Parity with the foreground path: install the post-commit
            // auto-reindex hook on first successful index (idempotent).
            if !no_hook {
                let _ = crate::codebase::ensure_git_hook(&project_path, true);
            }

            // Reclaim disk from indexes whose worktree was removed.
            crate::codebase::prune_orphan_indexes();

            // Desktop notification
            if !no_notify {
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
            index.release_lock_if_held_by(std::process::id());

            if !no_notify {
                send_notification(
                    &format!("mur: {} index failed", project_name),
                    &format!("Error: {:.100}", e),
                );
            }
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

#[cfg(test)]
mod tests {
    use super::cmd_project_index_worker;
    use std::path::Path;

    /// A git repo with no indexable files (the worker's build returns on the
    /// success arm before any embedding call) and an *empty* hooks dir.
    fn empty_repo(root: &Path) -> std::path::PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git init failed");
        let hooks = repo.join(".git").join("hooks");
        // `git init` seeds *.sample files from the template dir; start empty.
        let _ = std::fs::remove_dir_all(&hooks);
        std::fs::create_dir_all(&hooks).unwrap();
        repo
    }

    /// Hide the developer's global/system git config from both `git init`
    /// and the worker's own `git rev-parse`: a global `core.hooksPath`
    /// (lefthook, husky-global, company hooks) would otherwise redirect the
    /// hook into the real global hooks dir. An empty temp file, not
    /// `/dev/null`, so the same isolation holds on Windows.
    fn isolate_git_config(envg: &mut mur_common::test_env::EnvGuard, root: &Path) {
        let empty = root.join("empty.gitconfig");
        std::fs::write(&empty, "").unwrap();
        envg.set_var("GIT_CONFIG_GLOBAL", &empty);
        envg.set_var("GIT_CONFIG_NOSYSTEM", "1");
    }

    fn hooks_entries(repo: &Path) -> Vec<String> {
        std::fs::read_dir(repo.join(".git").join("hooks"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// `--no-hook`: the worker succeeds and leaves the hooks dir untouched.
    /// `MUR_HOME` is a temp dir, so nothing touches the real `~/.mur`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_hook_leaves_empty_hooks_dir_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let mut envg = mur_common::test_env::EnvGuard::hold();
        envg.set_var("MUR_HOME", tmp.path().join("mur-home"));
        isolate_git_config(&mut envg, tmp.path());
        let repo = empty_repo(tmp.path());

        cmd_project_index_worker("w-no-hook", &repo.display().to_string(), false, true, true)
            .await
            .unwrap();

        assert_eq!(hooks_entries(&repo), Vec::<String>::new());
    }

    /// Control: without `--no-hook` the same worker installs `post-commit`,
    /// so the test above would notice a worker that ignored the flag.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hook_is_installed_when_no_hook_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let mut envg = mur_common::test_env::EnvGuard::hold();
        envg.set_var("MUR_HOME", tmp.path().join("mur-home"));
        isolate_git_config(&mut envg, tmp.path());
        let repo = empty_repo(tmp.path());

        cmd_project_index_worker("w-hook", &repo.display().to_string(), false, true, false)
            .await
            .unwrap();

        assert_eq!(hooks_entries(&repo), vec!["post-commit".to_string()]);
    }
}
