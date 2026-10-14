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
///
/// Idempotent with no writes when already prepared: `chmod` only runs on a
/// mode that differs. The runtime calls this again AFTER the seal (via
/// `SandboxPolicy::from_entitlements` when building the MCP pool), where the
/// parent `<mur_home>/tmp` is not writable — an unconditional `chmod` there
/// was the EPERM behind "scratch dir not prepared" (#1678).
pub fn ensure_scratch_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        if let Some(parent) = dir.parent() {
            tighten_to_scratch_mode(parent)?;
        }
        tighten_to_scratch_mode(dir)?;
    }
    Ok(())
}

/// `chmod` `p` to [`SCRATCH_MODE`] unless it already has exactly that mode.
#[cfg(unix)]
fn tighten_to_scratch_mode(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let current = std::fs::metadata(p)?.permissions().mode();
    if needs_tighten(current) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(SCRATCH_MODE))?;
    }
    Ok(())
}

/// Whether a dir with st_mode `mode` must be `chmod`ed to [`SCRATCH_MODE`].
#[cfg(unix)]
fn needs_tighten(mode: u32) -> bool {
    mode & 0o777 != SCRATCH_MODE
}

/// `[("TMPDIR", dir), ("TMP", dir), ("TEMP", dir)]`.
pub fn scratch_env(dir: &Path) -> [(String, String); 3] {
    let v = dir.to_string_lossy().into_owned();
    SCRATCH_ENV_KEYS.map(|k| (k.to_string(), v.clone()))
}

/// Longest Unix-socket path we will hand out. macOS `sun_path` is 104 bytes
/// including the NUL; leave headroom rather than sit on the limit.
const MAX_SOCKET_PATH: usize = 100;

/// Build-tool cache defaults for an agent-side child, rooted in the agent's
/// scratch dir (#1860). `is_set` reports whether a variable is already
/// defined, so a value the caller chose is never overridden.
///
/// - `SCCACHE_DIR` / `ORT_CACHE_DIR`: the tools' own defaults live under
///   `~/Library/Caches`, which the sandbox denies.
/// - `SCCACHE_SERVER_UDS`: an sccache server already running on the machine
///   ignores a new `SCCACHE_DIR`, so the agent needs a server of its own. A
///   socket under the scratch dir is unique per agent without inventing port
///   numbers. Skipped when the user pinned a server (`SCCACHE_SERVER_PORT` or
///   `..._UDS`), or when the path would exceed the socket limit.
pub fn cache_env(dir: &Path, is_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut put = |key: &str, value: PathBuf| {
        if !is_set(key) {
            out.push((key.to_string(), value.to_string_lossy().into_owned()));
        }
    };
    put("SCCACHE_DIR", dir.join("sccache"));
    let socket = dir.join("sccache.sock");
    let server_pinned = is_set("SCCACHE_SERVER_PORT");
    if !server_pinned && socket.as_os_str().len() <= MAX_SOCKET_PATH {
        put("SCCACHE_SERVER_UDS", socket);
    }
    put("ORT_CACHE_DIR", dir.join("ort-cache"));
    out
}

/// Everything an agent-side bash child gets from its scratch dir: the
/// `TMPDIR` family, plus build-cache defaults (#1860) that never override a
/// variable already in the runtime's own environment or in `env`.
pub fn extend_scratch_env(env: &mut Vec<(String, String)>, dir: &Path) {
    env.extend(scratch_env(dir));
    let add = cache_env(dir, |k| {
        std::env::var_os(k).is_some() || env.iter().any(|(e, _)| e == k)
    });
    env.extend(add);
}

pub mod prune;

#[cfg(test)]
mod tests;
