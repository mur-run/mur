//! `mur agent doctor` lines for MCP servers.
//!
//! Two checks, because an MCP server can fail in two places nobody else sees:
//!
//! - **Recorded** (runtime): the running agent writes each server's
//!   `tools/list` outcome to `agent_home/mcp_status.json`. A failed server's
//!   tools silently vanish from the agent; this surfaces the reason.
//! - **Predicted** (static): resolve each enabled server's command the way the
//!   runtime will (`mcp_child_path`), follow an `#!/usr/bin/env <x>` shebang
//!   one hop, and ask whether the seal lets each exec in that chain — the
//!   command, the shebang's `env`, and `<x>` — happen. Catches a
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
    let bundled = mur_common::exec::bundled_mcp_server_path();
    predict_with(&profile.enabled_mcp_servers(), &bundled, &path, |p| {
        exec_permitted(&policy, p)
    })
}

#[cfg(not(target_os = "macos"))]
pub fn predicted(_profile: &AgentProfile, _agent_home: &Path) -> Vec<Blocked> {
    Vec::new()
}

/// Pure core of [`predicted`]: `bundled` is MUR's own MCP server copy (the
/// runtime execs it in place of a PATH `mur-mcp-server`), `path` the child
/// `PATH`, `permitted` the seal predicate. A command that does not resolve is skipped — the runtime
/// reports that one itself, as a spawn error in the recorded status.
/// Only the macOS seal calls it outside tests.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn predict_with(
    servers: &[McpServerEntry],
    bundled: &Path,
    path: &std::ffi::OsStr,
    permitted: impl Fn(&Path) -> bool,
) -> Vec<Blocked> {
    let mut out = Vec::new();
    for entry in servers {
        // Resolve with the spawn's own resolver: the whole command (args
        // live in `entry.args`), and the bundled-server redirect.
        let prog = entry.command.as_str();
        let Ok(bin) = mur_common::exec::resolve_mcp_launch_with(bundled, path, prog) else {
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
        let Some((env, interp)) = env_shebang(&bin) else {
            continue;
        };
        // The kernel execs the shebang's own executable before `env` looks
        // anything up, and Strict can grant the script and its target while
        // refusing `/usr/bin/env` — so judge that hop first.
        if !permitted(&env) {
            out.push(Blocked {
                server: entry.name.clone(),
                wanted: format!("#!{}", env.display()),
                resolved: env,
            });
            continue;
        }
        if let Ok(ib) = mur_common::exec::resolve_command_in(path, &interp)
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

/// The shebang executable and `x` from a leading `#!…/env [-S] x …` line,
/// else `None`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn env_shebang(bin: &Path) -> Option<(PathBuf, String)> {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(bin).ok()?.read(&mut buf).ok()?;
    let head = std::str::from_utf8(&buf[..n]).ok()?;
    let line = head.strip_prefix("#!")?.lines().next()?;
    let mut parts = line.split_whitespace();
    let env = parts.next()?;
    if !env.ends_with("/env") {
        return None;
    }
    let interp = parts.find(|p| !p.starts_with('-'))?;
    Some((PathBuf::from(env), interp.to_string()))
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
