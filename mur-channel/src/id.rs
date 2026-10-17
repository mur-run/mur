//! The one channel-id safety check (#1936).
//!
//! A channel id names a directory under `channels/` and a file under each
//! `*-heads/` directory, so an id that is empty, `.`, `..`, or carries a path
//! separator, NUL, or `:` could point outside them. Every place that turns an
//! id into a path goes through [`check_channel_id`]; nothing re-derives the
//! rule.

use anyhow::{Result, bail};

/// Characters that may never appear in a channel id: both path separators
/// (Windows accepts `\` as one), NUL, which truncates a C path, and `:`.
/// On Windows a `:` makes `C:x` a drive prefix — `Path::join` then replaces
/// the base instead of appending to it — or `c1:x` an NTFS alternate data
/// stream. Refused on every platform so the rule does not depend on where it
/// runs; generated ids (UUIDs, `fleet-<name>`) never carry one.
const FORBIDDEN_CHARS: [char; 4] = ['/', '\\', '\0', ':'];

/// Whether `id` can safely be joined onto a directory as one path component.
pub fn is_safe_channel_id(id: &str) -> bool {
    !(id.is_empty() || id == "." || id == ".." || id.contains(FORBIDDEN_CHARS))
}

/// [`is_safe_channel_id`] as an error, for callers that propagate with `?`.
pub fn check_channel_id(id: &str) -> Result<()> {
    if !is_safe_channel_id(id) {
        bail!("not a valid channel id: {id:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_ids_are_safe() {
        for id in ["c1", "fleet-x", "dm.alice.bob", "a..b", "..x", "x..", "é"] {
            assert!(is_safe_channel_id(id), "{id:?} should be safe");
        }
    }

    #[test]
    fn ids_that_could_leave_the_directory_are_refused() {
        for id in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "..\\x",
            "a\0b",
            "/abs",
            // Windows drive prefixes: `join` replaces the base path with them.
            "C:",
            "C:escape",
            "c:x",
            // NTFS alternate data stream on an otherwise ordinary name.
            "c1:stream",
        ] {
            assert!(!is_safe_channel_id(id), "{id:?} should be refused");
            assert!(check_channel_id(id).is_err());
        }
    }
}
