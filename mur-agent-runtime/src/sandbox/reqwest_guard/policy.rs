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
pub(crate) fn is_link_local_or_unspecified(ip: IpAddr) -> bool {
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

/// Resolve `target` (`host:port`, or an IP-literal:port) via the OS resolver and
/// return only the socket addresses that pass the SSRF screen — link-local and
/// unspecified addresses (the genuine metadata/SSRF targets) are dropped.
/// Loopback and private-LAN are intentionally KEPT (local-first platform).
/// An empty result means every resolved address was screened out → the caller
/// must refuse the connection (fail-closed).
pub(crate) fn screened_socket_addrs(target: &str) -> std::io::Result<Vec<SocketAddr>> {
    Ok(target
        .to_socket_addrs()?
        .filter(|sa| !is_link_local_or_unspecified(sa.ip()))
        .collect())
}

/// reqwest DNS resolver guard. Rejects hostnames not in the allowlist
/// before the OS resolver is called.
///
/// What this guard permits. Spelled out rather than encoded in an
/// `Option<Vec<String>>`, because that representation made `Some([])` mean two
/// different things — `Off` (no network at all) and `Restricted` with nothing
/// listed — and those need opposite answers about loopback.
#[derive(Clone, Debug)]
enum Policy {
    /// `Unrestricted`: no host allowlist. The SSRF screen still applies.
    AllowAll,
    /// `Restricted` / `ProxyOnly`: these hosts, plus loopback.
    Allowlist(Vec<String>),
    /// `Off`: nothing, loopback included.
    DenyAll,
    /// One remote MCP server: exactly these hosts, compared literally, with
    /// **no** loopback exemption — a redirect to `127.0.0.1` must not receive
    /// that server's bearer. A server that really is on loopback names it as
    /// its own `url` host and is admitted that way.
    Exact(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct HostGuard {
    policy: Policy,
}

impl HostGuard {
    pub fn unrestricted() -> Self {
        Self {
            policy: Policy::AllowAll,
        }
    }

    pub fn restricted(hosts: Vec<String>) -> Self {
        Self {
            policy: Policy::Allowlist(hosts),
        }
    }

    pub fn off() -> Self {
        Self {
            policy: Policy::DenyAll,
        }
    }

    /// Exactly `hosts` and nothing else: no wildcard, no loopback exemption.
    /// For a client that carries one MCP server's credentials (#D6).
    pub fn exact(hosts: Vec<String>) -> Self {
        Self {
            policy: Policy::Exact(hosts.iter().map(|h| bare_host(h).to_string()).collect()),
        }
    }

    /// Apply this guard to a whole URL, before the request is sent.
    ///
    /// This is the only layer that sees the **real destination**. The DNS
    /// resolver below sees hostnames only: reqwest never calls a custom
    /// resolver for an IP-literal URL, and when a proxy is configured it
    /// resolves the proxy's host instead of the target's. Both were confirmed
    /// by probe (#1297), so `allow_hosts` has to be applied here as well.
    ///
    /// Order matters. The SSRF screen runs in **every** mode, including
    /// `Unrestricted`: an agent with no host allowlist still must not be
    /// steered at the cloud metadata endpoint. The allowlist runs after, and
    /// only bites when one is configured.
    pub fn check_url(&self, url: &reqwest::Url) -> Result<(), String> {
        check_request_url(url)?;
        let Some(host) = url.host_str() else {
            return Ok(()); // no host — nothing to match, handled elsewhere
        };
        if !self.is_allowed(host) {
            let scope = match self.policy {
                Policy::Exact(_) => "this MCP server's host set (its url and OAuth token endpoint)",
                _ => {
                    "this agent's outbound allowlist \
                     (entitlements.network.outbound.allow_hosts)"
                }
            };
            return Err(format!(
                "request to '{url}' blocked: host '{host}' is not in {scope}"
            ));
        }
        Ok(())
    }

    /// One rule, consulted by both entry points — the DNS resolver below and
    /// [`Self::check_url`] — so a host cannot be permitted by one and refused
    /// by the other.
    fn is_allowed(&self, host: &str) -> bool {
        match &self.policy {
            Policy::AllowAll => true,
            Policy::DenyAll => false,
            // Loopback is not governed by `allow_hosts` and never was. The
            // runtime keeps it off the list deliberately — `provider_host`
            // returns `None` for `127.0.0.1` / `localhost` / `::1` — and grants
            // the agent's own local LLM *port* through the B1 sandbox instead.
            // `ProxyOnly` makes the point louder: its definition is "egress
            // ONLY via loopback proxies", so refusing loopback would make that
            // mode unusable.
            //
            // Enforcement is not removed, it stays where it belongs: the OS
            // sandbox decides which local ports an agent may reach. And the
            // SSRF screen is separate, so cloud metadata — link-local, not
            // loopback — does not get in this way.
            Policy::Allowlist(list) => {
                host_str_is_loopback(host)
                    || list
                        .iter()
                        .any(|pattern| host_matches_pattern(host, pattern))
            }
            Policy::Exact(list) => {
                let host = bare_host(host);
                list.iter().any(|h| h.eq_ignore_ascii_case(host))
            }
        }
    }
}

/// `url::Url::host_str` keeps an IPv6 literal's brackets; the DNS resolver
/// gets it without. Compare the bare form so both entry points agree.
fn bare_host(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Is this URL's host this machine? Mirrors `llm::loopback::host_is_loopback`,
/// duplicated rather than imported so `sandbox` does not depend on `llm`; the
/// two are checked against each other by `the_two_loopback_predicates_agree`.
fn host_is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The same question asked of a bare host string, which is all the DNS resolver
/// gets. An IPv6 literal arrives here without its brackets.
fn host_str_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    trimmed
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
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
            // — to the cloud metadata endpoint).
            //
            // This layer sees HOSTNAMES ONLY, and that is no longer a tracked
            // follow-up: reqwest does not call a custom resolver for an
            // IP-literal URL, and with a proxy configured it resolves the
            // proxy's host rather than the target's. A connector layer cannot
            // cover the gap either — reqwest hands it `Unnameable(pub(super)
            // Uri)`, whose destination is unreadable from outside the crate.
            // So the allowlist is applied at the request layer too, by
            // `HostGuard::check_url`, and `GuardedHttpClient` is what makes
            // that impossible to skip (#1297).
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

    #[test]
    fn screened_socket_addrs_drops_link_local_keeps_public() {
        // Loopback is a legitimate local-first target and must be KEPT.
        let lo = screened_socket_addrs("127.0.0.1:443").unwrap();
        assert!(lo.iter().all(|sa| sa.ip().to_string() == "127.0.0.1"));
        assert_eq!(lo.len(), 1);

        // A link-local IP literal (cloud-metadata) must be dropped → empty.
        let meta = screened_socket_addrs("169.254.169.254:80").unwrap();
        assert!(
            meta.is_empty(),
            "link-local metadata IP must be screened out"
        );

        // Unspecified dropped too.
        let unspec = screened_socket_addrs("0.0.0.0:80").unwrap();
        assert!(unspec.is_empty());
    }
}

#[cfg(test)]
mod loopback_regression_tests {
    use super::*;

    /// The regression #1303 introduced, now a guard.
    ///
    /// A Restricted agent's own model endpoint is on loopback, and loopback is
    /// deliberately absent from `allow_hosts`: `provider_host` returns `None`
    /// for it and the B1 sandbox grants the port instead. Applying the
    /// allowlist at the request layer without an exemption refused the agent's
    /// own model — probed as `127.0.0.1=false localhost=false gateway8088=false`.
    #[test]
    fn a_restricted_agent_can_still_reach_its_own_local_model() {
        let g = HostGuard::restricted(vec![]);
        for url in [
            "http://127.0.0.1:11434/api/chat",          // Ollama
            "http://localhost:11434/api/chat",          // the same by name
            "http://127.0.0.1:8088/v1/messages",        // the loopback model gateway
            "http://127.0.0.1:8088/codex/v1/responses", // and its codex route
            "http://[::1]:50320/v1/chat/completions",   // bundled MLX over IPv6
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert!(
                g.check_url(&parsed).is_ok(),
                "loopback is governed by the sandbox port grant, not allow_hosts: {url}"
            );
        }
    }

    /// `ProxyOnly` is defined as "egress ONLY via loopback proxies", so it is
    /// the mode that would break hardest.
    #[test]
    fn proxy_only_can_reach_its_loopback_proxies() {
        // ProxyOnly resolves to the same HostGuard shape as Restricted.
        let g = HostGuard::restricted(vec!["api.anthropic.com".into()]);
        assert!(
            g.check_url(&reqwest::Url::parse("http://127.0.0.1:8088/v1/messages").unwrap())
                .is_ok()
        );
    }

    /// `exact` has no loopback exemption: an MCP bearer must not follow a
    /// redirect to this machine unless the server itself lives there.
    #[test]
    fn exact_refuses_loopback_unless_named() {
        let g = HostGuard::exact(vec!["mcp.example.com".into()]);
        for u in [
            "http://127.0.0.1:9/",
            "http://localhost:9/",
            "http://[::1]:9/",
        ] {
            let err = g.check_url(&reqwest::Url::parse(u).unwrap()).unwrap_err();
            assert!(err.contains("MCP server's host set"), "{u}: {err}");
        }
        assert!(!g.is_allowed("localhost"));
        let local = HostGuard::exact(vec!["127.0.0.1".into(), "[::1]".into()]);
        assert!(
            local
                .check_url(&reqwest::Url::parse("http://127.0.0.1:9/").unwrap())
                .is_ok()
        );
        assert!(
            local
                .check_url(&reqwest::Url::parse("http://[::1]:9/").unwrap())
                .is_ok()
        );
        assert!(local.is_allowed("::1"), "resolver form, no brackets");
    }

    /// Literal, case-insensitive; a listed name is not a wildcard for its
    /// subdomains, and the SSRF screen still runs first.
    #[test]
    fn exact_is_literal_and_still_ssrf_screened() {
        let g = HostGuard::exact(vec!["MCP.example.com".into(), "169.254.169.254".into()]);
        assert!(g.is_allowed("mcp.example.com"));
        assert!(!g.is_allowed("evil.mcp.example.com"));
        assert!(!g.is_allowed("example.com"));
        let meta = reqwest::Url::parse("http://169.254.169.254/latest").unwrap();
        assert!(g.check_url(&meta).unwrap_err().contains("SSRF"));
    }

    /// `Off` means no network at all, and that has to include loopback —
    /// otherwise the exemption would become a hole in the strictest mode.
    #[test]
    fn off_still_denies_loopback() {
        let g = HostGuard::off();
        let err = g
            .check_url(&reqwest::Url::parse("http://127.0.0.1:11434/api/chat").unwrap())
            .unwrap_err();
        assert!(err.contains("allowlist"), "{err}");
    }

    /// The exemption must not become a way to reach cloud metadata: that
    /// address is link-local, not loopback, and the SSRF screen runs first.
    #[test]
    fn the_exemption_does_not_cover_link_local_metadata() {
        for g in [
            HostGuard::unrestricted(),
            HostGuard::restricted(vec!["127.0.0.1".into()]),
        ] {
            let err = g
                .check_url(
                    &reqwest::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap(),
                )
                .unwrap_err();
            assert!(err.contains("link-local"), "{err}");
        }
    }

    /// Two copies of "is this host me?" exist, one here and one in
    /// `llm::loopback`, because `sandbox` must not depend on `llm`. This keeps
    /// them from drifting.
    #[test]
    fn the_two_loopback_predicates_agree() {
        for (raw, expected) in [
            ("http://127.0.0.1:1/", true),
            ("http://127.0.0.2:1/", true),
            ("http://localhost:1/", true),
            ("http://LOCALHOST:1/", true),
            ("http://[::1]:1/", true),
            ("http://169.254.169.254/", false),
            ("http://api.anthropic.com/", false),
            ("http://0.0.0.0:1/", false),
        ] {
            let url = reqwest::Url::parse(raw).unwrap();
            assert_eq!(host_is_loopback(&url), expected, "local predicate: {raw}");
            assert_eq!(
                crate::llm::loopback::is_loopback_base_url(raw),
                expected,
                "llm predicate disagrees: {raw}"
            );
        }
    }
}
