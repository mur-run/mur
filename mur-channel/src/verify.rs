//! Channel log verification (channel-log-integrity spec section 8, #1770).
//!
//! Read-only: takes no `events.lock` and writes nothing. It reads raw bytes, not
//! [`ChannelStore::load_events`], because that returns an empty `Vec` for an
//! absent log and for an all-unparseable one, and skips bad lines silently.
//!
//! "Line" means exactly what sealing means by it ([`split_lines`]: `\n`
//! terminated, unterminated tail excluded), so the two cannot drift apart.
//!
//! **Read order matters.** The head is read before the log. A seal writes the
//! log first and the head second, so a head read first can only describe a
//! prefix of the log read afterwards. The other order could pair a newer head
//! with an older log snapshot and report a false `TailTruncated`.
//!
//! `Intact` means "unchanged since seal", never "genuine" (spec 8.1), and
//! `NoHead` means "not sealed", never "no tampering found" (spec 8.3).
//!
//! Decisions made here where the spec is silent or still open (flag in review):
//! * D1 (migration-epoch marker) is not implemented: where the marker lives is
//!   still open inside the decision. So `NoHead` is never an error here.
//! * A channel with neither a log nor a head is [`IntegrityState::NoLog`], a
//!   state outside the 8.1 table, which only covers a head or a log.
//! * Chain membership is by line position (`index < head.lines`), the thing the
//!   chain actually commits to, not by `seq <= head.seq`. They agree on an
//!   untampered log.
//! * A head that is not [`IntegrityState::Intact`]-trustworthy (`HeadInvalid`)
//!   classifies every line `Unsealed` and its `legacy_through` is not honoured.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use mur_common::channel::ChannelEvent;

use crate::head::{HEAD_VERSION, Head, chain_prefix, split_lines};
use crate::head_io::head_path;
use crate::seal::HEADS_DIR_NAME;
use crate::sign::verify_one;
use crate::store::ChannelStore;

/// Resolves the router's public key for a `key_version`; `None` if unknown.
/// The caller owns key resolution (rotation chain), this module does not.
pub type ResolveKey<'a> = &'a dyn Fn(Option<u32>) -> Option<[u8; 32]>;

/// Why a head is not usable (spec 8.1 `HeadInvalid`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadFault {
    /// The file exists but is not a head. Carries the parse error.
    Unparseable(String),
    UnknownVersion(u32),
    /// The head names another channel than the file it sits in.
    WrongChannel(String),
    /// No router key is known for the head's `key_version`.
    UnknownKeyVersion(Option<u32>),
    BadSignature,
}

/// The headline verdict, first match wins (spec 8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityState {
    /// Head exists, log does not: an orphan from an interrupted delete (7.4).
    LogMissing,
    /// Neither a head nor a log. Not in the spec's table; nothing to verify.
    NoLog,
    HeadInvalid(HeadFault),
    /// Log present, no head. "Not sealed" - proves nothing either way (8.3).
    NoHead,
    Gap,
    Reordered,
    DuplicateSeq,
    TailTruncated,
    /// The first `lines` lines no longer hash to the head's `chain_hash`.
    ChainBroken {
        lines: u64,
    },
    /// Unchanged since seal. Not "genuine".
    Intact,
}

impl IntegrityState {
    /// Whether this state alone is a failure (the `Error?` column of 8.1).
    pub fn is_error(&self) -> bool {
        !matches!(
            self,
            Self::LogMissing | Self::NoLog | Self::NoHead | Self::Intact
        )
    }
}

/// What kind of `seq` discontinuity between two consecutive parseable lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqFaultKind {
    Gap,
    Reordered,
    DuplicateSeq,
}

/// A `seq` discontinuity, located by the 1-based file line of the later line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqFinding {
    pub kind: SeqFaultKind,
    pub line_no: u64,
    pub prev_seq: u64,
    pub seq: u64,
    /// At or below `legacy_through`: predates the chain, a warning not an error.
    pub legacy: bool,
}

/// How much a line can be trusted (spec 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineClass {
    /// `seq <= legacy_through`: sealed as found, not claimed unmodified before.
    Legacy,
    /// In the chain and router-signed.
    Sealed,
    /// In the chain, but unsigned or agent-signed. Deletion and reordering are
    /// detected; the content may predate the seal, so this is not authenticity.
    SealedUnsigned,
    /// Past the head. Not tamper-evident; callers must not treat it as trusted.
    Unsealed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineReport {
    /// 1-based file line, the numbering of `unparseable_lines`.
    pub line_no: u64,
    pub seq: u64,
    pub class: LineClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelReport {
    pub channel_id: String,
    pub state: IntegrityState,
    /// The head that was read, even when it is the thing that is invalid.
    pub head: Option<Head>,
    /// Every `seq` discontinuity, legacy ones included (flagged).
    pub seq_findings: Vec<SeqFinding>,
    /// 1-based file lines that parse as no event. Blank lines are not damage.
    pub unparseable_lines: Vec<u64>,
    /// Bytes after the last `\n`. Informational: with no lock held it may be an
    /// append in progress; evidence of a torn write only if it persists.
    pub unterminated_tail_bytes: u64,
    pub lines: Vec<LineReport>,
}

impl ChannelReport {
    /// True when the state is an error or any non-legacy `seq` hole exists,
    /// including under `NoHead`, where the state alone would say nothing.
    pub fn is_failure(&self) -> bool {
        self.state.is_error() || self.seq_findings.iter().any(|f| !f.legacy)
    }
}

/// Verify one channel under `mur_home`. `resolve_key` supplies router keys.
pub fn verify_channel(
    mur_home: &Path,
    channel_id: &str,
    resolve_key: ResolveKey<'_>,
) -> Result<ChannelReport> {
    let heads_dir = mur_home.join(HEADS_DIR_NAME);
    let hpath = head_path(&heads_dir, channel_id)?;
    let events_path = ChannelStore::new(mur_home).events_path(channel_id);

    // Head first, then log (module doc).
    let head_bytes = read_optional(&hpath)?;
    let log = read_optional(&events_path)?;

    let (head, parse_fault) = match &head_bytes {
        None => (None, None),
        Some(b) => match serde_json::from_slice::<Head>(b) {
            Ok(h) => (Some(h), None),
            Err(e) => (None, Some(HeadFault::Unparseable(e.to_string()))),
        },
    };

    let mut report = ChannelReport {
        channel_id: channel_id.to_string(),
        state: IntegrityState::NoLog,
        head: head.clone(),
        seq_findings: Vec::new(),
        unparseable_lines: Vec::new(),
        unterminated_tail_bytes: 0,
        lines: Vec::new(),
    };

    let Some(log) = log else {
        report.state = if head_bytes.is_some() {
            IntegrityState::LogMissing
        } else {
            IntegrityState::NoLog
        };
        return Ok(report);
    };

    let fault = parse_fault.or_else(|| {
        head.as_ref()
            .and_then(|h| head_fault(h, channel_id, resolve_key))
    });
    // A faulty head is evidence, not a trust anchor: nothing is sealed by it.
    let trusted = if fault.is_none() { head.as_ref() } else { None };

    let lines = split_lines(&log);
    let consumed: usize = lines.iter().map(|l| l.len() + 1).sum();
    report.unterminated_tail_bytes = (log.len() - consumed) as u64;

    let legacy_through = trusted.and_then(|h| h.legacy_through);
    let events = scan(&lines, &mut report, legacy_through);

    classify(&mut report, &events, trusted, channel_id, resolve_key);

    report.state = match (fault, trusted) {
        (Some(f), _) => IntegrityState::HeadInvalid(f),
        (None, Some(h)) => state_for_valid_head(&report, h, &log, &events, lines.len() as u64),
        // No head at all. A non-legacy hole is surfaced by `is_failure`.
        (None, None) => IntegrityState::NoHead,
    };
    Ok(report)
}

/// `Ok(None)` only for a file that is not there.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn head_fault(head: &Head, channel_id: &str, resolve_key: ResolveKey<'_>) -> Option<HeadFault> {
    if head.v != HEAD_VERSION {
        return Some(HeadFault::UnknownVersion(head.v));
    }
    if head.channel_id != channel_id {
        return Some(HeadFault::WrongChannel(head.channel_id.clone()));
    }
    let Some(key) = resolve_key(head.key_version) else {
        return Some(HeadFault::UnknownKeyVersion(head.key_version));
    };
    (!head.verify(&key)).then_some(HeadFault::BadSignature)
}

/// A parseable line with its position.
struct Parsed {
    /// 0-based index into the terminated lines.
    idx: usize,
    event: ChannelEvent,
}

/// Parse every line, recording unparseable ones and `seq` discontinuities.
fn scan(lines: &[&[u8]], report: &mut ChannelReport, legacy_through: Option<u64>) -> Vec<Parsed> {
    let mut events: Vec<Parsed> = Vec::new();
    for (idx, raw) in lines.iter().enumerate() {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue; // same as load_events: blank is not damage
        }
        let Ok(event) = serde_json::from_slice::<ChannelEvent>(raw) else {
            report.unparseable_lines.push(idx as u64 + 1);
            continue;
        };
        if let Some(prev) = events.last() {
            let (p, c) = (prev.event.seq, event.seq);
            let kind = match c {
                _ if c == p => Some(SeqFaultKind::DuplicateSeq),
                _ if c < p => Some(SeqFaultKind::Reordered),
                _ if c > p + 1 => Some(SeqFaultKind::Gap),
                _ => None,
            };
            if let Some(kind) = kind {
                report.seq_findings.push(SeqFinding {
                    kind,
                    line_no: idx as u64 + 1,
                    prev_seq: p,
                    seq: c,
                    legacy: legacy_through.is_some_and(|l| c <= l),
                });
            }
        }
        events.push(Parsed { idx, event });
    }
    events
}

/// Per-line class (spec 8.2). With no trusted head every line is `Unsealed`.
fn classify(
    report: &mut ChannelReport,
    events: &[Parsed],
    head: Option<&Head>,
    channel_id: &str,
    resolve_key: ResolveKey<'_>,
) {
    for p in events {
        let class = match head {
            None => LineClass::Unsealed,
            Some(h) if h.legacy_through.is_some_and(|l| p.event.seq <= l) => LineClass::Legacy,
            Some(h) if (p.idx as u64) < h.lines => {
                let router_signed = resolve_key(p.event.key_version).is_some_and(|k| {
                    p.event.sig.is_some() && verify_one(channel_id, &p.event, &k, true)
                });
                if router_signed {
                    LineClass::Sealed
                } else {
                    LineClass::SealedUnsigned
                }
            }
            Some(_) => LineClass::Unsealed,
        };
        report.lines.push(LineReport {
            line_no: p.idx as u64 + 1,
            seq: p.event.seq,
            class,
        });
    }
}

/// States 4-7 of the 8.1 table, for a head that passed its own checks.
fn state_for_valid_head(
    report: &ChannelReport,
    head: &Head,
    log: &[u8],
    events: &[Parsed],
    lines_present: u64,
) -> IntegrityState {
    // Named holes first: `Gap` names the place a bare `ChainBroken` cannot.
    if let Some(f) = report.seq_findings.iter().find(|f| !f.legacy) {
        return match f.kind {
            SeqFaultKind::Gap => IntegrityState::Gap,
            SeqFaultKind::Reordered => IntegrityState::Reordered,
            SeqFaultKind::DuplicateSeq => IntegrityState::DuplicateSeq,
        };
    }
    let last_seq = events.last().map(|p| p.event.seq);
    if last_seq.is_none_or(|s| head.seq > s) || lines_present < head.lines {
        return IntegrityState::TailTruncated;
    }
    match chain_prefix(log, head.lines) {
        Some(c) if hex::encode(c.hash) == head.chain_hash => IntegrityState::Intact,
        _ => IntegrityState::ChainBroken { lines: head.lines },
    }
}
