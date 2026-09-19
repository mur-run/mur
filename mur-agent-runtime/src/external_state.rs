//! Does a reply name external state — a SHA, a PR number, a test tally, a
//! diff — the shapes a tool leaves behind? Structural on purpose: no
//! completion vocabulary in any language, so a rephrased report does not
//! slip through and a well-worded answer is not caught. Mis-fires are
//! accepted (spec 2026-09-19-unverified-claim-card §2.3).

use regex::Regex;
use std::sync::OnceLock;

/// Length bounds of a git object id, abbreviated to full.
const SHA_MIN: usize = 7;
const SHA_MAX: usize = 40;
/// Digits a `#` reference needs. `#1` is a heading or a footnote; `#14` is a
/// pull request.
const HASH_MIN_DIGITS: usize = 2;

/// Rows that regexes can express without lookaround. `\b` is safe here: every
/// token is an ASCII word, so the CJK boundary problem does not apply.
fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // GitHub's own state strings, as `gh … --json` prints them.
            r"\b(?:MERGED|MERGEABLE|CONFLICTING|UNSTABLE)\b",
            // A test runner's tally.
            r"\d+ passed|\d+ failed|test result:",
            // A GitHub object page.
            r"github\.com/\S+/(?:pull|commit|issues)/",
            // A diff or patch: markers that only mean something at line start.
            r"(?m)^(?:diff --git|@@|\+\+\+ |--- )",
            // `git diff --stat` / merge summary.
            r"\d+ files? changed|\d+ insertions?\(\+\)|\d+ deletions?\(-\)",
        ]
        .into_iter()
        .map(|p| Regex::new(p).expect("static pattern"))
        .collect()
    })
}

/// A maximal run of hex characters that is a plausible git object id: 7–40
/// long, all lowercase, at least one letter. Maximal-run scanning is the
/// lookaround-free form of `(?<![0-9a-fA-F])[0-9a-f]{7,40}(?![0-9a-fA-F])`,
/// and unlike `\b` it works flush against CJK text.
fn has_sha(text: &str) -> bool {
    fn plausible(len: usize, lowercase: bool, letter: bool) -> bool {
        lowercase && letter && (SHA_MIN..=SHA_MAX).contains(&len)
    }
    let mut run_len = 0usize;
    let mut run_ok = true; // all lowercase hex so far
    let mut run_has_letter = false;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run_len += 1;
            if c.is_ascii_uppercase() {
                run_ok = false;
            }
            if c.is_ascii_alphabetic() {
                run_has_letter = true;
            }
        } else {
            if plausible(run_len, run_ok, run_has_letter) {
                return true;
            }
            run_len = 0;
            run_ok = true;
            run_has_letter = false;
        }
    }
    plausible(run_len, run_ok, run_has_letter)
}

/// `#` followed by at least `HASH_MIN_DIGITS` digits, not glued to a word
/// character on the left (`abc#1402` is not a reference; `（#1402）` is).
fn has_hash_ref(text: &str) -> bool {
    let mut prev: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && !prev.is_some_and(|p| p.is_ascii_alphanumeric() || p == '_') {
            let mut digits = 0usize;
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                chars.next();
                digits += 1;
            }
            if digits >= HASH_MIN_DIGITS {
                return true;
            }
            prev = Some('0'); // we consumed digits; next char cannot be flush
            continue;
        }
        prev = Some(c);
    }
    false
}

/// Does `text` carry external-state evidence (spec §2.1)? One pass per rule.
pub fn claims_external_state(text: &str) -> bool {
    has_sha(text) || has_hash_ref(text) || patterns().iter().any(|re| re.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signal_row_fires_on_its_own() {
        for (label, text) in [
            ("sha", "main is now at 1e0a4d40"),
            ("pr", "opened #1402"),
            ("state", "the PR is MERGED now"),
            ("tally", "3048 passed / 0 failed"),
            ("tally-result", "test result: ok. 12 passed"),
            ("url", "see https://github.com/mur-run/mur/pull/1402"),
            ("diff", "here:\ndiff --git a/x.rs b/x.rs\n"),
            ("hunk", "patch:\n@@ -1,3 +1,4 @@\n"),
            ("stat", "2 files changed, 10 insertions(+), 1 deletion(-)"),
        ] {
            assert!(claims_external_state(text), "{label}: {text:?}");
        }
    }

    #[test]
    fn a_sha_is_found_flush_against_cjk_text() {
        assert!(claims_external_state("main現在是1e0a4d40。"));
        assert!(claims_external_state("`main` 現在是 `1e0a4d40`"));
    }

    #[test]
    fn digits_alone_are_not_a_sha() {
        assert!(!claims_external_state("call 1234567 today"));
        assert!(!claims_external_state("Version 2.85.0"));
    }

    #[test]
    fn a_hex_run_longer_than_a_sha_is_not_a_sha() {
        let long = "1".to_string() + &"deadbeef00".repeat(5); // 51 hex chars
        assert!(!claims_external_state(&long));
    }

    #[test]
    fn a_hash_needs_two_digits_and_a_non_word_before_it() {
        assert!(!claims_external_state("#1"));
        assert!(!claims_external_state("abc#1402"));
        assert!(claims_external_state("#1402"));
        assert!(claims_external_state("（#1402）"));
    }

    #[test]
    fn state_words_must_be_upper_case_whole_words() {
        assert!(!claims_external_state("the branch was merged"));
        assert!(!claims_external_state("UNMERGEDX"));
        assert!(claims_external_state("state: MERGEABLE"));
    }

    #[test]
    fn diff_markers_must_start_a_line() {
        assert!(!claims_external_state("use --- as a separator"));
        assert!(!claims_external_state("email me @@ 5pm"));
        assert!(claims_external_state("\n--- a/x\n+++ b/x\n"));
    }

    #[test]
    fn plain_chat_and_explanations_do_not_fire() {
        assert!(!claims_external_state("你好"));
        assert!(!claims_external_state("哈囉，今天想折騰點什麼？"));
        assert!(!claims_external_state(
            "rebase 會把你的 commit 重新套到新的 base 上，衝突要逐個解。"
        ));
        assert!(!claims_external_state(""));
    }
}
