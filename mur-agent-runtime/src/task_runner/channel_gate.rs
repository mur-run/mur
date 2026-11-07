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

use super::TaskRunner;
use super::channel_conversation::{AUDIT, Busy};
use crate::llm::RichMessage;
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

/// The `TaskError.code` a refused channel turn carries: the §7 name, so a
/// client can branch on it without parsing the message.
pub(crate) fn wire_code(e: &HandlerError) -> &'static str {
    match e {
        HandlerError::UnknownConversation(_) => "UNKNOWN_CONVERSATION",
        HandlerError::ChannelTooNew(_) => "CHANNEL_TOO_NEW",
        HandlerError::ChannelNotConversation(_) => "CHANNEL_NOT_CONVERSATION",
        HandlerError::NotAParticipant(_) => "NOT_A_PARTICIPANT",
        HandlerError::ConversationRepairing(_) => "CONVERSATION_REPAIRING",
        HandlerError::ConversationDamaged(_) => "CONVERSATION_DAMAGED",
        HandlerError::ConversationBusy(_) => "CONVERSATION_BUSY",
        _ => "conversation_error",
    }
}

/// A channel turn admitted by the gate: its conversation's lock, held until
/// the turn ends, and the reconciled history to send.
pub(crate) struct ChannelTurn {
    pub(crate) permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) prior: Vec<RichMessage>,
}

/// Channel reads are file and SQLite I/O: off the async worker.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HandlerError> + Send + 'static,
) -> Result<T, HandlerError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| HandlerError::Internal(format!("channel gate: {e}")))?
}

fn open(home: &Path) -> Result<ChannelService, HandlerError> {
    ChannelService::open(home).map_err(|e| HandlerError::Internal(format!("open channels: {e:#}")))
}

impl TaskRunner {
    /// Admit a channel turn (§3.4, §7 order): manifest checks, then the
    /// per-conversation lock, then the locked read and reconciliation.
    /// `Ok(None)`: this runner does not serve channel conversations, so the
    /// turn keeps today's task-id keying.
    pub(crate) async fn begin_channel_turn(
        &self,
        ctx: &ChannelContext,
        input: &str,
    ) -> Result<Option<ChannelTurn>, HandlerError> {
        let Some(cc) = &self.channel_conversations else {
            return Ok(None);
        };
        let (home, agent, c, text) = (
            cc.mur_home.clone(),
            self.agent_name.clone(),
            ctx.clone(),
            input.to_string(),
        );
        blocking(move || {
            let g = GateInput {
                mur_home: &home,
                agent: &agent,
                // Not read before the lock: the manifest checks need no key.
                writer_pubkey: [0; 32],
                ctx: &c,
                turn_input: &text,
            };
            pre_lock_checks(&open(&home)?, &g)
        })
        .await?;
        let permit = match cc.locks.acquire(&ctx.channel_id).await {
            Ok(p) => p,
            Err(Busy) => {
                tracing::info!(
                    target: AUDIT,
                    channel_id = %ctx.channel_id,
                    channel_seq = ctx.channel_seq,
                    "CONVERSATION_BUSY"
                );
                return Err(HandlerError::ConversationBusy(ctx.channel_id.clone()));
            }
        };
        let prior = self.channel_prior(ctx, input).await?;
        Ok(Some(ChannelTurn { permit, prior }))
    }

    /// The history a channel turn sends: the log assessed under one shared
    /// lock, folded into the cache, reconciled with the provisional tail.
    /// Call with the conversation's lock held.
    pub(crate) async fn channel_prior(
        &self,
        ctx: &ChannelContext,
        input: &str,
    ) -> Result<Vec<RichMessage>, HandlerError> {
        let Some(cc) = &self.channel_conversations else {
            return Ok(Vec::new());
        };
        let writer_pubkey = cc.writer_pubkey(&self.agent_name).ok_or_else(|| {
            HandlerError::Internal(format!(
                "{} has no channel writer key; cannot verify the conversation",
                self.agent_name
            ))
        })?;
        let cache = cc
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cache_state(&ctx.channel_id);
        let (home, agent, c, text) = (
            cc.mur_home.clone(),
            self.agent_name.clone(),
            ctx.clone(),
            input.to_string(),
        );
        let a = blocking(move || {
            let g = GateInput {
                mur_home: &home,
                agent: &agent,
                writer_pubkey,
                ctx: &c,
                turn_input: &text,
            };
            assess(&open(&home)?, &g, &cache)
        })
        .await?;
        if a.anchor_dirty {
            tracing::info!(
                target: AUDIT,
                channel_id = %ctx.channel_id,
                "anchor_dirty tolerated"
            );
        }
        Ok(cc
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reconcile(&ctx.channel_id, &a, ctx.channel_seq))
    }
}
