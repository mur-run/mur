use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

/// Host-pattern matcher, hoisted to `mur-common` as the single source of
/// truth shared by the egress proxy (this module) and the research
/// gateway's SSRF guard. Re-exported here so existing callers
/// (`egress_proxy.rs` et al.) are unchanged.
pub use mur_common::net::{host_allowed, host_matches_pattern};

/// True for IP ranges an outbound request must never reach: the link-local /
/// cloud-metadata range (IPv4 169.254.0.0/16 — includes 169.254.169.254 — and
/// IPv6 fe80::/10) plus the unspecified address.
///
/// Loopback (127.0.0.0/8, ::1) and RFC1918/ULA private ranges are intentionally
/// NOT blocked: this is a local-first platform that legitimately talks to local
/// (127.0.0.1 Ollama) and LAN LLM endpoints. Blocking those would break core
/// functionality; the metadata endpoint is the genuine SSRF target.
fn is_link_local_or_unspecified(ip: IpAddr) -> bool {
    // Normalize IPv4-in-IPv6 forms down to their embedded IPv4 first. Both the
    // mapped form (`::ffff:a.b.c.d`) and the compatible form (`::a.b.c.d`) have
    // `segments()[0] == 0`, so without this they slip past the IPv6 checks below
    // (which only catch fe80::/10 and `::`) — e.g. `http://[::ffff:169.254.169.254]/`
    // would otherwise reach the cloud-metadata endpoint.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
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
    fn host_allowed_is_fail_closed_and_pattern_aware() {
        let allow = vec!["example.com".to_string(), "*.api.example.com".to_string()];
        assert!(host_allowed("example.com", &allow));
        assert!(host_allowed("v1.api.example.com", &allow));
        assert!(!host_allowed("evil.com", &allow));
        assert!(!host_allowed("example.com", &[]), "empty allowlist denies");
    }

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
    fn ipv4_in_ipv6_metadata_blocked() {
        // The metadata address must not slip through the IPv6 path via the
        // mapped (`::ffff:`) or compatible (`::`) IPv4-in-IPv6 encodings.
        assert!(is_link_local_or_unspecified(
            "::ffff:169.254.169.254".parse().unwrap()
        ));
        assert!(is_link_local_or_unspecified(
            "::169.254.169.254".parse().unwrap()
        ));
        assert!(is_link_local_or_unspecified(
            "::ffff:169.254.0.1".parse().unwrap()
        ));
        // Mapped loopback/private/public stay reachable (local-first).
        assert!(!is_link_local_or_unspecified(
            "::ffff:127.0.0.1".parse().unwrap()
        ));
        assert!(!is_link_local_or_unspecified(
            "::ffff:192.168.1.10".parse().unwrap()
        ));
        assert!(!is_link_local_or_unspecified(
            "::ffff:1.1.1.1".parse().unwrap()
        ));
    }

    #[test]
    fn check_request_url_blocks_ipv6_mapped_metadata() {
        // End-to-end through the connector-level guard wired into the LLM
        // clients: an IP-literal URL embedding the metadata address is refused.
        let url = reqwest::Url::parse("http://[::ffff:169.254.169.254]/latest/meta-data/").unwrap();
        assert!(check_request_url(&url).is_err());
        // A normal IP-literal to a public host is fine.
        let ok = reqwest::Url::parse("http://1.1.1.1/").unwrap();
        assert!(check_request_url(&ok).is_ok());
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
