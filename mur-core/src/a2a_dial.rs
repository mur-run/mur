//! Shared A2A dial helper — issues a JSON-RPC request to a local agent,
//! either via its running Unix socket or by spawning the runtime
//! ephemerally in stdio mode.
//!
//! Consumed by:
//!   - `cmd/agent/comm.rs` for `mur agent card` / `mur agent send`
//!   - `cmd/skill_install.rs` for `mur skill install agent://...`

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use mur_common::LockFile;
use serde_json::{Value, json};

use crate::cmd::agent::resolve_runtime_target;

/// Strategy for reaching the target agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DialMode {
    /// Use the running agent's socket if available, otherwise spawn an
    /// ephemeral runtime in stdio mode. Default for CLI use.
    Auto,
    /// Require the target agent to be running. Fail otherwise. Used by
    /// flows that must not pay the cold-start cost (or where ephemeral
    /// spawn would mask a misconfiguration).
    RequireRunning,
    /// Always spawn an ephemeral runtime. Useful for tests and for
    /// pulling skills from agents that the user explicitly does not want
    /// to keep resident.
    ForceEphemeral,
}

/// Resolve a user-typed agent name to the canonical on-disk agent name,
/// matching case-insensitively. CLI users shouldn't have to remember the
/// exact casing (`mur agent send mur` should work as well as `... Mur`).
///
/// The runtime's spoof check ([`mur-agent-runtime`] `verify_name_match`)
/// requires the name passed downstream to equal the profile's `name` field,
/// which for every MUR-created agent equals its directory name — so we
/// return the real directory name. If an exact match already exists, or no
/// case-insensitive match is found, the input is returned unchanged so
/// downstream code emits its normal "not found / not running" error.
pub fn canonicalize_agent_name(home: &Path, typed: &str) -> String {
    let agents = home.join("agents");
    // Exact match wins — cheapest, and correct on case-sensitive filesystems.
    if agents.join(typed).join("profile.yaml").is_file() {
        return typed.to_string();
    }
    if let Ok(entries) = fs::read_dir(&agents) {
        for entry in entries.flatten() {
            let dir = entry.file_name();
            let dir = dir.to_string_lossy();
            if dir.eq_ignore_ascii_case(typed) && entry.path().join("profile.yaml").is_file() {
                return dir.into_owned();
            }
        }
    }
    typed.to_string()
}

/// Dial the named agent and return the `result` field of the JSON-RPC
/// response. Errors carry the agent name and method for diagnosability.
///
/// `request_id` is auto-generated; the helper enforces id matching so
/// callers can't accidentally race their own requests.
pub fn dial_method(
    home: &Path,
    agent_name: &str,
    method: &str,
    params: Value,
    mode: DialMode,
) -> Result<Value> {
    // Case-insensitive name resolution: `mur agent send mur` works the same as
    // `... Mur`. Returns the canonical (on-disk) name so the runtime's
    // exact-match spoof check still passes downstream.
    let canonical = canonicalize_agent_name(home, agent_name);
    let agent_name = canonical.as_str();
    let _span = tracing::info_span!("a2a.dial", agent = %agent_name, method = %method).entered();
    tracing::debug!(?mode, "dialing");

    let request_id = json!(1);
    let request = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params,
    });

    let lock_path = home.join("agents").join(agent_name).join("running.lock");
    let is_running = lock_path.exists();

    match (mode, is_running) {
        (DialMode::RequireRunning, false) => bail!(
            "agent '{agent_name}' is not running (no {})",
            lock_path.display()
        ),
        (DialMode::ForceEphemeral, _) => dial_ephemeral(home, agent_name, &request, &request_id),
        (_, true) => dial_socket(&lock_path, agent_name, &request, &request_id),
        (_, false) => match dial_ephemeral(home, agent_name, &request, &request_id) {
            Ok(v) => Ok(v),
            // Race: another runtime (e.g. the Hub's auto-start) came up between
            // our lock check and the spawn, so the ephemeral child refused with
            // "already running". The agent IS up — wait for it to publish its
            // running.lock, then dial its socket instead of failing.
            Err(e) if e.to_string().contains("already running") => {
                for _ in 0..20 {
                    if lock_path.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if lock_path.exists() {
                    dial_socket(&lock_path, agent_name, &request, &request_id)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        },
    }
}

fn dial_socket(
    lock_path: &Path,
    agent_name: &str,
    request: &Value,
    request_id: &Value,
) -> Result<Value> {
    let bytes = fs::read(lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    let lock: LockFile = serde_json::from_slice(&bytes).context("parse running.lock")?;
    let sock = lock.transports.unix_socket.ok_or_else(|| {
        anyhow!(
            "agent '{agent_name}' has no unix-socket transport (TCP-only transports are not yet supported by the install path)"
        )
    })?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(&sock)
            .with_context(|| format!("connect {sock}"))?;
        let line = format!("{}\n", serde_json::to_string(request)?);
        stream.write_all(line.as_bytes())?;
        let reader = BufReader::new(stream.try_clone()?);
        for line in reader.lines() {
            let line = line.context("read response line")?;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id") == Some(request_id) {
                if let Some(err) = v.get("error") {
                    bail!("agent '{agent_name}' returned error: {err}");
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        bail!("EOF before matching response from '{agent_name}'");
    }
    #[cfg(not(unix))]
    {
        let _ = sock;
        bail!("unix socket transport is only supported on unix hosts")
    }
}

fn dial_ephemeral(
    home: &Path,
    agent_name: &str,
    request: &Value,
    request_id: &Value,
) -> Result<Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let runtime = resolve_runtime_target();
    if !runtime.is_absolute() && !runtime.exists() {
        bail!(
            "agent '{agent_name}' not running and runtime binary not found at {} (set MUR_AGENT_RUNTIME_BIN)",
            runtime.display()
        );
    }

    let mut child = std::process::Command::new(&runtime)
        .env("MUR_HOME", home)
        .args(["--profile", agent_name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", runtime.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin on spawned runtime"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout on spawned runtime"))?;
    // Drain stderr on a thread so a failed startup (invalid profile.id,
    // name mismatch, etc.) is surfaced instead of swallowed (H2). Reading on
    // a thread also avoids a pipe-buffer deadlock if the runtime is chatty.
    let stderr_thread = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf
        })
    });

    let req_line = format!("{}\n", serde_json::to_string(request)?);
    stdin
        .write_all(req_line.as_bytes())
        .context("write to runtime stdin")?;
    drop(stdin);

    let reader = BufReader::new(stdout);
    let mut found: Option<Value> = None;
    let mut last_err: Option<Value> = None;
    for line in reader.lines() {
        let line = line.context("read runtime stdout")?;
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id") == Some(request_id) {
            if let Some(err) = v.get("error") {
                last_err = Some(err.clone());
                break;
            }
            found = Some(v.get("result").cloned().unwrap_or(Value::Null));
            break;
        }
    }

    // Best-effort SIGTERM. The ephemeral runtime will also exit when its
    // stdin closes, but we don't want to wait indefinitely.
    #[cfg(unix)]
    {
        let pid = child.id();
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    let _ = child.wait();

    if let Some(err) = last_err {
        bail!("agent '{agent_name}' returned error: {err}");
    }
    if let Some(result) = found {
        return Ok(result);
    }

    // No JSON-RPC result. Surface the runtime's stderr tail so startup
    // failures (invalid profile.id, name mismatch, sandbox errors) aren't
    // swallowed behind a generic message (H2).
    let stderr = stderr_thread
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let tail: Vec<&str> = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(8)
        .collect();
    if tail.is_empty() {
        bail!("ephemeral runtime did not produce a response for '{agent_name}'");
    }
    let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    bail!("ephemeral runtime for '{agent_name}' exited before responding:\n{tail}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn require_running_fails_without_lock() {
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/nobody")).unwrap();
        let err = dial_method(
            home.path(),
            "nobody",
            "agent/card",
            Value::Null,
            DialMode::RequireRunning,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not running"));
    }

    #[test]
    fn auto_mode_falls_through_to_ephemeral_when_no_lock() {
        // Without a runtime binary on PATH the ephemeral spawn fails
        // with a recognizable error — that's what we assert, since this
        // is a pure unit test.
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/nobody")).unwrap();
        unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", "/does/not/exist") };
        let err = dial_method(
            home.path(),
            "nobody",
            "agent/card",
            Value::Null,
            DialMode::Auto,
        )
        .unwrap_err();
        unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
        let msg = err.to_string();
        assert!(
            msg.contains("runtime binary not found") || msg.contains("spawn"),
            "unexpected error: {msg}"
        );
    }
}
