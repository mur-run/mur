//! B0 M10 — redacted crashlog writer.
//!
//! Installs a `std::panic::set_hook` so any panic in the runtime task
//! (or any tokio task it spawns) lands in
//! `<agent_home>/crashlogs/<RFC3339-utc>.log` with the M8.1 redactor
//! applied. The previous panic hook is still chained so the user
//! sees the unredacted panic on stderr (their terminal); only the
//! on-disk record is scrubbed.
//!
//! Closes the gap acknowledged in
//! `docs/release/privacy-statement.md` §4: prior to M10, the privacy
//! statement said the redactor in §3.2 (telemetry) did NOT apply to
//! crashlogs. Now it does.

use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::hooks::b0_helpers::{redact_home_path, redact_secrets};

/// Install the panic hook. Idempotent: calling more than once is a
/// no-op (later registrations would otherwise wrap an arbitrary
/// number of times and double-write each panic).
pub fn install_panic_hook(agent_home: PathBuf) {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let _first = INSTALLED.get_or_init(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
            // Best-effort write — never propagate IO errors out of
            // the panic hook itself (would mask the real panic).
            if let Err(e) = write_crashlog_from_panic(&agent_home, info) {
                eprintln!("warn[crashlog]: failed to write redacted crashlog: {e}");
            }
            // Chain to the previous hook so the user still sees the
            // unredacted panic on stderr (their terminal). Local
            // visibility, not on-disk persistence.
            prev(info);
        }));
    });
}

/// Write a redacted crashlog from a real `PanicHookInfo`. Used by the
/// installed hook; tests use `write_crashlog` directly with raw
/// string inputs to avoid racing on the global panic hook.
fn write_crashlog_from_panic(
    agent_home: &Path,
    info: &PanicHookInfo<'_>,
) -> std::io::Result<PathBuf> {
    let payload = panic_payload_str(info);
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    write_crashlog(agent_home, &payload, location.as_deref(), &backtrace)
}

/// Format `payload` + `location` + `backtrace` into a redacted
/// crashlog body and write to `<agent_home>/crashlogs/<ts>.log`.
/// Public for tests so they don't have to fake a `PanicHookInfo`.
pub fn write_crashlog(
    agent_home: &Path,
    payload: &str,
    location: Option<&str>,
    backtrace: &str,
) -> std::io::Result<PathBuf> {
    let dir = agent_home.join("crashlogs");
    std::fs::create_dir_all(&dir)?;

    // RFC3339 with milliseconds + filesystem-safe colons replaced.
    // Example: 2026-05-06T08-15-32.123Z.log
    let ts = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .replace(':', "-");
    // PID suffix disambiguates rapid-fire panics (multiple panics
    // inside the same millisecond from the same process get unique
    // names — last-resort tie-break).
    let pid = std::process::id();
    let path = dir.join(format!("{ts}-{pid}.log"));

    let body = format_redacted_body(payload, location, backtrace);

    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    f.write_all(body.as_bytes())?;
    Ok(path)
}

/// Build the redacted panic body. Pure function — tests exercise the
/// format + redaction independently of the filesystem.
pub fn format_redacted_body(payload: &str, location: Option<&str>, backtrace: &str) -> String {
    let location = location.unwrap_or("<unknown>");
    let raw = format!(
        "panic at {location}\n\
         payload: {payload}\n\
         \n\
         backtrace:\n{backtrace}\n",
    );
    // Two-pass redaction: secrets first, then home paths. Order is
    // independent (no overlapping classes) but deterministic-stable
    // for test fixtures.
    let stage1 = redact_secrets(&raw);
    let stage2 = redact_home_path(&stage1);
    stage2.into_owned()
}

fn panic_payload_str(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_under_agent_home_crashlogs() {
        let dir = TempDir::new().unwrap();
        let path = write_crashlog(dir.path(), "boom", Some("foo.rs:1:1"), "<bt>").unwrap();
        assert!(path.starts_with(dir.path().join("crashlogs")));
        assert!(path.extension().is_some_and(|e| e == "log"));
        // File contents survived the round-trip.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("payload: boom"));
        assert!(body.contains("panic at foo.rs:1:1"));
    }

    #[test]
    fn redacts_secret_in_payload() {
        let body = format_redacted_body(
            "config error: sk-ant-abcdefghijklmnop-9999",
            Some("foo.rs:42:1"),
            "",
        );
        assert!(
            body.contains("[REDACTED:anthropic_key]"),
            "got body: {body}"
        );
        assert!(
            !body.contains("sk-ant-abcdefghijklmnop-9999"),
            "got body: {body}"
        );
    }

    #[test]
    fn redacts_home_path_in_payload() {
        let body = format_redacted_body("io error reading /Users/alice/.ssh/id_rsa", None, "");
        assert!(body.contains("~/.ssh/id_rsa"), "got body: {body}");
        assert!(!body.contains("/Users/alice/"), "got body: {body}");
    }

    #[test]
    fn redacts_home_path_in_backtrace() {
        let body = format_redacted_body(
            "thread 'x' panicked",
            None,
            "stack frame at /home/bob/projects/mur/src/lib.rs:42",
        );
        assert!(
            body.contains("~/projects/mur/src/lib.rs"),
            "got body: {body}"
        );
        assert!(!body.contains("/home/bob/"), "got body: {body}");
    }

    #[test]
    fn includes_location_and_backtrace_section() {
        let body = format_redacted_body("boom", Some("src/foo.rs:10:5"), "0: a\n1: b");
        assert!(
            body.contains("panic at src/foo.rs:10:5"),
            "got body: {body}"
        );
        assert!(body.contains("backtrace:\n0: a\n1: b"), "got body: {body}");
    }

    #[test]
    fn distinct_writes_get_distinct_filenames() {
        let dir = TempDir::new().unwrap();
        let p1 = write_crashlog(dir.path(), "first", None, "").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let p2 = write_crashlog(dir.path(), "second", None, "").unwrap();
        assert_ne!(p1, p2);
    }

    #[test]
    fn redacts_secret_and_path_together() {
        let body = format_redacted_body(
            "api_key=abcdefghij1234567890 saved to /home/bob/.env",
            None,
            "",
        );
        assert!(
            body.contains("[REDACTED:env_assignment]"),
            "got body: {body}",
        );
        assert!(body.contains("~/.env"), "got body: {body}");
    }

    /// Sanity check on the unknown-location fallback in
    /// `format_redacted_body` so a production hook with `location ==
    /// None` still produces a parseable header.
    #[test]
    fn missing_location_uses_unknown_marker() {
        let body = format_redacted_body("boom", None, "");
        assert!(body.contains("panic at <unknown>"), "got body: {body}");
    }
}
