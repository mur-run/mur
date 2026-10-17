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
use crate::head::{Head, HeadFields, chain_log, chain_log_with_prefix};
use crate::head_io;
use crate::store::{PreSealInput, SealHook, SealHooks, SealInput};

/// Who signs the head of an anchored append: the writer's identity and its
/// key version, the same pair that signs the event.
#[derive(Clone, Copy)]
pub(super) struct HeadSigner<'a> {
    pub identity: &'a AgentIdentity,
    pub kv: u32,
    /// The signing agent's home (`rotations.jsonl` lives there). Heads sealed
    /// under an earlier key version are verified against the rotation chain
    /// found here; without it a normal rekey would brick every later append.
    pub key_home: &'a Path,
}

impl HeadSigner<'_> {
    /// The pubkey that should have signed a head claiming `key_version`.
    /// The signer's own key when the version is its own (or unstated);
    /// otherwise whatever the trusted rotation history says. `None` means
    /// the head cannot be attributed to any key we trust.
    fn pubkey_for(&self, key_version: Option<u32>) -> Option<[u8; 32]> {
        match key_version {
            None => Some(self.identity.verifying_key_bytes()),
            Some(kv) if kv == self.kv => Some(self.identity.verifying_key_bytes()),
            Some(kv) => crate::sign::resolve_writer_pubkey_strict(self.key_home, kv),
        }
    }
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
    // Walk back from the end and decode one line at a time, so the cost is
    // the unparseable tail plus one event, not the whole log.
    let last_seq = |bytes: &[u8]| {
        bytes
            .rsplit(|&b| b == b'\n')
            .find_map(|l| serde_json::from_str::<ChannelEvent>(&String::from_utf8_lossy(l)).ok())
            .map(|e| e.seq)
    };
    (last_seq(log), last_seq(body))
}

/// The part of `log` ahead of `record` when `record` is its final line, else
/// the whole log (a dedup hit on an older line).
fn body_before<'a>(log: &'a [u8], record: &[u8]) -> &'a [u8] {
    if log.ends_with(record) {
        &log[..log.len() - record.len()]
    } else {
        log
    }
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
    // One scan serves both the new head and the check of the old one.
    let (state, prefix) = chain_log_with_prefix(log, current.map_or(0, |h| h.lines));
    let (last_seq, before) = seqs(log, input.record);
    // The new line must stand on its own. If it was appended onto an
    // unterminated tail it is glued to that garbage (and unparseable itself);
    // `chain_log` ignores unterminated bytes, so nothing below would notice.
    let body = body_before(log, input.record);
    if log.ends_with(input.record) && !body.is_empty() && !body.ends_with(b"\n") {
        bail!("the log ends in an unterminated line; the new event was glued onto it");
    }
    let legacy_through = match current {
        Some(h) => {
            // The head is only a trust anchor if it is the one we signed for
            // THIS channel. Chaining the log is not enough: anyone who can
            // write `channel-heads/` can chain a log, and re-sealing over
            // their file would launder it into a legitimate-looking head.
            if h.channel_id != input.channel_id {
                bail!(
                    "existing head names channel {:?}, expected {:?}",
                    h.channel_id,
                    input.channel_id
                );
            }
            let Some(key) = signer.pubkey_for(h.key_version) else {
                bail!(
                    "existing head claims key_version {:?}, which the rotation history \
                     does not resolve; refusing to overwrite it",
                    h.key_version
                );
            };
            if !h.verify(&key) {
                bail!("existing head fails signature verification; refusing to overwrite it");
            }
            let prefix = prefix
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
        None => {
            // Lines exist but none parse, so `legacy_through` would be `None`
            // ("head since the first line") while unverifiable history sits
            // behind it. Refuse rather than sign that claim.
            if before.is_none() && chain_log(body).lines > 0 {
                bail!(
                    "first seal: the log has lines before this one but no parseable event, \
                     so legacy_through has no value"
                );
            }
            before
        }
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
            advance(home, &self.store.events_path(i.channel_id)?, signer, i)?;
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
        anchored: Option<&Path>,
    ) -> Result<ChannelEvent> {
        let signer = anchored.map(|key_home| HeadSigner {
            identity,
            kv,
            key_home,
        });
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
        anchored: Option<&Path>,
    ) -> Result<ChannelEvent> {
        let signer = anchored.map(|key_home| HeadSigner {
            identity,
            kv,
            key_home,
        });
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
    /// `key_home` is the signing agent's home directory (where
    /// `rotations.jsonl` lives): a head sealed under an earlier key version
    /// is verified through that rotation history, so a normal rekey does not
    /// strand the channel.
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
        key_home: &Path,
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
            Some(key_home),
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
        key_home: &Path,
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
            Some(key_home),
        )
    }
}

#[cfg(test)]
mod seqs_tests {
    use super::*;

    fn line(seq: u64) -> Vec<u8> {
        let ev = ChannelEvent {
            seq,
            ts: Utc::now(),
            actor: ChannelActor::System,
            kind: EventKind::Message,
            payload: serde_json::json!({ "n": seq }),
            idempotency_key: None,
            sig: None,
            key_version: None,
            prev_hash: None,
        };
        let mut v = serde_json::to_vec(&ev).unwrap();
        v.push(b'\n');
        v
    }

    /// The whole-log scan `seqs` replaced: kept here as the oracle.
    fn last_seq_full_scan(bytes: &[u8]) -> Option<u64> {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<ChannelEvent>(l).ok())
            .map(|e| e.seq)
    }

    /// Several unparseable lines at the tail (garbage, blank, CRLF, non-UTF-8,
    /// unterminated) must be skipped back to the last real event, exactly as
    /// the old whole-log scan did.
    #[test]
    fn tail_walk_matches_full_scan_over_a_garbage_tail() {
        let tails: [&[u8]; 6] = [
            b"",
            b"not json\n",
            b"not json\n\n{\"half\":\n",
            b"\xff\xfe\n{broken\r\n",
            b"\n\n\n",
            b"garbage-without-newline",
        ];
        for tail in tails {
            let mut log = Vec::new();
            for s in 1..=3 {
                log.extend(line(s));
            }
            log.extend_from_slice(tail);
            let record = line(9);
            for (log, record) in [(log.clone(), record.as_slice()), {
                let mut fresh = log.clone();
                fresh.extend(&record);
                (fresh, record.as_slice())
            }] {
                let body = body_before(&log, record);
                assert_eq!(
                    seqs(&log, record),
                    (last_seq_full_scan(&log), last_seq_full_scan(body)),
                    "tail {:?}",
                    String::from_utf8_lossy(tail)
                );
            }
        }
        // Nothing parseable at all.
        assert_eq!(seqs(b"x\ny\n", b"z\n"), (None, None));
    }
}
