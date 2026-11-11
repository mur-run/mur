//! K6 (SSH setup journey): when murmur hands the terminal to a `!mur …`
//! command and it fails, show why, not only `exited with exit status: 2`.
//!
//! A file handshake, not a pipe: piping the child's stderr would make
//! `stderr().is_terminal()` false, and `Consent::detect` would then refuse
//! the attended commands (`mur agent ssh allow`) the hand-over exists for.
//! murmur creates the file and names it in [`ENV`]; a failing `mur` process
//! overwrites it. The writer never creates a file, so the variable cannot be
//! used to make `mur` drop files in arbitrary places.

use std::io::{Read, Write};
use std::path::Path;

/// Names the file a failing `mur` writes its error into.
pub const ENV: &str = "MUR_HANDOVER_ERROR_FILE";
/// Most bytes written, and read back. The end of an error is the useful part.
pub const MAX_BYTES: usize = 16 * 1024;
/// Lines murmur shows under the "exited with" line.
pub const SHOWN_LINES: usize = 6;

/// Record `e` (with its whole context chain) if murmur asked for it.
pub fn record(e: &anyhow::Error) {
    record_text(&format!("{e:#}"));
}

/// Record `text` if [`ENV`] names a file. Best-effort and silent: this runs
/// on the way out of a failed command, and the error is printed anyway.
pub fn record_text(text: &str) {
    if let Some(p) = std::env::var_os(ENV) {
        record_to(Path::new(&p), text);
    }
}

fn record_to(path: &Path, text: &str) {
    let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
    else {
        return;
    };
    let _ = f.write_all(tail(text, MAX_BYTES).as_bytes());
}

/// The last `max` bytes of `s`, moved forward to a char boundary.
fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// The last [`SHOWN_LINES`] non-blank lines of the file, or `None`.
pub fn last_lines(path: &Path) -> Option<String> {
    let mut buf = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BYTES as u64)
        .read_to_string(&mut buf)
        .ok()?;
    let lines: Vec<&str> = buf
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines[lines.len().saturating_sub(SHOWN_LINES)..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_into_a_file_that_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        record_to(&missing, "boom");
        assert!(
            !missing.exists(),
            "must never create a file it was pointed at"
        );

        let f = dir.path().join("err");
        std::fs::write(&f, "old content that is longer").unwrap();
        record_to(&f, "error: boom");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "error: boom");
    }

    #[test]
    fn keeps_only_the_tail_of_a_huge_error_on_a_char_boundary() {
        let s = format!("{}é{}", "x".repeat(MAX_BYTES), "y".repeat(10));
        let t = tail(&s, MAX_BYTES);
        assert!(t.len() <= MAX_BYTES);
        assert!(t.ends_with("yyyyyyyyyy"));
    }

    #[test]
    fn last_lines_skips_blanks_and_keeps_the_last_few() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("err");
        let body: String = (1..=10).map(|i| format!("line {i}\n\n")).collect();
        std::fs::write(&f, body).unwrap();
        let got = last_lines(&f).unwrap();
        assert_eq!(got.lines().count(), SHOWN_LINES);
        assert!(got.ends_with("line 10"));
        assert!(got.starts_with(&format!("line {}", 10 - SHOWN_LINES + 1)));
    }

    #[test]
    fn an_empty_or_missing_file_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("err");
        assert_eq!(last_lines(&f), None);
        std::fs::write(&f, "\n  \n").unwrap();
        assert_eq!(last_lines(&f), None);
    }
}
