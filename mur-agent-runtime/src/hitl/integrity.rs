//! Tell "a record was written and then deleted" from "never written" for gate
//! B's decision channel (#1859).
//!
//! After every append the runtime seals a [`Head`] — line count plus the
//! cumulative chain hash of the whole log (`mur_channel::head`) — signed with
//! the agent's own key and kept in `<agent_home>/hitl-heads/`. A reader
//! recomputes the chain from `events.jsonl` and compares. Deleting or
//! truncating a line, whether a revocation Note, a newer deny or the tail,
//! changes the chain and the check fails; the caller then stops using
//! remembered approvals and asks again. It never falls back to the older
//! allow that happened to survive.
//!
//! The check is "the sealed prefix is still there byte for byte", not "the log
//! equals the last seal". Lines after the sealed prefix are not a deletion:
//! a crash between append and seal leaves one, and a foreign line is already
//! ignored by the fold because it does not verify against the agent key.
//!
//! What this does not cover, deliberately: restoring `events.jsonl` AND its
//! head together from an earlier copy is an older, internally consistent
//! state. Only an anchor the agent cannot write could tell it apart, and
//! forging a fresh head or a fresh allow needs the agent key, which is the
//! compromised-agent case #1859 puts out of scope.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use mur_channel::ChannelStore;
use mur_channel::head::{ChainState, Head, HeadFields, chain_log};
use mur_channel::head_io::{read_head, write_head};
use mur_channel::service::SharedSealHook;
use mur_common::agent::HITL_HEAD_DIR;
use mur_common::identity::AgentIdentity;

/// The log's integrity could not be established. Carries the reason so
/// `hitl/list` can say why the memory is unavailable instead of showing it empty.
///
/// Two kinds, because they need different handling at the reporting edge
/// (#2033): a *verdict* ("the log is not what the seal says") is something
/// `mur channel repair` can act on, while an *I/O* failure (the log or head
/// could not even be read) is an internal error the operator must see as one.
/// Both fail closed for the gate.
#[derive(Debug)]
pub struct IntegrityError {
    pub reason: String,
    io: bool,
}

impl IntegrityError {
    fn verdict(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            io: false,
        }
    }

    fn io(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            io: true,
        }
    }

    /// True when the check could not run (unreadable file), as opposed to
    /// running and finding the log untrustworthy.
    pub fn is_io(&self) -> bool {
        self.io
    }
}

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "remembered approvals are unavailable: decision log integrity check failed ({})",
            self.reason
        )
    }
}

impl std::error::Error for IntegrityError {}

/// Where this agent's heads live.
pub fn heads_dir(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home.join("agents").join(agent).join(HITL_HEAD_DIR)
}

fn log_path(mur_home: &Path, channel_id: &str) -> anyhow::Result<PathBuf> {
    ChannelStore::new(mur_home).events_path(channel_id)
}

/// Shared lock on the channel's append lock, so a reader never sees a line
/// that is written but not yet sealed. `None` when nothing was ever appended.
pub fn read_lock(mur_home: &Path, channel_id: &str) -> Option<std::fs::File> {
    let lock = log_path(mur_home, channel_id)
        .ok()?
        .with_file_name("events.lock");
    let f = OpenOptions::new().read(true).open(lock).ok()?;
    f.lock_shared().ok()?;
    Some(f)
}

fn read_log(path: &Path) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Chain of the first `lines` terminated lines of `log`, or `None` if the log
/// has fewer. An unterminated tail is never chained (spec 7.2), so a log cut
/// mid-line has fewer than it did.
fn prefix_chain(log: &[u8], lines: u64) -> Option<ChainState> {
    let mut state = ChainState::genesis();
    let mut rest = log;
    while state.lines < lines {
        let nl = rest.iter().position(|&b| b == b'\n')?;
        state.extend(&rest[..nl]);
        rest = &rest[nl + 1..];
    }
    Some(state)
}

fn covers(log: &[u8], head: &Head) -> bool {
    prefix_chain(log, head.lines).is_some_and(|c| hex::encode(c.hash) == head.chain_hash)
}

/// `Ok` only when the log still contains everything the last seal covered. A log with
/// no head is acceptable only if it has no lines (never written).
pub fn verify(
    mur_home: &Path,
    agent: &str,
    channel_id: &str,
    pubkey: &[u8; 32],
) -> Result<(), IntegrityError> {
    let bad = |why: String| IntegrityError::verdict(why);
    // An unfinished `mur channel repair` may have left an empty log and no
    // head, which would otherwise pass below as a never-written channel.
    if mur_channel::repair_marker::is_pending(mur_home, channel_id) {
        return Err(bad("a channel repair began and did not finish".into()));
    }
    let log = log_path(mur_home, channel_id)
        .and_then(|p| read_log(&p))
        .map_err(|e| IntegrityError::io(format!("{e:#}")))?;
    let head = read_head(&heads_dir(mur_home, agent), channel_id).map_err(|e| {
        let why = format!("head unreadable: {e:#}");
        // Only a head that read fine but does not parse is a damaged seal that
        // repair can act on; anything else (unreadable path, bad channel id)
        // is an internal failure and must stay one.
        if e.root_cause().is::<serde_json::Error>() {
            IntegrityError::verdict(why)
        } else {
            IntegrityError::io(why)
        }
    })?;
    let Some(head) = head else {
        return if log.is_empty() {
            Ok(())
        } else {
            Err(bad("log has records but no signed head".into()))
        };
    };
    if head.channel_id != channel_id {
        return Err(bad("head belongs to another channel".into()));
    }
    if !head.verify(pubkey) {
        return Err(bad("head signature does not verify".into()));
    }
    if !covers(&log, &head) {
        return Err(bad(format!(
            "the last seal covered {} lines and the log no longer contains them unchanged; a record was removed or altered",
            head.lines
        )));
    }
    Ok(())
}

/// The hook run under the channel's append lock after each line is written:
/// check the log before this line still matches the previous head, then seal
/// a new head over the whole log. On a mismatch it seals nothing, so the
/// failure stays visible instead of being papered over.
pub fn seal_hook(
    mur_home: PathBuf,
    agent: String,
    identity: Arc<AgentIdentity>,
    key_version: u32,
) -> SharedSealHook {
    Arc::new(move |input| {
        let dir = heads_dir(&mur_home, &agent);
        let bytes = read_log(&log_path(&mur_home, input.channel_id)?)?;
        let full = chain_log(&bytes);
        let prior = read_head(&dir, input.channel_id)?;
        let pubkey = identity.verifying_key_bytes();
        // Never seal over a hole: the previous head must still be covered by
        // the log. A dedup hit re-runs the hook on an already sealed line,
        // which this lets through and re-seals to the same value.
        match &prior {
            Some(h) if h.verify(&pubkey) && covers(&bytes, h) => {}
            None if bytes.len() == input.record.len() => {}
            _ => bail!("decision log no longer matches its signed head; not sealing over it"),
        }
        let head = Head::seal(
            &identity,
            HeadFields {
                channel_id: input.channel_id.to_owned(),
                seq: input.event.seq,
                lines: full.lines,
                chain_hash: hex::encode(full.hash),
                key_version: Some(key_version),
                legacy_through: None,
            },
            chrono::Utc::now(),
        );
        write_head(&dir, &head)
    })
}

/// Re-seal whatever the log holds now, as the runtime would after a
/// legitimate write. Tests that edit the log on purpose use it to model a
/// writer rather than a deleter.
#[cfg(test)]
pub(crate) fn reseal_for_test(
    mur_home: &Path,
    agent: &str,
    channel_id: &str,
    identity: &AgentIdentity,
) {
    let bytes = read_log(&log_path(mur_home, channel_id).unwrap()).unwrap();
    let c = chain_log(&bytes);
    let head = Head::seal(
        identity,
        HeadFields {
            channel_id: channel_id.to_owned(),
            seq: 0,
            lines: c.lines,
            chain_hash: hex::encode(c.hash),
            key_version: Some(0),
            legacy_through: None,
        },
        chrono::Utc::now(),
    );
    write_head(&heads_dir(mur_home, agent), &head).unwrap();
}
