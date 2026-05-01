//! Append-only provenance ledger for `telemetry/inputs.jsonl`.
//!
//! Atomic via flock; readers skip malformed lines so a half-written
//! entry from a crashed writer can't poison subsequent reads.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;

use super::ProvenanceEntry;

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
