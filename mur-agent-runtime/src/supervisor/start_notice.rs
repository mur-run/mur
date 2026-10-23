//! Start-time notices on the agent's channel (plan Phase 1, Task 1.5).
//!
//! One signed `Note` per start, in a channel remembered under
//! `START_CHANNEL_FILE` — the same create-once-then-remember shape as the
//! schedule and HITL channels. Not the HITL channel: its signed head is what
//! the gate checks for tampering, and a notice must not move it.
//!
//! Best-effort throughout. A start must never fail because the channel store
//! was busy; the log line and the status file already carry the same facts.

use mur_channel::ChannelService;
use mur_common::agent::START_CHANNEL_FILE;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use mur_common::ssh_proxy_status::SshProxyStatus;
use std::path::Path;

/// `payload.notice` value marking an ssh-agent proxy classification, so a
/// reader can pick these out of the channel without parsing `text`.
pub(super) const SSH_PROXY_NOTICE: &str = "ssh_proxy_classification";

/// Who signs, and as whom.
pub(super) struct Signer<'a> {
    pub mur_home: &'a Path,
    pub agent: &'a str,
    pub identity: &'a AgentIdentity,
    pub key_version: u32,
}

/// The channel start notices go to — created once, then remembered. A marker
/// naming a channel that no longer loads is replaced, not an error.
pub(super) fn start_channel(
    svc: &ChannelService,
    mur_home: &Path,
    agent: &str,
) -> anyhow::Result<String> {
    let marker = mur_home.join("agents").join(agent).join(START_CHANNEL_FILE);
    if let Ok(raw) = std::fs::read_to_string(&marker) {
        let id = raw.trim();
        if !id.is_empty() && svc.exists(id) {
            return Ok(id.to_string());
        }
    }
    let ch = svc.create_for_agent(agent)?;
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&marker, &ch.id)?;
    Ok(ch.id)
}

/// The one line a person reads in `mur channel follow`: the same words as the
/// `ssh_proxy:` line in `mur agent status`.
pub(super) fn ssh_proxy_text(s: &SshProxyStatus) -> String {
    format!("ssh_proxy: {} — {}", s.label(), s.explanation)
}

/// Append the classification. Returns the channel id written to.
pub(super) fn append_ssh_proxy(signer: &Signer<'_>, s: &SshProxyStatus) -> anyhow::Result<String> {
    let svc = ChannelService::open(signer.mur_home)?;
    let channel_id = start_channel(&svc, signer.mur_home, signer.agent)?;
    svc.append_signed(
        &channel_id,
        signer.identity,
        signer.key_version,
        ChannelActor::Agent {
            id: signer.agent.to_string(),
        },
        EventKind::Note,
        serde_json::json!({
            "notice": SSH_PROXY_NOTICE,
            "text": ssh_proxy_text(s),
            "state": s.state,
            "code": s.code,
            "at_unix_ms": s.at_unix_ms,
        }),
        // One event per start: the classification time identifies the start.
        Some(format!("ssh-proxy:{}", s.at_unix_ms)),
    )?;
    Ok(channel_id)
}

#[cfg(test)]
#[path = "start_notice_tests.rs"]
mod tests;
