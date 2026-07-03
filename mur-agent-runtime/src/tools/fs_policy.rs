//! Shared call-time filesystem-entitlement gate for the mutating file tools
//! (issue #591 PR2). `deny` always wins; writes require a `write` grant.
//! read_file keeps its own equivalent check — dedup is a follow-up.

use std::path::{Path, PathBuf};

use mur_common::agent::FilesystemEntitlement;

use crate::tools::ToolError;

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
