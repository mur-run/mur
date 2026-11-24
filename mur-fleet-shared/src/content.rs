//! Content rules for a published object and its summary (spec Transfer):
//! text only, no archive, no C0/C1 control character except `\t`, `\n`, `\r`.

/// Longest summary, in UTF-8 bytes.
pub const MAX_SUMMARY_BYTES: usize = 512;

/// Leading byte signatures of archive and compressed formats that are refused.
/// A format not listed here (tar, for one) still fails the control-character
/// rule: its header holds NUL bytes.
pub const ARCHIVE_MAGICS: &[&[u8]] = &[
    b"PK\x03\x04",
    b"PK\x05\x06",
    b"\x1f\x8b",
    b"\xfd7zXZ\x00",
    b"\x28\xb5\x2f\xfd",
    b"7z\xbc\xaf\x27\x1c",
    b"Rar!\x1a\x07",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContentError {
    #[error("content is not valid UTF-8")]
    NotUtf8,
    #[error("content has a C0 or C1 control character")]
    ControlChar,
    #[error("content is an archive")]
    Archive,
    #[error("summary is longer than {MAX_SUMMARY_BYTES} bytes")]
    SummaryTooLong,
}

/// Check a draft's bytes and return them as text.
pub fn check_content(bytes: &[u8]) -> Result<&str, ContentError> {
    // Magic first: most archive formats are not valid UTF-8, and a zip header
    // is valid text with control bytes; both should say "archive".
    if ARCHIVE_MAGICS.iter().any(|m| bytes.starts_with(m)) {
        return Err(ContentError::Archive);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ContentError::NotUtf8)?;
    check_controls(text)?;
    Ok(text)
}

fn check_controls(text: &str) -> Result<(), ContentError> {
    // C0 is U+0000..=U+001F, C1 is U+0080..=U+009F.
    let bad = |c: char| {
        matches!(c, '\u{0}'..='\u{1f}' | '\u{80}'..='\u{9f}') && !matches!(c, '\t' | '\n' | '\r')
    };
    if text.chars().any(bad) {
        Err(ContentError::ControlChar)
    } else {
        Ok(())
    }
}

/// Check a summary: at most [`MAX_SUMMARY_BYTES`] bytes, same control rule.
pub fn check_summary(summary: &str) -> Result<(), ContentError> {
    if summary.len() > MAX_SUMMARY_BYTES {
        return Err(ContentError::SummaryTooLong);
    }
    check_controls(summary)
}
