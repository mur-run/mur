//! Voice-quality linter — Spec §8.5 C1.
//!
//! Called by the outbox before delivering a generated companion message.  If
//! the message violates any rule, the outbox regenerates once; a second failure
//! drops the message.
//!
//! ## Rules
//!
//! | Rule | Condition |
//! |------|-----------|
//! | `SentenceCount` | Split on `[.!?。！？]`, count non-empty trimmed segments. Passes iff 1–3 inclusive. |
//! | `BannedPhrase` | Substring match on an embedded list keyed by language prefix (`zh` / `en`). |
//! | `EmojiCount` | Count Unicode scalars in emoji blocks; > 1 fails. |
//! | `ExclamationCount` | Count `!` + `！`; > 1 fails. |
//! | `PreservedEnglishRatioZh` | Only for `locale.starts_with("zh")`. Ratio of "English tokens" to total tokens > 0.30 fails. |
//!
//! ### PreservedEnglishRatioZh formula
//!
//! A *token* is any whitespace-separated run of characters.  An *English token*
//! is a token whose every character is `is_ascii_alphabetic()` **and** whose
//! length is ≥ 2 (to avoid stray "I" or "a").  The ratio is
//! `english_tokens / total_tokens`.  0 total tokens → ratio = 0.0 → no violation.

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ViolationRule {
    /// 0 or > 3 sentence-ending punctuation segments.
    SentenceCount,
    /// A banned phrase was found in the body.
    BannedPhrase,
    /// More than 1 emoji codepoint detected.
    EmojiCount,
    /// More than 1 exclamation mark (`!` or `！`).
    ExclamationCount,
    /// zh-* locale: preserved-English token ratio > 30 %.
    PreservedEnglishRatioZh,
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub rule: ViolationRule,
    /// Human-readable detail for telemetry only — never shown to the user.
    pub detail: String,
}

/// Report returned by [`check`].
#[derive(Debug)]
pub struct LinterReport {
    /// `true` iff `violations` is empty.
    pub passed: bool,
    pub violations: Vec<Violation>,
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Check `body` against all applicable voice-quality rules for `locale`.
pub fn check(body: &str, locale: &str) -> LinterReport {
    let mut violations: Vec<Violation> = Vec::new();

    check_sentence_count(body, &mut violations);
    check_banned_phrase(body, locale, &mut violations);
    check_emoji_count(body, &mut violations);
    check_exclamation_count(body, &mut violations);
    check_english_ratio_zh(body, locale, &mut violations);

    let passed = violations.is_empty();
    LinterReport { passed, violations }
}

// ─── Rule implementations ─────────────────────────────────────────────────────

/// Rule 1 — 1–3 sentences.
fn check_sentence_count(body: &str, out: &mut Vec<Violation>) {
    // Split on sentence-ending punctuation (both ASCII and CJK full-width).
    let count = body
        .split(['.', '!', '?', '。', '！', '？'])
        .filter(|s| !s.trim().is_empty())
        .count();

    if !(1..=3).contains(&count) {
        out.push(Violation {
            rule: ViolationRule::SentenceCount,
            detail: format!("got {count} sentence segment(s); expected 1–3"),
        });
    }
}

/// Rule 2 — no banned phrases.
///
/// Phrases for `zh` (covers zh-TW, zh-CN, zh-Hant, …) are exact CJK substring
/// matches (CJK is already case-insensitive).  Phrases for `en` are ASCII
/// lower-cased before comparison.
fn check_banned_phrase(body: &str, locale: &str, out: &mut Vec<Violation>) {
    let banned_zh: &[&str] = &["好棒", "加油加油", "太厲害了"];
    let banned_en: &[&str] = &["amazing!!", "awesome!!"];

    if locale.starts_with("zh") {
        for phrase in banned_zh {
            if body.contains(phrase) {
                out.push(Violation {
                    rule: ViolationRule::BannedPhrase,
                    detail: format!("found banned phrase: {phrase}"),
                });
            }
        }
    } else if locale.starts_with("en") {
        let body_lc = body.to_lowercase();
        for phrase in banned_en {
            if body_lc.contains(*phrase) {
                out.push(Violation {
                    rule: ViolationRule::BannedPhrase,
                    detail: format!("found banned phrase: {phrase}"),
                });
            }
        }
    }
}

/// Rule 3 — at most 1 emoji.
///
/// Counts Unicode scalars in standard emoji blocks:
/// - `0x1F300..=0x1FAFF` — emoticons, symbols & pictographs, transport, food, faces, …
/// - `0x2600..=0x27BF`   — miscellaneous symbols + dingbats
fn check_emoji_count(body: &str, out: &mut Vec<Violation>) {
    let count = body.chars().filter(|&c| is_emoji(c as u32)).count();
    if count > 1 {
        out.push(Violation {
            rule: ViolationRule::EmojiCount,
            detail: format!("found {count} emoji codepoint(s); limit is 1"),
        });
    }
}

/// Returns `true` if `cp` falls in a standard emoji Unicode block.
#[inline]
fn is_emoji(cp: u32) -> bool {
    matches!(cp, 0x1F300..=0x1FAFF | 0x2600..=0x27BF)
}

/// Rule 4 — at most 1 exclamation mark (ASCII `!` or FULLWIDTH `！`).
fn check_exclamation_count(body: &str, out: &mut Vec<Violation>) {
    let count = body.chars().filter(|&c| c == '!' || c == '！').count();
    if count > 1 {
        out.push(Violation {
            rule: ViolationRule::ExclamationCount,
            detail: format!("found {count} exclamation mark(s); limit is 1"),
        });
    }
}

/// Rule 5 — preserved-English ratio ≤ 30 % for zh-* locales.
///
/// See module-level doc for the token/ratio formula.
fn check_english_ratio_zh(body: &str, locale: &str, out: &mut Vec<Violation>) {
    if !locale.starts_with("zh") {
        return;
    }

    let tokens: Vec<&str> = body.split_whitespace().collect();
    let total = tokens.len();
    if total == 0 {
        return;
    }

    let english = tokens
        .iter()
        .filter(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
        .count();

    let ratio = english as f64 / total as f64;
    if ratio > 0.30 {
        out.push(Violation {
            rule: ViolationRule::PreservedEnglishRatioZh,
            detail: format!(
                "English token ratio {:.0}% exceeds 30% ({english}/{total} tokens)",
                ratio * 100.0
            ),
        });
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. Clean zh-TW body — should pass all rules ───────────────────────────
    #[test]
    fn passes_clean_zh_tw_one_sentence() {
        // "David" is 1 English token; total tokens ≈ 4 ("早安", "David。今天想從哪一件小事開始？")
        // But let's count carefully: split_whitespace → ["早安", "David。今天想從哪一件小事開始？"]
        // English tokens with len≥2 all-ascii-alpha: "David" would fail because "David。"
        // includes non-ascii. Actually "David。今天想從哪一件小事開始？" is one token — no
        // all-ascii-alpha chars at len≥2 level. So ratio = 0/2 = 0%. Pass.
        let report = check("早安 David。今天想從哪一件小事開始？", "zh-TW");
        assert!(
            report.passed,
            "expected pass; violations: {:?}",
            report
                .violations
                .iter()
                .map(|v| &v.detail)
                .collect::<Vec<_>>()
        );
    }

    // ── 2. Banned phrase zh ───────────────────────────────────────────────────
    #[test]
    fn fails_banned_phrase_zh() {
        let report = check("今天好棒！", "zh-TW");
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::BannedPhrase),
            "expected BannedPhrase violation"
        );
    }

    // ── 3. Emoji count > 1 ────────────────────────────────────────────────────
    #[test]
    fn fails_emoji_count() {
        // Two emoji: 😊 (U+1F60A) + 🎉 (U+1F389) — both in 0x1F300..=0x1FAFF
        let report = check("Hello 😊 and 🎉 today.", "en-US");
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::EmojiCount),
            "expected EmojiCount violation"
        );
    }

    // ── 4. Exclamation count ──────────────────────────────────────────────────
    #[test]
    fn fails_exclamation_count() {
        // "Awesome!!" contains the banned phrase AND two exclamation marks.
        let report = check("Awesome!! Great!!", "en-US");
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::ExclamationCount),
            "expected ExclamationCount violation"
        );
        // Also BannedPhrase ("awesome!!") should fire.
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::BannedPhrase),
            "expected BannedPhrase violation for 'awesome!!'"
        );
    }

    // ── 5. zh-TW mostly-English body ─────────────────────────────────────────
    #[test]
    fn fails_zh_english_ratio() {
        // 4 out of 5 tokens are all-ascii-alpha with length ≥ 2 → ratio 80%
        let report = check("hello world foo bar 好。", "zh-TW");
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::PreservedEnglishRatioZh),
            "expected PreservedEnglishRatioZh violation"
        );
    }

    // ── 6. Empty body → SentenceCount (0 sentences) ──────────────────────────
    #[test]
    fn fails_zero_sentences() {
        let report = check("", "en-US");
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::SentenceCount),
            "expected SentenceCount violation for empty body"
        );
    }

    // ── 7. Too many sentences (> 3) ───────────────────────────────────────────
    #[test]
    fn fails_too_many_sentences() {
        // "a. b. c. d." → 4 non-empty segments
        let report = check("a. b. c. d.", "en-US");
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::SentenceCount),
            "expected SentenceCount violation for 4 segments"
        );
    }

    // ── 8. Exactly 3 sentences passes ─────────────────────────────────────────
    #[test]
    fn passes_three_sentences_en() {
        let report = check("Good morning. How are you? Let me know.", "en-US");
        assert!(
            report.passed,
            "3 sentences should pass; violations: {:?}",
            report
                .violations
                .iter()
                .map(|v| &v.detail)
                .collect::<Vec<_>>()
        );
    }

    // ── 9. One emoji passes ───────────────────────────────────────────────────
    #[test]
    fn passes_one_emoji() {
        let report = check("早安 😊 今天想從哪一件小事開始？", "zh-TW");
        // Should NOT have EmojiCount violation (only 1 emoji).
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::EmojiCount),
            "1 emoji should not trigger EmojiCount"
        );
    }

    // ── 10. Non-zh locale: English ratio rule does not fire ───────────────────
    #[test]
    fn en_locale_skips_english_ratio_rule() {
        // All-English body → rule should not fire for en-US
        let report = check("Hello world foo bar baz.", "en-US");
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.rule == ViolationRule::PreservedEnglishRatioZh),
            "PreservedEnglishRatioZh must not fire for en-US locale"
        );
    }
}
