//! Recent ssh-agent proxy refusals, shared by the runtime (writer) and
//! `mur agent status` (reader).
//!
//! The ssh-agent protocol has no error text: every refusal reaches the ssh
//! client as a bare `SSH_AGENT_FAILURE`. This file is where the user finds
//! the actual reason (spec §"Refusals must be diagnosable outside the ssh
//! client").

use std::io;
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

/// Prepend `r`, keep [`RECENT_REFUSALS`], write atomically (temp + rename).
pub fn record(agent_home: &Path, r: SshRefusal) -> io::Result<()> {
    let mut all = read(agent_home);
    all.insert(0, r);
    all.truncate(RECENT_REFUSALS);
    let path = refusals_path(agent_home);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&all).map_err(io::Error::other)?,
    )?;
    std::fs::rename(&tmp, &path)
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

    #[test]
    fn garbled_file_reads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(refusals_path(tmp.path()), "[nope").unwrap();
        assert!(read(tmp.path()).is_empty());
    }
}
