//! Signed head of a channel log (channel-log-integrity spec section 7, #1770).
//!
//! This slice holds only the cumulative chain of spec 7.2. It is distinct from
//! [`crate::store::line_hash`], the per-line hash that a v2 event carries as
//! `prev_hash`: that commits to one line, this commits to the whole history.

use chrono::{DateTime, Utc};
use mur_common::identity::AgentIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sign::verify_input_sig;

/// Head format version, separate from `SIG_INPUT_VERSION` (spec 7.2). Fixes
/// `H_0`, the `\n` split and the raw-byte rule; changing any is a new version.
pub const HEAD_VERSION: u32 = 1;

/// The running chain: how many lines are chained and `H_n` after the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainState {
    /// Number of file lines chained, empty and unparseable lines included.
    pub lines: u64,
    /// `H_lines`; 32 zero bytes when `lines == 0`.
    pub hash: [u8; 32],
}

impl ChainState {
    /// `H_0`: no lines, 32 zero bytes.
    pub fn genesis() -> Self {
        Self {
            lines: 0,
            hash: [0u8; 32],
        }
    }

    /// Chain one more line: `H_n = SHA-256(H_{n-1} || raw)`. `raw` is the line
    /// without its `\n`; a preceding `\r` is part of it.
    pub fn extend(&mut self, raw: &[u8]) {
        let mut h = Sha256::new();
        h.update(self.hash);
        h.update(raw);
        self.hash = h.finalize().into();
        self.lines += 1;
    }
}

/// Chain every `\n`-terminated line of `log`, as found. An unterminated tail is
/// not chained (spec 7.2 / section 9). Works on raw bytes, so a non-UTF-8 line
/// is just another line.
pub fn chain_log(log: &[u8]) -> ChainState {
    let mut state = ChainState::genesis();
    let mut rest = log;
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        state.extend(&rest[..nl]);
        rest = &rest[nl + 1..];
    }
    state
}

/// Chain only the first `n` `\n`-terminated lines of `log`, same rules as
/// [`chain_log`]. `None` when the log has fewer than `n` such lines — the
/// shape of a log truncated below a head that covered `n` lines.
pub fn chain_prefix(log: &[u8], n: u64) -> Option<ChainState> {
    let mut state = ChainState::genesis();
    let mut rest = log;
    while state.lines < n {
        let nl = rest.iter().position(|&b| b == b'\n')?;
        state.extend(&rest[..nl]);
        rest = &rest[nl + 1..];
    }
    Some(state)
}

/// [`chain_log`] and [`chain_prefix`] in one pass over `log`: the state after
/// every `\n`-terminated line, and the state after the first `n` of them
/// (`None` when the log has fewer than `n`). Same result as calling both, at
/// the cost of one scan, which matters because the head check needs both on
/// every anchored append.
pub fn chain_log_with_prefix(log: &[u8], n: u64) -> (ChainState, Option<ChainState>) {
    let mut state = ChainState::genesis();
    let mut prefix = (n == 0).then_some(state);
    let mut rest = log;
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        state.extend(&rest[..nl]);
        if state.lines == n {
            prefix = Some(state);
        }
        rest = &rest[nl + 1..];
    }
    (state, prefix)
}

/// The signed fields of a head, before `ts` and `sig` are attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadFields {
    pub channel_id: String,
    /// `seq` of the last parseable event at seal time.
    pub seq: u64,
    /// File lines chained; detects loss of a trailing unparseable line.
    pub lines: u64,
    /// Hex of `H_lines`.
    pub chain_hash: String,
    pub key_version: Option<u32>,
    /// Last `seq` that existed at first seal, or `None` for a channel that has
    /// had a head since its first line. Signed so it cannot be raised later to
    /// hide a hole.
    pub legacy_through: Option<u64>,
}

/// A router-signed head (spec 7.2). `ts` is informational and excluded from the
/// signature so verification is deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub v: u32,
    pub channel_id: String,
    pub seq: u64,
    pub lines: u64,
    pub chain_hash: String,
    pub key_version: Option<u32>,
    pub legacy_through: Option<u64>,
    pub ts: DateTime<Utc>,
    /// Multibase (base58btc) Ed25519 signature over [`Head::sign_input`].
    pub sig: String,
}

impl Head {
    /// Canonical signed bytes: the object `{v, channel_id, seq, lines,
    /// chain_hash, key_version, legacy_through}` with sorted keys (`serde_json`
    /// has no `preserve_order` here), no `sig`, no `ts`. Absent optionals are
    /// signed as `null`, so dropping one changes the bytes.
    pub fn sign_input(&self) -> Vec<u8> {
        let canon = serde_json::json!({
            "v": self.v,
            "channel_id": self.channel_id,
            "seq": self.seq,
            "lines": self.lines,
            "chain_hash": self.chain_hash,
            "key_version": self.key_version,
            "legacy_through": self.legacy_through,
        });
        serde_json::to_vec(&canon).unwrap_or_default()
    }

    /// Sign `fields` with the router `identity` at [`HEAD_VERSION`].
    pub fn seal(identity: &AgentIdentity, fields: HeadFields, ts: DateTime<Utc>) -> Self {
        let mut head = Self {
            v: HEAD_VERSION,
            channel_id: fields.channel_id,
            seq: fields.seq,
            lines: fields.lines,
            chain_hash: fields.chain_hash,
            key_version: fields.key_version,
            legacy_through: fields.legacy_through,
            ts,
            sig: String::new(),
        };
        let sig = identity.sign_bytes(&head.sign_input());
        head.sig = multibase::encode(multibase::Base::Base58Btc, sig);
        head
    }

    /// True only for a known `v` whose signature verifies under `pubkey`.
    /// Fail-closed: a malformed signature is just `false`.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        self.v == HEAD_VERSION && verify_input_sig(&self.sign_input(), &self.sig, pubkey)
    }
}
