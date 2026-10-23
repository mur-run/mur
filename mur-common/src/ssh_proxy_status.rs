//! What the runtime concluded about the ssh-agent proxy at its last start.
//!
//! The runtime classifies every start (boundary vs audit-only, see
//! `mur-agent-runtime/src/sandbox/ssh_bypass.rs`) and writes the result to
//! `<agent_home>/`[`STATUS_FILE`]; `mur agent status` reads it. Both sides
//! share this one definition so the file name and shape cannot drift.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File under the agent's home holding the last classification.
pub const STATUS_FILE: &str = "ssh_proxy.json";

/// The three things MUR may say about the proxy for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshProxyState {
    /// The agent holds no `entitlements.ssh` keys: there is nothing to claim.
    Off,
    /// The agent's own sandbox refuses `connect(2)` to the upstream socket.
    Boundary,
    /// The agent can reach the upstream itself; the proxy only binds an agent
    /// that cooperates.
    AuditOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProxyStatus {
    pub state: SshProxyState,
    /// Stable reason code (`AuditReason::as_str`) for `AuditOnly`, else `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// One line a human can act on.
    pub explanation: String,
    /// When the runtime classified, as unix milliseconds.
    pub at_unix_ms: u64,
}

/// Where the status for the agent living at `agent_home` is stored.
pub fn status_path(agent_home: &Path) -> PathBuf {
    agent_home.join(STATUS_FILE)
}

/// Persist `status` (temp file + rename, so a reader never sees half of it).
pub fn write(agent_home: &Path, status: &SshProxyStatus) -> io::Result<()> {
    let path = status_path(agent_home);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(status).map_err(io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// The recorded status, or `None` when absent or unreadable. A garbled file
/// reads as absent: `status` then says nothing rather than something false.
pub fn read(agent_home: &Path) -> Option<SshProxyStatus> {
    let bytes = std::fs::read(status_path(agent_home)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl SshProxyStatus {
    /// `off`, `boundary`, or `audit-only (<code>)`. The one rendering of the
    /// value, shared by `mur agent status` and the start notice on the
    /// agent's channel so the two can never word it differently.
    pub fn label(&self) -> String {
        match self.state {
            SshProxyState::Off => "off".to_string(),
            SshProxyState::Boundary => "boundary".to_string(),
            SshProxyState::AuditOnly => {
                format!("audit-only ({})", self.code.as_deref().unwrap_or("unknown"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_each_state() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read(tmp.path()).is_none());
        for (state, code) in [
            (SshProxyState::Off, None),
            (SshProxyState::Boundary, None),
            (
                SshProxyState::AuditOnly,
                Some("probe_reachable".to_string()),
            ),
        ] {
            let s = SshProxyStatus {
                state,
                code,
                explanation: "why".into(),
                at_unix_ms: 9,
            };
            write(tmp.path(), &s).unwrap();
            assert_eq!(read(tmp.path()), Some(s));
        }
    }

    #[test]
    fn garbled_file_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(status_path(tmp.path()), "{nope").unwrap();
        assert!(read(tmp.path()).is_none());
    }
}
