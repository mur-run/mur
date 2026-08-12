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

/// Default idle read/write timeout for a dialed agent socket. Chosen to sit
/// comfortably above the runtime's default HITL approval wait (300s, see
/// `mur-agent-runtime::task_runner::HitlConfig::default`) plus headroom for
/// slow generation, while still guaranteeing `mur agent send` cannot hang
/// forever on a stalled or crashed peer (dogfood issue: one-shot send hung
/// for 5+ minutes with no feedback). Overridable for tests and for callers
/// who know their turns run long.
const DEFAULT_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(600);

/// Resolve the idle I/O timeout for a dialed socket, honoring
/// `MUR_A2A_IO_TIMEOUT_SECS` (mainly for tests) with a sane default.
fn dial_io_timeout() -> Duration {
    std::env::var("MUR_A2A_IO_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_DIAL_IO_TIMEOUT)
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
    let sock = lock.transports.unix_socket.ok_or_else(|| {
        anyhow!(
            "agent '{agent_name}' has no unix-socket transport (TCP-only transports are not yet supported by the install path)"
        )
    })?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let timeout = dial_io_timeout();
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
        for line in reader.lines() {
            let line = line.map_err(|e| {
                if is_io_timeout(&e) {
                    anyhow!(
                        "agent '{agent_name}' did not respond within {}s; \
                         check `mur agent logs {agent_name}`",
                        timeout.as_secs()
                    )
                } else {
                    anyhow!(e).context("read response line")
                }
            })?;
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
    },
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
    let sock = lock
        .transports
        .unix_socket
        .ok_or_else(|| anyhow!("agent '{agent_name}' has no unix-socket transport"))?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let timeout = dial_io_timeout();
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
        for line in reader.lines() {
            let line = line.map_err(|e| {
                if is_io_timeout(&e) {
                    anyhow!(
                        "agent '{agent_name}' went idle for {}s without a response; \
                         check `mur agent logs {agent_name}`",
                        timeout.as_secs()
                    )
                } else {
                    anyhow!(e).context("read response line")
                }
            })?;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
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
            msg.contains("runtime binary not found")
                || msg.contains("spawn")
                || msg.contains("attestation"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dial_gates_channel_delegate_on_stale_proto() {
        let tmp = tempfile::TempDir::new().unwrap();
        let adir = tmp.path().join("agents").join("rustsmith");
        std::fs::create_dir_all(&adir).unwrap();
        // Old lock: proto_version absent → 0 < channel/delegate's min (1).
        std::fs::write(
            adir.join("running.lock"),
            r#"{"schema":1,"uuid":"u",
          "name":"rustsmith","pid":1,"ppid":1,"started_at":"t",
          "binary_version":"old","transports":{"stdio":true,
          "unix_socket":"/nonexistent.sock"},"card_digest":"d","capabilities":[]}"#,
        )
        .unwrap();

        let err = dial_method(
            tmp.path(),
            "rustsmith",
            "channel/delegate",
            serde_json::json!({}),
            DialMode::RequireRunning,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("stale runtime"), "got: {msg}");
        assert!(msg.contains("mur agent restart rustsmith"), "got: {msg}");
        // It must NOT have tried to connect to the (nonexistent) socket.
        assert!(
            !msg.contains("connect"),
            "gate should fire before dialing: {msg}"
        );
    }

    #[test]
    fn dial_does_not_gate_ungated_method() {
        // message/send (min 0) on the same stale lock must NOT be proto-gated
        // (it will fail later for a different reason — socket — which is fine).
        let tmp = tempfile::TempDir::new().unwrap();
        let adir = tmp.path().join("agents").join("a");
        std::fs::create_dir_all(&adir).unwrap();
        std::fs::write(
            adir.join("running.lock"),
            r#"{"schema":1,"uuid":"u","name":"a",
          "pid":1,"ppid":1,"started_at":"t","binary_version":"old",
          "transports":{"stdio":true,"unix_socket":"/nonexistent.sock"},
          "card_digest":"d","capabilities":[]}"#,
        )
        .unwrap();
        let err = dial_method(
            tmp.path(),
            "a",
            "message/send",
            serde_json::json!({}),
            DialMode::RequireRunning,
        )
        .unwrap_err();
        assert!(
            !err.to_string().contains("stale runtime"),
            "must not gate message/send"
        );
    }
}

#[cfg(test)]
#[cfg(unix)]
mod timeout_tests {
    //! Regression coverage for the dogfood hang: `mur agent send` blocked
    //! indefinitely against a peer that accepted the connection but never
    //! wrote a response line (simulating a stalled/wedged runtime, or one
    //! silently waiting past the client's patience). `dial_socket` must now
    //! bound the wait and return an actionable error instead of hanging.
    //!
    //! Gated behind `MUR_TEST_SOCKETS=1`: some sandboxed CI/dev environments
    //! deny `AF_UNIX` `connect(2)` outright (observed as `EPERM`, "Operation
    //! not permitted", even against a freshly bound `/tmp` socket with a
    //! same-process listener already accepting) regardless of socket path
    //! length or `TMPDIR` location. The test is fully functional on CI and
    //! normal dev machines; set the env var there (or anywhere unix-domain
    //! sockets are actually permitted) to run it.
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    /// Serial guard: `MUR_A2A_IO_TIMEOUT_SECS` is process-global env, and
    /// `cargo test` runs tests in this file concurrently by default.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn dial_socket_times_out_instead_of_hanging_forever() {
        if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
            eprintln!(
                "skipping dial_socket_times_out_instead_of_hanging_forever: \
                 set MUR_TEST_SOCKETS=1 on a machine that permits AF_UNIX \
                 connects (see module docs for why this is gated)"
            );
            return;
        }
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("agent.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Fake peer: accepts the connection, reads the request so the
        // client's write doesn't itself block, then goes silent forever
        // (drops the byte stream only when the test ends and the listener
        // thread's stream is dropped).
        let server = std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                // Drain whatever the client sent; ignore the result — we
                // never reply, which is the whole point of this test.
                let _ = conn.read(&mut buf);
                // Hold the connection open (don't drop `conn`) well past
                // the client's configured timeout, so a passing test proves
                // the client-side timeout fired rather than an EOF race.
                std::thread::sleep(Duration::from_secs(5));
            }
        });

        let agent_dir = tmp.path().join("agents").join("stalled");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("running.lock"),
            serde_json::json!({
                "schema": 1,
                "uuid": "u",
                "name": "stalled",
                "pid": 1,
                "ppid": 1,
                "started_at": "t",
                "binary_version": "test",
                "transports": { "stdio": true, "unix_socket": sock_path.to_str().unwrap() },
                "card_digest": "d",
                "capabilities": [],
            })
            .to_string(),
        )
        .unwrap();

        // SAFETY (test-only env mutation): serialized by `ENV_LOCK` above so
        // no other test in this process observes a torn value.
        unsafe { std::env::set_var("MUR_A2A_IO_TIMEOUT_SECS", "1") };
        let start = std::time::Instant::now();
        let result = dial_method(
            tmp.path(),
            "stalled",
            "message/send",
            serde_json::json!({}),
            DialMode::RequireRunning,
        );
        let elapsed = start.elapsed();
        unsafe { std::env::remove_var("MUR_A2A_IO_TIMEOUT_SECS") };

        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("did not respond") && msg.contains("stalled"),
            "expected an actionable timeout error, got: {msg}"
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "dial_socket should time out near the configured 1s bound, took {elapsed:?} \
             (a regression here means the hang is back)"
        );

        let _ = server.join();
    }
}

#[cfg(test)]
mod step_parse_tests {
    use super::{StepEvent, parse_step};

    #[test]
    fn parses_started() {
        let p = serde_json::json!({
            "step_id": "s1",
            "task_id": "t1",
            "name": "edit",
            "args": { "path": "a.rs" }
        });
        match parse_step(&p, false) {
            StepEvent::Started {
                step_id,
                task_id,
                name,
                args,
            } => {
                assert_eq!(step_id, "s1");
                assert_eq!(task_id, "t1");
                assert_eq!(name, "edit");
                assert_eq!(args["path"], "a.rs");
            }
            StepEvent::Completed { .. } => panic!("expected Started"),
        }
    }

    #[test]
    fn parses_completed() {
        let p = serde_json::json!({
            "step_id": "s2",
            "task_id": "t2",
            "ok": true,
            "output": "done",
            "truncated": false,
            "full_len": 4u64,
            "error": null,
            "duration_ms": 123u64,
            "denied": false
        });
        match parse_step(&p, true) {
            StepEvent::Completed {
                step_id,
                task_id,
                ok,
                output,
                truncated,
                full_len,
                error,
                duration_ms,
                denied,
            } => {
                assert_eq!(step_id, "s2");
                assert_eq!(task_id, "t2");
                assert!(ok);
                assert_eq!(output, "done");
                assert!(!truncated);
                assert_eq!(full_len, 4);
                assert!(error.is_none());
                assert_eq!(duration_ms, 123);
                assert!(!denied);
            }
            StepEvent::Started { .. } => panic!("expected Completed"),
        }
    }
}
