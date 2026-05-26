//! Peer agent enumeration (M7a).
//!
//! Scans `<MUR_HOME>/agents/*/` for peer agent directories on the same host.
//! Read-only — never writes to peer state.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct PeerAgent {
    pub name: String,
    pub home_path: PathBuf,
    pub skills_count: usize,
}

/// Enumerate peer agents on this host.
///
/// Skips hidden directories (names starting with `.`).
pub fn list_peer_agents(mur_home: &Path) -> std::io::Result<Vec<PeerAgent>> {
    let agents_dir = mur_home.join("agents");
    if !agents_dir.exists() {
        return Ok(vec![]);
    }
    let mut peers = Vec::new();
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let home_path = entry.path();
        let skills_dir = home_path.join("skills");
        let skills_count = if skills_dir.exists() {
            std::fs::read_dir(&skills_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .count()
        } else {
            0
        };
        peers.push(PeerAgent {
            name,
            home_path,
            skills_count,
        });
    }
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lists_agent_directories() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("agents").join("alice").join("skills").join("s1"))
            .unwrap();
        std::fs::create_dir_all(home.join("agents").join("bob").join("skills")).unwrap();
        std::fs::create_dir_all(home.join("agents").join(".hidden")).unwrap();

        let peers = list_peer_agents(home).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].name, "alice");
        assert_eq!(peers[0].skills_count, 1);
        assert_eq!(peers[1].name, "bob");
        assert_eq!(peers[1].skills_count, 0);
    }

    #[test]
    fn empty_when_agents_dir_missing() {
        let dir = tempdir().unwrap();
        assert!(list_peer_agents(dir.path()).unwrap().is_empty());
    }
}
