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
fn probe_socket(sock: &str, timeout: Duration) -> Result<()> {
    use anyhow::Context;
    use std::io::{BufRead, BufReader, Write};
    let mut stream =
        std::os::unix::net::UnixStream::connect(sock).with_context(|| format!("connect {sock}"))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    let req =
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "agent/card", "params": null});
    stream.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    let n = BufReader::new(stream)
        .read_line(&mut line)
        .with_context(|| format!("no reply within {}s", timeout.as_secs()))?;
    if n == 0 {
        bail!("socket closed without a reply");
    }
    Ok(())
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
