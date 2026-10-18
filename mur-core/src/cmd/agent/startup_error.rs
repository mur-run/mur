//! The runtime's own fatal line, quoted back to whoever started it (#1979).
//!
//! A runtime that dies during startup writes `Error: …` to its stderr log and
//! never writes a `running.lock`. `mur agent start` / `restart` used to answer
//! that with only "not confirmed running", which hid the one line that names
//! the cause (a stale MCP entry, a pin drift) in a log nobody is told about.
//!
//! The logs are append-forever, so a plain tail would happily quote a crash
//! from last week. [`LogMark`] records the log length *before* the start, and
//! only lines written after it count.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// How much freshly written log to scan. The fatal line is the last thing a
/// dying runtime prints; a crash loop repeats it, so the newest copy is enough.
const SCAN_BYTES: u64 = 16_384;

/// A position in a stderr log, taken before a start attempt.
pub(super) struct LogMark {
    path: PathBuf,
    offset: u64,
}

impl LogMark {
    /// Mark the current end of `path` (0 when it does not exist yet).
    pub(super) fn at_end(path: PathBuf) -> Self {
        let offset = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { path, offset }
    }

    /// The last `Error:` line written since the mark, if any.
    pub(super) fn error_since(&self) -> Option<String> {
        let mut f = fs::File::open(&self.path).ok()?;
        let len = f.metadata().ok()?.len();
        // A shrunk file was rotated/truncated: everything in it is new.
        let start = if len < self.offset { 0 } else { self.offset };
        let start = start.max(len.saturating_sub(SCAN_BYTES));
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut raw = Vec::new();
        f.read_to_end(&mut raw).ok()?;
        last_error_line(&String::from_utf8_lossy(&raw))
    }

    /// Two indented lines for a failure message: the error and where the
    /// rest of the story is. `None` when nothing new failed.
    pub(super) fn report(&self) -> Option<String> {
        let line = self.error_since()?;
        Some(format!(
            "\n    {line}\n    full log: tail {}",
            self.path.display()
        ))
    }
}

/// The stderr log a start of `name` writes to: the launchd unit's file on
/// macOS when a service is installed, otherwise the agent's own
/// `stderr.log`. `None` for a systemd unit — its stderr goes to the journal.
pub(super) fn startup_log(name: &str, agent_home: &Path, has_service: bool) -> Option<PathBuf> {
    if !has_service {
        return Some(agent_home.join("stderr.log"));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = agent_home;
        Some(super::service::service_stderr_log(name))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        None
    }
}

/// The last hard `Error:` line in a log tail, stripped of ANSI colour codes.
///
/// Matches the runtime's own fatal shape (`anyhow` prints `Error: …` on exit),
/// not `WARN`/`ERROR`-level tracing lines — a sealed agent logs several
/// warnings on every healthy start, and reporting one of those as the cause
/// would send the user after the wrong thing.
pub(super) fn last_error_line(tail: &str) -> Option<String> {
    tail.lines()
        .map(strip_ansi)
        .rfind(|l| l.trim_start().starts_with("Error:"))
        .map(|l| l.trim().to_string())
}

/// Drop ANSI escape sequences — the runtime writes coloured output and the
/// service log keeps the escapes verbatim.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI sequence: ESC '[' … final byte in @-~
        for c in chars.by_ref() {
            if ('@'..='~').contains(&c) && c != '[' {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(p: &Path, s: &str) {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap()
            .write_all(s.as_bytes())
            .unwrap();
    }

    /// The #1979 shape: the agent dies on a stale serena entry and the
    /// start must quote that line, not just "not confirmed running".
    #[test]
    fn quotes_the_error_written_after_the_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("err.log");
        let mark = LogMark::at_end(log.clone());
        append(
            &log,
            "INFO starting\n\u{1b}[31mError\u{1b}[0m: MCP server `cjps-temp` (kind: serena): `project` is gone\n",
        );
        let report = mark.report().expect("fresh crash must be reported");
        assert!(report.contains("Error: MCP server `cjps-temp`"), "{report}");
        assert!(report.contains(log.to_string_lossy().as_ref()), "{report}");
    }

    /// An old crash already in the append-forever log is not this start's
    /// cause. Quoting it would send the user after a fixed problem.
    #[test]
    fn ignores_errors_from_before_the_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("err.log");
        append(&log, "Error: last week's pin drift\n");
        let mark = LogMark::at_end(log.clone());
        append(&log, "INFO started\nWARN sandbox note\n");
        assert!(mark.error_since().is_none());
    }

    #[test]
    fn missing_log_and_quiet_log_report_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("never-written.log");
        let mark = LogMark::at_end(log.clone());
        assert!(mark.report().is_none());
        append(&log, "INFO started\n");
        assert!(mark.report().is_none());
    }

    /// A rotated (shrunk) log: everything in the new file is new.
    #[test]
    fn truncated_log_is_read_from_the_start() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("err.log");
        append(&log, &"x".repeat(1000));
        let mark = LogMark::at_end(log.clone());
        fs::write(&log, "Error: after rotation\n").unwrap();
        assert_eq!(mark.error_since().as_deref(), Some("Error: after rotation"));
    }

    #[test]
    fn last_error_line_skips_tracing_error_level() {
        assert_eq!(
            last_error_line("ERROR mcp: handshake slow\nError: real cause\nINFO bye\n").as_deref(),
            Some("Error: real cause")
        );
        assert!(last_error_line("ERROR mcp: only tracing\n").is_none());
    }
}
