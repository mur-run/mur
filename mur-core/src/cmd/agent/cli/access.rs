//! Project-access consent for `mur agent cli`.
//!
//! On launch, if the current working directory is outside the agent's
//! filesystem entitlements, offer to grant it (read+write) and persist it to
//! the profile. We grant the enclosing git repo root when there is one, so a
//! single consent covers subdirs and `<repo>/.worktrees/` checkouts. The
//! agent's sandbox is sealed at startup (no hot reload), so the consent runs
//! BEFORE autostart (#1939): a stopped agent then seals with the grant already
//! in place, and a live one is offered a restart right there.

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
    let out = mur_common::repo_walk::bound_git(&mut std::process::Command::new("git"))
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

/// The path to offer for `cwd`: its git toplevel when there is one (covers
/// subdirs + `.worktrees/` checkouts), else `cwd` itself.
///
/// Never a toplevel that is or contains MUR's own home (#2448): `~/.mur` is a
/// git repo, so widening from `~/.mur/artifacts/x` would offer `~/.mur`, which
/// the sandbox always refuses. Then the cwd itself is the candidate.
fn grant_target(cwd: &Path, repo_root: Option<PathBuf>, mur_home: &Path) -> PathBuf {
    let mur_home = canon(mur_home);
    match repo_root.map(|r| canon(&r)) {
        Some(root) if !mur_home.starts_with(&root) => root,
        _ => canon(cwd),
    }
}

/// Why the sandbox of the agent at `agent_home` would refuse `grant`, via the
/// runtime's own seal-time rules — one judgement, not a copied path list.
fn sandbox_refusal(agent_home: &Path, grant: &Path) -> Option<String> {
    mur_agent_runtime::sandbox::protected::grant_refusal(agent_home, grant)
}

/// What [`ensure_cwd_access`] did to the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CwdAccess {
    /// Nothing written: already allowed, declined, non-interactive, or refused.
    Unchanged,
    /// A new read+write grant on this path was persisted to the profile.
    Granted(PathBuf),
}

impl CwdAccess {
    /// A grant only reaches a sandbox sealed after it was written, so a live
    /// agent must be restarted; a stopped one picks it up when it starts.
    pub(super) fn needs_restart(&self, agent_live: bool) -> bool {
        matches!(self, CwdAccess::Granted(_)) && agent_live
    }
}

/// The undo for a grant the agent then failed to start with: the restart
/// detail already quotes the runtime's `Error:` line; this names the fix.
fn undo_grant_hint(agent: &str, grant: &Path) -> String {
    let g = grant.display();
    format!(
        "  The new grant may be the cause. Undo it with:\n    \
         mur agent perm remove-path {agent} write {g}\n    \
         mur agent perm remove-path {agent} read {g}\n  \
         then: mur agent start {agent}"
    )
}

/// Ensure the agent may touch the current project dir; offer consent if not.
///
/// Returns `Ok(_)` in every case the launch should continue (grant, decline,
/// non-interactive, or already-allowed). Only a profile read/write *error*
/// propagates. Call it BEFORE starting the agent (#1939).
pub(super) fn ensure_cwd_access(agent: &str) -> Result<CwdAccess> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(CwdAccess::Unchanged);
    };
    let home = user_home();

    // Read-only probe of the current entitlements.
    let (_path, profile) = super::super::load_profile_for_edit(agent)?;
    let fs = &profile.entitlements.filesystem;
    if path_within_roots(&cwd, &fs.write, &home) || path_within_roots(&cwd, &fs.read, &home) {
        return Ok(CwdAccess::Unchanged);
    }

    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let grant = grant_target(&cwd, git_repo_root(&cwd), &mur_home);

    if is_overbroad_root(&grant, &home) {
        eprintln!(
            "note: '{agent}' has no access to {} and it is too broad to grant automatically.\n      Grant a specific dir with: mur agent perm allow-write {agent} <path>",
            grant.display()
        );
        return Ok(CwdAccess::Unchanged);
    }

    // Never offer what the next seal refuses (#2448): ask the sandbox itself.
    let agent_home = mur_home.join("agents").join(agent);
    if let Some(why) = sandbox_refusal(&agent_home, &grant) {
        eprintln!(
            "note: '{agent}' has no access to {} and the sandbox cannot grant it:\n      {why}\n      Grant a narrower dir with: mur agent perm allow-write {agent} <path>",
            grant.display()
        );
        return Ok(CwdAccess::Unchanged);
    }

    // Non-interactive (piped) launch: never block on stdin; just inform.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "note: '{agent}' has no filesystem access to {}.\n      Grant it with: mur agent perm allow-write {agent} {}",
            grant.display(),
            grant.display()
        );
        return Ok(CwdAccess::Unchanged);
    }

    eprintln!("'{agent}' has no filesystem access to this project:");
    eprintln!("    {}", grant.display());
    eprint!("Grant read+write and remember it for this agent? [y/N] ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        eprintln!("Skipped — the agent cannot read or write here.");
        return Ok(CwdAccess::Unchanged);
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
    Ok(CwdAccess::Granted(grant))
}

/// After a fresh grant on an agent that is ALREADY running, its sealed sandbox
/// cannot see the new path. Offer to restart it now (default yes) instead of
/// printing a warning the TUI immediately paints over (#1939).
pub(super) fn offer_restart_after_grant(agent: &str, grant: &Path) -> Result<()> {
    eprint!(
        "'{agent}' is running with its old sandbox; restart it now so the grant applies? [Y/n] "
    );
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no") {
        eprintln!("Not restarted — run `mur agent restart {agent}` before working here.");
        return Ok(());
    }
    eprintln!("Restarting '{agent}'…");
    let r = crate::cmd::agent::restart_quiet(agent)?;
    for (failed, note) in &r.notes {
        if *failed {
            eprintln!("✗ {note}");
        }
    }
    if !r.ok {
        eprintln!(
            "✗ {} — run `mur agent restart {agent}` and retry.",
            r.detail
        );
        eprintln!("{}", undo_grant_hint(agent, grant));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fresh_grant_on_a_live_agent_needs_a_restart() {
        // #1939: a grant written while the agent is stopped is picked up when
        // it starts; only a live (already sealed) sandbox needs a bounce.
        let g = CwdAccess::Granted(PathBuf::from("/p"));
        assert!(g.needs_restart(true));
        assert!(!g.needs_restart(false));
        assert!(!CwdAccess::Unchanged.needs_restart(true));
        assert!(!CwdAccess::Unchanged.needs_restart(false));
    }

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

    #[test]
    fn a_git_toplevel_at_mur_home_is_not_the_grant_target() {
        // #2448: `~/.mur` is itself a git repo. Widening from a subdir must
        // not land on MUR's home; the cwd itself is offered instead.
        let mh = PathBuf::from("/nonexistent-2448/.mur");
        let cwd = mh.join("artifacts/mur/run");
        assert_eq!(grant_target(&cwd, Some(mh.clone()), &mh), cwd);
        // A toplevel ABOVE mur_home (a dotfiles repo at ~) contains it too.
        let above = PathBuf::from("/nonexistent-2448");
        assert_eq!(grant_target(&cwd, Some(above), &mh), cwd);
        // An ordinary project still widens to its repo root.
        let proj = PathBuf::from("/nonexistent-2448/Projects/app");
        assert_eq!(
            grant_target(&proj.join("src"), Some(proj.clone()), &mh),
            proj
        );
        assert_eq!(grant_target(&proj, None, &mh), proj);
    }

    #[test]
    fn the_prompt_candidate_is_judged_by_the_sandbox_rules() {
        // #2448: the shared runtime check, not a duplicated list, decides.
        let tmp = tempfile::tempdir().unwrap();
        let mh = tmp.path().join(".mur");
        let agent_home = mh.join("agents").join("a");
        let narrow = mh.join("artifacts").join("x");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&narrow).unwrap();
        let why = sandbox_refusal(&agent_home, &mh).expect("~/.mur is refused");
        assert!(why.contains("channel heads"), "{why}");
        assert_eq!(sandbox_refusal(&agent_home, &narrow), None);
    }

    #[test]
    fn a_failed_restart_names_the_undo_for_the_grant() {
        let h = undo_grant_hint("card", Path::new("/g/x"));
        assert!(
            h.contains("mur agent perm remove-path card write /g/x"),
            "{h}"
        );
        assert!(
            h.contains("mur agent perm remove-path card read /g/x"),
            "{h}"
        );
    }
}
