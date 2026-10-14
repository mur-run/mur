//! Anchored appends (#1770): the trusted writer advances the chain-head
//! anchor in `<mur_home>/channel-heads/` through the store's [`SealHooks`],
//! so the whole protocol runs under the channel lock:
//!
//! `lock` → `pre_write: mark_dirty` → write line → `post_write: write_anchor`
//! → `clear_dirty` → `unlock`.
//!
//! Under the lock, two anchored appends cannot interleave, so an older tail
//! can never overwrite a newer anchor. The one remaining regression source
//! is a dedup hit, which re-runs `post_write` on an EARLIER line; the post
//! hook is therefore monotonic (it never moves the anchor backwards).

use std::path::Path;

use anyhow::{Context, Result};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

use super::ChannelService;
use crate::anchor::{self, AnchorLagError, ChainTail};
use crate::store::{PreSealInput, SealHook, SealHooks, SealInput, line_hash};

/// Pre-write half: the dirty marker must be on disk before the line is, so a
/// crash between the write and the anchor leaves evidence. Failing here
/// aborts the append with nothing written.
fn mark(mur_home: &Path, input: &PreSealInput<'_>) -> Result<()> {
    anchor::mark_dirty(mur_home, input.channel_id)
        .with_context(|| format!("mark chain-head dirty for {}", input.channel_id))
}

/// Post-write half: advance the anchor to the line just written, then clear
/// the marker. A failed anchor write is an [`AnchorLagError`] (the line is
/// durable; the marker stays). A failed `clear_dirty` is only a warning: the
/// anchor is correct, the lingering marker over-reports until the next
/// anchored append clears it.
fn advance(mur_home: &Path, input: &SealInput<'_>) -> Result<()> {
    let id = input.channel_id;
    let body = std::str::from_utf8(input.record)
        .context("event record is not UTF-8")?
        .strip_suffix('\n')
        .context("event record has no LF terminator")?;
    let tail = ChainTail {
        seq: input.event.seq,
        hash: line_hash(body),
    };
    // Monotonic: a dedup hit hands us an earlier line, and a log truncated
    // below the anchor hands us a lower seq. Never move backwards — that
    // would launder the truncation — and leave any marker in place. A corrupt
    // anchor is an error, not "absent": overwriting it would erase evidence.
    // Everything below runs after the line is durable, so EVERY failure —
    // reading the current anchor included — is an `AnchorLagError`, never a
    // plain error a caller could mistake for "nothing was written".
    let advanced = anchor::read_anchor(mur_home, id).and_then(|current| {
        if current.as_ref().is_some_and(|a| a.seq >= tail.seq) {
            return Ok(false);
        }
        anchor::write_anchor(mur_home, id, &tail).map(|()| true)
    });
    match advanced {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(source) => {
            tracing::error!(channel = id, seq = tail.seq, error = %source,
                "chain-head anchor not advanced; channel stays dirty");
            return Err(AnchorLagError {
                event: input.event.clone(),
                tail,
                source,
            }
            .into());
        }
    }
    if let Err(e) = anchor::clear_dirty(mur_home, id) {
        tracing::warn!(channel = id, error = %e, "could not clear dirty marker");
    }
    Ok(())
}

impl ChannelService {
    /// Lend `f` the hooks for one write: the service's own post-write seal
    /// (if any), plus — when `anchored` — the chain-head anchor protocol. The
    /// anchor runs first so an installed seal sees an anchored head.
    pub(super) fn with_hooks<T>(
        &self,
        anchored: bool,
        f: impl FnOnce(SealHooks<'_>) -> Result<T>,
    ) -> Result<T> {
        let home = self.store.mur_home();
        let seal = self.seal.as_deref();
        let pre = |i: &PreSealInput<'_>| mark(home, i);
        let post = |i: &SealInput<'_>| {
            advance(home, i)?;
            seal.map_or(Ok(()), |s| s(i))
        };
        if anchored {
            f(SealHooks {
                pre_write: Some(&pre),
                post_write: Some(&post as &SealHook<'_>),
            })
        } else {
            f(SealHooks::post(seal.map(|h| h as &SealHook<'_>)))
        }
    }

    /// The sig-input v2 write behind `append_signed` / `append_and_anchor`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn signed_v2(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        anchored: bool,
    ) -> Result<ChannelEvent> {
        let ev = self.with_hooks(anchored, |hooks| {
            self.store.append_signed_event_with(
                channel_id,
                identity,
                kv,
                actor,
                kind,
                payload,
                idempotency_key,
                hooks,
            )
        })?;
        self.after_write(channel_id, &ev, None);
        Ok(ev)
    }

    /// The sig-input v1 write behind `append_signed_v1` /
    /// `append_and_anchor_v1` (see `append_signed_v1` for why v1 exists).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn signed_v1(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        anchored: bool,
    ) -> Result<ChannelEvent> {
        let sig = crate::sign::sign_event(
            identity,
            channel_id,
            &actor,
            kind,
            &payload,
            idempotency_key.as_deref(),
        );
        let ev = self.with_hooks(anchored, |hooks| {
            self.store.append_event_with(
                channel_id,
                actor,
                kind,
                payload,
                idempotency_key,
                Some(sig),
                Some(kv),
                hooks,
            )
        })?;
        self.after_write(channel_id, &ev, None);
        Ok(ev)
    }

    /// [`append_signed`](Self::append_signed) that also advances the
    /// chain-head anchor, under the same lock as the write.
    ///
    /// **Only call from outside the agent sandbox** (mur-core, Hub). A
    /// sandboxed agent's fs grant covers `channels/` but deliberately not
    /// `channel-heads/`, so there the pre-write `mark_dirty` fails and the
    /// event is NOT written — use [`append_signed`](Self::append_signed).
    ///
    /// An `Err` that downcasts to [`AnchorLagError`] means the line IS on
    /// disk and the anchor lags (the dirty marker stays); do not retry
    /// blindly — the same idempotency key retries the anchor without a
    /// duplicate line.
    #[allow(clippy::too_many_arguments)]
    pub fn append_and_anchor(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        self.signed_v2(
            channel_id,
            identity,
            kv,
            actor,
            kind,
            payload,
            idempotency_key,
            true,
        )
    }

    /// v1 twin of [`append_and_anchor`](Self::append_and_anchor) for
    /// commander directives; same sandbox caveat and protocol.
    #[allow(clippy::too_many_arguments)]
    pub fn append_and_anchor_v1(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        self.signed_v1(
            channel_id,
            identity,
            kv,
            actor,
            kind,
            payload,
            idempotency_key,
            true,
        )
    }
}
