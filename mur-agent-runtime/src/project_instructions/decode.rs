//! Decoding instruction files into text for the block (spec §5.2).

/// More replacement characters than this share of all chars means the file
/// is not UTF-8 at all (Big5, Latin-1) rather than UTF-8 with a stray byte.
const MAX_REPLACEMENT_PERCENT: usize = 10;

/// The file is binary or in an encoding the block cannot carry.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Unreadable;

/// In order: a NUL byte anywhere is binary; otherwise decode lossily and
/// refuse past [`MAX_REPLACEMENT_PERCENT`]; otherwise strip a leading BOM.
pub(super) fn decode_file(bytes: &[u8]) -> Result<String, Unreadable> {
    if bytes.contains(&0) {
        return Err(Unreadable);
    }
    let text = String::from_utf8_lossy(bytes);
    let chars = text.chars().count();
    let replaced = text.chars().filter(|&c| c == '\u{FFFD}').count();
    if replaced * 100 > chars * MAX_REPLACEMENT_PERCENT {
        return Err(Unreadable);
    }
    Ok(text.strip_prefix('\u{FEFF}').unwrap_or(&text).to_owned())
}
