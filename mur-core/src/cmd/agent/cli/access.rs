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

/// `<mur_home>` for `<mur_home>/agents/<name>`: the derivation the seal uses
/// (`sandbox::seal_from_profile`), so both judge the same protected tree.
fn mur_home_of(agent_home: &Path) -> PathBuf {
    agent_home
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(mur_common::home::mur_home)
}

/// The path to offer: the repo root, unless that root is MUR's own home or
/// contains it (#2448). `~/.mur` is itself a git repo, so widening from any
/// directory under it would offer the whole MUR home, which the sandbox
/// always refuses. Fall back to the cwd, which is then vetted like any grant.
fn pick_grant(cwd: &Path, repo_root: Option<PathBuf>, mur_home: &Path) -> PathBuf {
    let mur_home = canon(mur_home);
    match repo_root {
        Some(root) if !mur_home.starts_with(&root) => root,
        _ => cwd.to_path_buf(),
    }
}

/// Why the sandbox would refuse `grant` as a read+write entitlement, using the
/// seal's own checks rather than a copy of their path lists: the E2
/// channel-heads refusal, the launch-chain write partition, and the
/// credential-store read partition.
fn seal_refusal(grant: &Path, agent_home: &Path) -> Option<String> {
    use mur_agent_runtime::sandbox::{launch_chain::LaunchChain, protected};

    if let Some(err) = protected::write_grant_refusal(grant, &mur_home_of(agent_home)) {
        return Some(err.to_string());
    }
    let chain = LaunchChain::new(agent_home);
    let one = [grant.to_path_buf()];
    if !chain.partition_grants(&one).1.is_empty() {
        return Some(format!(
            "write grant {} reaches MUR's launch chain (agent profiles, pins, the runtime binary)",
            grant.display()
        ));
    }
    if !chain.partition_read_grants(&one).1.is_empty() {
        return Some(format!(
            "read grant {} reaches the credential store or a signing key",
            grant.display()
        ));
    }
    None
}

/// What [`ensure_cwd_access`] did to the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CwdAccess {
    /// Nothing written: already allowed, declined, non-interactive, or too broad.
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

    // Grant the enclosing repo root when there is one (covers subdirs +
    // `.worktrees/` checkouts); else the cwd itself.
    let agent_home = super::super::resolve_mur_home()?.join("agents").join(agent);
    let mur_home = mur_home_of(&agent_home);
    let grant = pick_grant(
        &canon(&cwd),
        git_repo_root(&cwd).map(|r| canon(&r)),
        &mur_home,
    );

    if is_overbroad_root(&grant, &home) {
        eprintln!(
            "note: '{agent}' has no access to {} and it is too broad to grant automatically.\n      Grant a specific dir with: mur agent perm allow-write {agent} <path>",
            grant.display()
        );
        return Ok(CwdAccess::Unchanged);
    }

    // Never offer a grant the sandbox refuses at seal time (#2448): accepting
    // it would write a profile the agent cannot start with.
    if let Some(why) = seal_refusal(&grant, &agent_home) {
        eprintln!(
            "note: '{agent}' has no access to {}, and MUR's sandbox never allows that grant:\n      {why}\n      Grant a narrower dir with: mur agent perm allow-write {agent} <path>",
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
        // A refusal is not transient: retrying with the same profile fails
        // the same way. Name the undo for the grant just written (#2448).
        if let Some(fix) = refusal_fix(&r.detail, agent, grant) {
            eprintln!("{fix}");
        }
    }
    Ok(())
}

/// The undo for a grant the sandbox refused on restart, or `None` when the
/// restart failed for some other reason.
fn refusal_fix(detail: &str, agent: &str, grant: &Path) -> Option<String> {
    detail.contains("sandbox profile refused").then(|| {
        let g = grant.display();
        format!(
            "  The sandbox refuses the grant just written. Undo it with:\n    mur agent perm remove-path {agent} write {g}\n    mur agent perm remove-path {agent} read {g}\n    mur agent start {agent}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fresh_grant_on_a_live_agent_needs_a_restart() {
        // #1939: a grant written while the agent is stopped is picked up when
        // it starts; only a live (already sealed) sandbox needs a bounce.
        let g = CwdAccess::Granted(PathBuf::from("/work/proj"));
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
    fn a_repo_root_that_is_the_mur_home_is_not_offered() {
        // #2448: `~/.mur` is a git repo, so widening from a dir inside it
        // must fall back to the cwd instead of offering MUR's whole home.
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = canon(tmp.path());
        let cwd = mur_home.join("artifacts/mur/run");
        assert_eq!(pick_grant(&cwd, Some(mur_home.clone()), &mur_home), cwd);
        // A repo that merely contains the MUR home is just as wrong.
        let outer = mur_home.parent().unwrap().to_path_buf();
        assert_eq!(pick_grant(&cwd, Some(outer), &mur_home), cwd);
        // An ordinary project still widens to its root.
        let proj = PathBuf::from("/work/proj");
        assert_eq!(
            pick_grant(&proj.join("src"), Some(proj.clone()), &mur_home),
            proj
        );
        assert_eq!(pick_grant(&proj, None, &mur_home), proj);
    }

    #[test]
    fn the_prompt_refuses_what_the_seal_refuses() {
        // #2448: the pre-check must agree with `check_protected_writes`, not
        // with a separate path list.
        use mur_agent_runtime::sandbox::protected::write_grant_refusal;
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = canon(tmp.path());
        let agent_home = mur_home.join("agents/alice");
        std::fs::create_dir_all(&agent_home).unwrap();

        for g in [mur_home.clone(), mur_home.join("channel-heads/x")] {
            assert!(write_grant_refusal(&g, &mur_home).is_some());
            assert!(seal_refusal(&g, &agent_home).is_some(), "{}", g.display());
        }
        // The credential store is refused on the read side too.
        assert!(seal_refusal(&mur_home.join("secrets"), &agent_home).is_some());
        // A dir under the MUR home that reaches nothing protected is fine.
        let ok = mur_home.join("artifacts/mur/run");
        std::fs::create_dir_all(&ok).unwrap();
        assert!(write_grant_refusal(&ok, &mur_home).is_none());
        assert!(seal_refusal(&ok, &agent_home).is_none());
    }

    #[test]
    fn a_sandbox_refusal_on_restart_names_the_undo() {
        let g = Path::new("/work/proj");
        let fix = refusal_fix(
            "'a' not confirmed running — its startup failed: Error: sandbox profile refused: write grant",
            "a",
            g,
        )
        .unwrap();
        assert!(fix.contains("mur agent perm remove-path a write /work/proj"));
        assert!(fix.contains("mur agent perm remove-path a read /work/proj"));
        assert!(refusal_fix("'a' not confirmed running", "a", g).is_none());
    }
}
