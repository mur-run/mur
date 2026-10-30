//! Where an agent's pinned SSH client files live under its home.
//!
//! The CLI (`mur agent ssh allow` / `revoke`) writes `known_hosts` here from
//! host keys the user confirmed. The runtime never reads or writes it; it only
//! points the bash child's ssh at this path (plan O2). Defined once so both
//! sides agree on the location.

use std::path::{Path, PathBuf};

/// Directory under `agent_home` for the agent's SSH client files.
pub const SSH_DIR: &str = "ssh";

/// Pinned host keys file inside [`SSH_DIR`].
pub const KNOWN_HOSTS: &str = "known_hosts";

/// `<agent_home>/ssh/known_hosts`.
pub fn known_hosts_path(agent_home: &Path) -> PathBuf {
    agent_home.join(SSH_DIR).join(KNOWN_HOSTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hosts_path_is_under_agent_ssh_dir() {
        assert_eq!(
            known_hosts_path(Path::new("/a")),
            PathBuf::from("/a/ssh/known_hosts")
        );
    }
}
