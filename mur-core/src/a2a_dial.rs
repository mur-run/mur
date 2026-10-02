//! Shared A2A dial helper — issues a JSON-RPC request to a local agent,
//! either via its running Unix socket or by spawning the runtime
//! ephemerally in stdio mode.
//!
//! Consumed by:
//!   - `cmd/agent/comm.rs` for `mur agent card` / `mur agent send`
//!   - `cmd/skill_install.rs` for `mur skill install agent://...`

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use mur_common::LockFile;
use serde_json::{Value, json};

use crate::cmd::agent::{attest::verify_runtime_at, resolve_runtime_target};

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

/// Idle read/write timeout for a peer that does NOT beat (proto < 2). Sits
/// above the runtime's default HITL wait (300 s) plus generation headroom;
/// never raise it — a beating peer is what makes a short timeout safe (spec
/// 2026-09-12 execution-limits D7).
const LEGACY_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(600);
/// Idle timeout for a peer that beats every 30 s (spec §3.6): three missed
/// beats. Every heartbeat, delta and step frame resets it.
const HEARTBEAT_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(90);

/// The idle timeout for THIS peer, from its running.lock: the env override
/// (`MUR_A2A_IO_TIMEOUT_SECS`, tests) wins; else a runtime that advertises
/// heartbeats gets the short one and anything older keeps the long one, so a
/// fleet mid-upgrade never starts failing at 90 s on a router that is merely
/// thinking.
fn dial_io_timeout_for(proto: u32) -> Duration {
    if let Some(d) = std::env::var("MUR_A2A_IO_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
    {
        return d;
    }
    if proto >= mur_common::build::HEARTBEAT_MIN_PROTO {
        HEARTBEAT_DIAL_IO_TIMEOUT
    } else {
        LEGACY_DIAL_IO_TIMEOUT
    }
}

/// What a read timeout means, in words that fit the peer: a beating peer
/// that went silent STOPPED RESPONDING (and we say when it last spoke); a
/// legacy peer merely did not answer in time.
fn timeout_error(
    agent_name: &str,
    proto: u32,
    timeout: Duration,
    last_frame: Option<&(String, String)>,
    legacy_wording: &str,
) -> anyhow::Error {
    if proto >= mur_common::build::HEARTBEAT_MIN_PROTO {
        anyhow!(
            "agent '{agent_name}' stopped responding — no frame for {}s (last: {}); \
             check `mur agent logs {agent_name}`",
            timeout.as_secs(),
            last_frame
                .map(|(m, at)| format!("{at} · {m}"))
                .unwrap_or_else(|| "none since the request".into())
        )
    } else {
        anyhow!(
            "agent '{agent_name}' {legacy_wording} {}s; check `mur agent logs {agent_name}`",
            timeout.as_secs()
        )
    }
}

/// Remember the last non-response frame the peer sent — its method and its
/// own timestamp when it carries one — for the timeout message.
fn note_frame(last_frame: &mut Option<(String, String)>, v: &Value) {
    if let Some(m) = v.get("method").and_then(Value::as_str) {
        let at = v
            .get("params")
            .and_then(|p| p.get("at"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        *last_frame = Some((m.to_string(), at));
    }
}

/// True if `err` looks like a socket read/write timeout (`SO_RCVTIMEO`/
/// `SO_SNDTIMEO` firing), as opposed to a genuine connection error.
fn is_io_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
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

    // Pre-flight version gate: refuse a versioned method against a running peer
    // whose advertised proto is too low — with an actionable error, not -32601.
    // Reads the peer's running.lock (cheap, local). Ungated methods (min 0) skip.
    if is_running {
        let needed = mur_common::build::method_min_proto(method);
        if needed > 0
            && let Ok(bytes) = fs::read(&lock_path)
            && let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
            && lock.proto_version < needed
        {
            let sha = if lock.build_sha.is_empty() {
                "unknown"
            } else {
                &lock.build_sha
            };
            bail!(
                "agent '{agent_name}' is running a stale runtime (proto {}, build {}); \
                 the requested capability '{method}' needs proto {needed}. \
                 Run 'mur agent restart {agent_name}' to apply the installed runtime.",
                lock.proto_version,
                sha
            );
        }
    }

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
    let proto = lock.proto_version;
    let sock = lock.transports.unix_socket.ok_or_else(|| {
        anyhow!(
            "agent '{agent_name}' has no unix-socket transport (TCP-only transports are not yet supported by the install path)"
        )
    })?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let timeout = dial_io_timeout_for(proto);
        let mut stream = std::os::unix::net::UnixStream::connect(&sock)
            .with_context(|| format!("connect {sock}"))?;
        // Bound both directions: without these, a stalled or crashed peer
        // (or one silently waiting on a HITL prompt) blocks `mur agent send`
        // forever with no feedback (dogfood issue: hung for 5+ minutes).
        stream
            .set_write_timeout(Some(timeout))
            .context("set write timeout")?;
        stream
            .set_read_timeout(Some(timeout))
            .context("set read timeout")?;
        let line = format!("{}\n", serde_json::to_string(request)?);
        stream.write_all(line.as_bytes()).map_err(|e| {
            if is_io_timeout(&e) {
                anyhow!(
                    "agent '{agent_name}' did not accept the request within {}s (write timed out); \
                     check `mur agent logs {agent_name}`",
                    timeout.as_secs()
                )
            } else {
                anyhow!(e).context("write request")
            }
        })?;
        stream.flush().context("flush request")?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut last_frame: Option<(String, String)> = None;
        for line in reader.lines() {
            let line = line.map_err(|e| {
                if is_io_timeout(&e) {
                    timeout_error(
                        agent_name,
                        proto,
                        timeout,
                        last_frame.as_ref(),
                        "did not respond within",
                    )
                } else {
                    anyhow!(e).context("read response line")
                }
            })?;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            note_frame(&mut last_frame, &v);
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

/// A tool-step notification received from the runtime during a streaming turn.
/// Emitted by the runtime as `step/started` and `step/completed` JSON-RPC
/// notifications; parsed by [`parse_step`] and forwarded via the `on_step`
/// callback of [`dial_message_streaming`].
#[derive(Debug)]
pub enum StepEvent {
    /// The agent started executing a tool call.
    Started {
        step_id: String,
        task_id: String,
        name: String,
        args: Value,
    },
    /// A tool call finished.
    Completed {
        step_id: String,
        task_id: String,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
        /// The sandbox refused to run this call (e.g. a denied write). Distinct
        /// from a legitimate non-zero exit (`ok: false`); an older runtime that
        /// doesn't send this key is treated as not-denied.
        denied: bool,
        /// The call yielded and the command is still running (a runtime ≥ the
        /// bash-yield release sends this; older ones omit it = not running).
        running: bool,
    },
    /// Tokens the model received for a finished call, counted by the runtime
    /// after its post-tool hooks (compression, redaction) rewrote the result.
    /// Arrives after `Completed`; an older runtime never sends it.
    Tokens {
        step_id: String,
        task_id: String,
        tokens: usize,
    },
}

/// Parse the `params` of a `step/tokens` notification, or `None` when the
/// frame lacks the count (nothing to show, so nothing to forward).
pub fn parse_step_tokens(p: &Value) -> Option<StepEvent> {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let tokens = p.get("tokens").and_then(Value::as_u64)? as usize;
    Some(StepEvent::Tokens {
        step_id: s("step_id"),
        task_id: s("task_id"),
        tokens,
    })
}

/// Parse the `params` of a `step/started` (`completed = false`) or
/// `step/completed` (`completed = true`) JSON-RPC notification into a
/// [`StepEvent`].
pub fn parse_step(p: &Value, completed: bool) -> StepEvent {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let step_id = s("step_id");
    let task_id = s("task_id");
    if completed {
        StepEvent::Completed {
            step_id,
            task_id,
            ok: p.get("ok").and_then(Value::as_bool).unwrap_or(true),
            output: s("output"),
            truncated: p.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            full_len: p.get("full_len").and_then(Value::as_u64).unwrap_or(0) as usize,
            error: p.get("error").and_then(Value::as_str).map(str::to_string),
            duration_ms: p.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
            denied: p.get("denied").and_then(Value::as_bool).unwrap_or(false),
            running: p.get("running").and_then(Value::as_bool).unwrap_or(false),
        }
    } else {
        StepEvent::Started {
            step_id,
            task_id,
            name: s("name"),
            args: p.get("args").cloned().unwrap_or(Value::Null),
        }
    }
}

/// Dial a *running* agent's `message/send` and stream token deltas to
/// `on_delta` as they arrive, returning the final task result. Requires the
/// agent to be up (uses its unix socket); the runtime emits `message/delta`
/// notifications during generation. Names resolve case-insensitively.
#[allow(dead_code)] // used by workspace-excluded mur-hub-gui
/// Stream a `message/send` turn. `on_delta` receives `(text, thinking,
/// task_id)`, where `task_id` is the turn id the runtime stamps on each
/// `message/delta` (empty string if the agent predates per-connection routing).
/// The runtime already routes deltas to this connection only; the id lets a
/// client defensively drop anything not matching the turn it issued.
/// `on_step` receives tool-step events (`step/started`, `step/completed`).
pub fn dial_message_streaming(
    home: &Path,
    agent_name: &str,
    params: Value,
    mut on_delta: impl FnMut(&str, bool, &str),
    mut on_hitl: impl FnMut(Value),
    mut on_step: impl FnMut(StepEvent),
) -> Result<Value> {
    let agent_name = &canonicalize_agent_name(home, agent_name);
    let lock_path = home.join("agents").join(agent_name).join("running.lock");
    if !lock_path.exists() {
        bail!(
            "agent '{agent_name}' is not running (no {})",
            lock_path.display()
        );
    }
    let request_id = json!(1);
    let request = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "message/send",
        "params": params,
    });
    let bytes = fs::read(&lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    let lock: LockFile = serde_json::from_slice(&bytes).context("parse running.lock")?;
    let proto = lock.proto_version;
    let sock = lock
        .transports
        .unix_socket
        .ok_or_else(|| anyhow!("agent '{agent_name}' has no unix-socket transport"))?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let timeout = dial_io_timeout_for(proto);
        let mut stream = std::os::unix::net::UnixStream::connect(&sock)
            .with_context(|| format!("connect {sock}"))?;
        // Bound both directions — see `dial_socket` for why. This is an idle
        // timeout: each `message/delta`/`step/*` notification during
        // generation resets it, so legitimately long-but-active turns are
        // unaffected; only a genuinely stalled peer trips it.
        stream
            .set_write_timeout(Some(timeout))
            .context("set write timeout")?;
        stream
            .set_read_timeout(Some(timeout))
            .context("set read timeout")?;
        let line = format!("{}\n", serde_json::to_string(&request)?);
        stream.write_all(line.as_bytes()).map_err(|e| {
            if is_io_timeout(&e) {
                anyhow!(
                    "agent '{agent_name}' did not accept the request within {}s (write timed out); \
                     check `mur agent logs {agent_name}`",
                    timeout.as_secs()
                )
            } else {
                anyhow!(e).context("write request")
            }
        })?;
        stream.flush().context("flush request")?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut last_frame: Option<(String, String)> = None;
        for line in reader.lines() {
            let line = line.map_err(|e| {
                if is_io_timeout(&e) {
                    timeout_error(
                        agent_name,
                        proto,
                        timeout,
                        last_frame.as_ref(),
                        "went idle for",
                    )
                } else {
                    anyhow!(e).context("read response line")
                }
            })?;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            note_frame(&mut last_frame, &v);
            if v.get("method").and_then(Value::as_str) == Some("message/delta") {
                let params = v.get("params");
                if let Some(t) = params.and_then(|p| p.get("text")).and_then(Value::as_str) {
                    let thinking = params
                        .and_then(|p| p.get("thinking"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let delta_task_id = params
                        .and_then(|p| p.get("task_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    on_delta(t, thinking, delta_task_id);
                }
                continue;
            }
            if v.get("method").and_then(Value::as_str) == Some("step/started") {
                if let Some(params) = v.get("params") {
                    on_step(parse_step(params, false));
                }
                continue;
            }
            if v.get("method").and_then(Value::as_str) == Some("step/completed") {
                if let Some(params) = v.get("params") {
                    on_step(parse_step(params, true));
                }
                continue;
            }
            if v.get("method").and_then(Value::as_str) == Some("step/tokens") {
                if let Some(ev) = v.get("params").and_then(parse_step_tokens) {
                    on_step(ev);
                }
                continue;
            }
            if v.get("method").and_then(Value::as_str) == Some("tool/approval_needed") {
                if let Some(params) = v.get("params").cloned() {
                    on_hitl(params);
                }
                continue;
            }
            if v.get("id") == Some(&request_id) {
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
    verify_runtime_at(&runtime).with_context(|| {
        format!("cannot reach agent '{agent_name}' — runtime attestation failed")
    })?;

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
mod tests;

#[cfg(test)]
#[cfg(unix)]
mod timeout_tests;

#[cfg(test)]
mod step_parse_tests;
