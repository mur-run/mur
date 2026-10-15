//! `mur agent doctor` lines for MCP servers.
//!
//! Two checks, because an MCP server can fail in two places nobody else sees:
//!
//! - **Recorded** (runtime): the running agent writes each server's
//!   `tools/list` outcome to `agent_home/mcp_status.json`. A failed server's
//!   tools silently vanish from the agent; this surfaces the reason.
//! - **Predicted** (static): resolve each enabled server's command the way the
//!   runtime will (`mcp_child_path`), follow an `#!/usr/bin/env <x>` shebang
//!   one hop, and ask whether the seal lets that binary exec. Catches a
//!   version-manager shim's `node` the allowlist never granted before the
//!   agent even starts.

use mur_common::agent::{AgentProfile, MCP_STATUS_FILE, McpServerEntry, McpStatus};
use std::path::{Path, PathBuf};

/// Lines describing failed servers recorded by the running process `pid`.
/// A status file from another PID is a previous boot and is ignored.
pub fn recorded_lines(name: &str, agent_home: &Path, pid: u32) -> Vec<String> {
    let Some(status) = McpStatus::load(&agent_home.join(MCP_STATUS_FILE)) else {
        return Vec::new();
    };
    format_recorded(name, &status, pid)
}

/// `{name, error}` objects for `doctor --json`.
pub fn recorded_failures(agent_home: &Path, pid: u32) -> Vec<serde_json::Value> {
    McpStatus::load(&agent_home.join(MCP_STATUS_FILE))
        .filter(|s| s.pid == pid)
        .map(|s| {
            s.failed()
                .map(|f| serde_json::json!({ "name": f.name, "error": f.error }))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn format_recorded(name: &str, status: &McpStatus, pid: u32) -> Vec<String> {
    if status.pid != pid {
        return Vec::new();
    }
    status
        .failed()
        .map(|s| {
            format!(
                "  {name}: MCP server '{}' FAILED to load, its tools are missing: {}",
                s.name,
                s.error.as_deref().unwrap_or("unknown error")
            )
        })
        .collect()
}

/// One binary in an MCP server's launch chain that the seal would refuse.
#[derive(Debug, PartialEq, Eq)]
pub struct Blocked {
    pub server: String,
    /// What was asked for (`node`, or the shebang's `env node`).
    pub wanted: String,
    pub resolved: PathBuf,
}

/// Predict exec denials for every enabled MCP server of `profile`.
#[cfg(target_os = "macos")]
pub fn predicted(profile: &AgentProfile, agent_home: &Path) -> Vec<Blocked> {
    use mur_agent_runtime::sandbox::{macos::exec_permitted, policy::SandboxPolicy};
    let policy = SandboxPolicy::from_entitlements(&profile.entitlements, agent_home);
    let path = mur_agent_runtime::sandbox::search_dirs::mcp_child_path(policy.spawn_mode);
    predict_with(&profile.enabled_mcp_servers(), &path, |p| {
        exec_permitted(&policy, p)
    })
}

#[cfg(not(target_os = "macos"))]
pub fn predicted(_profile: &AgentProfile, _agent_home: &Path) -> Vec<Blocked> {
    Vec::new()
}

/// Pure core of [`predicted`]: `path` is the child `PATH`, `permitted` the
/// seal predicate. A command that does not resolve is skipped — the runtime
/// reports that one itself, as a spawn error in the recorded status.
pub(crate) fn predict_with(
    servers: &[McpServerEntry],
    path: &std::ffi::OsStr,
    permitted: impl Fn(&Path) -> bool,
) -> Vec<Blocked> {
    let mut out = Vec::new();
    for entry in servers {
        let prog = entry
            .command
            .split_whitespace()
            .next()
            .unwrap_or(&entry.command);
        let Ok(bin) = mur_common::exec::resolve_command_in(path, prog) else {
            continue;
        };
        if !permitted(&bin) {
            out.push(Blocked {
                server: entry.name.clone(),
                wanted: prog.to_string(),
                resolved: bin,
            });
            continue;
        }
        if let Some(interp) = env_shebang(&bin)
            && let Ok(ib) = mur_common::exec::resolve_command_in(path, &interp)
            && !permitted(&ib)
        {
            out.push(Blocked {
                server: entry.name.clone(),
                wanted: format!("env {interp}"),
                resolved: ib,
            });
        }
    }
    out
}

/// `x` from a leading `#!/usr/bin/env [-S] x …` line, else `None`.
fn env_shebang(bin: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(bin).ok()?.read(&mut buf).ok()?;
    let head = std::str::from_utf8(&buf[..n]).ok()?;
    let line = head.strip_prefix("#!")?.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.ends_with("/env") {
        return None;
    }
    parts.find(|p| !p.starts_with('-')).map(str::to_string)
}

/// One line per enabled MCP server whose launch chain the seal
/// would refuse. Best-effort: an unloadable profile contributes nothing.
pub fn predictions(mur_home: &std::path::Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(mur_home.join("agents")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("profile.yaml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let Ok((_p, prof)) = super::load_profile_for_edit(&name) else {
            continue;
        };
        let blocked = predicted(&prof, &mur_home.join("agents").join(&name));
        out.extend(predicted_lines(&name, &blocked));
    }
    out
}

pub fn predicted_lines(name: &str, blocked: &[Blocked]) -> Vec<String> {
    blocked
        .iter()
        .map(|b| {
            format!(
                "  {name}: MCP server '{}' will be REFUSED by the sandbox: '{}' resolves to {}, which the spawn allowlist does not grant\n    \
                 fix: put the granted binary first on PATH, or set the server's command to an allowed absolute path",
                b.server,
                b.wanted,
                b.resolved.display()
            )
        })
        .collect()
}

#[cfg(all(test, unix))]
#[path = "doctor_mcp_tests.rs"]
mod tests;
