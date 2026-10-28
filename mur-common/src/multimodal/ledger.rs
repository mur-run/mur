//! Append-only provenance ledger for `telemetry/inputs.jsonl`.
//!
//! Atomic via flock; readers skip malformed lines so a half-written
//! entry from a crashed writer can't poison subsequent reads.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::ProvenanceEntry;

/// Extension of the claim cursor kept beside the ledger.
const CURSOR_EXTENSION: &str = "cursor";

/// A missing or unreadable cursor means nothing has been claimed yet.
fn read_cursor(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Temp file + rename so a crash never leaves a torn offset.
fn write_cursor(path: &Path, offset: u64) -> Result<()> {
    let tmp = path.with_extension(format!("{CURSOR_EXTENSION}.tmp"));
    std::fs::write(&tmp, offset.to_string()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))
}

pub struct ProvenanceLedger {
    path: PathBuf,
}

impl ProvenanceLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Append one entry as a single JSON line. Atomic via flock.
    /// Creates the parent directory if missing.
    ///
    /// Note on the open mode: we deliberately use `write(true)` +
    /// explicit `seek(SeekFrom::End(0))` rather than `append(true)`.
    /// On Windows, `append(true)` requests `FILE_APPEND_DATA` only,
    /// which is *not* sufficient for `LockFileEx` (used internally by
    /// `fs2::FileExt::lock_exclusive`) — Windows demands `GENERIC_WRITE`
    /// access, otherwise the lock call fails with `ERROR_ACCESS_DENIED`.
    /// Manual seek-to-end gives us append semantics on every platform
    /// while keeping the flock contract intact.
    pub fn append(&self, entry: &ProvenanceEntry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        f.lock_exclusive().context("flock inputs.jsonl")?;
        f.seek(SeekFrom::End(0))
            .context("seek to end of inputs.jsonl")?;
        let line = serde_json::to_string(entry).context("serialize provenance")?;
        writeln!(f, "{line}").context("append provenance")?;
        f.unlock().context("unlock inputs.jsonl")?;
        Ok(())
    }

    /// Hand back every entry appended since the last `claim_new`, then
    /// move the cursor past them, so each attachment reaches exactly one
    /// turn.
    ///
    /// The cursor is a byte offset kept beside the ledger (see
    /// [`Self::cursor_path`]), not a turn id: bridges stage entries
    /// out-of-turn with `turn_id: 0`, and the runtime's turn counter
    /// restarts at 0 with the process, so neither number can say which
    /// entries a turn has already seen. The ledger itself stays
    /// append-only.
    ///
    /// Taken under the same flock as [`Self::append`]. Only complete
    /// lines are consumed; a trailing half-written line is left for the
    /// next call. A cursor past EOF (the ledger was rotated or truncated)
    /// restarts from the beginning. Malformed lines are skipped and
    /// consumed.
    pub fn claim_new(&self) -> Result<Vec<ProvenanceEntry>> {
        let mut f = match OpenOptions::new().read(true).write(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => {
                return Err(anyhow::Error::from(e).context(format!("open {}", self.path.display())));
            }
        };
        f.lock_exclusive().context("flock inputs.jsonl")?;
        let res = self.claim_locked(&mut f);
        f.unlock().context("unlock inputs.jsonl")?;
        res
    }

    fn claim_locked(&self, f: &mut std::fs::File) -> Result<Vec<ProvenanceEntry>> {
        let len = f.metadata().context("stat inputs.jsonl")?.len();
        let cursor_path = self.cursor_path();
        let mut start = read_cursor(&cursor_path);
        if start > len {
            start = 0;
        }
        f.seek(SeekFrom::Start(start))
            .context("seek inputs.jsonl")?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).context("read inputs.jsonl")?;
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
            return Ok(vec![]);
        };
        let complete = &buf[..=last_nl];
        let mut out = Vec::new();
        for line in complete.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            match serde_json::from_slice::<ProvenanceEntry>(line) {
                Ok(e) => out.push(e),
                Err(e) => tracing::warn!(
                    "ledger {}: skipping malformed line: {e}",
                    self.path.display()
                ),
            }
        }
        write_cursor(&cursor_path, start + complete.len() as u64)?;
        Ok(out)
    }

    /// `<ledger stem>.cursor` beside the ledger
    /// (`telemetry/inputs.jsonl` → `telemetry/inputs.cursor`).
    pub fn cursor_path(&self) -> PathBuf {
        self.path.with_extension(CURSOR_EXTENSION)
    }

    /// Read every entry whose `turn_id == turn`. Malformed lines are
    /// silently skipped (logged at warn level). Returns Ok(empty)
    /// when the file does not exist.
    pub fn read_turn(&self, turn: u64) -> Result<Vec<ProvenanceEntry>> {
        let f = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => {
                return Err(anyhow::Error::from(e).context(format!("open {}", self.path.display())));
            }
        };
        let r = BufReader::new(f);
        let mut out = Vec::new();
        for (i, line) in r.lines().enumerate() {
            let Ok(line) = line else { continue };
            match serde_json::from_str::<ProvenanceEntry>(&line) {
                Ok(e) if e.turn_id == turn => out.push(e),
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(
                        "ledger {}: skipping malformed line {}: {e}",
                        self.path.display(),
                        i + 1
                    );
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str, turn_id: u64) -> ProvenanceEntry {
        ProvenanceEntry {
            sha256: sha.into(),
            source: "user_drop".into(),
            decoder_version: "test/v1".into(),
            ocr_engine_version: None,
            turn_id,
            recorded_at: chrono::Utc::now(),
        }
    }

    fn shas(v: &[ProvenanceEntry]) -> Vec<&str> {
        v.iter().map(|e| e.sha256.as_str()).collect()
    }

    #[test]
    fn claim_new_returns_each_entry_once_whatever_its_turn_id() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = ProvenanceLedger::new(tmp.path().join("telemetry/inputs.jsonl"));
        assert!(ledger.claim_new().unwrap().is_empty(), "no ledger yet");

        // Bridges write turn_id 0; card import writes 1. Neither matters.
        ledger.append(&entry("a", 0)).unwrap();
        ledger.append(&entry("b", 1)).unwrap();
        assert_eq!(shas(&ledger.claim_new().unwrap()), ["a", "b"]);
        assert!(ledger.claim_new().unwrap().is_empty(), "already claimed");

        ledger.append(&entry("c", 0)).unwrap();
        assert_eq!(shas(&ledger.claim_new().unwrap()), ["c"]);
    }

    #[test]
    fn claim_new_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("telemetry/inputs.jsonl");
        ProvenanceLedger::new(&path)
            .append(&entry("old", 0))
            .unwrap();
        assert_eq!(
            shas(&ProvenanceLedger::new(&path).claim_new().unwrap()),
            ["old"]
        );

        // A fresh handle (new process, turn counter back at 0) must not
        // hand the old attachment to the next turn again.
        let reopened = ProvenanceLedger::new(&path);
        assert!(reopened.claim_new().unwrap().is_empty());
        reopened.append(&entry("new", 0)).unwrap();
        assert_eq!(shas(&reopened.claim_new().unwrap()), ["new"]);
    }

    #[test]
    fn claim_new_leaves_a_half_written_line_for_next_time() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("telemetry/inputs.jsonl");
        let ledger = ProvenanceLedger::new(&path);
        ledger.append(&entry("a", 0)).unwrap();
        let full = serde_json::to_string(&entry("b", 0)).unwrap();
        let (head, tail) = full.split_at(full.len() / 2);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(head.as_bytes())
            .unwrap();
        assert_eq!(shas(&ledger.claim_new().unwrap()), ["a"]);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{tail}").unwrap();
        assert_eq!(shas(&ledger.claim_new().unwrap()), ["b"]);
    }

    #[test]
    fn claim_new_skips_malformed_lines_and_moves_past_them() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("telemetry/inputs.jsonl");
        let ledger = ProvenanceLedger::new(&path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json\n").unwrap();
        ledger.append(&entry("a", 0)).unwrap();
        assert_eq!(shas(&ledger.claim_new().unwrap()), ["a"]);
        assert!(ledger.claim_new().unwrap().is_empty());
    }

    #[test]
    fn claim_new_rereads_from_the_start_when_the_ledger_shrank() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("telemetry/inputs.jsonl");
        let ledger = ProvenanceLedger::new(&path);
        ledger.append(&entry("a", 0)).unwrap();
        ledger.append(&entry("b", 0)).unwrap();
        ledger.claim_new().unwrap();

        // Rotated / truncated by an operator: the cursor now points past EOF.
        std::fs::remove_file(&path).unwrap();
        ledger.append(&entry("c", 0)).unwrap();
        assert_eq!(shas(&ledger.claim_new().unwrap()), ["c"]);
    }
}
