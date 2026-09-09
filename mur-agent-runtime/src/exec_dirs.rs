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

/// Absolute path to the `mur` CLI that belongs to THIS runtime build.
///
/// Resolved as a sibling of the running binary rather than through `PATH`.
/// Three reasons, all of which bit at once on 2026-09-09:
///
/// 1. **It exists at seal time.** The sandbox resolves `spawn.allowed` names
///    by scanning the filesystem when the agent starts; `PATH` is walked at
///    exec time. A `mur` that appears in a higher-priority `PATH` directory
///    *after* the seal (here: a `brew` symlink written 39 seconds later) is
///    the one that gets exec'd and was never allowlisted — EPERM, under a
///    profile that plainly says `mur` is allowed. Nothing about the grant is
///    wrong; the two resolutions simply answered different questions.
/// 2. **Versions cannot skew.** A runtime from one install paired with a
///    `mur` from another is a recurring failure; siblings ship together.
/// 3. **One derivation.** The sandbox grant and the spawn read the same
///    value here, so they cannot disagree again.
///
/// `MUR_BIN` still wins — it is the deliberate override, and the sandbox
/// policy reads this same function, so an overridden path is granted too.
/// Falls back to the bare name only when there is no better answer.
pub(crate) fn mur_cli() -> PathBuf {
    if let Some(v) = std::env::var_os("MUR_BIN") {
        return PathBuf::from(v);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mur")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("mur"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mur_cli_prefers_an_explicit_override() {
        // Not a PATH lookup and not a guess: whatever MUR_BIN names is what
        // both the grant and the spawn use.
        unsafe { std::env::set_var("MUR_BIN", "/nowhere/mur") };
        assert_eq!(mur_cli(), PathBuf::from("/nowhere/mur"));
        unsafe { std::env::remove_var("MUR_BIN") };
    }

    #[test]
    fn mur_cli_is_absolute_or_the_bare_name() {
        // The bare name is the documented last resort; anything else must be
        // absolute, or the sandbox grant cannot name it.
        let p = mur_cli();
        assert!(p.is_absolute() || p == std::path::Path::new("mur"), "{p:?}");
    }
}
