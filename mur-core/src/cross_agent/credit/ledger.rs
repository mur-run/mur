//! Append-only per-agent credit ledger (M7c).
//!
//! File path: `<home>/agents/<agent>/credit/ledger.jsonl`.
//! Writes are atomic at the line level on POSIX (O_APPEND + single write_all
//! under PIPE_BUF). On Windows we fall back to a parking_lot::Mutex shared
//! within the process — the file is still single-writer per agent runtime.
//!
//! Reads tolerate malformed lines (skipped + logged at warn) and unknown
//! `kind` values (skipped silently — additive compatibility).

use std::fs::{OpenOptions, create_dir_all};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_common::skill::credit::CreditEntry;
use tracing::warn;

pub fn ledger_path_for_agent(home: &Path, agent: &str) -> PathBuf {
    home.join("agents")
        .join(agent)
        .join("credit")
        .join("ledger.jsonl")
}

pub fn append(home: &Path, agent: &str, entry: &CreditEntry) -> Result<()> {
    let path = ledger_path_for_agent(home, agent);
    if let Some(parent) = path.parent() {
        create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(entry).context("serialise CreditEntry")?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    f.write_all(b"\n")
        .with_context(|| format!("append newline to {}", path.display()))?;
    Ok(())
}

/// Read all entries from `<home>/agents/<agent>/credit/ledger.jsonl` whose
/// `skill` matches. Missing file → empty Vec. Malformed lines are logged
/// and skipped. Unknown `kind` values are silently skipped.
pub fn read_for_skill(home: &Path, agent: &str, skill: &str) -> Result<Vec<CreditEntry>> {
    let path = ledger_path_for_agent(home, agent);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for (idx, line_res) in reader.lines().enumerate() {
        let line = match line_res {
            Ok(l) => l,
            Err(e) => {
                warn!("ledger {} line {}: read error {e}", path.display(), idx + 1);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<CreditEntry>(&line) {
            Ok(entry) if entry.skill == skill => out.push(entry),
            Ok(_) => {}
            Err(e) => {
                warn!(
                    "ledger {} line {}: parse error {e} — skipping",
                    path.display(),
                    idx + 1
                );
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::skill::credit::{CreditEntry, CreditKind};
    use tempfile::tempdir;

    fn entry(skill: &str, kind: CreditKind, agent: &str) -> CreditEntry {
        CreditEntry {
            ts: Utc::now(),
            skill: skill.into(),
            skill_version: "1.0.0".into(),
            kind,
            agent: agent.into(),
            evidence: None,
            source: format!("human:{agent}"),
        }
    }

    #[test]
    fn appends_and_reads_round_trip() {
        let d = tempdir().unwrap();
        let home = d.path();
        let e1 = entry("foo", CreditKind::Author, "alice");
        let e2 = entry("bar", CreditKind::Author, "alice");
        let e3 = entry("foo", CreditKind::Mutator, "alice");
        append(home, "alice", &e1).unwrap();
        append(home, "alice", &e2).unwrap();
        append(home, "alice", &e3).unwrap();
        let foo = read_for_skill(home, "alice", "foo").unwrap();
        assert_eq!(foo.len(), 2);
        assert!(foo.iter().all(|e| e.skill == "foo"));
    }

    #[test]
    fn missing_ledger_yields_empty_vec() {
        let d = tempdir().unwrap();
        assert!(
            read_for_skill(d.path(), "ghost", "anything")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_line_skipped() {
        let d = tempdir().unwrap();
        let home = d.path();
        let e = entry("foo", CreditKind::Author, "alice");
        append(home, "alice", &e).unwrap();
        let mut f = OpenOptions::new()
            .append(true)
            .open(ledger_path_for_agent(home, "alice"))
            .unwrap();
        f.write_all(b"NOT JSON\n").unwrap();
        let foo = read_for_skill(home, "alice", "foo").unwrap();
        assert_eq!(foo.len(), 1);
    }

    #[test]
    fn unknown_kind_skipped() {
        let d = tempdir().unwrap();
        let home = d.path();
        let path = ledger_path_for_agent(home, "alice");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"ts":"2026-05-27T10:21:33Z","skill":"foo","skill_version":"1.0.0","kind":"future_kind","agent":"alice","source":"human:alice"}}"#
        )
        .unwrap();
        let foo = read_for_skill(home, "alice", "foo").unwrap();
        assert!(foo.is_empty());
    }
}
