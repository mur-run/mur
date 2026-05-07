use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::ToSocketAddrs;

/// reqwest DNS resolver guard. Rejects hostnames not in the allowlist
/// before the OS resolver is called.
///
/// `None` for `allow_hosts` = allow all (Unrestricted).
/// `Some([])` = deny all (Off). `Some(hosts)` = restricted.
#[derive(Clone, Debug)]
pub struct HostGuard {
    allow_hosts: Option<Vec<String>>,
}

impl HostGuard {
    pub fn unrestricted() -> Self {
        Self { allow_hosts: None }
    }

    pub fn restricted(hosts: Vec<String>) -> Self {
        Self {
            allow_hosts: Some(hosts),
        }
    }

    pub fn off() -> Self {
        Self {
            allow_hosts: Some(vec![]),
        }
    }

    pub fn from_policy_hosts(allow_hosts: &Option<Vec<String>>) -> Self {
        Self {
            allow_hosts: allow_hosts.clone(),
        }
    }

    fn is_allowed(&self, host: &str) -> bool {
        match &self.allow_hosts {
            None => true,
            Some(list) => {
                if list.is_empty() {
                    return false;
                }
                list.iter().any(|allowed| {
                    if let Some(suffix) = allowed.strip_prefix("*.") {
                        host == suffix || host.ends_with(&format!(".{suffix}"))
                    } else {
                        allowed == host
                    }
                })
            }
        }
    }
}

impl Resolve for HostGuard {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let allowed = self.is_allowed(&host);

        Box::pin(async move {
            if !allowed {
                return Err(
                    Box::new(HostGuardError(host)) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            // Delegate to the OS resolver.
            let addrs: Addrs = Box::new(
                format!("{host}:0")
                    .to_socket_addrs()
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                    .map(|mut sa| {
                        sa.set_port(0);
                        sa
                    }),
            );
            Ok(addrs)
        })
    }
}

#[derive(Debug)]
struct HostGuardError(String);

impl std::fmt::Display for HostGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "host '{}' not in outbound allowlist (B1 HostGuard)",
            self.0
        )
    }
}

impl std::error::Error for HostGuardError {}
