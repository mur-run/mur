//! Shared call-time filesystem-entitlement gate for the mutating file tools
//! (issue #591 PR2). `deny` always wins; writes require a `write` grant.
//! read_file keeps its own equivalent check — dedup is a follow-up.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use mur_common::agent::FilesystemEntitlement;

use crate::tools::ToolError;

/// Session-wide current working directory shared by the `bash` tool and the
/// file tools (`read_file`/`write_file`/`edit_file`), so a relative path
/// resolves against the same base no matter which tool the agent reached for.
///
/// Dogfood bug: `bash pwd` showed one directory while `read_file rel/path`
/// resolved against `agent_home`, because the two tools never shared a base.
/// The `bash` tool updates this only when it is given an explicit `cwd`
/// argument; a `cd` *inside* a spawned subprocess cannot be observed by the
/// parent and is deliberately NOT tracked (that's called out in both tools'
/// descriptions). Readers take a cheap snapshot (`current()`), never holding
/// the lock across an `.await`.
#[derive(Clone)]
pub struct SessionCwd(Arc<RwLock<PathBuf>>);

impl SessionCwd {
    /// Create a session cwd seeded with the agent home (the historical base).
    pub fn new(initial: PathBuf) -> Self {
        Self(Arc::new(RwLock::new(initial)))
    }

    /// Snapshot the current base. Clones the `PathBuf` and releases the read
    /// lock immediately, so callers never hold a guard across `.await`.
    pub fn current(&self) -> PathBuf {
        self.0.read().expect("session cwd lock poisoned").clone()
    }

    /// Update the session base (called by `bash` when given explicit `cwd`).
    pub fn set(&self, dir: PathBuf) {
        *self.0.write().expect("session cwd lock poisoned") = dir;
    }
}

/// The accepted `path` forms, worded for tool schemas. Single source of truth
/// for every path-taking tool's parameter description — do NOT re-word it per
/// tool.
///
/// It lives beside `resolve_path` because it documents exactly what that
/// function accepts. A schema advertising fewer forms than the resolver
/// implements is not a cosmetic gap: it made the model expand `~` itself, and
/// since nothing tells it what `~` is, it invented a username and wrote to
/// `/Users/i/` and `/Users/lidj/` while the real home was `/Users/david`.
/// The system prompt's output-locations rule tells agents to write under
/// `~/.mur/artifacts/`, so hiding `~` here put two MUR-authored strings in
/// direct contradiction. Enforced by `tools::tests::path_taking_tools_advertise_tilde`.
pub(crate) const PATH_FORMS: &str = "absolute, `~`-relative (expanded to your real home — write `~` literally, \
     never guess a home path), or relative to the session cwd";

/// Resolve a tool-supplied path: expand a leading `~`/`~/` to the user's
/// home, keep absolute paths as-is, and join relative paths onto
/// `working_dir`. Entitlement checks run on the canonicalized result,
/// so expansion never widens what a grant covers.
pub(crate) fn resolve_path(working_dir: &Path, raw: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if raw == "~" {
            return home;
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        working_dir.join(raw)
    }
}

/// Re-export of the canonical guidance string, which now lives in
/// `mur-common` so `mur agent doctor` and the runtime tools share one source
/// of truth (issue #1). Do NOT inline the wording here — keep it a re-export.
pub use mur_common::REMOVABLE_VOLUME_EPERM_HINT;

/// True when `err` is an EPERM ("Operation not permitted", raw `os error 1`)
/// against a path under `/Volumes/*`. This is the exact macOS Full-Disk-Access
/// failure mode on removable/external volumes — distinct from a plain
/// `PermissionDenied` (EACCES) so we don't hijack ordinary permission errors.
pub fn is_removable_volume_eperm(path: &Path, err: &std::io::Error) -> bool {
    let is_eperm = err.raw_os_error() == Some(1);
    is_eperm && path.starts_with("/Volumes/")
}

/// Format an I/O error message, appending [`REMOVABLE_VOLUME_EPERM_HINT`] when
/// the failure is the macOS Full-Disk-Access EPERM on a `/Volumes/*` path.
/// `base` (the resolution base) is preserved verbatim so existing "relative to
/// session cwd" diagnostics stay intact. Used by every file tool's error path.
pub fn format_io_error(verb: &str, path: &Path, base: &Path, err: &std::io::Error) -> String {
    let mut msg = format!(
        "cannot {verb} {}: {err} (relative to session cwd {})",
        path.display(),
        base.display()
    );
    if is_removable_volume_eperm(path, err) {
        msg.push_str("\n\n");
        msg.push_str(REMOVABLE_VOLUME_EPERM_HINT);
    }
    msg
}

/// Adapt a raw profile entitlement for the file tools: grant the agent its own
/// home, then carve the self-protected files back out.
///
/// **Why the write grant is here and not in the profile.** The sandbox builder
/// force-grants `agent_home` unconditionally ("runtime cannot function without
/// it" — [`crate::sandbox::policy::SandboxPolicy::from_entitlements`]), but the
/// file tools were handed `profile.entitlements.filesystem` verbatim. So the
/// kernel allowed a write the tool gate refused, and an agent whose profile did
/// not happen to list its own home could not write there at all:
/// `path not write-entitled: ~/.mur/agents/<name>/… (grant it via mur agent
/// perm allow-write)`. Observed 2026-09-13, where it pushed the agent to reach
/// for `/tmp` instead and trip the tool-withdrawal path.
///
/// **Why only `agent_home`.** The sandbox force-grants three more paths —
/// `<mur_home>/channels`, `<mur_home>/index/channels`, `open-items.jsonl` —
/// and those deliberately stay out. The two layers answer different questions:
/// the kernel policy bounds what this PROCESS may write (the runtime appends
/// signed channel events itself), while this gate bounds what the MODEL may
/// write through `write_file`/`edit_file`. Aligning the lists wholesale would
/// hand a prompt-injected agent a way to forge channel events and corrupt the
/// channels read-model. `open_item` does not route through this gate at all.
///
/// Issue #712: the agent's own `profile.yaml` and `identity.key` are appended
/// to the deny list, so the gate refuses them even under a grant covering the
/// whole agent dir — including the one added just above. `deny` is checked
/// before `write` in [`check_write_entitlement`], so the carve-out wins.
/// On Linux this gate is the enforcement point (Landlock cannot express
/// deny-within-allow); on macOS it fronts the SBPL kernel deny with a clear
/// error instead of a raw EPERM.
///
/// Issue #007: this list is consulted by the READ gate too (`deny` is one
/// list), which made the agent's own `profile.yaml` unreadable — while the
/// sibling profile next door stayed readable, so the rule bought no
/// confidentiality and only cost the agent the ability to answer "what am I
/// allowed to do?". Reads are now carved back via
/// [`self_protected_write_only`]: `identity.key` remains read-denied through
/// `LaunchChain::protects_read` (signing authority), `profile.yaml` does not.
pub(crate) fn for_file_tools(
    mut fs: FilesystemEntitlement,
    agent_home: &Path,
) -> FilesystemEntitlement {
    let home = agent_home.to_string_lossy().into_owned();
    if !fs.write.contains(&home) {
        fs.write.push(home);
    }
    // `<mur_home>/artifacts/<agent>`: the sandbox grants it (see
    // `from_entitlements`) because the system prompt's output-locations rule
    // sends every agent there for reports and scratch output. Unlike the other
    // runtime-owned grants this one is FOR the model, so it belongs in this
    // gate too — otherwise the kernel allows the write and `write_file`
    // refuses it, which is how an agent ended up probing `/tmp` instead.
    if let (Some(mur_home), Some(agent_name)) = (
        agent_home.parent().and_then(|p| p.parent()),
        agent_home.file_name(),
    ) {
        let mine = mur_home
            .join("artifacts")
            .join(agent_name)
            .to_string_lossy()
            .into_owned();
        if !fs.write.contains(&mine) {
            fs.write.push(mine);
        }
    }
    for f in crate::sandbox::policy::SELF_PROTECTED_AGENT_FILES {
        let p = agent_home.join(f).to_string_lossy().into_owned();
        if !fs.deny.contains(&p) {
            fs.deny.push(p);
        }
    }
    fs
}

/// True when `canonical` falls under any of `roots`.
///
/// Roots go through the SAME `~` expansion the sandbox builder applies
/// ([`crate::sandbox::policy::expand_entitlement_path`]) before
/// canonicalization. Without it the two layers disagreed about one grant:
/// the kernel policy expanded `~/...` and let the access through, while this
/// gate compared a literal `~/...` root (whose `canonicalize` always fails,
/// falling back to the un-expanded string) and answered "not entitled".
/// A `~`-written *deny* entry — the very form `detect_warnings` suggests for
/// `~/.ssh` — was silently inert here for the same reason.
///
/// Windows note: `canonicalize` returns a `\\?\` UNC path and succeeds only
/// for a path that exists, so a present root canonicalizes to a different
/// spelling than an absent one. Callers pass an already-canonical path, so a
/// live grant matches; a grant naming a missing path fails closed — and
/// `mur agent perm` rejects those up front (`reject_dead_grant`).
pub(crate) fn under_any(roots: &[String], canonical: &Path) -> bool {
    roots.iter().any(|r| {
        let expanded = crate::sandbox::policy::expand_entitlement_path(r);
        let root = std::fs::canonicalize(&expanded).unwrap_or(expanded);
        canonical.starts_with(&root)
    })
}

/// Allow-side membership test: `under_any`, plus one derived hop for a path
/// that lives in a git worktree of a granted checkout (issue #004).
///
/// ## Why this is a separate function, and why `deny` must never call it
///
/// Prefix matching cannot see that `~/work/repo-feat` and `~/work/repo` are the
/// same project, so a user who granted the repo they work in was refused the
/// moment the work moved into a worktree, and had to grant the same repo a
/// second time under a different path. That is the whole of #004.
///
/// The derivation is one-way ALLOW-side only. Applying it to `deny` would be
/// fail-open in the most dangerous direction: `deny ~/secrets` must keep
/// meaning exactly `~/secrets`, and "this path's main checkout is denied"
/// widening into "so is every worktree" is a rule the user never wrote. Deny
/// stays literal, and because `check_write_entitlement`/`check_entitlement`
/// evaluate `deny` FIRST and unconditionally, a denied path inside a derived
/// worktree grant is still refused.
///
/// The derived root is never written back to `profile.yaml`: an entitlement
/// the user did not type must not silently appear in the file they audit.
/// It is recomputed from git's on-disk metadata on every call, so removing the
/// worktree removes the access with no stale grant left behind.
pub(crate) fn under_any_or_worktree(roots: &[String], canonical: &Path) -> bool {
    if under_any(roots, canonical) {
        return true;
    }
    // Not directly granted — ask git whether this path is a worktree of
    // something that IS granted. Read-only, no subprocess (this gate is what
    // decides whether spawning is permitted in the first place).
    match mur_common::worktree::main_checkout_of(canonical) {
        Some(main) => {
            let main = std::fs::canonicalize(&main).unwrap_or(main);
            under_any(roots, &main)
        }
        None => false,
    }
}

/// The self-protected files whose deny is WRITE-only (issue #007).
///
/// `for_file_tools` pushes `SELF_PROTECTED_AGENT_FILES` onto `fs.deny`, and
/// `deny` is one list shared by both gates — so the #712 write rule silently
/// became a read rule too. This names the subset the READ gate must skip.
///
/// `identity.key` is deliberately absent: it stays read-denied, but through
/// `LaunchChain::protects_read`, which sits *before* the lists and cannot be
/// satisfied by any entitlement. That is the right layer for "reading this is
/// holding a credential"; the deny list is not, because a user-written grant
/// could otherwise be argued to override it.
fn self_protected_write_only(agent_home: &Path) -> Vec<PathBuf> {
    vec![agent_home.join("profile.yaml")]
}

/// Deny-list membership for the READ gate: `under_any`, minus the entries that
/// exist only to express a WRITE rule (issue #007).
///
/// Takes `agent_home` rather than filtering by file name so a user who
/// explicitly wrote `deny <some other agent>/profile.yaml` keeps that deny —
/// only THIS agent's own profile is carved back out.
pub(crate) fn under_any_read_deny(roots: &[String], canonical: &Path, agent_home: &Path) -> bool {
    let carved: Vec<PathBuf> = self_protected_write_only(agent_home)
        .into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect();
    if carved.iter().any(|p| p == canonical) {
        return false;
    }
    under_any(roots, canonical)
}

pub(crate) fn check_write_entitlement(
    fs: &FilesystemEntitlement,
    canonical: &Path,
    chain: &crate::sandbox::launch_chain::LaunchChain,
) -> Result<(), ToolError> {
    // Checked first and unconditionally: no entitlement can satisfy this, and
    // the kernel's bare EPERM reads the same as "not granted", so this is the
    // only layer that can say which of the two happened.
    if let Some(reason) = chain.protects_write(canonical) {
        return Err(ToolError::Execution(format!(
            "path is part of MUR's launch chain and can never be written: {} ({reason})",
            canonical.display()
        )));
    }
    if under_any(&fs.deny, canonical) {
        return Err(ToolError::Execution(format!(
            "path denied by entitlement: {}",
            canonical.display()
        )));
    }
    // `under_any_or_worktree` tries the literal grants first, then one derived
    // hop for a worktree of a granted checkout (#004).
    if under_any_or_worktree(&fs.write, canonical) {
        return Ok(());
    }
    Err(ToolError::Execution(format!(
        "path not write-entitled: {} (grant it via `mur agent perm allow-write`)",
        canonical.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R5: the sandbox force-grants `agent_home` ("runtime cannot function
    /// without it" — `sandbox::policy::from_entitlements`), but the file tools
    /// were handed the RAW profile entitlement, so `write_file` refused a path
    /// the kernel would have allowed and the agent could not write inside its
    /// own home (`path not write-entitled: ~/.mur/agents/<name>/…`, observed
    /// 2026-09-13; it then reached for `/tmp` and tripped the withdrawal).
    ///
    /// Only `agent_home` is aligned. The other paths the sandbox force-grants
    /// (`channels/`, `index/channels/`, `open-items.jsonl`) stay out on
    /// purpose: those are written by the RUNTIME, and this gate bounds what
    /// the MODEL may write. Granting them here would let a prompt-injected
    /// agent forge channel events through `write_file`.
    /// Same grant, other layer: `write_file`/`edit_file` must accept the path
    /// the system prompt sends the agent to. The sandbox granting it is not
    /// enough — the kernel allowed the write while this gate refused it, which
    /// is exactly how the agent ended up probing `/tmp`.
    #[test]
    fn the_agents_own_artifacts_dir_is_writable_by_the_file_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = std::fs::canonicalize(tmp.path()).unwrap();
        let agent_home = mur_home.join("agents/rustsmith");
        std::fs::create_dir_all(&agent_home).unwrap();
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();
        let fs = for_file_tools(FilesystemEntitlement::default(), &agent_home);

        check_write_entitlement(
            &fs,
            &mur_home.join("artifacts/rustsmith/task4/check.rs"),
            &chain,
        )
        .expect("the path the system prompt names must be writable");

        check_write_entitlement(&fs, &mur_home.join("artifacts/pm/report.md"), &chain)
            .expect_err("a sibling agent's artifacts must not be writable");
    }

    #[test]
    fn agent_home_is_writable_by_the_file_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        let agent_home = home.join("agents/rustsmith");
        std::fs::create_dir_all(&agent_home).unwrap();
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();

        // Grants nothing — exactly the profile rustsmith had for its own home.
        let fs = for_file_tools(FilesystemEntitlement::default(), &agent_home);

        check_write_entitlement(&fs, &agent_home.join("task4_check.rs"), &chain)
            .expect("an agent must be able to write inside its own home");

        // Negative controls: the carve-outs still hold, so the line above is a
        // scoped grant and not a blanket allow.
        check_write_entitlement(&fs, &agent_home.join("profile.yaml"), &chain)
            .expect_err("the agent's own profile stays denied (#712)");
        check_write_entitlement(&fs, &home.join("agents/pm/notes.md"), &chain)
            .expect_err("a sibling agent's home is not granted");
        check_write_entitlement(&fs, &home.join("channels/x/events.jsonl"), &chain)
            .expect_err("runtime-owned channel store must NOT be model-writable");
    }

    #[test]
    fn launch_chain_beats_an_explicit_write_grant() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical base: the gate compares canonicalized paths, and on macOS
        // /var is a symlink to /private/var — raw tempdir paths would never
        // match the canonicalized grant roots the check computes.
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        let agents = home.join("agents");
        let chain = crate::sandbox::launch_chain::LaunchChain::for_test(
            &agents.join("mur"),
            &home.join("bin"),
            &home.join("home"),
        );

        // The most permissive grant a user could write.
        let fs = FilesystemEntitlement {
            write: vec![home.to_string_lossy().into_owned()],
            ..Default::default()
        };

        let err = check_write_entitlement(&fs, &agents.join("pm/profile.yaml"), &chain)
            .expect_err("a sibling profile must be refused even under a grant covering it");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("entitlements"),
            "error must explain why: {msg}"
        );

        // Negative control: the same grant still works for a path outside the
        // set, so the refusal above is the launch chain and not a broken check.
        check_write_entitlement(&fs, &home.join("skills/x.yaml"), &chain)
            .expect("unprotected path under the same grant must still be allowed");
    }

    /// The regression this helper exists for: a `~`-written grant must cover
    /// the expanded path, because the sandbox builder already expands it. The
    /// probe dir deliberately does NOT exist, so `canonicalize` fails on both
    /// sides and the assertion turns purely on the expansion — no dependency
    /// on the test host's home layout.
    #[test]
    fn tilde_grant_is_expanded_like_the_sandbox() {
        let home = dirs::home_dir().expect("home dir");
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();
        let fs = FilesystemEntitlement {
            write: vec!["~/mur-tilde-grant-probe".to_string()],
            ..Default::default()
        };

        check_write_entitlement(&fs, &home.join("mur-tilde-grant-probe/photo.jpg"), &chain)
            .expect("a ~ grant must cover the path it expands to");

        // Negative control: expansion must not widen the grant to everything,
        // or the assertion above would pass on a helper that always says yes.
        check_write_entitlement(
            &fs,
            Path::new("/tmp/mur-tilde-grant-probe/photo.jpg"),
            &chain,
        )
        .expect_err("grant must stay scoped to the expanded root");
    }

    /// Same expansion, deny side. `detect_warnings` tells users to deny
    /// `~/.ssh`; before this helper that entry was inert at the tool gate.
    /// Every path here sits under a probe dir that does NOT exist, grant and
    /// deny alike. That is not tidiness: on Windows `canonicalize` succeeds
    /// only for a real path and returns a `\\?\` UNC prefix, so an earlier
    /// version of this test that mixed a real root (UNC) with an absent one
    /// (verbatim) compared two spellings of the same place — it passed on
    /// Unix and failed on Windows CI. Keeping both sides absent makes the
    /// assertion turn on the tilde expansion, which is what it is about.
    #[test]
    fn tilde_deny_is_expanded_too() {
        let home = dirs::home_dir().expect("home dir");
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();
        let fs = FilesystemEntitlement {
            write: vec!["~/mur-deny-probe".to_string()],
            deny: vec!["~/mur-deny-probe/keys".to_string()],
            ..Default::default()
        };

        check_write_entitlement(&fs, &home.join("mur-deny-probe/keys/id_ed25519"), &chain)
            .expect_err("a ~ deny entry must outrank a ~ grant covering it");

        // Negative control: the surrounding grant still works, so the refusal
        // above is the deny entry and not a grant that never matched.
        check_write_entitlement(&fs, &home.join("mur-deny-probe/notes.md"), &chain)
            .expect("the write grant itself must still hold");
    }

    fn eperm() -> std::io::Error {
        std::io::Error::from_raw_os_error(1) // EPERM, "Operation not permitted"
    }

    #[test]
    fn removable_eperm_matches_only_volumes_path() {
        assert!(is_removable_volume_eperm(
            Path::new("/Volumes/Ext/spec.md"),
            &eperm()
        ));
        // Same EPERM but NOT under /Volumes → not our case.
        assert!(!is_removable_volume_eperm(
            Path::new("/Users/me/spec.md"),
            &eperm()
        ));
    }

    #[test]
    fn removable_eperm_ignores_eacces() {
        // Plain PermissionDenied (EACCES = os error 13) under /Volumes must
        // NOT be hijacked — only the exact EPERM (os error 1) qualifies.
        let eacces = std::io::Error::from_raw_os_error(13);
        assert!(!is_removable_volume_eperm(
            Path::new("/Volumes/Ext/spec.md"),
            &eacces
        ));
    }

    #[test]
    fn format_io_error_appends_hint_on_volumes_eperm() {
        let msg = format_io_error(
            "read",
            Path::new("/Volumes/Ext/spec.md"),
            Path::new("/Volumes/Ext"),
            &eperm(),
        );
        assert!(msg.contains("relative to session cwd /Volumes/Ext"));
        assert!(msg.contains(REMOVABLE_VOLUME_EPERM_HINT));
    }

    #[test]
    fn format_io_error_plain_error_has_no_hint() {
        let not_found = std::io::Error::from_raw_os_error(2); // ENOENT
        let msg = format_io_error(
            "read",
            Path::new("/Users/me/spec.md"),
            Path::new("/Users/me"),
            &not_found,
        );
        assert!(msg.contains("relative to session cwd /Users/me"));
        assert!(!msg.contains(REMOVABLE_VOLUME_EPERM_HINT));
    }

    #[test]
    fn resolve_path_expands_tilde() {
        let home = dirs::home_dir().unwrap();
        let wd = Path::new("/tmp/wd");
        assert_eq!(resolve_path(wd, "~/.mur/skills"), home.join(".mur/skills"));
        assert_eq!(resolve_path(wd, "~"), home);
        assert_eq!(resolve_path(wd, "/abs/x"), PathBuf::from("/abs/x"));
        assert_eq!(resolve_path(wd, "rel/x"), wd.join("rel/x"));
        // `~user` form is not expanded — treated as a relative name.
        assert_eq!(resolve_path(wd, "~other/x"), wd.join("~other/x"));
    }

    #[test]
    fn self_protected_denies_own_profile_despite_write_grant() {
        // Issue #712: a write grant covering the whole agent dir must not
        // let the file tools write the agent's own profile.yaml/identity.key.
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent_home = tmp.path().join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::write(agent_home.join("profile.yaml"), "name: mur\n").unwrap();
        std::fs::write(agent_home.join("identity.key"), "KEY").unwrap();
        let fs = for_file_tools(
            FilesystemEntitlement {
                read: vec![],
                write: vec![agent_home.to_string_lossy().into_owned()],
                deny: vec![],
            },
            &agent_home,
        );
        let canonical_home = std::fs::canonicalize(&agent_home).unwrap();
        for f in ["profile.yaml", "identity.key"] {
            assert!(
                check_write_entitlement(
                    &fs,
                    &canonical_home.join(f),
                    &crate::sandbox::launch_chain::LaunchChain::inert(),
                )
                .is_err(),
                "{f} must be write-denied despite the agent-dir grant"
            );
        }
        // The rest of the agent dir stays writable (running.lock etc.).
        assert!(
            check_write_entitlement(
                &fs,
                &canonical_home.join("running.lock"),
                &crate::sandbox::launch_chain::LaunchChain::inert(),
            )
            .is_ok()
        );
    }

    /// Build a real repo + linked worktree. Returns `None` when git is
    /// unavailable so the test skips loudly rather than passing vacuously.
    fn repo_with_worktree() -> Option<(tempfile::TempDir, PathBuf, PathBuf)> {
        let tmp = tempfile::tempdir().ok()?;
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).ok()?;
        let git = |args: &[&str]| -> bool {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&main)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            return None;
        }
        let _ = git(&["config", "user.email", "t@example.com"]);
        let _ = git(&["config", "user.name", "t"]);
        std::fs::write(main.join("f.txt"), "x").ok()?;
        let _ = git(&["add", "f.txt"]);
        let _ = git(&["commit", "-qm", "init"]);
        let wt = tmp.path().join("wt");
        if !git(&["worktree", "add", "-q", wt.to_str()?, "-b", "feat"]) {
            return None;
        }
        Some((tmp, main, wt))
    }

    /// Issue #004: granting the checkout must reach its worktrees. The user
    /// reported having to authorise the same repo twice — once for the repo,
    /// once for each worktree — because this gate is pure prefix matching and
    /// a worktree lives outside the checkout it belongs to.
    #[test]
    fn a_worktree_of_a_granted_checkout_is_writable() {
        let Some((_tmp, main, wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable");
            return;
        };
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();
        let fs = FilesystemEntitlement {
            read: vec![],
            // ONLY the main checkout is granted — exactly what the user typed.
            write: vec![main.to_string_lossy().into_owned()],
            deny: vec![],
        };
        let target = std::fs::canonicalize(&wt).unwrap().join("src/new.rs");
        assert!(
            check_write_entitlement(&fs, &target, &chain).is_ok(),
            "a worktree of the granted checkout must be writable without a second grant"
        );
    }

    /// The derivation must not become a general widening: an unrelated repo
    /// is still refused, and so is an ordinary directory that merely sits
    /// beside the grant.
    #[test]
    fn worktree_derivation_does_not_widen_to_unrelated_paths() {
        let Some((_tmp, main, _wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable");
            return;
        };
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();
        let fs = FilesystemEntitlement {
            read: vec![],
            write: vec![main.to_string_lossy().into_owned()],
            deny: vec![],
        };
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = std::fs::canonicalize(elsewhere.path())
            .unwrap()
            .join("nope.rs");
        assert!(
            check_write_entitlement(&fs, &outside, &chain).is_err(),
            "a path outside every grant must stay refused"
        );
    }

    /// The safety boundary of #004: `deny` is evaluated first and stays
    /// LITERAL. A denied path inside a derived worktree grant must still be
    /// refused — the derivation is allow-side only, so it can never reopen
    /// something the user explicitly closed.
    #[test]
    fn deny_still_wins_inside_a_derived_worktree_grant() {
        let Some((_tmp, main, wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable");
            return;
        };
        let chain = crate::sandbox::launch_chain::LaunchChain::inert();
        let wt_canon = std::fs::canonicalize(&wt).unwrap();
        let secrets = wt_canon.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();

        let fs = FilesystemEntitlement {
            read: vec![],
            write: vec![main.to_string_lossy().into_owned()],
            deny: vec![secrets.to_string_lossy().into_owned()],
        };
        assert!(
            check_write_entitlement(&fs, &secrets.join("key.pem"), &chain).is_err(),
            "deny must beat a derived worktree grant"
        );
        // ...while the rest of the same worktree remains writable.
        assert!(check_write_entitlement(&fs, &wt_canon.join("ok.rs"), &chain).is_ok());
    }
}
