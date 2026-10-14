//! Signed head of a channel log (channel-log-integrity spec section 7, #1770).
//!
//! This slice holds only the cumulative chain of spec 7.2. It is distinct from
//! [`crate::store::line_hash`], the per-line hash that a v2 event carries as
//! `prev_hash`: that commits to one line, this commits to the whole history.

use sha2::{Digest, Sha256};

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
