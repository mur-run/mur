use super::policy::HostGuard;

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
        // No seam here: reqwest reads the proxy from the real environment at
        // build time, so proving it is honoured (or ignored) needs the real
        // variable. The guard serializes that and restores it on unwind.
        let _env = mur_common::test_env::EnvGuard::set([("HTTP_PROXY", format!("http://{proxy}"))]);
        let c = guarded(HostGuard::restricted(vec![]));
        let _ = c.post(&format!("http://{target}/")).map(|b| b.send());
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
        // 203.0.113.0/24 is TEST-NET-3: a real IP literal that is not
        // loopback, so the loopback exemption does not apply to it.
        let err = c.post("http://203.0.113.5/").unwrap_err();
        assert!(
            err.contains("not in this agent's outbound allowlist"),
            "{err}"
        );
    }

    /// And an IP literal that IS named stays reachable — the check is an
    /// allowlist, not a ban on addresses.
    #[test]
    fn an_ip_literal_on_the_allowlist_is_allowed() {
        let c = guarded(HostGuard::restricted(vec!["203.0.113.5".into()]));
        assert!(c.post("http://203.0.113.5/v1/chat").is_ok());
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
        // Loopback included: `Off` means no network, and the loopback
        // exemption must not become a hole in the strictest mode.
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
                // A NAME, not the loopback literal: loopback is exempt by
                // design (an agent's own model lives there), so the property
                // under test has to be "this host is not on the allowlist".
                let r = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://blocked.example:{}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    dest.port()
                );
                let _ = s.write_all(r.as_bytes()).await;
                let _ = s.flush().await;
                let _ = s.shutdown().await;
            }
        });
        let c = GuardedHttpClient::build(
            reqwest::Client::builder()
                .resolve("allowed.example", origin_addr)
                // Reachable if it were followed, which is the point: the
                // refusal is the allowlist's, not a connection failure.
                .resolve("blocked.example", dest),
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
                    "HTTP/1.1 302 Found\r\nLocation: http://second.example:{}/moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    dest.port()
                );
                let _ = s.write_all(r.as_bytes()).await;
                let _ = s.flush().await;
                let _ = s.shutdown().await;
            }
        });
        let c = GuardedHttpClient::build(
            reqwest::Client::builder()
                .resolve("allowed.example", origin_addr)
                .resolve("second.example", dest),
            // Both hops named, so it is the allowlist permitting the follow.
            HostGuard::restricted(vec!["allowed.example".into(), "second.example".into()]),
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
