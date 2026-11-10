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
//! macOS emits each entry as a `deny process-exec*` literal after the allows;
//! SBPL read and write grants never carry exec. Landlock has no deny rule and
//! its read and write rights include `Execute`, so Linux runs every rule
//! source through [`split_exec_grants`]: a grant above a denied file loses
//! `Execute`, which is given back by carving around the denied files.

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
/// exec clause is emitted at all. `reach` is every grant that can carry exec
/// on the backend — `fs_exec` for SBPL, plus the read and write grants for
/// Landlock. `extra` is the CLI path the runtime itself would exec
/// (`exec_dirs::mur_cli`), covered even when it sits deeper than a grant's
/// immediate entries so long as one of them reaches it.
pub(crate) fn mur_cli_exec_denies(
    spawn_mode: SpawnMode,
    reach: &[PathBuf],
    spawn_allowed_paths: &[PathBuf],
    extra: Option<&Path>,
) -> Vec<PathBuf> {
    if spawn_mode == SpawnMode::Any {
        return Vec::new();
    }
    let granted = |p: &Path| forms(p).iter().any(|f| spawn_allowed_paths.contains(f));
    let mut candidates: Vec<PathBuf> = reach
        .iter()
        .flat_map(|dir| MUR_CLI_EXEC_NAMES.iter().map(move |n| dir.join(n)))
        .filter(|p| p.is_file())
        .collect();
    if let Some(e) = extra
        && e.is_absolute()
        && e.is_file()
        && reach.iter().any(|d| {
            forms(e)
                .iter()
                .any(|f| forms(d).iter().any(|g| f.starts_with(g)))
        })
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

/// `true` when `p` is a denied file or an ancestor of one, in either its
/// written or symlink-resolved form. A rule installed on a symlink binds to
/// its target, recursively, so containment is checked on the resolved form,
/// not just by exact path or immediate parent. A link merely nested inside a
/// kept-whole grant needs no check: Landlock matches a file against the
/// ancestors of its resolved path, which a link elsewhere is not.
fn reaches_denied(p: &Path, deny: &[PathBuf]) -> bool {
    forms(p)
        .iter()
        .any(|f| deny.iter().any(|d| d.starts_with(f)))
}

/// Grant everything beneath `dir` except the denied files: entries that reach
/// no denied file are kept whole, a directory above one is opened and carved
/// in turn, and a denied file (or a link resolving to it) is dropped.
///
/// `seen` holds the resolved directories already carved. A symlink that loops
/// back to one of them is dropped rather than followed — granting it would
/// re-grant that directory whole. Fail-closed: an unreadable directory grants
/// nothing.
fn carve(dir: &Path, deny: &[PathBuf], seen: &mut Vec<PathBuf>, out: &mut Vec<PathBuf>) {
    let Ok(real) = std::fs::canonicalize(dir) else {
        return;
    };
    if seen.contains(&real) {
        return;
    }
    seen.push(real);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if !reaches_denied(&p, deny) {
            out.push(p);
        } else if p.is_dir() {
            carve(&p, deny, seen, out);
        }
    }
}

/// One rule source split for a backend that can only allow (Landlock), where
/// read and write rules carry `Execute` too, not just exec rules.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ExecSplit {
    /// Grants reaching no denied file: install with the source's full access.
    pub clean: Vec<PathBuf>,
    /// Grants above a denied file: install with the source's access minus
    /// `Execute`, so reading and writing beneath them is unchanged.
    pub no_exec: Vec<PathBuf>,
    /// The `no_exec` grants carved around the denied files: install with
    /// `Execute` alone. Listed once, at seal time — a binary added later is
    /// not exec-able until the agent restarts, the trade every literal grant
    /// makes.
    pub exec_only: Vec<PathBuf>,
}

/// Split `grants` so no rule grants `Execute` on a denied file (#2317).
///
/// Landlock grants are additive: withholding `Execute` from one rule does
/// nothing while another rule above the file still carries it. Every rule
/// source that includes `Execute` must go through this, not just `fs_exec`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn split_exec_grants(grants: &[PathBuf], deny: &[PathBuf]) -> ExecSplit {
    let mut split = ExecSplit::default();
    let mut seen = Vec::new();
    for g in grants {
        if deny.is_empty() || !reaches_denied(g, deny) {
            split.clean.push(g.clone());
        } else {
            split.no_exec.push(g.clone());
            carve(g, deny, &mut seen, &mut split.exec_only);
        }
    }
    split
}

#[cfg(test)]
#[path = "exec_deny_tests.rs"]
mod tests;
