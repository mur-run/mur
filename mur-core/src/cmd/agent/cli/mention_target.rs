//! `@agent[#N]` → a deliverable conversation (at-mention C3, design §3.4,
//! §3.5, §7). Shared by `mur agent send '@agent#N'` and murmur's `@`.
//!
//! Every refusal here happens before anything is written: no channel is
//! created, no participant added, no event appended. A `#N` that does not
//! name a conversation of the target is reported with its §7 code, in the §7
//! order (`UNKNOWN_CONVERSATION` → `CHANNEL_NOT_CONVERSATION` →
//! `NOT_A_PARTICIPANT`).

use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{Channel, ChannelActor, ChannelPurpose};

use super::mention::{Mention, MentionError, parse_mention};
use super::mention_resolve::{ResolveError, Resolved, resolve};

/// Why an `@` target cannot be delivered. The message leads with the §7
/// code so a script can branch on it.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("malformed mention: {0}")]
    Malformed(String),
    #[error("RESERVED_NAME: '{0}' is reserved (agent, fleet, all)")]
    ReservedName(String),
    #[error("UNKNOWN_TARGET: no agent or fleet named '{0}'")]
    UnknownTarget(String),
    #[error("AMBIGUOUS_TARGET: '{0}' names more than one target; write @agent:{0} or @fleet:{0}")]
    AmbiguousTarget(String),
    #[error("fleet targets are Phase 4: '@fleet:{0}' cannot be delivered yet")]
    FleetNotSupported(String),
    #[error("UNKNOWN_CONVERSATION: there is no channel #{0}")]
    UnknownConversation(u64),
    #[error(
        "CHANNEL_NOT_CONVERSATION: channel #{0} is a fleet or workflow run, not a conversation"
    )]
    ChannelNotConversation(u64),
    #[error("NOT_A_PARTICIPANT: {agent} is not in channel #{ordinal}")]
    NotAParticipant { agent: String, ordinal: u64 },
    #[error("TARGET_OFFLINE: agent '{0}' is not running; start it with 'mur agent start {0}'")]
    TargetOffline(String),
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

/// A conversation turn whose Human event is written, ready to dial.
#[derive(Debug)]
pub struct PreparedSend {
    /// Canonical agent name to dial.
    pub agent: String,
    pub channel_id: String,
    /// The channel's ordinal (`0` = not numbered yet).
    pub ordinal: u64,
    /// Seq of the Human event this turn answers.
    pub channel_seq: u64,
    /// `message/send` params: the text, `context.channel_id` +
    /// `context.channel_seq`, `can_approve: false`.
    pub params: serde_json::Value,
}

/// Resolve `@<target>` (the leading `@` included) to a canonical agent name.
/// Fleets resolve but are refused: delivery to a fleet is Phase 4.
pub fn resolve_agent(home: &Path, target: &str) -> Result<(String, Option<u64>), TargetError> {
    let m = match parse_mention(target) {
        None => parse_mention(&format!("@{target}")).expect("prefixed with @"),
        Some(r) => r,
    }
    .map_err(TargetError::from)?;
    if !m.rest.is_empty() {
        return Err(TargetError::Malformed(format!(
            "{target}: the target is one token; pass the message separately"
        )));
    }
    Ok((resolve_mention(home, &m)?, m.ordinal))
}

impl From<MentionError> for TargetError {
    fn from(e: MentionError) -> Self {
        match e {
            MentionError::Malformed(s) => Self::Malformed(s),
            MentionError::ReservedName(s) => Self::ReservedName(s),
        }
    }
}

/// The canonical agent a parsed mention names, over the agents on disk and
/// the stored fleets (§3.5: every namespace is consulted).
pub fn resolve_mention(home: &Path, m: &Mention) -> Result<String, TargetError> {
    let agents: Vec<String> = crate::a2a_dial::agent_dir_names(home)
        .into_iter()
        .filter(|n| mur_common::validate_agent_name(n).is_ok())
        .collect();
    let fleets = crate::cmd::fleet::store::list_fleets(home)?;
    match resolve(m, &agents, &fleets) {
        Ok(Resolved::Agent(a)) => Ok(a),
        Ok(Resolved::Fleet(f)) => Err(TargetError::FleetNotSupported(f)),
        Err(ResolveError::UnknownTarget(n)) => Err(TargetError::UnknownTarget(n)),
        Err(ResolveError::AmbiguousTarget(n)) => Err(TargetError::AmbiguousTarget(n)),
        Err(ResolveError::ReservedName(n)) => Err(TargetError::ReservedName(n)),
    }
}

/// `#n` for `agent`, or the §7 reason it is not addressable. Never writes.
pub fn conversation_at(svc: &ChannelService, n: u64, agent: &str) -> Result<Channel, TargetError> {
    if let Some(ch) = svc.conversation_by_ordinal(n, agent)? {
        return Ok(ch);
    }
    // `conversation_by_ordinal` folds three refusals into `None`; tell them
    // apart in the §7 order. `#0` is the "not numbered" sentinel: unknown.
    let Some(id) = svc.index().by_ordinal(n)? else {
        return Err(TargetError::UnknownConversation(n));
    };
    let Ok(ch) = svc.store().load_manifest(&id) else {
        return Err(TargetError::UnknownConversation(n));
    };
    if mur_channel::purpose::effective_purpose(&ch) != ChannelPurpose::Conversation {
        return Err(TargetError::ChannelNotConversation(n));
    }
    let member = ch
        .participants
        .iter()
        .any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent));
    if member {
        // Raced: it became addressable between the two reads.
        return Ok(ch);
    }
    Err(TargetError::NotAParticipant {
        agent: agent.to_string(),
        ordinal: n,
    })
}

/// `mur agent send '@agent[#N]' <text>`: resolve, refuse what cannot be
/// delivered, then append `text` as the Human event (writer = the target
/// agent) and build the dial params naming it.
///
/// Without `#N` the agent's latest conversation is continued, or a new one
/// created. A stopped agent is refused before the append, so a send that
/// cannot be delivered leaves no orphan Human event behind.
pub fn prepare_send(home: &Path, target: &str, text: &str) -> Result<PreparedSend, TargetError> {
    let (agent, ordinal) = resolve_agent(home, target)?;
    let svc = ChannelService::open(home)?;
    let existing = match ordinal {
        Some(n) => Some(conversation_at(&svc, n, &agent)?),
        None => None,
    };
    if !home
        .join("agents")
        .join(&agent)
        .join("running.lock")
        .exists()
    {
        return Err(TargetError::TargetOffline(agent));
    }
    let channel_id = match existing {
        Some(ch) => ch.id,
        None => match svc.latest_for_agent(&agent)? {
            Some(id) => id,
            None => svc.create_for_agent(&agent)?.id,
        },
    };
    let text = mur_channel::conversation::normalize_user_text(text);
    let ev = crate::channel_writer::append_as_writer(
        &svc,
        home,
        &channel_id,
        &agent,
        ChannelActor::local_human(),
        mur_common::channel::EventKind::Message,
        serde_json::json!({ "text": text }),
        None,
    )?;
    let ordinal = svc.index().ordinal_of(&channel_id)?.unwrap_or(0);
    // One-shot send, as `cmd_send` without `@`: nobody is there to approve.
    let params = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] },
        "can_approve": false,
        "context": { "channel_id": channel_id, "channel_seq": ev.seq },
    });
    Ok(PreparedSend {
        agent,
        channel_id,
        ordinal,
        channel_seq: ev.seq,
        params,
    })
}
