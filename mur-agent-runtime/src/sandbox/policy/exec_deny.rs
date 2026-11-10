//! Keep the `mur` CLI out of an agent's reach even though it lives in a
//! system exec directory (#2317).
//!
//! [`system_exec_paths`](super::system_exec_paths) grants whole directories —
//! `~/.local/bin`, `/usr/local/bin`, … — so the agent's shell keeps its
//! interpreter and coreutils. Those are also where the install script and
//! Intel Homebrew put `mur`, and the `mur` CLI includes `agent perm`: an agent
//! that can run it can widen its own entitlements. The spawn allowlist is the
//! consent for it; a directory grant is not.
//!
//! So every `mur` CLI file inside an exec directory becomes an exec DENY,
//! unless the policy already grants that exact file by literal — the profile
//! allowlisted it (a `browser` MCP entry runs `mur browser record`) or
//! `fleet_run` added it. The kernel cannot tell the bash tool from an MCP
//! child, so an explicit grant has to win for both.
//!
//! The runtime binary and the `mur_agent_<name>` links are deliberately NOT
//! here: the runtime re-execs itself after the seal (the ssh-agent bypass
//! probe), and `agent perm` is not reachable through it.
//!
//! macOS emits each entry as a `deny process-exec*` literal after the allows.
//! Landlock has no deny rule, so Linux uses [`exec_grants_without`] to split a
//! directory holding a denied file into per-entry grants that skip it.

use std::path::{Path, PathBuf};

use mur_common::agent::SpawnMode;

/// File names of the `mur` CLI: the binary, its Windows form, and the
/// `murmur` alias the installer links to it.
pub(crate) const MUR_CLI_EXEC_NAMES: &[&str] = &["mur", "mur.exe", "murmur"];

/// A path plus its symlink-resolved form when that differs. The kernel checks
/// the resolved file, so both forms must be named.
fn forms(p: &Path) -> Vec<PathBuf> {
    let mut v = vec![p.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(p)
        && real != p
    {
        v.push(real);
    }
    v
}

/// The exact `mur` CLI files the agent must not exec under `spawn_mode`.
///
/// Empty for [`SpawnMode::Any`]: the operator chose unrestricted spawn, and no
/// exec clause is emitted at all. `extra` is the CLI path the runtime itself
/// would exec (`exec_dirs::mur_cli`), covered even when it sits outside the
/// exec directories so long as one of them reaches it.
pub(crate) fn mur_cli_exec_denies(
    spawn_mode: SpawnMode,
    fs_exec: &[PathBuf],
    spawn_allowed_paths: &[PathBuf],
    extra: Option<&Path>,
) -> Vec<PathBuf> {
    if spawn_mode == SpawnMode::Any {
        return Vec::new();
    }
    let granted = |p: &Path| forms(p).iter().any(|f| spawn_allowed_paths.contains(f));
    let mut candidates: Vec<PathBuf> = fs_exec
        .iter()
        .flat_map(|dir| MUR_CLI_EXEC_NAMES.iter().map(move |n| dir.join(n)))
        .filter(|p| p.is_file())
        .collect();
    if let Some(e) = extra
        && e.is_absolute()
        && e.is_file()
        && fs_exec
            .iter()
            .any(|d| forms(e).iter().any(|f| f.starts_with(d)))
    {
        candidates.push(e.to_path_buf());
    }
    let mut out = Vec::new();
    for c in candidates {
        if granted(&c) {
            continue;
        }
        for f in forms(&c) {
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// `fs_exec` with every denied file carved out, for a backend that can only
/// allow (Landlock).
///
/// A directory holding no denied file is kept whole. One that does is
/// replaced by its entries, minus any whose resolved target is denied — a
/// `murmur → mur` link would otherwise grant `mur`'s inode, since the rule
/// follows the link. Entries are listed once, at seal time: a binary added to
/// that directory later is not exec-able until the agent restarts, the same
/// trade every literal grant makes.
pub(crate) fn exec_grants_without(fs_exec: &[PathBuf], deny: &[PathBuf]) -> Vec<PathBuf> {
    if deny.is_empty() {
        return fs_exec.to_vec();
    }
    let is_denied = |p: &Path| forms(p).iter().any(|f| deny.contains(f));
    let mut out = Vec::new();
    for dir in fs_exec {
        let dir_forms = forms(dir);
        let holds_denied = deny
            .iter()
            .any(|d| d.parent().is_some_and(|p| dir_forms.iter().any(|f| f == p)));
        if !holds_denied {
            out.push(dir.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut kept: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| !is_denied(p))
            .collect();
        kept.sort();
        out.extend(kept);
    }
    out
}

#[cfg(test)]
#[path = "exec_deny_tests.rs"]
mod tests;
