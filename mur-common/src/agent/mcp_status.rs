//! What a running agent got from each MCP server at boot.
//!
//! Tool discovery (`tools/list`) that fails is not fatal: the runtime logs a
//! warning to its own stderr and serves the agent without that server's tools.
//! Nothing outside the process could see that, so `mur agent doctor` reported
//! a healthy agent whose MCP tools had silently vanished. The runtime now
//! records each server's outcome here; `doctor` reads it back.

use serde::{Deserialize, Serialize};

/// File in `agent_home` where a running agent records MCP discovery results.
/// Written by the runtime (agent_home stays writable), read by `doctor`.
pub const MCP_STATUS_FILE: &str = "mcp_status.json";

/// Outcome of one server's `tools/list` at boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerStatus {
    pub name: String,
    /// Tools the server listed (before per-tool deny rules). Zero on failure.
    pub tools: usize,
    /// Why discovery failed; `None` when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Every enabled server's discovery outcome for the current boot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpStatus {
    /// PID of the runtime that wrote this. A reader compares it with
    /// `running.lock`'s `pid` to tell this boot from a stale previous one.
    #[serde(default)]
    pub pid: u32,
    pub servers: Vec<McpServerStatus>,
}

impl McpStatus {
    /// Servers whose discovery failed.
    pub fn failed(&self) -> impl Iterator<Item = &McpServerStatus> {
        self.servers.iter().filter(|s| s.error.is_some())
    }

    /// Atomic write: temp file + rename.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// `None` when absent or unreadable — a runtime that predates this file.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_reports_only_failures() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(MCP_STATUS_FILE);
        let s = McpStatus {
            pid: 42,
            servers: vec![
                McpServerStatus {
                    name: "ok".into(),
                    tools: 3,
                    error: None,
                },
                McpServerStatus {
                    name: "bad".into(),
                    tools: 0,
                    error: Some("io: Operation not permitted".into()),
                },
            ],
        };
        s.save(&p).unwrap();
        let back = McpStatus::load(&p).unwrap();
        assert_eq!(back, s);
        let failed: Vec<_> = back.failed().map(|f| f.name.as_str()).collect();
        assert_eq!(failed, ["bad"]);
    }

    #[test]
    fn missing_file_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert!(McpStatus::load(&d.path().join(MCP_STATUS_FILE)).is_none());
    }
}
