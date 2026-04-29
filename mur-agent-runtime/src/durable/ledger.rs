//! Append-only JSONL ledger with per-day file rotation.
//!
//! Per Spec §4.10: each `append` writes one JSON line to `<base>/<YYYY-MM-DD>.jsonl`,
//! debounced fsync (≤1s coalesce) to bound R14 data-loss risk, and `scan_days` reads
//! the last N daily files in chronological order, skipping malformed lines.

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub struct Ledger {
    base_dir: PathBuf,
    last_fsync: Instant,
}

impl Ledger {
    /// Open (or create) a ledger directory.
    pub fn open(base_dir: &Path) -> Result<Self> {
        fs::create_dir_all(base_dir).context("create ledger dir")?;
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            last_fsync: Instant::now(),
        })
    }

    /// Append one event to today's JSONL file. Debounced fsync: only flushes to
    /// disk if more than 1s has elapsed since the last fsync.
    pub fn append<E: Serialize>(&mut self, event: &E) -> Result<()> {
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let path = self.base_dir.join(format!("{today}.jsonl"));
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open ledger {}", path.display()))?;
        let line = serde_json::to_string(event).context("serialize event")?;
        writeln!(f, "{line}").context("write event")?;

        if self.last_fsync.elapsed() > Duration::from_secs(1) {
            f.sync_data().context("fsync ledger")?;
            self.last_fsync = Instant::now();
        }
        Ok(())
    }

    /// Return the directory this ledger writes to.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Force fsync today's file. Used at shutdown / from tests.
    pub fn flush(&mut self) -> Result<()> {
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let path = self.base_dir.join(format!("{today}.jsonl"));
        if path.exists() {
            let f = OpenOptions::new()
                .write(true)
                .open(&path)
                .with_context(|| format!("open for fsync {}", path.display()))?;
            f.sync_data().context("fsync ledger")?;
        }
        self.last_fsync = Instant::now();
        Ok(())
    }

    /// Scan the most recent `days` daily files (today, today-1, …) in chronological
    /// order (oldest first). Skips malformed lines (logged via `tracing::warn!`),
    /// missing files, and missing base_dir.
    pub fn scan_days<E: DeserializeOwned>(base_dir: &Path, days: u32) -> Vec<Result<E>> {
        let mut out = Vec::new();
        if !base_dir.exists() {
            return out;
        }
        let today = Local::now().date_naive();
        let mut dates: Vec<_> = (0..days as i64)
            .map(|i| today - chrono::Duration::days(i))
            .collect();
        dates.sort(); // chronological (oldest first)
        for date in dates {
            let p = base_dir.join(format!("{}.jsonl", date.format("%Y-%m-%d")));
            if !p.exists() {
                continue;
            }
            let f = match std::fs::File::open(&p) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for (i, line) in BufReader::new(f).lines().enumerate() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<E>(&line) {
                    Ok(e) => out.push(Ok(e)),
                    Err(err) => {
                        tracing::warn!(
                            "ledger {}:{} skip malformed line: {err}",
                            p.display(),
                            i + 1
                        );
                    }
                }
            }
        }
        out
    }
}
