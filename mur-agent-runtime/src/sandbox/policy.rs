use mur_common::agent::{Entitlements, NetworkOutboundMode};
use std::path::{Path, PathBuf};

/// Resolved, OS-ready sandbox policy derived from agent entitlements.
/// All paths are absolute (tilde expanded). All fields are ready to
/// feed directly to Landlock / SBPL / Job Object APIs.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// Paths the process may read (not write).
    pub fs_read: Vec<PathBuf>,
    /// Paths the process may read AND write.
    pub fs_write: Vec<PathBuf>,
    /// Paths that are explicitly denied (override fs_read/fs_write).
    pub fs_deny: Vec<PathBuf>,
    /// Directories containing executable binaries the process may exec.
    pub fs_exec: Vec<PathBuf>,
    /// Outbound TCP ports that are allowed. `None` = allow all; `Some([])` = deny all.
    pub net_allow_ports: Option<Vec<u16>>,
    /// Outbound hostnames for the reqwest guard layer.
    /// `None` = allow all (Unrestricted). `Some([])` = deny all (Off).
    pub net_allow_hosts: Option<Vec<String>>,
    /// Memory limit in megabytes (for Windows Job Object).
    pub memory_limit_mb: Option<u64>,
}

impl SandboxPolicy {
    pub fn from_entitlements(ent: &Entitlements, agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

        let expand = |s: &str| -> PathBuf {
            if s.starts_with("~/") {
                home.join(&s[2..])
            } else if s == "~" {
                home.clone()
            } else {
                PathBuf::from(s)
            }
        };

        let mut fs_read: Vec<PathBuf> = ent.filesystem.read.iter().map(|s| expand(s)).collect();
        let mut fs_write: Vec<PathBuf> = ent.filesystem.write.iter().map(|s| expand(s)).collect();
        let fs_deny: Vec<PathBuf> = ent.filesystem.deny.iter().map(|s| expand(s)).collect();

        // agent_home is always read+write — runtime cannot function without it.
        if !fs_write.contains(&agent_home.to_path_buf()) {
            fs_write.push(agent_home.to_path_buf());
        }

        // Standard system read paths: libraries, certs, DNS config.
        let system_read = system_read_paths();
        for p in system_read {
            if !fs_read.contains(&p) {
                fs_read.push(p);
            }
        }

        // Standard binary exec paths (needed for MCP spawn + shell tools).
        let fs_exec = vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/bin"),
            home.join(".local/bin"),
        ];

        let (net_allow_ports, net_allow_hosts) = match ent.network.outbound.mode {
            NetworkOutboundMode::Unrestricted => (None, None),
            NetworkOutboundMode::Restricted => {
                let ports = Some(vec![80u16, 443, 8080, 8443]);
                let hosts = Some(ent.network.outbound.allow_hosts.clone());
                (ports, hosts)
            }
            NetworkOutboundMode::Off => (Some(vec![]), Some(vec![])),
        };

        SandboxPolicy {
            fs_read,
            fs_write,
            fs_deny,
            fs_exec,
            net_allow_ports,
            net_allow_hosts,
            memory_limit_mb: Some(ent.limits.memory_mb),
        }
    }
}

fn system_read_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/etc"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/share"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/proc/self"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/null"),
    ];
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/System/Library"));
        paths.push(PathBuf::from("/private/var/folders"));
        paths.push(PathBuf::from("/private/tmp"));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, NetworkOutboundMode, OutboundNetwork,
        NetworkEntitlement, InboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
    };

    fn minimal_entitlements() -> Entitlements {
        Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Restricted,
                    allow_hosts: vec!["api.anthropic.com".to_string()],
                    protocols: vec!["tcp".to_string()],
                    resolve_dns: Default::default(),
                },
            },
            filesystem: FilesystemEntitlement {
                read: vec!["~/Documents".to_string()],
                write: vec!["~/Downloads".to_string()],
                deny: vec!["~/.ssh".to_string()],
            },
            processes: ProcessesEntitlement {
                spawn: SpawnEntitlement { mode: SpawnMode::Allowlist, allowed: vec![] },
            },
            syscalls: Default::default(),
            limits: Default::default(),
            llm: Default::default(),
        }
    }

    #[test]
    fn agent_home_always_in_write() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert!(policy.fs_write.contains(&home));
    }

    #[test]
    fn tilde_expands_to_home_dir() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        let has_docs = policy.fs_read.iter().any(|p| p.ends_with("Documents"));
        assert!(has_docs);
    }

    #[test]
    fn deny_paths_propagated() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        let has_ssh = policy.fs_deny.iter().any(|p| p.ends_with(".ssh"));
        assert!(has_ssh);
    }

    #[test]
    fn restricted_mode_populates_allow_hosts() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert_eq!(policy.net_allow_hosts, Some(vec!["api.anthropic.com".to_string()]));
    }

    #[test]
    fn unrestricted_mode_allows_all_hosts() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Unrestricted;
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&ent, &home);
        assert_eq!(policy.net_allow_hosts, None);
    }

    #[test]
    fn off_mode_blocks_all_hosts() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Off;
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&ent, &home);
        assert_eq!(policy.net_allow_hosts, Some(vec![]));
    }
}
