//! Project-access consent for `mur agent cli`.
//!
//! On launch, if the current working directory is outside the agent's
//! filesystem entitlements, offer to grant it (read+write) and persist it to
//! the profile. We grant the enclosing git repo root when there is one, so a
//! single consent covers subdirs and `<repo>/.worktrees/` checkouts. The running
//! agent's sandbox is sealed at startup (no hot reload), so a grant only takes
//! effect after a restart — we say so.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;

/// `~`-expanded user home, used to resolve `~/...` entitlement paths the same
/// way the runtime sandbox does (`dirs::home_dir()`, i.e. `$HOME` on unix).
fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Best-effort symlink-resolving canonicalize that falls back to the input when
/// the path does not exist (a configured root may point at a missing dir).
fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Is `target` inside any of `roots`, after `~` expansion + canonicalization?
fn path_within_roots(target: &Path, roots: &[String], home: &Path) -> bool {
    let target = canon(target);
    roots.iter().any(|r| {
        let root = if let Some(rest) = r.strip_prefix("~/") {
            home.join(rest)
        } else if r == "~" {
            home.to_path_buf()
        } else {
            PathBuf::from(r)
        };
        target.starts_with(canon(&root))
    })
}

/// Refuse to auto-grant an over-broad root: `/`, the user's whole home, or any
/// path shallower than two normal components (e.g. `/Users`, `/Volumes`, `/tmp`).
///
/// One judgement, one home: `launch_chain.rs` holds the union (it also knows
/// `/usr`, `/opt`, `/opt/homebrew` and volume roots), and the `perm` boundary
/// refuses with the same helper. See `reject_ungrantable` in `perm.rs`.
fn is_overbroad_root(p: &Path, home: &Path) -> bool {
    mur_agent_runtime::sandbox::launch_chain::is_overbroad_grant_root(p, home)
}

/// The git toplevel containing `cwd`, if any.
fn git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Ensure the agent may touch the current project dir; offer consent if not.
///
/// Returns `Ok(())` in every case the launch should continue (grant, decline,
/// non-interactive, or already-allowed). Only a profile read/write *error*
/// propagates.
pub(super) fn ensure_cwd_access(agent: &str) -> Result<()> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let home = user_home();

    // Read-only probe of the current entitlements.
    let (_path, profile) = super::super::load_profile_for_edit(agent)?;
    let fs = &profile.entitlements.filesystem;
    if path_within_roots(&cwd, &fs.write, &home) || path_within_roots(&cwd, &fs.read, &home) {
        return Ok(());
    }

    // Grant the enclosing repo root when there is one (covers subdirs +
    // `.worktrees/` checkouts); else the cwd itself.
    let grant = canon(&git_repo_root(&cwd).unwrap_or_else(|| cwd.clone()));

    if is_overbroad_root(&grant, &home) {
        eprintln!(
            "note: '{agent}' has no access to {} and it is too broad to grant automatically.\n      Grant a specific dir with: mur agent perm allow-write {agent} <path>",
            grant.display()
        );
        return Ok(());
    }

    // Non-interactive (piped) launch: never block on stdin; just inform.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "note: '{agent}' has no filesystem access to {}.\n      Grant it with: mur agent perm allow-write {agent} {}",
            grant.display(),
            grant.display()
        );
        return Ok(());
    }

    eprintln!("'{agent}' has no filesystem access to this project:");
    eprintln!("    {}", grant.display());
    eprint!("Grant read+write and remember it for this agent? [y/N] ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        eprintln!("Skipped — the agent cannot read or write here.");
        return Ok(());
    }

    // One load/mutate/save (vs. two perm calls) keeps a single restart warning.
    let grant_str = grant.to_string_lossy().to_string();
    let (ppath, mut profile) = super::super::load_profile_for_edit(agent)?;
    let fs = &mut profile.entitlements.filesystem;
    if !fs.read.iter().any(|p| p == &grant_str) {
        fs.read.push(grant_str.clone());
    }
    if !fs.write.iter().any(|p| p == &grant_str) {
        fs.write.push(grant_str.clone());
    }
    super::super::save_profile(&ppath, &mut profile)?;
    eprintln!("Granted {grant_str} (read + write).");
    // Prints the canonical "restart required for changes to take effect" line
    // when the agent is live — its sandbox is sealed at startup.
    super::super::perm::warn_if_running(agent);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_roots_matches_nested_and_tilde() {
        let home = PathBuf::from("/home/u");
        // exact + nested (paths need not exist: canon() falls back to literal)
        assert!(path_within_roots(
            Path::new("/home/u/proj/src"),
            &["/home/u/proj".into()],
            &home
        ));
        assert!(path_within_roots(
            Path::new("/home/u/proj"),
            &["/home/u/proj".into()],
            &home
        ));
        // tilde expansion resolves to the user home
        assert!(path_within_roots(
            Path::new("/home/u/proj/x"),
            &["~/proj".into()],
            &home
        ));
        // sibling outside the root
        assert!(!path_within_roots(
            Path::new("/home/u/other"),
            &["/home/u/proj".into()],
            &home
        ));
        // no roots at all
        assert!(!path_within_roots(Path::new("/home/u/proj"), &[], &home));
    }

    #[test]
    fn overbroad_blocks_shallow_and_home() {
        let home = PathBuf::from("/Users/d");
        assert!(is_overbroad_root(Path::new("/"), &home));
        assert!(is_overbroad_root(Path::new("/Users"), &home));
        assert!(is_overbroad_root(Path::new("/Volumes"), &home));
        assert!(is_overbroad_root(&home, &home));
        // a real project dir is fine
        assert!(!is_overbroad_root(
            Path::new("/Users/d/Projects/mur"),
            &home
        ));
        assert!(!is_overbroad_root(
            Path::new("/Volumes/Disk/Projects/mur"),
            &home
        ));
    }
}
