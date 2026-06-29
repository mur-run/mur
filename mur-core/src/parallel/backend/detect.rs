#![allow(dead_code, unused_imports)]
use super::{GitWorktreeBackend, ParallelBackend, git_worktree::find_git_root};
use std::path::Path;

/// Returns the best available backend. Always falls back to GitWorktreeBackend.
/// P2 will add ZFS socket detection above the fallback.
pub fn detect_backend(project: &Path) -> Box<dyn ParallelBackend> {
    let root = find_git_root(project).unwrap_or_else(|| project.to_path_buf());
    Box::new(GitWorktreeBackend::new(root))
}
