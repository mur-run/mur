use super::ParallelBackend;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const WORKTREES_DIR: &str = ".worktrees";
const PARALLEL_BASE_FILE: &str = ".parallel-base";

pub struct GitWorktreeBackend {
    repo_root: PathBuf,
}

impl GitWorktreeBackend {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

impl ParallelBackend for GitWorktreeBackend {
    fn create_track(&self, name: &str) -> Result<PathBuf> {
        let path = self.repo_root.join(WORKTREES_DIR).join(name);
        let status = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .current_dir(&self.repo_root)
            .status()
            .context("spawn git worktree add")?;
        if !status.success() {
            anyhow::bail!("git worktree add failed with {status}");
        }

        // Write the initial base snapshot to a sentinel file so promote() can use it
        let base_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&path)
            .output()
            .context("git rev-parse HEAD for sentinel")?;
        if !base_head.status.success() {
            anyhow::bail!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&base_head.stderr)
            );
        }
        std::fs::write(path.join(PARALLEL_BASE_FILE), base_head.stdout.as_slice())?;

        Ok(path)
    }

    fn base_snapshot(&self, track: &Path) -> Result<String> {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(track)
            .output()
            .context("spawn git rev-parse")?;
        if !out.status.success() {
            anyhow::bail!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>> {
        let out = Command::new("git")
            .args(["diff", "--name-only", since_snapshot, "HEAD"])
            .current_dir(track)
            .output()
            .context("spawn git diff")?;
        if !out.status.success() {
            anyhow::bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        let files = String::from_utf8(out.stdout)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| track.join(l))
            .collect();
        Ok(files)
    }

    fn promote(&self, track: &Path, target: &Path) -> Result<()> {
        // Read the initial HEAD saved at create_track time
        let since = std::fs::read_to_string(track.join(PARALLEL_BASE_FILE))
            .context("read .parallel-base sentinel — was create_track called?")?;
        let since = since.trim();
        let files = self.diff_files(track, since)?;
        for src in files {
            let rel = src.strip_prefix(track).context("strip prefix")?;
            let dst = target.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst)?;
        }
        Ok(())
    }

    fn destroy(&self, track: &Path) -> Result<()> {
        let status = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(track)
            .current_dir(&self.repo_root)
            .status()
            .context("spawn git worktree remove")?;
        if !status.success() {
            anyhow::bail!("git worktree remove failed with {status}");
        }
        Ok(())
    }
}

pub fn find_git_root(from: &Path) -> Option<PathBuf> {
    let mut cur = from.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_destroy_worktree() {
        // Requires running inside a git repo — skip in CI if not
        let repo = std::env::current_dir().unwrap();
        if !repo.join(".git").exists() && !repo.join("../../.git").exists() {
            eprintln!("skip: not in a git repo");
            return;
        }
        let backend = GitWorktreeBackend::new(
            // Walk up to find actual git root
            find_git_root(&repo).unwrap_or(repo),
        );
        let track = backend.create_track("test-parallel-track-tmp").unwrap();
        assert!(track.exists());
        backend.destroy(&track).unwrap();
        assert!(!track.exists());
    }
}
