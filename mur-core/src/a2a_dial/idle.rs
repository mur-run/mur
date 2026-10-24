//! The per-peer idle timeout of a dial and the words for when it fires.

use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;

/// Idle read/write timeout for a peer that does NOT beat (proto < 2). Sits
/// above the runtime's default HITL wait (300 s) plus generation headroom;
/// never raise it — a beating peer is what makes a short timeout safe (spec
/// 2026-09-12 execution-limits D7).
pub(super) const LEGACY_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(600);
/// Idle timeout for a peer that beats every 30 s (spec §3.6): three missed
/// beats. Every heartbeat, delta and step frame resets it.
pub(super) const HEARTBEAT_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(90);

/// The idle timeout for THIS peer, from its running.lock: the env override
/// (`MUR_A2A_IO_TIMEOUT_SECS`, tests) wins; else a runtime that advertises
/// heartbeats gets the short one and anything older keeps the long one, so a
/// fleet mid-upgrade never starts failing at 90 s on a router that is merely
/// thinking.
pub(super) fn dial_io_timeout_for(proto: u32) -> Duration {
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
pub(super) fn timeout_error(
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
pub(super) fn note_frame(last_frame: &mut Option<(String, String)>, v: &Value) {
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
pub(super) fn is_io_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}
