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
use mur_common::agent::{OverrideRecord, START_CHANNEL_FILE};
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

/// `payload.notice` value marking one ssh-agent proxy refusal.
pub(super) const SSH_REFUSAL_NOTICE: &str = "ssh_proxy_refusal";

/// A `'static` poster for the proxy's refusal sink: each refusal becomes one
/// signed note on the start channel. Best-effort, like every notice here.
pub(super) fn refusal_poster(
    signer: &Signer<'_>,
) -> impl Fn(&crate::ssh_proxy::refusal::Refusal, &str) + Send + Sync + 'static {
    let mur_home = signer.mur_home.to_path_buf();
    let agent = signer.agent.to_string();
    let identity = signer.identity.clone();
    let key_version = signer.key_version;
    move |r, message| {
        let signer = Signer {
            mur_home: &mur_home,
            agent: &agent,
            identity: &identity,
            key_version,
        };
        if let Err(e) = append_ssh_refusal(&signer, r.code.as_str(), message) {
            tracing::warn!(error = %e, "could not post the ssh-agent proxy refusal to the agent's channel");
        }
    }
}

/// Append one refusal. Returns the channel id written to.
pub(super) fn append_ssh_refusal(
    signer: &Signer<'_>,
    code: &str,
    message: &str,
) -> anyhow::Result<String> {
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
            "notice": SSH_REFUSAL_NOTICE,
            "text": message,
            "code": code,
        }),
        // Every refusal is its own event.
        None,
    )?;
    Ok(channel_id)
}

/// `payload.notice` value marking the startup-override banner (boot step 11).
pub(super) const STARTUP_OVERRIDE_NOTICE: &str = "startup_override";

/// The banner a person reads in `mur channel follow`, in the same words as
/// the `⚠ override active` line in `mur agent status`.
pub(super) fn override_text(record: &OverrideRecord) -> String {
    format!(
        "⚠ startup override active ({}): {} — this boot only; restart without flags to end it",
        record.nonce,
        record.flags.cli_names().join(", ")
    )
}

/// Post the banner. Returns the channel id written to.
pub(super) fn append_override(
    signer: &Signer<'_>,
    record: &OverrideRecord,
    changes: &[String],
) -> anyhow::Result<String> {
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
            "notice": STARTUP_OVERRIDE_NOTICE,
            "text": override_text(record),
            "nonce": record.nonce,
            "flags": record.flags,
            "confirmed_by": record.confirmed_by,
            "changes": changes,
        }),
        // One banner per override: the nonce is single-use.
        Some(format!("startup-override:{}", record.nonce)),
    )?;
    Ok(channel_id)
}

#[cfg(test)]
#[path = "start_notice_tests.rs"]
mod tests;
