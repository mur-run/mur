//! The head's cumulative chain (channel-log-integrity spec 7.2, #1770):
//! `H_n = SHA-256(H_{n-1} || raw_n)`, `H_0` = 32 zero bytes, `raw_n` = the bytes
//! of line `n` between two `\n`, terminator excluded. This is NOT the per-line
//! `line_hash` that v2 events carry as `prev_hash`; the two coexist.
//!
//! The expected values here are recomputed with `sha2` directly, never with the
//! code under test, so the test pins the spec rather than the implementation.

use mur_channel::head::{ChainState, HEAD_VERSION, chain_log};
use sha2::{Digest, Sha256};

fn step(prev: [u8; 32], raw: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev);
    h.update(raw);
    h.finalize().into()
}

fn expected(lines: &[&[u8]]) -> [u8; 32] {
    lines.iter().fold([0u8; 32], |acc, raw| step(acc, raw))
}

#[test]
fn head_version_starts_at_one() {
    assert_eq!(HEAD_VERSION, 1);
}

#[test]
fn empty_log_is_genesis_of_32_zero_bytes() {
    let s = chain_log(b"");
    assert_eq!(s.lines, 0);
    assert_eq!(s.hash, [0u8; 32]);
    assert_eq!(ChainState::genesis(), s);
}

#[test]
fn n_lines_chain_cumulatively_from_h0() {
    let log = b"{\"seq\":1}\n{\"seq\":2}\n{\"seq\":3}\n";
    let s = chain_log(log);
    assert_eq!(s.lines, 3);
    assert_eq!(
        s.hash,
        expected(&[b"{\"seq\":1}", b"{\"seq\":2}", b"{\"seq\":3}"])
    );
}

#[test]
fn the_terminator_is_not_part_of_the_chained_bytes() {
    // Same raw bytes, so the chain must be the same as the one built from
    // exactly those bytes.
    assert_eq!(chain_log(b"abc\n").hash, expected(&[b"abc"]));
}

#[test]
fn incremental_extend_matches_whole_log_chain() {
    let mut s = ChainState::genesis();
    for raw in [&b"one"[..], b"two", b"three"] {
        s.extend(raw);
    }
    assert_eq!(s, chain_log(b"one\ntwo\nthree\n"));
}

#[test]
fn empty_and_unparseable_lines_are_chained_and_counted() {
    let log = b"{\"seq\":1}\n\n\xff\xfe not json\n";
    let s = chain_log(log);
    assert_eq!(s.lines, 3, "an empty line and a non-UTF-8 line each count");
    assert_eq!(
        s.hash,
        expected(&[b"{\"seq\":1}", b"", b"\xff\xfe not json"])
    );
}

#[test]
fn a_cr_before_the_newline_stays_in_the_chained_bytes() {
    let s = chain_log(b"a\r\n");
    assert_eq!(s.hash, expected(&[b"a\r"]));
    assert_ne!(
        s.hash,
        expected(&[b"a"]),
        "str::lines() would have stripped it"
    );
}

#[test]
fn an_unterminated_tail_is_not_chained() {
    let s = chain_log(b"done\npartial");
    assert_eq!(s.lines, 1);
    assert_eq!(s.hash, expected(&[b"done"]));
}

#[test]
fn any_byte_change_breaks_the_chain_from_that_line_on() {
    let a = chain_log(b"x\ny\nz\n");
    let b = chain_log(b"x\nY\nz\n");
    assert_ne!(a.hash, b.hash);
}

#[test]
fn chain_is_order_sensitive() {
    assert_ne!(chain_log(b"x\ny\n").hash, chain_log(b"y\nx\n").hash);
}

/// `chain_log_with_prefix` must agree with the two separate passes it
/// replaces, for every prefix length, on logs with empty, CRLF, non-UTF-8 and
/// unterminated lines.
#[test]
fn chain_with_prefix_matches_the_two_separate_passes() {
    use mur_channel::head::{chain_log_with_prefix, chain_prefix};
    let logs: [&[u8]; 6] = [
        b"",
        b"a\n",
        b"a\nb\nc\n",
        b"a\r\n\n\xff\xfe\nz\n",
        b"a\nb\npartial",
        b"partial-only",
    ];
    for log in logs {
        let total = chain_log(log);
        for n in 0..=(total.lines + 2) {
            let (whole, prefix) = chain_log_with_prefix(log, n);
            assert_eq!(whole, total, "whole-log state, n={n}, log={log:?}");
            assert_eq!(
                prefix,
                chain_prefix(log, n),
                "prefix state, n={n}, log={log:?}"
            );
        }
    }
}
