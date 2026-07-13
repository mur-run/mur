//! Read-only status/preflight model for the deep-research UX shell.
//! Pure data collection — no repairs, no side effects.

use std::path::Path;

use mur_common::agent::{AgentProfile, McpNetMode};

use super::provision::{DEFAULT_WORKER_PREFIX, GATEWAY_MCP_NAME};

/// Canonical fleet name the wizard provisions and the smart run drives.
pub const DEFAULT_FLEET_NAME: &str = "deep-research";

pub struct WorkerStatus {
    pub name: String,
    pub running: bool,
    pub egress_granted: bool,
}

pub struct DeepResearchStatus {
    pub workers: Vec<WorkerStatus>,
    pub fleet_exists: bool,
    /// model_ref of the first worker (all provisioned alike).
    pub model: Option<String>,
}

/// An agent is "running" when its unix socket accepts a connection.
pub fn is_agent_running(mur_home: &Path, name: &str) -> bool {
    #[cfg(unix)]
    {
        let sock = mur_home.join("agents").join(name).join("agent.sock");
        std::os::unix::net::UnixStream::connect(&sock).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (mur_home, name);
        false
    }
}

fn gateway_egress_granted(profile: &AgentProfile) -> bool {
    profile.mcp_servers.iter().any(|s| {
        s.name == GATEWAY_MCP_NAME
            && s.network
                .as_ref()
                .is_some_and(|n| n.mode == McpNetMode::BroadAudited)
    })
}

pub fn collect_status(mur_home: &Path, fleet_name: &str) -> DeepResearchStatus {
    let mut workers = Vec::new();
    let mut model = None;
    // Workers are `<prefix>_1..N`; scan the agents dir for that shape so a
    // custom --count keeps working without storing extra state.
    let agents_dir = mur_home.join("agents");
    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(&format!("{DEFAULT_WORKER_PREFIX}_")))
            .collect();
        names.sort();
        for name in names {
            let (running, egress) = match AgentProfile::load(mur_home, &name) {
                Ok(p) => {
                    if model.is_none() {
                        model = p.model_ref.clone();
                    }
                    (
                        is_agent_running(mur_home, &name),
                        gateway_egress_granted(&p),
                    )
                }
                Err(_) => (false, false),
            };
            workers.push(WorkerStatus {
                name,
                running,
                egress_granted: egress,
            });
        }
    }
    let fleet_exists = crate::cmd::fleet::store::fleet_path(mur_home, fleet_name).exists();
    DeepResearchStatus {
        workers,
        fleet_exists,
        model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_on_empty_home_is_all_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = collect_status(tmp.path(), DEFAULT_FLEET_NAME);
        assert!(s.workers.is_empty());
        assert!(!s.fleet_exists);
        assert!(s.model.is_none());
    }

    #[test]
    fn agent_without_socket_is_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agents/dr_worker_1")).unwrap();
        assert!(!is_agent_running(tmp.path(), "dr_worker_1"));
    }
}
