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

/// Harden an agent's filesystem entitlement for the file tools: append the
/// self-protected files (issue #712 — the agent's own `profile.yaml` and
/// `identity.key`) to the deny list, so the tool-level gate refuses reads
/// and writes on them even when a write grant covers the whole agent dir.
/// On Linux this gate is the enforcement point (Landlock cannot express
/// deny-within-allow); on macOS it fronts the SBPL kernel deny with a clear
/// error instead of a raw EPERM.
pub(crate) fn self_protected(
    mut fs: FilesystemEntitlement,
    agent_home: &Path,
) -> FilesystemEntitlement {
    for f in crate::sandbox::policy::SELF_PROTECTED_AGENT_FILES {
        let p = agent_home.join(f).to_string_lossy().into_owned();
        if !fs.deny.contains(&p) {
            fs.deny.push(p);
        }
    }
    fs
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
    let under = |roots: &[String]| {
        roots.iter().any(|r| {
            let root = std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r));
            canonical.starts_with(&root)
        })
    };
    if under(&fs.deny) {
        return Err(ToolError::Execution(format!(
            "path denied by entitlement: {}",
            canonical.display()
        )));
    }
    if under(&fs.write) {
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
        let fs = self_protected(
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
}
