//! The sealing hook (channel-log-integrity spec 7.3.1 and section 9, #1770).
//!
//! [`HeadSealer::hook`] is the closure a router-side writer lends to
//! `ChannelStore::append_event_with`. It runs under `events.lock`, after the
//! event line is written, and does the whole flow of 7.3.1: sync the log, chain
//! it, sign a [`Head`], and hand it to [`write_head`].
//!
//! Policy decided here where the spec is silent (flag in review):
//! * A previous head that no longer matches the log (bad signature, wrong
//!   channel, fewer lines than it claims, or a different chain over its own
//!   prefix) is **not** replaced. Sealing over it would bless the damage. The
//!   seal fails (decision B1) and the head is left as evidence.
//! * Unparseable lines before the router's first line leave no `seq` for
//!   `legacy_through` (spec 12 item 13). The first seal fails rather than sign
//!   a value that cannot be corrected later.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use mur_common::channel::ChannelEvent;
use mur_common::identity::AgentIdentity;

use crate::head::{ChainState, Head, HeadFields, chain_log};
use crate::head_io::{read_head, sync_dir, write_head};
use crate::service::SharedSealHook;
use crate::store::{ChannelStore, SealInput};

/// Directory under the MUR home that holds one `<channel_id>.head` per channel
/// (spec 7.4).
pub const HEADS_DIR_NAME: &str = "channel-heads";

/// Seals a channel after each router append. Cheap to clone via [`Arc`].
pub struct HeadSealer {
    mur_home: PathBuf,
    identity: Arc<AgentIdentity>,
    key_version: u32,
}

impl HeadSealer {
    pub fn new(
        mur_home: impl Into<PathBuf>,
        identity: Arc<AgentIdentity>,
        key_version: u32,
    ) -> Self {
        Self {
            mur_home: mur_home.into(),
            identity,
            key_version,
        }
    }

    /// The hook to install with `ChannelService::with_seal`.
    pub fn hook(self) -> SharedSealHook {
        let sealer = Arc::new(self);
        Arc::new(move |input: &SealInput<'_>| sealer.seal(input))
    }

    /// Where this sealer keeps heads.
    pub fn heads_dir(&self) -> PathBuf {
        self.mur_home.join(HEADS_DIR_NAME)
    }

    /// Seal `input`'s channel. Must be called with `events.lock` held, which is
    /// how `append_event_with` calls it.
    pub fn seal(&self, input: &SealInput<'_>) -> Result<()> {
        let id = input.channel_id;
        let store = ChannelStore::new(&self.mur_home);
        let log_path = store.events_path(id);
        let heads_dir = self.heads_dir();
        let prev = read_head(&heads_dir, id)?;

        // Step 1: the log is durable before any head can name its lines.
        // Opened for write (never truncating): Windows `FlushFileBuffers`
        // rejects a read-only handle with "Access is denied".
        OpenOptions::new()
            .write(true)
            .open(&log_path)
            .and_then(|f| f.sync_all())
            .with_context(|| format!("fsync log {}", log_path.display()))?;
        let log = fs::read(&log_path).with_context(|| format!("read {}", log_path.display()))?;

        let lines = split_lines(&log);
        let body = input.record.strip_suffix(b"\n").unwrap_or(input.record);
        let own_idx = lines
            .iter()
            .rposition(|l| *l == body)
            .context("the appended record is not in the log; refusing to seal")?;

        let legacy_through = match &prev {
            Some(p) => {
                self.check_prev(p, id, &log)?;
                // A dedup retry: the verified head already chains this line,
                // so there is nothing to add and it must not move. Judged by
                // line position, not `seq`, which a truncated log can reuse.
                if p.lines > own_idx as u64 {
                    return Ok(());
                }
                p.legacy_through
            }
            None => first_legacy_through(&lines[..own_idx])?,
        };

        let chain = chain_log(&log);
        let seq = lines
            .iter()
            .rev()
            .find_map(|l| serde_json::from_slice::<ChannelEvent>(l).ok())
            .map(|e| e.seq)
            .context("no parseable event in the log to anchor a head on")?;

        // First seal of a channel: its directory entries may be new too.
        if prev.is_none()
            && let Some(dir) = log_path.parent()
        {
            sync_dir(dir).context("fsync channel dir")?;
            if let Some(root) = dir.parent() {
                sync_dir(root).context("fsync channels dir")?;
            }
        }

        let head = Head::seal(
            &self.identity,
            HeadFields {
                channel_id: id.to_string(),
                seq,
                lines: chain.lines,
                chain_hash: hex::encode(chain.hash),
                key_version: Some(self.key_version),
                legacy_through,
            },
            Utc::now(),
        );
        write_head(&heads_dir, &head)
    }

    /// The previous head must still describe the log it was sealed over.
    fn check_prev(&self, prev: &Head, id: &str, log: &[u8]) -> Result<()> {
        if prev.channel_id != id {
            bail!(
                "existing head names channel {:?}, expected {id:?}",
                prev.channel_id
            );
        }
        if !prev.verify(&self.identity.verifying_key_bytes()) {
            bail!("existing head fails signature verification; refusing to overwrite it");
        }
        let prefix = chain_prefix(log, prev.lines).with_context(|| {
            format!(
                "log has fewer than the {} lines the head covers",
                prev.lines
            )
        })?;
        if hex::encode(prefix.hash) != prev.chain_hash {
            bail!("log no longer matches the existing head's chain; refusing to seal over it");
        }
        Ok(())
    }
}

/// `\n`-terminated segments of `log`, without the terminator. An unterminated
/// tail is not a line (spec section 9).
fn split_lines(log: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = log;
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        out.push(&rest[..nl]);
        rest = &rest[nl + 1..];
    }
    out
}

/// Chain the first `n` lines; `None` if the log has fewer.
fn chain_prefix(log: &[u8], n: u64) -> Option<ChainState> {
    let mut state = ChainState::genesis();
    for line in split_lines(log).into_iter().take(n as usize) {
        state.extend(line);
    }
    (state.lines == n).then_some(state)
}

/// Spec section 9: with no head yet, `legacy_through` is the last `seq` before
/// the router's own line (`before` is everything ahead of it), or `None` when
/// that line is the first.
fn first_legacy_through(before: &[&[u8]]) -> Result<Option<u64>> {
    if before.is_empty() {
        return Ok(None);
    }
    before
        .iter()
        .rev()
        .find_map(|l| serde_json::from_slice::<ChannelEvent>(l).ok())
        .map(|e| Some(e.seq))
        .context(
            "first seal: no parseable event before the router's line, so legacy_through has \
             no value (spec 12 item 13)",
        )
}
