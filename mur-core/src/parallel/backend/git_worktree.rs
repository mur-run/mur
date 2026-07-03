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
        // (The Time Machine local snapshot is taken ONCE in `create_tracks`,
        // before any track — not per-track here, and now for every backend.)
        // Track names come from user-editable fleet.yaml — reject anything
        // that could escape .worktrees/ (issue #546).
        if !mur_common::fleet::valid_fleet_name(name) {
            anyhow::bail!("invalid track name '{name}': use lowercase letters, digits, '-' or '_'");
        }
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

        // COW-copy build cache (e.g. target/) into the new track so agents
        // start with a warm cache instead of a cold build.
        super::cow::copy_build_cache(&self.repo_root, &path)?;

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
            // `git diff --name-only` lists deletions too — fs::copy on a
            // deleted file aborts promote halfway (issue #544). Propagate
            // the deletion instead.
            if src.exists() {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&src, &dst)?;
            } else if dst.exists() {
                std::fs::remove_file(&dst)?;
            }
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
        let repo = temp_git_repo();
        let backend = GitWorktreeBackend::new(repo.path().to_path_buf());
        let track = backend.create_track("test-parallel-track-tmp").unwrap();
        assert!(track.exists());
        backend.destroy(&track).unwrap();
        assert!(!track.exists());
    }

    #[test]
    fn create_track_rejects_traversal_names() {
        let td = tempfile::tempdir().unwrap();
        let b = GitWorktreeBackend::new(td.path().to_path_buf());
        for evil in ["../../etc", "a/b", "a\\b", "..", "UPPER", "sp ace"] {
            let err = b.create_track(evil).unwrap_err();
            assert!(
                err.to_string().contains("invalid track name"),
                "{evil}: {err}"
            );
        }
        assert!(!td.path().join("..").join("etc").exists());
    }

    fn temp_git_repo() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(args)
                .current_dir(td.path())
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        std::fs::write(td.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(td.path().join("gone.txt"), "gone").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        td
    }

    #[test]
    fn promote_propagates_deletions_instead_of_crashing() {
        let repo = temp_git_repo();
        let b = GitWorktreeBackend::new(repo.path().to_path_buf());
        let track = b.create_track("t1").unwrap();
        // Track deletes one file and modifies another, then commits.
        std::fs::remove_file(track.join("gone.txt")).unwrap();
        std::fs::write(track.join("keep.txt"), "changed").unwrap();
        let run_in = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(args)
                .current_dir(&track)
                .status()
                .unwrap();
            assert!(st.success());
        };
        run_in(&["add", "-A"]);
        run_in(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "track work",
        ]);
        // Promote into a fresh copy of the original tree.
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(target.path().join("gone.txt"), "gone").unwrap();
        b.promote(&track, target.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.path().join("keep.txt")).unwrap(),
            "changed"
        );
        assert!(
            !target.path().join("gone.txt").exists(),
            "deletion must propagate"
        );
    }
}
