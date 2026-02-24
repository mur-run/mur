//! Noise filter for the capture pipeline.
//!
//! Filters out low-value text before pattern extraction:
//! - Greetings and pleasantries
//! - Single-word responses
//! - Emoji-only messages
//! - Very short CJK text (<6 chars)
//! - Boilerplate AI responses

use regex::Regex;
use std::sync::LazyLock;

/// Result of noise filtering.
#[derive(Debug, PartialEq)]
pub enum FilterResult {
    /// Text passed the filter — worth analyzing
    Pass,
    /// Text is noise — skip it
    Noise(NoiseReason),
}

#[derive(Debug, PartialEq)]
pub enum NoiseReason {
    TooShort,
    Greeting,
    SingleWord,
    EmojiOnly,
    ShortCjk,
    Boilerplate,
}

static GREETING_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(hi|hello|hey|yo|sup|thanks|thank you|thx|ok|okay|sure|yes|no|yep|nope|great|good|nice|cool|awesome|perfect|got it|understood|好|好的|謝謝|感謝|嗯|對|是|不是|沒問題|收到|了解|明白)[.!?]*$").unwrap()
});

static EMOJI_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\s\p{Emoji}\p{Emoji_Presentation}\p{Emoji_Modifier}\p{Emoji_Component}]+$")
        .unwrap()
});

static BOILERPLATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(I'd be happy to help|Great question|Let me help you|Sure, I can|Here's what I think|That's a great|Absolutely|Of course|No problem|You're welcome|I understand|I see what you mean)").unwrap()
});

/// Count CJK characters in a string.
fn cjk_char_count(text: &str) -> usize {
    text.chars()
        .filter(|c| {
            let cp = *c as u32;
            // CJK Unified Ideographs + Extensions
            (0x4E00..=0x9FFF).contains(&cp)
                || (0x3400..=0x4DBF).contains(&cp)
                || (0x20000..=0x2A6DF).contains(&cp)
                // CJK Compatibility
                || (0xF900..=0xFAFF).contains(&cp)
                // Bopomofo
                || (0x3100..=0x312F).contains(&cp)
                // Katakana, Hiragana
                || (0x3040..=0x30FF).contains(&cp)
        })
        .count()
}

/// Check if text is predominantly CJK.
fn is_cjk_text(text: &str) -> bool {
    let total_chars = text.chars().filter(|c| !c.is_whitespace()).count();
    if total_chars == 0 {
        return false;
    }
    let cjk = cjk_char_count(text);
    cjk as f64 / total_chars as f64 > 0.5
}

/// Filter a text segment. Returns Pass if it's worth analyzing, Noise otherwise.
pub fn filter(text: &str) -> FilterResult {
    let trimmed = text.trim();

    // Empty or very short
    if trimmed.len() < 3 {
        return FilterResult::Noise(NoiseReason::TooShort);
    }

    // Single word (no spaces, < 30 chars)
    if !trimmed.contains(' ') && trimmed.len() < 30 {
        // Allow single long words (might be a command or identifier)
        if trimmed.len() < 15 {
            return FilterResult::Noise(NoiseReason::SingleWord);
        }
    }

    // Greeting / pleasantry
    if GREETING_PATTERN.is_match(trimmed) {
        return FilterResult::Noise(NoiseReason::Greeting);
    }

    // Emoji-only
    if EMOJI_PATTERN.is_match(trimmed) {
        return FilterResult::Noise(NoiseReason::EmojiOnly);
    }

    // Short CJK text (< 6 CJK characters)
    if is_cjk_text(trimmed) && cjk_char_count(trimmed) < 6 {
        return FilterResult::Noise(NoiseReason::ShortCjk);
    }

    // Boilerplate AI responses (only if the ENTIRE message is boilerplate-like, < 100 chars)
    if trimmed.len() < 100 && BOILERPLATE_PATTERN.is_match(trimmed) {
        return FilterResult::Noise(NoiseReason::Boilerplate);
    }

    FilterResult::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greetings_filtered() {
        // "hi" and "ok" are < 3 bytes → TooShort takes priority
        assert_eq!(filter("hi"), FilterResult::Noise(NoiseReason::TooShort));
        assert_eq!(filter("ok"), FilterResult::Noise(NoiseReason::TooShort));
        // "好" is 3 bytes in UTF-8, passes TooShort but hits SingleWord
        // These are long enough to hit SingleWord before Greeting
        assert_eq!(filter("Hello!"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("thanks"), FilterResult::Noise(NoiseReason::SingleWord));
        // Multi-word greetings
        assert_eq!(filter("thank you"), FilterResult::Noise(NoiseReason::Greeting));
        assert_eq!(filter("got it"), FilterResult::Noise(NoiseReason::Greeting));
        // CJK greetings — "好" is 3 bytes, passes TooShort, hits SingleWord
        assert_eq!(filter("好"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("好的"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("謝謝"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("收到"), FilterResult::Noise(NoiseReason::SingleWord));
    }

    #[test]
    fn test_short_text_filtered() {
        assert_eq!(filter(""), FilterResult::Noise(NoiseReason::TooShort));
        assert_eq!(filter("  "), FilterResult::Noise(NoiseReason::TooShort));
        assert_eq!(filter("no"), FilterResult::Noise(NoiseReason::TooShort));
    }

    #[test]
    fn test_single_word_filtered() {
        assert_eq!(filter("test"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("sure"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("whatever"), FilterResult::Noise(NoiseReason::SingleWord));
    }

    #[test]
    fn test_boilerplate_filtered() {
        assert_eq!(
            filter("I'd be happy to help!"),
            FilterResult::Noise(NoiseReason::Boilerplate)
        );
        assert_eq!(
            filter("Great question!"),
            FilterResult::Noise(NoiseReason::Boilerplate)
        );
    }

    #[test]
    fn test_short_cjk_filtered() {
        // These are single words (no space), so SingleWord hits first
        assert_eq!(filter("好的"), FilterResult::Noise(NoiseReason::SingleWord));
        assert_eq!(filter("嗯嗯"), FilterResult::Noise(NoiseReason::SingleWord));
        // Multi-word CJK that's still short
        assert_eq!(filter("是的 好"), FilterResult::Noise(NoiseReason::ShortCjk));
        assert_eq!(filter("好 嗎"), FilterResult::Noise(NoiseReason::ShortCjk));
    }

    #[test]
    fn test_meaningful_text_passes() {
        assert_eq!(
            filter("Use @Test macro instead of XCTest assertions in Swift Testing"),
            FilterResult::Pass
        );
        assert_eq!(
            filter("When writing tests, always prefer Swift Testing over XCTest"),
            FilterResult::Pass
        );
        assert_eq!(
            filter("在 Swift 中使用 @Test 巨集替代 XCTest 的斷言方法"),
            FilterResult::Pass
        );
    }

    #[test]
    fn test_long_cjk_passes() {
        assert_eq!(
            filter("在寫新的測試時，先確認 Swift Testing 是否可用"),
            FilterResult::Pass
        );
    }

    #[test]
    fn test_boilerplate_in_long_text_passes() {
        // Boilerplate filter only applies to short messages
        let long = "I'd be happy to help you with that. Here's a comprehensive guide to setting up Nginx reverse proxy with SSL termination, load balancing, and WebSocket support.";
        assert_eq!(filter(long), FilterResult::Pass);
    }

    #[test]
    fn test_cjk_char_count() {
        assert_eq!(cjk_char_count("hello"), 0);
        assert_eq!(cjk_char_count("你好世界"), 4);
        assert_eq!(cjk_char_count("Hello 世界"), 2);
    }
}
