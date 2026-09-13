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
            return Err(format!(
                "request to '{url}' blocked: host '{host}' is not in this \
                 agent's outbound allowlist \
                 (entitlements.network.outbound.allow_hosts)"
            ));
        }
        Ok(())
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

/// An HTTP client that cannot be used unguarded.
///
/// Four things have to hold together for `entitlements.network.outbound` to
/// mean anything, and #1297 happened because they were four separate things a
/// call site had to remember:
///
/// 1. the [`HostGuard`] DNS resolver, which screens hostnames;
/// 2. `.no_proxy()`, because a proxied request resolves the PROXY's host and so
///    never has the allowlist applied to its real destination;
/// 3. a redirect policy that re-checks every hop, because an allowed hostname
///    answering `302 Location: http://127.0.0.1:PORT/` lands on an IP literal
///    that the resolver never sees (confirmed by probe: the body came back);
/// 4. [`HostGuard::check_url`] before each send, which is the only layer that
///    sees the real destination, including IP literals.
///
/// This type owns all four. There is no accessor for the inner
/// `reqwest::Client`, so a holder cannot send a request that skipped the check
/// — the capability is indivisible rather than a convention.
#[derive(Clone, Debug)]
pub struct GuardedHttpClient {
    client: reqwest::Client,
    guard: HostGuard,
}

impl GuardedHttpClient {
    /// Wrap `base` with this guard. `base` carries policy that is not about
    /// hosts — timeouts, `.no_proxy()`, TLS — so its owner keeps deciding
    /// those; what is added here is everything host enforcement needs.
    pub fn build(base: reqwest::ClientBuilder, guard: HostGuard) -> reqwest::Result<Self> {
        let policy_guard = guard.clone();
        let client = base
            .dns_resolver(std::sync::Arc::new(guard.clone()))
            // Every hop is re-checked rather than redirects being switched off.
            // Off would also close the hole, and is the smaller change, but it
            // would silently turn a provider that legitimately redirects into a
            // 3xx the caller cannot follow — a behavioural gamble inside a
            // security fix. Re-checking cannot break a redirect the allowlist
            // permits and cannot follow one it does not.
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                match policy_guard.check_url(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    // `stop` rather than `error`: the caller then sees the 3xx
                    // and its Location, which says what was refused. An opaque
                    // redirect error would not.
                    Err(_) => attempt.stop(),
                }
            }))
            .build()?;
        Ok(Self { client, guard })
    }

    /// A client with no host allowlist, for the paths that have no profile to
    /// resolve one from (`OllamaClient::new`, `*::from_env`, tests). Same
    /// semantics those paths already had; the SSRF screen still applies.
    pub fn unrestricted(base: reqwest::ClientBuilder) -> reqwest::Result<Self> {
        Self::build(base, HostGuard::unrestricted())
    }

    /// Start a POST, or refuse the URL. The check is inside, so a caller cannot
    /// forget it.
    pub fn post(&self, url: &str) -> Result<reqwest::RequestBuilder, String> {
        self.checked(url).map(|u| self.client.post(u))
    }

    /// Start a GET, or refuse the URL.
    pub fn get(&self, url: &str) -> Result<reqwest::RequestBuilder, String> {
        self.checked(url).map(|u| self.client.get(u))
    }

    fn checked(&self, url: &str) -> Result<reqwest::Url, String> {
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;
        self.guard.check_url(&parsed)?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod guarded_client_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A listener that answers 200 and reports the first request line it saw.
    async fn echo_listener() -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = l.accept().await {
                let mut b = [0u8; 2048];
                let n = s.read(&mut b).await.unwrap_or(0);
                let first = String::from_utf8_lossy(&b[..n])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = s.flush().await;
                let _ = s.shutdown().await;
                let _ = tx.send(first);
            }
        });
        (addr, rx)
    }

    fn guarded(guard: HostGuard) -> GuardedHttpClient {
        GuardedHttpClient::build(reqwest::Client::builder(), guard).unwrap()
    }

    /// #1297 as reported: an ambient proxy must not receive the request.
    #[tokio::test]
    async fn an_ambient_proxy_never_sees_the_request() {
        let (proxy, proxy_rx) = echo_listener().await;
        let (target, _t) = echo_listener().await;
        // SAFETY: set and cleared in this test; nextest gives it its own process.
        unsafe { std::env::set_var("HTTP_PROXY", format!("http://{proxy}")) };
        let c = guarded(HostGuard::restricted(vec![]));
        let _ = c.post(&format!("http://{target}/")).map(|b| b.send());
        unsafe { std::env::remove_var("HTTP_PROXY") };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), proxy_rx)
                .await
                .is_err(),
            "the proxy must receive nothing"
        );
    }

    /// The second bypass, found by probe while comparing options: reqwest does
    /// not call a custom resolver for an IP literal, so before `check_url` an
    /// allowlist of `["api.anthropic.com"]` let `http://127.0.0.1:PORT/` through
    /// with a 200.
    #[test]
    fn an_ip_literal_outside_the_allowlist_is_refused() {
        let c = guarded(HostGuard::restricted(vec!["api.anthropic.com".into()]));
        let err = c.post("http://127.0.0.1:9/").unwrap_err();
        assert!(
            err.contains("not in this agent's outbound allowlist"),
            "{err}"
        );
    }

    /// And an IP literal that IS named stays reachable — the check is an
    /// allowlist, not a ban on addresses.
    #[test]
    fn an_ip_literal_on_the_allowlist_is_allowed() {
        let c = guarded(HostGuard::restricted(vec!["127.0.0.1".into()]));
        assert!(c.post("http://127.0.0.1:9/v1/chat").is_ok());
    }

    /// Hostname matching is unchanged: exact and wildcard both still work.
    #[test]
    fn hostname_exact_and_wildcard_behaviour_is_unchanged() {
        let c = guarded(HostGuard::restricted(vec![
            "api.anthropic.com".into(),
            "*.api.example.com".into(),
        ]));
        assert!(c.post("https://api.anthropic.com/v1/messages").is_ok());
        assert!(c.post("https://eu.api.example.com/v1").is_ok());
        assert!(c.post("https://evil.example.com/").is_err());
    }

    /// `Off` denies everything, including a host that would otherwise look
    /// innocuous.
    #[test]
    fn off_denies_every_host() {
        let c = guarded(HostGuard::off());
        assert!(c.post("https://api.anthropic.com/v1/messages").is_err());
        assert!(c.post("http://127.0.0.1:11434/api/chat").is_err());
    }

    /// `Unrestricted` applies no allowlist but keeps the SSRF screen: an agent
    /// with no host policy still must not be steered at cloud metadata.
    #[test]
    fn unrestricted_allows_hosts_but_still_screens_metadata() {
        let c = guarded(HostGuard::unrestricted());
        assert!(c.post("https://anything.example.com/").is_ok());
        let err = c
            .post("http://169.254.169.254/latest/meta-data/")
            .unwrap_err();
        assert!(err.contains("link-local"), "{err}");
        let err6 = c
            .post("http://[::ffff:169.254.169.254]/latest/meta-data/")
            .unwrap_err();
        assert!(err6.contains("link-local"), "{err6}");
    }

    /// **The review's finding.** An allowed hostname answering
    /// `302 Location: http://127.0.0.1:PORT/` used to land on the IP literal:
    /// the pre-send check only ever saw the original URL and the resolver is
    /// never called for a literal. Probed before the fix — the final body came
    /// back as `PWNED`.
    #[tokio::test]
    async fn a_redirect_to_a_host_outside_the_allowlist_is_not_followed() {
        let (dest, dest_rx) = echo_listener().await;
        // The allowed origin, which redirects to that address.
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = origin.accept().await {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b).await;
                let r = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{dest}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(r.as_bytes()).await;
                let _ = s.flush().await;
                let _ = s.shutdown().await;
            }
        });
        let c = GuardedHttpClient::build(
            reqwest::Client::builder().resolve("allowed.example", origin_addr),
            HostGuard::restricted(vec!["allowed.example".into()]),
        )
        .unwrap();
        let resp = c
            .get(&format!("http://allowed.example:{}/", origin_addr.port()))
            .expect("the first hop is allowed")
            .send()
            .await
            .expect("the 3xx itself is returned, not an error");
        assert_eq!(
            resp.status(),
            302,
            "the redirect must be surfaced, not followed"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(400), dest_rx)
                .await
                .is_err(),
            "the redirect target must never be contacted"
        );
    }

    /// A redirect the allowlist permits is still followed, which is why the
    /// policy re-checks rather than switching redirects off.
    #[tokio::test]
    async fn a_redirect_within_the_allowlist_is_followed() {
        let (dest, dest_rx) = echo_listener().await;
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = origin.accept().await {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b).await;
                let r = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{dest}/moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(r.as_bytes()).await;
                let _ = s.flush().await;
                let _ = s.shutdown().await;
            }
        });
        let c = GuardedHttpClient::build(
            reqwest::Client::builder().resolve("allowed.example", origin_addr),
            // Both hops named.
            HostGuard::restricted(vec!["allowed.example".into(), "127.0.0.1".into()]),
        )
        .unwrap();
        let resp = c
            .get(&format!("http://allowed.example:{}/", origin_addr.port()))
            .expect("first hop allowed")
            .send()
            .await
            .expect("the followed redirect answers");
        assert_eq!(resp.status(), 200);
        let saw = tokio::time::timeout(std::time::Duration::from_secs(2), dest_rx)
            .await
            .expect("the target was contacted")
            .expect("and reported its request line");
        assert!(saw.contains("/moved"), "got {saw:?}");
    }
}
