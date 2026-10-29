//! Recent ssh-agent proxy refusals, shared by the runtime (writer) and
//! `mur agent status` (reader).
//!
//! The ssh-agent protocol has no error text: every refusal reaches the ssh
//! client as a bare `SSH_AGENT_FAILURE`. This file is where the user finds
//! the actual reason (spec §"Refusals must be diagnosable outside the ssh
//! client").

use std::io::{self, Write};

use fs2::FileExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File under the agent's home holding the most recent refusals.
pub const REFUSALS_FILE: &str = "ssh_proxy_refusals.json";

/// How many refusals are kept and shown by `mur agent status`.
pub const RECENT_REFUSALS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshRefusal {
    /// Stable reason code (`RefusalCode::as_str` in the runtime).
    pub code: String,
    /// The full human message, as logged and posted to the channel.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_fp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key_fp: Option<String>,
    pub at_unix_ms: u64,
}

pub fn refusals_path(agent_home: &Path) -> PathBuf {
    agent_home.join(REFUSALS_FILE)
}

/// Most recent first. Absent or garbled reads as empty.
pub fn read(agent_home: &Path) -> Vec<SshRefusal> {
    std::fs::read(refusals_path(agent_home))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Prepend `r`, keep [`RECENT_REFUSALS`], and atomically replace the file.
/// A stable sidecar lock serializes the entire read-modify-write across threads
/// and processes. Never remove the lock file: waiters may hold its inode open.
pub fn record(agent_home: &Path, r: SshRefusal) -> io::Result<()> {
    let path = refusals_path(agent_home);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("json.lock"))?;
    lock.lock_exclusive()?;
    // Closing `lock` releases it on every return path, including errors.
    let mut all = read(agent_home);
    all.insert(0, r);
    all.truncate(RECENT_REFUSALS);
    let mut tmp = tempfile::NamedTempFile::new_in(agent_home)?;
    tmp.write_all(&serde_json::to_vec_pretty(&all).map_err(io::Error::other)?)?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(i: u64) -> SshRefusal {
        SshRefusal {
            code: "key_not_allowed".into(),
            message: format!("m{i}"),
            key_fp: None,
            host_key_fp: None,
            at_unix_ms: i,
        }
    }

    #[test]
    fn keeps_the_newest_n_most_recent_first() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read(tmp.path()).is_empty());
        for i in 0..(RECENT_REFUSALS as u64 + 2) {
            record(tmp.path(), r(i)).unwrap();
        }
        let got = read(tmp.path());
        assert_eq!(got.len(), RECENT_REFUSALS);
        assert_eq!(got[0].at_unix_ms, RECENT_REFUSALS as u64 + 1);
    }

    /// Concurrent writers must not lose each other's records or trip over a
    /// shared temp file (each `record` is its own read-modify-rename).
    #[test]
    fn concurrent_records_are_all_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let n = RECENT_REFUSALS as u64;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(n as usize));
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let (home, barrier) = (home.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    record(&home, r(i))
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let mut got: Vec<u64> = read(&home).iter().map(|x| x.at_unix_ms).collect();
        got.sort_unstable();
        assert_eq!(got, (0..n).collect::<Vec<_>>());
        let leftovers: Vec<_> = std::fs::read_dir(&home)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn garbled_file_reads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(refusals_path(tmp.path()), "[nope").unwrap();
        assert!(read(tmp.path()).is_empty());
    }
}
