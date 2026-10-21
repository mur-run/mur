//! A runtime that refuses to start, and the record it leaves behind (#2015).
//!
//! The per-agent runtime deliberately refuses to start on a profile it cannot
//! trust (`entitlements_unpinned`, `entitlements_pin`, `profile_invalid`).
//! Exiting `1` and letting the service manager respawn it turned that refusal
//! into an endless crash loop with the reason buried in an append-forever log.
//!
//! A refusal now exits with [`EXIT_CODE`] — which the systemd unit lists in
//! `RestartPreventExitStatus=` — and, before exiting, writes a
//! [`StartRefusal`] to `<agent_home>/`[`MARKER_FILE`] so `mur agent status` /
//! `mur agent start` can say *why* without anyone reading a log.
//!
//! The runtime writes the marker; the CLI reads it and `perm reseal` clears it.
//! Both sides share this one definition so the file name, exit code, and shape
//! cannot drift.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Exit status of a runtime that refused to start. `78` is `EX_CONFIG`
/// (sysexits.h): "something was found in an unconfigured or misconfigured
/// state" — which is exactly a profile that failed its integrity check.
pub const EXIT_CODE: i32 = 78;

/// File under the agent's home holding the last refusal.
pub const MARKER_FILE: &str = "start_refused.json";

/// Why the last start was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRefusal {
    /// Machine-readable code, e.g. `entitlements_unpinned`.
    pub code: String,
    /// The full human-readable refusal, including the remedy.
    pub message: String,
    /// When it was refused, as unix milliseconds. Lets a `start` that began
    /// later tell this refusal from an older one it has since outlived.
    pub at_unix_ms: u64,
}

impl StartRefusal {
    /// Build a refusal from the runtime's `error[<code>]: <message>` line.
    /// A line without that shape keeps the whole text under `unknown`.
    pub fn from_line(line: &str, at_unix_ms: u64) -> Self {
        let (code, message) = parse_line(line);
        Self {
            code,
            message,
            at_unix_ms,
        }
    }

    /// `true` for refusals that `mur agent perm reseal` resolves.
    pub fn is_entitlements(&self) -> bool {
        self.code.starts_with("entitlements_")
    }
}

/// Split `error[code]: message` into `(code, message)`; the message is the
/// full original line so nothing the runtime said is lost.
fn parse_line(line: &str) -> (String, String) {
    let line = line.trim();
    let code = line
        .strip_prefix("error[")
        .and_then(|rest| rest.split_once(']'))
        .map(|(code, _)| code)
        .filter(|c| !c.is_empty())
        .unwrap_or("unknown");
    (code.to_string(), line.to_string())
}

/// Where the marker for the agent living at `agent_home` is stored.
pub fn marker_path(agent_home: &Path) -> PathBuf {
    agent_home.join(MARKER_FILE)
}

/// Persist `refusal` (temp file + rename, so a reader never sees half of it).
pub fn write(agent_home: &Path, refusal: &StartRefusal) -> io::Result<()> {
    let path = marker_path(agent_home);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(refusal).map_err(io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// The recorded refusal, or `None` when absent or unreadable. A garbled
/// marker is treated as absent: it must never make a healthy agent look
/// refused.
pub fn read(agent_home: &Path) -> Option<StartRefusal> {
    let bytes = std::fs::read(marker_path(agent_home)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Remove the marker. Returns whether one was there.
pub fn clear(agent_home: &Path) -> bool {
    std::fs::remove_file(marker_path(agent_home)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_code_out_of_the_runtime_line() {
        let r = StartRefusal::from_line(
            "error[entitlements_unpinned]: bob's profile.yaml entitlements changed",
            7,
        );
        assert_eq!(r.code, "entitlements_unpinned");
        assert!(r.message.starts_with("error[entitlements_unpinned]:"));
        assert!(r.is_entitlements());
        assert_eq!(r.at_unix_ms, 7);
    }

    #[test]
    fn unshaped_line_is_unknown_but_keeps_its_text() {
        let r = StartRefusal::from_line("something odd", 1);
        assert_eq!(r.code, "unknown");
        assert_eq!(r.message, "something odd");
        assert!(!r.is_entitlements());
    }

    #[test]
    fn round_trips_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read(tmp.path()).is_none());
        let r = StartRefusal::from_line("error[profile_invalid]: bad yaml", 42);
        write(tmp.path(), &r).unwrap();
        assert_eq!(read(tmp.path()), Some(r));
        assert!(clear(tmp.path()));
        assert!(read(tmp.path()).is_none());
        assert!(!clear(tmp.path()), "second clear finds nothing");
    }

    #[test]
    fn garbled_marker_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(marker_path(tmp.path()), "{not json").unwrap();
        assert!(read(tmp.path()).is_none());
    }
}
