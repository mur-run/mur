//! Hash chain over a channel's `events.jsonl` (#1770).
//!
//! A v2-signed event carries `prev_hash`: the SHA-256 (lowercase hex) of the
//! raw bytes of the line immediately before it, without the trailing newline,
//! or [`GENESIS_PREV_HASH`] when it is the first line. The signature covers
//! `prev_hash`, so deleting, reordering, or editing an earlier line breaks the
//! chain at the next v2 event — and the stored value says exactly which link
//! broke, instead of every failure collapsing into "signature invalid".
//!
//! Lines without `prev_hash` (v1 commander commands signed before they reach
//! the store, unsigned agent lines) are covered only once a later v2 line
//! commits to them.

use anyhow::Result;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

use super::ChannelStore;

/// `prev_hash` of the first line of a log. The anchor of every chain: changing
/// it breaks verification of every existing log, so it is pinned by a test.
pub const GENESIS_PREV_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Chain hash of one raw log line (no trailing newline).
pub fn line_hash(line: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(line.as_bytes()))
}

/// The chain-relevant lines of a raw log: every non-blank line, in order.
/// Blank lines are skipped by every reader, so they are not chain links either.
fn chain_lines(content: &str) -> impl Iterator<Item = &str> {
    content.lines().filter(|l| !l.trim().is_empty())
}

/// What the next appended event's `prev_hash` must be for this log content.
pub(super) fn tail_hash(content: &str) -> String {
    chain_lines(content)
        .last()
        .map(line_hash)
        .unwrap_or_else(|| GENESIS_PREV_HASH.to_owned())
}

/// `seq` of every event whose `prev_hash` does not match its predecessor line,
/// in file order. The predecessor is the raw previous line whatever it is —
/// v1, unsigned, or unparseable — which is how v2 lines cover the others.
pub(super) fn chain_breaks(content: &str) -> Vec<u64> {
    let mut breaks = Vec::new();
    let mut expected = GENESIS_PREV_HASH.to_owned();
    for line in chain_lines(content) {
        if let Ok(ev) = serde_json::from_str::<ChannelEvent>(line)
            && let Some(prev) = ev.prev_hash.as_deref()
            && prev != expected
        {
            breaks.push(ev.seq);
        }
        expected = line_hash(line);
    }
    breaks
}

/// The event as it will be written, minus the seal — handed to the sealer
/// under the append lock.
pub(crate) struct Unsealed<'a> {
    pub channel_id: &'a str,
    pub actor: &'a ChannelActor,
    pub kind: EventKind,
    pub payload: &'a serde_json::Value,
    pub idempotency_key: Option<&'a str>,
    /// Chain hash of the current tail line, or genesis for an empty log.
    pub tail_hash: &'a str,
}

/// The signing fields the sealer decides.
pub(crate) struct Seal {
    pub sig: Option<String>,
    pub key_version: Option<u32>,
    pub prev_hash: Option<String>,
}

impl ChannelStore {
    /// Sign (sig-input v2, covering `prev_hash`) and append one event. The
    /// signature is made under the append lock, against the real tail, so
    /// no concurrent writer can land between reading the tail and writing.
    #[allow(clippy::too_many_arguments)]
    pub fn append_signed_event(
        &self,
        id: &str,
        identity: &AgentIdentity,
        key_version: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        self.append_signed_event_with(
            id,
            identity,
            key_version,
            actor,
            kind,
            payload,
            idempotency_key,
            None,
        )
    }

    /// [`append_signed_event`](Self::append_signed_event) plus the post-write
    /// seal hook, so the signer and the head share one lock acquisition.
    #[allow(clippy::too_many_arguments)]
    pub fn append_signed_event_with(
        &self,
        id: &str,
        identity: &AgentIdentity,
        key_version: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        seal: Option<&super::SealHook<'_>>,
    ) -> Result<ChannelEvent> {
        self.append_sealed(
            id,
            actor,
            kind,
            payload,
            idempotency_key,
            |u| {
                let sig = crate::sign::sign_event_v2(
                    identity,
                    u.channel_id,
                    u.actor,
                    u.kind,
                    u.payload,
                    u.idempotency_key,
                    u.tail_hash,
                );
                Seal {
                    sig: Some(sig),
                    key_version: Some(key_version),
                    prev_hash: Some(u.tail_hash.to_owned()),
                }
            },
            seal,
        )
    }
}

#[cfg(test)]
mod tests;
