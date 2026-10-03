//! Per-agent scratch directory: `<mur_home>/tmp/<agent>`.
//!
//! One helper shared by the kernel grant (`sandbox::policy::build`), the
//! file-tool gate (`tools::fs_policy`), the child env (`TMPDIR`) and the
//! prompt. Deriving the path in one place is the point: the two grant
//! sites drifting apart is exactly the bug class this module prevents.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum AgentPathError {
    #[error("agent home {0} has no <mur_home>/agents ancestor")]
    NoMurHome(PathBuf),
    #[error("agent home {0} has no agent name component")]
    NoAgentName(PathBuf),
}

/// Directory name under `<mur_home>` holding every agent's scratch dir.
/// Owned by `mur_common::repo_walk`, whose `.git` walk is bounded by it.
pub use mur_common::repo_walk::SCRATCH_ROOT;

/// Unix mode for both `<mur_home>/tmp` and `<mur_home>/tmp/<agent>`.
#[cfg(unix)]
const SCRATCH_MODE: u32 = 0o700;

/// Env vars pointed at the scratch dir in every agent-side child.
pub const SCRATCH_ENV_KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];

/// `<mur_home>/agents/<agent>` → `<mur_home>/tmp/<agent>`.
///
/// Same derivation as the artifacts grant (`parent().parent()` plus
/// `file_name()`); no canonicalization, so the result matches the
/// on-disk agent directory name exactly.
pub fn agent_scratch_dir(agent_home: &Path) -> Result<PathBuf, AgentPathError> {
    let mur_home = agent_home
        .parent()
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| AgentPathError::NoMurHome(agent_home.to_path_buf()))?;
    let name = agent_home
        .file_name()
        .ok_or_else(|| AgentPathError::NoAgentName(agent_home.to_path_buf()))?;
    Ok(mur_home.join(SCRATCH_ROOT).join(name))
}

/// Create `dir` (and its parent) and force both to `0700`, also when they
/// already existed with a looser mode.
pub fn ensure_scratch_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = || std::fs::Permissions::from_mode(SCRATCH_MODE);
        if let Some(parent) = dir.parent() {
            std::fs::set_permissions(parent, perms())?;
        }
        std::fs::set_permissions(dir, perms())?;
    }
    Ok(())
}

/// `[("TMPDIR", dir), ("TMP", dir), ("TEMP", dir)]`.
pub fn scratch_env(dir: &Path) -> [(String, String); 3] {
    let v = dir.to_string_lossy().into_owned();
    SCRATCH_ENV_KEYS.map(|k| (k.to_string(), v.clone()))
}

pub mod prune;

#[cfg(test)]
mod tests;
