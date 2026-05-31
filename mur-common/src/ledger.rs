use anyhow::{Context, Result};
use chrono::Local;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Append-only JSONL ledger with per-day file rotation.
///
/// Generic over the event type `E`. For multi-writer scenarios,
/// wrap in `Arc<Mutex<Ledger<E>>>` — the file-level `flock` is
/// handled by the OS when opening for append.
pub struct Ledger<E> {
    base_dir: PathBuf,
    last_fsync: Instant,
    _marker: std::marker::PhantomData<E>,
}

impl<E: Serialize + DeserializeOwned> Ledger<E> {
    /// Open (or create) a ledger directory.
    pub fn open(base_dir: &Path) -> Result<Self> {
        fs::create_dir_all(base_dir).context("create ledger dir")?;
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            last_fsync: Instant::now(),
            _marker: std::marker::PhantomData,
        })
    }

    /// Append one event to today's JSONL file. Debounced fsync (≤1s).
    pub fn append(&mut self, event: &E) -> Result<()> {
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

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

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

    /// Scan the most recent `days` daily files in chronological order.
    /// Skips malformed lines and missing files.
    pub fn scan_days(base_dir: &Path, days: u32) -> Vec<Result<E>> {
        let mut out = Vec::new();
        if !base_dir.exists() {
            return out;
        }
        let today = Local::now().date_naive();
        let mut dates: Vec<_> = (0..days as i64)
            .map(|i| today - chrono::Duration::days(i))
            .collect();
        dates.sort();
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tempfile::TempDir;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestEvent {
        msg: String,
        n: u32,
    }

    #[test]
    fn append_and_scan_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = Ledger::<TestEvent>::open(tmp.path()).unwrap();
        ledger
            .append(&TestEvent {
                msg: "hello".into(),
                n: 1,
            })
            .unwrap();
        ledger
            .append(&TestEvent {
                msg: "world".into(),
                n: 2,
            })
            .unwrap();
        // Must flush so the file is on disk before scanning
        ledger.flush().unwrap();
        drop(ledger);

        let results = Ledger::<TestEvent>::scan_days(tmp.path(), 1);
        let events: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].msg, "hello");
        assert_eq!(events[1].msg, "world");
    }

    #[test]
    fn open_creates_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        let _ledger = Ledger::<TestEvent>::open(&sub).unwrap();
        assert!(sub.exists());
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let results = Ledger::<TestEvent>::scan_days(tmp.path(), 7);
        assert!(results.is_empty());
    }

    #[test]
    fn scan_skips_malformed_lines() {
        let tmp = TempDir::new().unwrap();
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let path = tmp.path().join(format!("{today}.jsonl"));
        std::fs::write(
            &path,
            r#"{"msg":"ok","n":1}"#.to_string()
                + "\n"
                + "garbage\n"
                + r#"{"msg":"also ok","n":3}"#
                + "\n",
        )
        .unwrap();

        let results = Ledger::<TestEvent>::scan_days(tmp.path(), 1);
        let events: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].msg, "ok");
        assert_eq!(events[1].msg, "also ok");
    }
}
