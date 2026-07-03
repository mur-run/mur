//! Shared directory list for locating developer-toolchain binaries beyond
//! the OS's minimal default `PATH` (Homebrew, Cargo, user-local installs).
//!
//! Used by both the bash tool (to build the `PATH` env var for spawned
//! commands — dogfood issue 1) and the sandbox policy builder (to search
//! for `spawn.allowed` binaries — Issue 17), so the two stay in lockstep:
//! a directory that augments one but not the other reproduces exactly the
//! "on PATH but kernel-denied" bug this shared module exists to prevent.

use std::path::PathBuf;

/// Directories that must be searched (or `PATH`-augmented) for a
/// service-manager launch with a minimal default `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`), covering Homebrew, Cargo, and
/// user-local installs — even when the agent-runtime process itself was
/// launched by launchd/systemd rather than an interactive shell.
pub(crate) fn standard_exec_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs
}
