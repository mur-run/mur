//! Byte-budget helpers for the project instructions block.

/// The longest prefix of `text` within `max` bytes, cut on a char boundary so
/// a CJK or emoji character straddling the limit is dropped, not split.
pub(super) fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
