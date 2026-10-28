use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
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
            let p = day_path(base_dir, date);
            if p.exists() {
                let _ = read_day(&p, |e| out.push(Ok(e)));
            }
        }
        out
    }

    /// Scan every daily file dated `since` or later (all of them when `None`),
    /// oldest first, tagging each event with its file's day.
    ///
    /// Unlike [`Self::scan_days`] the window is not tied to today, so a reader
    /// that must not lose old records (trash expiry, #2174) can resume from a
    /// day it chose. Files not named `YYYY-MM-DD.jsonl` and malformed lines are
    /// skipped. Read failures are NOT silent: the result says which days could
    /// not be read in full and whether the directory listing itself failed, so
    /// a caller that checkpoints progress can tell a complete replay from one
    /// that missed records (review #2176 P2).
    pub fn scan_since(base_dir: &Path, since: Option<NaiveDate>) -> DayScan<E> {
        let mut scan = DayScan {
            events: Vec::new(),
            unreadable_days: Vec::new(),
            malformed_days: Vec::new(),
            listing_failed: false,
        };
        let dir = match fs::read_dir(base_dir) {
            Ok(dir) => dir,
            // No ledger yet: an empty but complete replay.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return scan,
            Err(e) => {
                tracing::warn!("ledger {} not listable: {e}", base_dir.display());
                scan.listing_failed = true;
                return scan;
            }
        };
        let mut days = Vec::new();
        for item in dir {
            let Ok(item) = item else {
                scan.listing_failed = true;
                continue;
            };
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            let Some(day) = name
                .strip_suffix(".jsonl")
                .and_then(|stem| NaiveDate::parse_from_str(stem, DAY_FORMAT).ok())
            else {
                continue;
            };
            if since.is_none_or(|s| day >= s) {
                days.push(day);
            }
        }
        days.sort();
        for day in days {
            let read = read_day(&day_path(base_dir, day), |e| scan.events.push((day, e)));
            if !read.complete {
                scan.unreadable_days.push(day);
            }
            if read.malformed {
                scan.malformed_days.push(day);
            }
        }
        scan
    }
}

/// Result of [`Ledger::scan_since`].
#[derive(Debug)]
pub struct DayScan<E> {
    /// Every well-formed event read, oldest day first, with its file's day.
    pub events: Vec<(NaiveDate, E)>,
    /// Days whose file could not be opened or read to the end; their records
    /// may be missing from `events`.
    pub unreadable_days: Vec<NaiveDate>,
    /// Days with at least one line that did not parse; that record is lost,
    /// so a reader that must account for every record cannot trust the sum.
    pub malformed_days: Vec<NaiveDate>,
    /// The directory listing failed, so whole days may be missing.
    pub listing_failed: bool,
}

impl<E> DayScan<E> {
    /// True when every matching day file was read to the end.
    pub fn is_complete(&self) -> bool {
        !self.listing_failed && self.unreadable_days.is_empty()
    }

    /// [`Self::is_complete`] and no malformed line either: every record that
    /// was written is in `events`.
    pub fn is_exact(&self) -> bool {
        self.is_complete() && self.malformed_days.is_empty()
    }
}

/// What reading one day file found.
struct DayRead {
    /// The file opened and every line was read (I/O-wise).
    complete: bool,
    /// At least one line did not parse.
    malformed: bool,
}

const DAY_FORMAT: &str = "%Y-%m-%d";

fn day_path(base_dir: &Path, day: NaiveDate) -> PathBuf {
    base_dir.join(format!("{}.jsonl", day.format(DAY_FORMAT)))
}

/// Feed every well-formed event in one day file to `push`, reporting whether
/// the file was read in full and whether any line failed to parse.
fn read_day<E: DeserializeOwned>(p: &Path, mut push: impl FnMut(E)) -> DayRead {
    let f = match fs::File::open(p) {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!("ledger {} not readable: {err}", p.display());
            return DayRead {
                complete: false,
                malformed: false,
            };
        }
    };
    let mut complete = true;
    let mut malformed = false;
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!("ledger {}:{} read failed: {err}", p.display(), i + 1);
                complete = false;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<E>(&line) {
            Ok(e) => push(e),
            Err(err) => {
                malformed = true;
                tracing::warn!(
                    "ledger {}:{} skip malformed line: {err}",
                    p.display(),
                    i + 1
                );
            }
        }
    }
    DayRead {
        complete,
        malformed,
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

    #[test]
    fn scan_since_reads_old_files_and_respects_the_start_day() {
        let tmp = TempDir::new().unwrap();
        let today = Local::now().date_naive();
        for (age, n) in [(400i64, 1u32), (40, 2), (0, 3)] {
            let day = today - chrono::Duration::days(age);
            std::fs::write(
                day_path(tmp.path(), day),
                format!("{{\"msg\":\"m\",\"n\":{n}}}\n"),
            )
            .unwrap();
        }
        std::fs::write(tmp.path().join("notes.jsonl"), "garbage\n").unwrap();

        let all = Ledger::<TestEvent>::scan_since(tmp.path(), None);
        assert!(all.is_complete());
        let all = all.events;
        let ns: Vec<u32> = all.iter().map(|(_, e)| e.n).collect();
        assert_eq!(ns, vec![1, 2, 3], "every day file, oldest first");
        assert_eq!(all[0].0, today - chrono::Duration::days(400));

        let since = today - chrono::Duration::days(40);
        let recent = Ledger::<TestEvent>::scan_since(tmp.path(), Some(since)).events;
        let ns: Vec<u32> = recent.iter().map(|(_, e)| e.n).collect();
        assert_eq!(ns, vec![2, 3], "start day inclusive");
    }

    #[test]
    fn scan_since_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let scan = Ledger::<TestEvent>::scan_since(&tmp.path().join("nope"), None);
        assert!(scan.events.is_empty());
        assert!(scan.is_complete(), "no ledger yet is complete, not failed");
    }

    #[cfg(unix)]
    #[test]
    fn scan_since_reports_an_unreadable_day() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let day = Local::now().date_naive() - chrono::Duration::days(100);
        let p = day_path(tmp.path(), day);
        std::fs::write(&p, "{\"msg\":\"m\",\"n\":1}\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        let scan = Ledger::<TestEvent>::scan_since(tmp.path(), None);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        if scan.events.len() == 1 {
            return; // running as root: permissions do not block reads
        }
        assert!(!scan.is_complete());
        assert_eq!(scan.unreadable_days, vec![day]);
    }

    #[test]
    fn scan_since_reports_a_malformed_line_without_calling_the_day_unreadable() {
        let tmp = TempDir::new().unwrap();
        let day = Local::now().date_naive();
        std::fs::write(
            day_path(tmp.path(), day),
            "{\"msg\":\"m\",\"n\":1}\ngarbage\n",
        )
        .unwrap();
        let scan = Ledger::<TestEvent>::scan_since(tmp.path(), None);
        assert_eq!(scan.events.len(), 1);
        assert!(scan.is_complete(), "a parse error is not an I/O failure");
        assert!(!scan.is_exact());
        assert_eq!(scan.malformed_days, vec![day]);
    }
}
