use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

/// Match a host against an allowlist pattern.
///
/// Canonical wildcard syntax is `*.example.com` (matches `api.example.com`
/// and `example.com`).  For backward compatibility the legacy leading-dot
/// form `.example.com` is also accepted and treated identically.
///
/// Both layers that perform host-allowlist checks (`HostGuard` DNS resolver
/// and the B0 safety hook) must call this function so they share a single
/// interpretation of wildcard patterns.
pub fn host_matches_pattern(host: &str, pattern: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    // Strip leading `*.` (canonical) or leading `.` (legacy) to get the suffix.
    let suffix = if let Some(s) = pattern.strip_prefix("*.") {
        s
    } else if let Some(s) = pattern.strip_prefix('.') {
        s
    } else {
        // Exact match only.
        return host == pattern;
    };
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// True for IP ranges an outbound request must never reach: the link-local /
/// cloud-metadata range (IPv4 169.254.0.0/16 — includes 169.254.169.254 — and
/// IPv6 fe80::/10) plus the unspecified address.
///
/// Loopback (127.0.0.0/8, ::1) and RFC1918/ULA private ranges are intentionally
/// NOT blocked: this is a local-first platform that legitimately talks to local
/// (127.0.0.1 Ollama) and LAN LLM endpoints. Blocking those would break core
/// functionality; the metadata endpoint is the genuine SSRF target.
fn is_link_local_or_unspecified(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4.is_unspecified(),
        // IPv6 link-local is fe80::/10; no stable std helper, so mask manually.
        IpAddr::V6(v6) => v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

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
                list.iter()
                    .any(|pattern| host_matches_pattern(host, pattern))
            }
        }
    }
}

/// Reject requests whose URL contains an IP-literal host in the blocked range.
///
/// `reqwest` only calls the custom DNS resolver for *hostnames* — IP-literal
/// URLs (`http://169.254.169.254/`) bypass `HostGuard::resolve` entirely and
/// connect directly.  Call this function before executing any request to
/// close the gap.
///
/// Returns `Ok(())` if the URL is safe to send, `Err(msg)` if it must be
/// rejected.
pub fn check_request_url(url: &reqwest::Url) -> Result<(), String> {
    use url::Host;
    let host = match url.host() {
        Some(h) => h,
        None => return Ok(()), // no host — handled elsewhere
    };
    let ip: Option<std::net::IpAddr> = match host {
        Host::Ipv4(ip) => Some(ip.into()),
        Host::Ipv6(ip) => Some(ip.into()),
        Host::Domain(_) => None,
    };
    if let Some(ip) = ip
        && is_link_local_or_unspecified(ip)
    {
        return Err(format!(
            "request to IP-literal URL '{url}' blocked: address {ip} is \
             link-local or unspecified (SSRF guard)"
        ));
    }
    Ok(())
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
            // Delegate to the OS resolver, then drop any link-local/metadata
            // address (defends against a hostname resolving — or DNS-rebinding
            // — to the cloud metadata endpoint). NOTE: reqwest only invokes a
            // custom resolver for *hostnames*; IP-literal URLs bypass this layer
            // entirely and need a connector-level guard (tracked follow-up).
            let resolved: Vec<SocketAddr> = format!("{host}:0")
                .to_socket_addrs()
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .filter(|sa| !is_link_local_or_unspecified(sa.ip()))
                .map(|mut sa| {
                    sa.set_port(0);
                    sa
                })
                .collect();
            if resolved.is_empty() {
                return Err(Box::new(HostGuardError(format!(
                    "{host} (resolved only to link-local/metadata addresses)"
                )))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            let addrs: Addrs = Box::new(resolved.into_iter());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_and_link_local_blocked() {
        assert!(is_link_local_or_unspecified(
            "169.254.169.254".parse().unwrap()
        )); // cloud metadata
        assert!(is_link_local_or_unspecified("169.254.0.1".parse().unwrap()));
        assert!(is_link_local_or_unspecified("0.0.0.0".parse().unwrap()));
        assert!(is_link_local_or_unspecified("fe80::1".parse().unwrap()));
        assert!(is_link_local_or_unspecified("::".parse().unwrap()));
    }

    #[test]
    fn loopback_and_private_allowed() {
        // Local-first: local Ollama + LAN endpoints must remain reachable.
        assert!(!is_link_local_or_unspecified("127.0.0.1".parse().unwrap()));
        assert!(!is_link_local_or_unspecified("::1".parse().unwrap()));
        assert!(!is_link_local_or_unspecified(
            "192.168.1.10".parse().unwrap()
        ));
        assert!(!is_link_local_or_unspecified("10.0.0.5".parse().unwrap()));
        assert!(!is_link_local_or_unspecified("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn host_allowlist_matches_exact_and_wildcard() {
        let g = HostGuard::restricted(vec!["api.anthropic.com".into(), "*.openai.com".into()]);
        assert!(g.is_allowed("api.anthropic.com"));
        assert!(g.is_allowed("api.openai.com"));
        assert!(g.is_allowed("openai.com"));
        assert!(!g.is_allowed("evil.com"));
        assert!(!g.is_allowed("api.anthropic.com.evil.com"));
    }

    #[test]
    fn off_denies_all_unrestricted_allows_all() {
        assert!(!HostGuard::off().is_allowed("anything.com"));
        assert!(HostGuard::unrestricted().is_allowed("anything.com"));
    }
}
