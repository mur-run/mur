//! Shared call-time filesystem-entitlement gate for the mutating file tools
//! (issue #591 PR2). `deny` always wins; writes require a `write` grant.
//! read_file keeps its own equivalent check — dedup is a follow-up.

use std::path::{Path, PathBuf};

use mur_common::agent::FilesystemEntitlement;

use crate::tools::ToolError;

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
) -> Result<(), ToolError> {
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
                check_write_entitlement(&fs, &canonical_home.join(f)).is_err(),
                "{f} must be write-denied despite the agent-dir grant"
            );
        }
        // The rest of the agent dir stays writable (running.lock etc.).
        assert!(check_write_entitlement(&fs, &canonical_home.join("running.lock")).is_ok());
    }
}
