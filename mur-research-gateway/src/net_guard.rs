//! Strict SSRF guard for the research gateway's `fetch` tool.
//!
//! Wired into `fetcher.rs` (Task 3) behind `tokio::task::spawn_blocking`,
//! since `to_socket_addrs()` below is synchronous DNS resolution. The caller
//! still does NOT pin its connection to the screened address — reqwest
//! re-resolves internally, leaving an advisory (not exploited-in-practice)
//! resolve→connect DNS-rebinding window; see the comment in `fetcher.rs`.

use std::net::{IpAddr, ToSocketAddrs};

#[derive(Debug, PartialEq)]
pub enum GuardReject {
    BadScheme,
    DeniedHost,
    PrivateAddress,
    Unresolvable,
}

/// Deny semantics via the shared matcher (single source of truth with the
/// egress proxy — `mur_common::net`). Same matcher; the deny LIST is what
/// makes a match mean "blocked".
pub fn host_denied(host: &str, deny: &[String]) -> bool {
    mur_common::net::host_allowed(host, deny)
}

/// STRICTER than the runtime's local-first guard: a web researcher has no
/// legitimate reason to reach loopback/private/link-local, so all are forbidden.
pub fn is_forbidden_target(ip: IpAddr) -> bool {
    // Normalize IPv4-in-IPv6 (mapped ::ffff:a.b.c.d and compatible ::a.b.c.d).
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 0 // 0.0.0.0/8
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        }
    }
}

/// Parse + screen a URL against the deny list and the resolved IPs. On pass
/// returns the parsed URL. NOTE: this screens the IPs resolved AT SCREEN TIME
/// only — it does NOT pin the connection. The caller (`fetcher.rs`) hands the
/// URL to reqwest, which re-resolves at connect time, so a resolve→connect
/// DNS-rebinding window remains. This is advisory enforcement per the
/// egress-governance spec; pin-to-proxy (airtight) is Phase 3.
pub fn screen_url(raw: &str, deny: &[String]) -> Result<url::Url, GuardReject> {
    let u = url::Url::parse(raw).map_err(|_| GuardReject::BadScheme)?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(GuardReject::BadScheme);
    }
    let host = u.host_str().ok_or(GuardReject::BadScheme)?;
    if host_denied(host, deny) {
        return Err(GuardReject::DeniedHost);
    }
    let port = u.port_or_known_default().unwrap_or(80);
    // url::Url::host_str() returns IPv6 literals WITH brackets (e.g. "[::1]"),
    // which ToSocketAddrs' IP fast-path can't parse — it would silently fall
    // through to DNS resolution (and fail) instead of screening the literal.
    let resolve_host = host.trim_start_matches('[').trim_end_matches(']');
    let mut any = false;
    for sa in (resolve_host, port)
        .to_socket_addrs()
        .map_err(|_| GuardReject::Unresolvable)?
    {
        any = true;
        if is_forbidden_target(sa.ip()) {
            return Err(GuardReject::PrivateAddress);
        }
    }
    if !any {
        return Err(GuardReject::Unresolvable);
    }
    Ok(u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn blocks_cloud_metadata_and_private_and_loopback() {
        for ip in [
            "169.254.169.254",
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "::1",
            "fe80::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(
                is_forbidden_target(ip.parse::<IpAddr>().unwrap()),
                "{ip} must be forbidden"
            );
        }
    }
    #[test]
    fn allows_public() {
        for ip in ["8.8.8.8", "203.0.113.7", "2606:4700:4700::1111"] {
            assert!(
                !is_forbidden_target(ip.parse::<IpAddr>().unwrap()),
                "{ip} must be allowed"
            );
        }
    }
    #[test]
    fn deny_host_patterns() {
        let deny = vec!["*.internal.corp".to_string(), "blocked.example".to_string()];
        assert!(host_denied("api.internal.corp", &deny));
        assert!(host_denied("internal.corp", &deny));
        assert!(host_denied("blocked.example", &deny));
        assert!(!host_denied("good.example", &deny));
    }
    #[test]
    fn screen_rejects_bad_scheme_and_denied() {
        assert!(matches!(
            screen_url("file:///etc/passwd", &[]),
            Err(GuardReject::BadScheme)
        ));
        assert!(matches!(
            screen_url("http://blocked.example/", &["blocked.example".into()]),
            Err(GuardReject::DeniedHost)
        ));
    }
    #[test]
    fn screen_rejects_ipv4_compatible_ipv6_metadata() {
        // `::169.254.169.254` (no `::ffff:` marker) is the deprecated
        // IPv4-compatible IPv6 form — a named SSRF bypass distinct from the
        // IPv4-mapped `::ffff:` form already covered above.
        assert!(matches!(
            screen_url("http://[::169.254.169.254]/", &[]),
            Err(GuardReject::PrivateAddress)
        ));
    }
}
