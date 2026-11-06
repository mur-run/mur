//! The channel-conversation gate (at-mention design §3.4, §7).
//!
//! Two steps, in the fixed §7 order, first failure wins:
//!
//! 1. [`pre_lock_checks`] — from the manifest alone, before any lock and
//!    before the log is opened: `UNKNOWN_CONVERSATION` → `CHANNEL_TOO_NEW` →
//!    `CONVERSATION_DAMAGED{manifest}` → `CHANNEL_NOT_CONVERSATION` →
//!    `NOT_A_PARTICIPANT`. A non-member therefore learns nothing about the
//!    log, damaged or not.
//! 2. [`assess`] — ONE shared lock over the log
//!    ([`mur_channel::conversation::assess_locked`]), its [`Verdict`] mapped
//!    to the wire errors.
use std::path::Path;

use mur_channel::ChannelService;
use mur_channel::conversation::{
    Assessment, CacheState, ConversationDamage, Keys, Verdict, assess_locked,
};
use mur_channel::store::ManifestError;
use mur_common::channel::{ChannelActor, ChannelPurpose};

use crate::protocol::a2a_server::HandlerError;
use crate::protocol::methods::message_send::ChannelContext;

/// Everything the gate needs to know about the turn and who is reading.
pub(crate) struct GateInput<'a> {
    pub mur_home: &'a Path,
    /// This runtime's agent (canonical name).
    pub agent: &'a str,
    /// This agent's current channel writer key.
    pub writer_pubkey: [u8; 32],
    pub ctx: &'a ChannelContext,
    /// The turn's user text, compared with the event at `channel_seq`.
    pub turn_input: &'a str,
}

/// The manifest-only checks, in §7 order. Never opens the log.
pub(crate) fn pre_lock_checks(svc: &ChannelService, g: &GateInput<'_>) -> Result<(), HandlerError> {
    let id = &g.ctx.channel_id;
    let dir_exists = mur_channel::check_channel_id(id).is_ok()
        && svc
            .store()
            .events_path(id)
            .is_ok_and(|p| p.parent().is_some_and(Path::is_dir));
    if !dir_exists {
        return Err(HandlerError::UnknownConversation(id.clone()));
    }
    let manifest =
        svc.store()
            .load_manifest(id)
            .map_err(|e| match e.downcast_ref::<ManifestError>() {
                Some(ManifestError::TooNew { .. }) => HandlerError::ChannelTooNew(e.to_string()),
                _ => HandlerError::ConversationDamaged(ConversationDamage::Manifest {
                    detail: format!("{e:#}"),
                }),
            })?;
    if mur_channel::purpose::effective_purpose(&manifest) != ChannelPurpose::Conversation {
        return Err(HandlerError::ChannelNotConversation(id.clone()));
    }
    let member = manifest
        .participants
        .iter()
        .any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == g.agent));
    if !member {
        return Err(HandlerError::NotAParticipant(format!(
            "{} is not in {id}",
            g.agent
        )));
    }
    Ok(())
}

/// The locked read: one shared lock, the verdict mapped to wire errors.
pub(crate) fn assess(
    svc: &ChannelService,
    g: &GateInput<'_>,
    cache: &CacheState,
) -> Result<Assessment, HandlerError> {
    let id = &g.ctx.channel_id;
    let keys = Keys {
        mur_home: g.mur_home,
        agent: g.agent,
        writer_pubkey: g.writer_pubkey,
    };
    let verdict = svc
        .store()
        .with_shared_lock(id, |s| {
            assess_locked(s, id, &keys, cache, g.ctx.channel_seq, g.turn_input)
        })
        .map_err(|e| HandlerError::Internal(format!("read conversation {id}: {e:#}")))?;
    match verdict {
        Verdict::Ok(a) => Ok(a),
        Verdict::Repairing => Err(HandlerError::ConversationRepairing(id.clone())),
        Verdict::Damaged(d) => Err(HandlerError::ConversationDamaged(d)),
    }
}
