//! Anchored appends (#1770 part 2): the trusted writer advances the signed
//! [`Head`] of #1868 in `<mur_home>/channel-heads/` through the store's
//! [`SealHooks`], so the whole protocol runs under the channel lock:
//!
//! `lock` → `pre_write: mark_dirty` → write line → `post_write: fsync log,
//! seal + write_head` → `clear_dirty` → `unlock`.
//!
//! Dedup (aligned with #1868): a hit skips `pre_write` (no new line, so no
//! new marker) but still runs `post_write`, which is a no-op when the head
//! already covers the log and otherwise repairs a head left behind by a crash
//! and clears the stale marker. Skipping it would leave that marker forever.
//!
//! The head is sealed over the WHOLE log, not just the line handed in, so it
//! also covers lines sandboxed agents appended since the last head. It only
//! moves forward along the same history: a log that no longer extends the
//! current head is refused, never re-sealed.

use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

use super::ChannelService;
use crate::anchor::{self, AnchorLagError};
use crate::head::{Head, HeadFields, chain_log, chain_prefix};
use crate::head_io;
use crate::store::{PreSealInput, SealHook, SealHooks, SealInput};

/// Who signs the head of an anchored append: the writer's identity and its
/// key version, the same pair that signs the event.
#[derive(Clone, Copy)]
pub(super) struct HeadSigner<'a> {
    pub identity: &'a AgentIdentity,
    pub kv: u32,
}

/// Pre-write half: the dirty marker must be on disk before the line is, so a
/// crash between the write and the head leaves evidence. Failing here aborts
/// the append with nothing written. Not run on a dedup hit: no new line.
fn mark(mur_home: &Path, input: &PreSealInput<'_>) -> Result<()> {
    anchor::mark_dirty(mur_home, input.channel_id)
        .with_context(|| format!("mark channel head dirty for {}", input.channel_id))
}

/// `seq` of the last parseable event in `log`, and of the last one that
/// predates `record` when `record` is the log's final line (a fresh write);
/// when it is not (a dedup hit on an older line), every line predates the
/// first head, so both are the last `seq`.
fn seqs(log: &[u8], record: &[u8]) -> (Option<u64>, Option<u64>) {
    let fresh = log.ends_with(record);
    let body = if fresh {
        &log[..log.len() - record.len()]
    } else {
        log
    };
    let last_seq = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<ChannelEvent>(l).ok())
            .map(|e| e.seq)
    };
    (last_seq(log), last_seq(body))
}

/// The head the log should have now, or `None` when the current head already
/// covers the whole log. Refuses (never overwrites) a head the log no longer
/// extends — truncated below it, or rewritten inside it — because sealing
/// over that would launder the damage. This is the monotonic guard: a head
/// only ever moves forward along the same history.
fn next_head(
    current: Option<&Head>,
    log: &[u8],
    input: &SealInput<'_>,
    signer: HeadSigner<'_>,
) -> Result<Option<Head>> {
    let state = chain_log(log);
    let (last_seq, before) = seqs(log, input.record);
    let legacy_through = match current {
        Some(h) => {
            let prefix = chain_prefix(log, h.lines)
                .with_context(|| format!("log has fewer than the head's {} lines", h.lines))?;
            if hex::encode(prefix.hash) != h.chain_hash {
                bail!("log no longer extends the signed head at line {}", h.lines);
            }
            if h.lines == state.lines {
                return Ok(None);
            }
            h.legacy_through
        }
        // First seal: every event written before this line predates any head.
        None => before,
    };
    let fields = HeadFields {
        channel_id: input.channel_id.to_owned(),
        seq: last_seq.unwrap_or(input.event.seq),
        lines: state.lines,
        chain_hash: hex::encode(state.hash),
        key_version: Some(signer.kv),
        legacy_through,
    };
    Ok(Some(Head::seal(signer.identity, fields, Utc::now())))
}

/// Post-write half (also run on a dedup hit): fsync the log (spec 7.3.1
/// step 1), seal the head over the whole log, then clear the marker. Every
/// failure here comes after the line is durable, so it is an
/// [`AnchorLagError`] — never a plain error a caller could read as "nothing
/// was written" — and the marker stays. A failed `clear_dirty` is only a
/// warning: the head is correct, the marker over-reports until the next
/// anchored append clears it.
fn advance(
    mur_home: &Path,
    log_path: &Path,
    signer: HeadSigner<'_>,
    input: &SealInput<'_>,
) -> Result<()> {
    let id = input.channel_id;
    let dir = anchor::heads_dir(mur_home);
    let sealed = (|| -> Result<()> {
        // Windows' FlushFileBuffers needs a handle with write access; a
        // read-only `File::open` fails there with "Access is denied". Opening
        // for write neither creates nor truncates, so this is safe everywhere.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(log_path)
            .context("open log for fsync")?;
        f.sync_all().context("fsync log before the head")?;
        let log = std::fs::read(log_path).context("read log to chain it")?;
        let current = head_io::read_head(&dir, id)?;
        if let Some(head) = next_head(current.as_ref(), &log, input, signer)? {
            head_io::write_head(&dir, &head)?;
        }
        Ok(())
    })();
    if let Err(source) = sealed {
        tracing::error!(channel = id, seq = input.event.seq, error = %format!("{source:#}"),
            "signed channel head not advanced; channel stays dirty");
        return Err(AnchorLagError {
            event: input.event.clone(),
            source,
        }
        .into());
    }
    if let Err(e) = anchor::clear_dirty(mur_home, id) {
        tracing::warn!(channel = id, error = %e, "could not clear dirty marker");
    }
    Ok(())
}

impl ChannelService {
    /// Lend `f` the hooks for one write: the service's own post-write seal
    /// (if any), plus — when `signer` is set — the signed-head protocol. The
    /// head runs first so an installed seal sees an advanced head.
    pub(super) fn with_hooks<T>(
        &self,
        signer: Option<HeadSigner<'_>>,
        f: impl FnOnce(SealHooks<'_>) -> Result<T>,
    ) -> Result<T> {
        let home = self.store.mur_home();
        let seal = self.seal.as_deref();
        let Some(signer) = signer else {
            return f(SealHooks::post(seal.map(|h| h as &SealHook<'_>)));
        };
        let pre = |i: &PreSealInput<'_>| mark(home, i);
        let post = |i: &SealInput<'_>| {
            advance(home, &self.store.events_path(i.channel_id), signer, i)?;
            seal.map_or(Ok(()), |s| s(i))
        };
        f(SealHooks {
            pre_write: Some(&pre),
            post_write: Some(&post as &SealHook<'_>),
        })
    }

    /// The sig-input v2 write behind `append_signed` / `append_and_anchor`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn signed_v2(
        &self,
        channel_id: &str,
        identity: &AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        anchored: bool,
    ) -> Result<ChannelEvent> {
        let signer = anchored.then_some(HeadSigner { identity, kv });
        let ev = self.with_hooks(signer, |hooks| {
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
        identity: &AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        anchored: bool,
    ) -> Result<ChannelEvent> {
        let signer = anchored.then_some(HeadSigner { identity, kv });
        let sig = crate::sign::sign_event(
            identity,
            channel_id,
            &actor,
            kind,
            &payload,
            idempotency_key.as_deref(),
        );
        let ev = self.with_hooks(signer, |hooks| {
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
    /// signed channel head, under the same lock as the write.
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
        identity: &AgentIdentity,
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
        identity: &AgentIdentity,
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
