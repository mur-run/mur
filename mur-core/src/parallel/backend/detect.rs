#![allow(dead_code, unused_imports)]
use super::{
    GitWorktreeBackend, ParallelBackend,
    git_worktree::find_git_root,
    zfs_native::{ZfsNativeBackend, is_on_zfs_pool, zfs_cli_available},
    zfs_socket::{ZfsSocketBackend, connect_lima_socket, connect_orbstack_socket},
};
use std::path::Path;

/// Returns the best available `ParallelBackend` for `project`.
///
/// Detection order:
/// 1. ZFS native  — Linux/FreeBSD with `zfs` CLI + project on a ZFS pool
/// 2. OrbStack    — macOS; mur-zfs-agent socket forwarded by OrbStack
/// 3. Lima        — macOS/Linux; Lima VM named "mur-zfs" with socket forwarding
/// 4. WSL2        — Windows; mur-zfs-agent socket forwarded from WSL2 distro
/// 5. GitWorktree — always available, zero extra deps
pub fn detect_backend(project: &Path) -> Box<dyn ParallelBackend> {
    // 1. ZFS native (Linux/FreeBSD only)
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if zfs_cli_available() && is_on_zfs_pool(project) {
        return Box::new(ZfsNativeBackend::new(project.to_path_buf()));
    }

    // 2. OrbStack socket (macOS)
    #[cfg(target_os = "macos")]
    if let Ok(sock) = connect_orbstack_socket() {
        return Box::new(ZfsSocketBackend::new(sock, project.to_path_buf()));
    }

    // 3. Lima "mur-zfs" instance socket (macOS and Linux)
    #[cfg(not(windows))]
    if let Ok(sock) = connect_lima_socket("mur-zfs") {
        return Box::new(ZfsSocketBackend::new(sock, project.to_path_buf()));
    }

    // 4. WSL2 socket (Windows)
    #[cfg(windows)]
    if let Ok(sock) = super::zfs_socket::connect_wsl2_socket() {
        return Box::new(ZfsSocketBackend::new(sock, project.to_path_buf()));
    }

    // 5. Fallback: git worktrees (always available, zero deps)
    let root = find_git_root(project).unwrap_or_else(|| project.to_path_buf());
    Box::new(GitWorktreeBackend::new(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_backend_never_panics() {
        let backend = detect_backend(std::path::Path::new("."));
        let _ = backend.diff_files(std::path::Path::new("/nonexistent"), "snap");
    }

    /// Gate 4 latency benchmark. Run manually on ZFS-equipped Linux machine:
    /// ORT_STRATEGY=download cargo test -p mur-core \
    /// "parallel::backend::detect::tests::bench_create_track" \
    /// -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_create_track() {
        use std::time::Instant;
        let project = std::path::Path::new(".");
        let backend = detect_backend(project);
        let n = 10usize;
        let mut total = std::time::Duration::ZERO;
        for i in 0..n {
            let name = format!("gate4-bench-{i}");
            let start = Instant::now();
            let result = backend.create_track(&name);
            let elapsed = start.elapsed();
            if let Ok(track) = result {
                let _ = backend.destroy(&track);
            }
            total += elapsed;
            eprintln!("  iter {i}: {}ms", elapsed.as_millis());
        }
        let mean_ms = total.as_millis() / n as u128;
        eprintln!("Mean latency: {}ms", mean_ms);
    }
}
