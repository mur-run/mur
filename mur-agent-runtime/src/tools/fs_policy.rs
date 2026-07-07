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
}
