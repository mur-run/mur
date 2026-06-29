pub mod detect;
pub mod git_worktree;

use std::path::{Path, PathBuf};
use anyhow::Result;

pub trait ParallelBackend: Send + Sync {
    fn create_track(&self, name: &str) -> Result<PathBuf>;
    fn base_snapshot(&self, track: &Path) -> Result<String>;
    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>>;
    fn promote(&self, track: &Path, target: &Path) -> Result<()>;
    fn destroy(&self, track: &Path) -> Result<()>;
}

pub use detect::detect_backend;
pub use git_worktree::GitWorktreeBackend;
