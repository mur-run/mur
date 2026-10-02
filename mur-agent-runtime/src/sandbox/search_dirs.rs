//! The one directory list the seal resolves bare `spawn.allowed` names
//! against, and the MCP child `PATH` derived from it.
//!
//! Two resolutions used to answer different questions: the seal resolved
//! `allow-spawn node` across fixed directories (never `PATH`), while an MCP
//! server's `#!/usr/bin/env node` shebang — and `npx`, which forwards the
//! inherited `PATH` to whatever it launches — resolved `node` through the
//! `PATH` the runtime inherited. A version-manager shim dir early on that
//! `PATH` (nvm, volta, BitL, …) then produced a `node` the seal never granted:
//! `env: node: Operation not permitted`, rc 126.
//!
//! The fix pins the child side, not the seal side: the allowlist's inputs stay
//! fixed directories (the environment never decides what a granted name
//! means), and the child's `PATH` puts those same directories first so a
//! shebang lookup lands on the binary the seal granted.
//!
//! Trade-off, stated plainly: under an allowlist an MCP server runs the `node`
//! (or `python3`, …) from these fixed directories, not the version a user
//! selected through a shim dir earlier on their own `PATH`. Granting the shim
//! binary's absolute path does NOT change that — a shebang never consults the
//! allowlist, only `PATH`. Using another interpreter means overriding the
//! server's whole launch chain with absolute paths.

use mur_common::agent::SpawnMode;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Directories the seal searches to resolve bare `spawn.allowed` names
/// (Issue 17): the shared exec_dirs list (Homebrew/Cargo/user-local, kept in
/// lockstep with the bash tool's PATH augmentation), the standard system exec
/// dirs, and — existence-checked, no subprocess spawned — the active
/// Xcode/CommandLineTools developer dirs and every rustup toolchain `bin`.
///
/// The active developer dir is read directly from the
/// `/var/db/xcode_select_link` symlink target (exactly what `xcode-select -p`
/// resolves); reading the symlink avoids spawning a subprocess, which the
/// sandboxed exec chain cannot rely on being permitted.
///
/// Rustup: the `cargo`/`rustc` shims in `~/.cargo/bin` are PROXIES that
/// re-exec the active toolchain's real binary under
/// `<rustup_home>/toolchains/<toolchain>/bin/`; Seatbelt must see that real
/// exec path too.
pub fn spawn_search_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = crate::exec_dirs::standard_exec_dirs();
    dirs.extend(super::policy::system_exec_paths(home));
    if let Ok(xcode_dir) = std::fs::read_link("/var/db/xcode_select_link") {
        let usr_bin = xcode_dir.join("usr/bin");
        if usr_bin.exists() {
            dirs.push(usr_bin);
        }
    }
    let clt_usr_bin = PathBuf::from("/Library/Developer/CommandLineTools/usr/bin");
    if clt_usr_bin.exists() {
        dirs.push(clt_usr_bin);
    }
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".rustup"));
    if let Ok(entries) = std::fs::read_dir(rustup_home.join("toolchains")) {
        for entry in entries.flatten() {
            let bin_dir = entry.path().join("bin");
            if bin_dir.is_dir() {
                dirs.push(bin_dir);
            }
        }
    }
    dirs
}

/// `PATH` for an MCP server child (and for resolving its command, so B0
/// admission hashes the binary that is exec'd).
///
/// Under an allowlist (`Allowlist` / `Strict`) the seal's search dirs come
/// first, then the augmented inherited `PATH` minus duplicates — so a bare
/// name resolves to the binary the seal granted, while tools that live only
/// in the inherited `PATH` stay reachable. `Any` / `None` have no allowlist to
/// agree with, so they keep the augmented inherited `PATH` unchanged.
pub fn mcp_child_path(mode: SpawnMode) -> OsString {
    let inherited = mur_common::exec::augmented_path_var();
    match mode {
        SpawnMode::Allowlist | SpawnMode::Strict => {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
            seal_first_path(&spawn_search_dirs(&home), &inherited)
        }
        SpawnMode::Any | SpawnMode::None => inherited,
    }
}

/// `seal_dirs` that exist, in order, then every `inherited` entry not already
/// present. Pure, so the ordering contract is unit-testable.
pub(crate) fn seal_first_path(seal_dirs: &[PathBuf], inherited: &std::ffi::OsStr) -> OsString {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in seal_dirs {
        if d.is_dir() && !out.contains(d) {
            out.push(d.clone());
        }
    }
    for d in std::env::split_paths(inherited) {
        if !d.as_os_str().is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    std::env::join_paths(out).unwrap_or_else(|_| inherited.to_os_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_bin(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn seal_dir_node_wins_over_a_shim_dir_first_on_inherited_path() {
        let seal = tempfile::tempdir().unwrap();
        let shim = tempfile::tempdir().unwrap();
        let granted = fake_bin(seal.path(), "node");
        fake_bin(shim.path(), "node");
        let inherited = std::env::join_paths([shim.path(), Path::new("/usr/bin")]).unwrap();

        let path = seal_first_path(&[seal.path().to_path_buf()], &inherited);
        let hit = mur_common::exec::resolve_command_in(&path, "node").unwrap();
        assert_eq!(hit, granted.canonicalize().unwrap());
    }

    #[test]
    fn inherited_only_tools_stay_reachable_and_are_not_duplicated() {
        let seal = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let only_here = fake_bin(extra.path(), "uvx");
        let inherited = std::env::join_paths([extra.path(), seal.path()]).unwrap();

        let path = seal_first_path(&[seal.path().to_path_buf()], &inherited);
        let parts: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(
            parts,
            vec![seal.path().to_path_buf(), extra.path().to_path_buf()]
        );
        let hit = mur_common::exec::resolve_command_in(&path, "uvx").unwrap();
        assert_eq!(hit, only_here.canonicalize().unwrap());
    }

    #[test]
    fn missing_seal_dirs_are_skipped() {
        let inherited = OsString::from("/usr/bin");
        let path = seal_first_path(&[PathBuf::from("/definitely/not/here")], &inherited);
        assert_eq!(path, inherited);
    }

    #[test]
    fn any_mode_keeps_the_augmented_inherited_path() {
        assert_eq!(
            mcp_child_path(SpawnMode::Any),
            mur_common::exec::augmented_path_var()
        );
    }
}
