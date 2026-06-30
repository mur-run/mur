pub mod cow;
pub mod detect;
pub mod git_worktree;
pub mod zfs_native;
pub mod zfs_socket;

use anyhow::Result;
use std::path::{Path, PathBuf};

pub trait ParallelBackend: Send + Sync {
    fn create_track(&self, name: &str) -> Result<PathBuf>;
    fn base_snapshot(&self, track: &Path) -> Result<String>;
    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>>;
    fn promote(&self, track: &Path, target: &Path) -> Result<()>;
    fn destroy(&self, track: &Path) -> Result<()>;
}

pub use detect::detect_backend;
pub use git_worktree::GitWorktreeBackend;
pub use zfs_native::ZfsNativeBackend;
pub use zfs_socket::ZfsSocketBackend;
