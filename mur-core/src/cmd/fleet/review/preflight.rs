//! #1762: member checks that run BEFORE a review session creates its fleet
//! and channel, so a misconfigured pair fails at the start with a fix instead
//! of minutes in with a half-built session.
//!
//! Two failures the issue reports, both caught here:
//! 1. a member whose `running.lock` outlived its runtime (a stale socket) —
//!    the lock-exists test in `require_running` passes, the first send fails;
//! 2. a member whose `hitl.timeout_secs` is shorter than the session's stuck
//!    window — an `Allow?` answered late is rejected as expired.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use mur_common::HitlConfig;
use mur_common::limits::Stuck;

use super::constants::{PREFLIGHT_PROBE_TIMEOUT, RUNNING_LOCK};

/// Proves a member's runtime answers on its socket. A trait so the checks
/// run without a live runtime; the real one is [`SocketProbe`].
pub(crate) trait MemberProbe {
    /// `Ok` = the member answered a request; `Err` = why it did not.
    fn round_trip(&self, mur_home: &Path, member: &str) -> Result<()>;
}

/// The least `hitl.timeout_secs` a member needs so an approval answered
/// anywhere inside the session's stuck window is still accepted. `None` when
/// stuck detection is off: there is no window to outlast. Rounds UP, so a
/// sub-second remainder never leaves the agent a hair short.
pub(crate) fn required_hitl_timeout(stuck: Stuck) -> Option<u32> {
    match stuck {
        Stuck::Off => None,
        Stuck::After(d) => {
            Some(u32::try_from(d.as_secs() + u64::from(d.subsec_nanos() > 0)).unwrap_or(u32::MAX))
        }
    }
}

/// `hitl.timeout_secs` as the member's `profile.yaml` states it, or the
/// built-in default when the key (or the whole block) is absent. Reads only
/// that key, so a profile this check does not care about never fails it.
fn configured_hitl_timeout(mur_home: &Path, member: &str) -> Result<u32> {
    let path = mur_home.join("agents").join(member).join("profile.yaml");
    let yaml = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    Ok(match doc.get("hitl").and_then(|h| h.get("timeout_secs")) {
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                anyhow::anyhow!("{}: hitl.timeout_secs is not a number", path.display())
            })?,
        None => HitlConfig::default().timeout_secs,
    })
}

/// Check every member; every problem is reported together, so one run shows
/// the whole list instead of one failure per attempt. Nothing is created or
/// changed.
pub(crate) fn preflight_members(
    mur_home: &Path,
    members: &[&str],
    stuck: Stuck,
    probe: &dyn MemberProbe,
) -> Result<()> {
    let need = required_hitl_timeout(stuck);
    let mut problems: Vec<String> = Vec::new();
    for m in members {
        if let Err(e) = probe.round_trip(mur_home, m) {
            problems.push(format!(
                "'{m}' does not answer ({e:#}). Restart it: mur agent restart {m}"
            ));
        }
        let Some(need) = need else { continue };
        match configured_hitl_timeout(mur_home, m) {
            Ok(have) if have >= need => {}
            Ok(have) => problems.push(format!(
                "'{m}' has hitl.timeout_secs {have}, shorter than the {need}s stuck window: an \
                 approval answered late would be rejected as expired. Set hitl.timeout_secs to \
                 at least {need} in {} and run: mur agent restart {m}",
                mur_home
                    .join("agents")
                    .join(m)
                    .join("profile.yaml")
                    .display()
            )),
            Err(e) => problems.push(format!("'{m}': cannot read hitl.timeout_secs: {e:#}")),
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    bail!("cannot start the review:\n  - {}", problems.join("\n  - "));
}

/// The real probe: connect to the unix socket the member's `running.lock`
/// names and wait for any reply to a cheap request, bounded by
/// [`PREFLIGHT_PROBE_TIMEOUT`] so a wedged runtime cannot stall the start.
#[cfg_attr(test, allow(dead_code))] // tests swap in `AlwaysUp`, see `preflight`
pub(crate) struct SocketProbe;

impl MemberProbe for SocketProbe {
    fn round_trip(&self, mur_home: &Path, member: &str) -> Result<()> {
        use anyhow::Context;
        let lock_path = mur_home.join("agents").join(member).join(RUNNING_LOCK);
        let bytes =
            std::fs::read(&lock_path).with_context(|| format!("read {}", lock_path.display()))?;
        let lock: mur_common::LockFile =
            serde_json::from_slice(&bytes).context("parse running.lock")?;
        let sock = lock
            .transports
            .unix_socket
            .context("running.lock names no unix socket")?;
        probe_socket(&sock, PREFLIGHT_PROBE_TIMEOUT)
    }
}

#[cfg(unix)]
/// JSON-RPC id of the probe request; only a reply carrying it counts.
const PROBE_REQUEST_ID: u64 = 1;

/// One absolute deadline covers connect, write and every read: a peer that
/// trickles bytes cannot stretch the probe past `timeout`. The work runs on a
/// helper thread and the caller waits with `recv_timeout`, so even a connect
/// that blocks (full accept backlog) is bounded; the thread's own socket
/// timeouts are re-armed from the remaining time and let it end soon after.
#[cfg(unix)]
fn probe_socket(sock: &str, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    let (tx, rx) = std::sync::mpsc::channel();
    let path = sock.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(probe_socket_until(&path, deadline));
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => bail!("no reply within {}s", timeout.as_secs()),
    }
}

#[cfg(unix)]
fn probe_socket_until(sock: &str, deadline: std::time::Instant) -> Result<()> {
    use anyhow::Context;
    use std::io::{Read, Write};
    let remaining = || {
        deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .context("probe deadline passed")
    };
    let mut stream =
        std::os::unix::net::UnixStream::connect(sock).with_context(|| format!("connect {sock}"))?;
    stream.set_write_timeout(Some(remaining()?))?;
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": PROBE_REQUEST_ID, "method": "agent/card", "params": null
    });
    stream.write_all(format!("{req}\n").as_bytes())?;
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        // Re-armed before every read, so the limit is the deadline, not a
        // per-read allowance.
        stream.set_read_timeout(Some(remaining()?))?;
        let n = stream
            .read(&mut chunk)
            .context("no reply before the probe deadline")?;
        if n == 0 {
            bail!("socket closed without a reply");
        }
        pending.extend_from_slice(&chunk[..n]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if let Some(outcome) = judge_reply(&line) {
                return outcome;
            }
        }
    }
}

#[cfg(unix)]
/// `Some` once `line` is the reply to the probe request: `Ok` for a result,
/// `Err` for a JSON-RPC error (the runtime's `accepts_from` gate answers
/// `CommunicationDenied` this way, and the first real send would fail the same
/// way). `None` for anything else — notifications, other ids, non-JSON — which
/// the caller skips.
fn judge_reply(line: &[u8]) -> Option<Result<()>> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    if v.get("id").and_then(serde_json::Value::as_u64) != Some(PROBE_REQUEST_ID) {
        return None;
    }
    if let Some(err) = v.get("error") {
        let code = err.get("code").and_then(serde_json::Value::as_i64);
        let msg = err
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no message");
        return Some(Err(anyhow::anyhow!(
            "the runtime refused the request (code {}): {msg}",
            code.map_or_else(|| "?".to_string(), |c| c.to_string())
        )));
    }
    v.get("result").map(|_| Ok(()))
}

#[cfg(not(unix))]
fn probe_socket(_sock: &str, _timeout: Duration) -> Result<()> {
    bail!("unix socket transport is only supported on unix hosts")
}

/// Run the checks with the real probe. Unit tests that reach this through
/// `prepare_session` have no live runtime, so they get an always-up probe;
/// the checks themselves are covered against `preflight_members` directly.
pub(crate) fn preflight(mur_home: &Path, members: &[&str], stuck: Stuck) -> Result<()> {
    #[cfg(test)]
    let probe = AlwaysUp;
    #[cfg(not(test))]
    let probe = SocketProbe;
    preflight_members(mur_home, members, stuck, &probe)
}

#[cfg(test)]
struct AlwaysUp;

#[cfg(test)]
impl MemberProbe for AlwaysUp {
    fn round_trip(&self, _: &Path, _: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod preflight_tests;
