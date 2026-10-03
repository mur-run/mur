//! Loopback egress proxy for per-MCP-server host allowlisting. ADVISORY
//! enforcement: a cooperating child honors `HTTP_PROXY` and is constrained to
//! its allowlist; a child that ignores `HTTP_PROXY` can still reach the network
//! directly (the OS sandbox here filters by port, not host). Airtight
//! containment is future work (Linux netns + a macOS pre-fork launcher).
//!
//! One shared proxy serves all policied servers. Each child is handed
//! `HTTP_PROXY=http://<token>:x@127.0.0.1:<port>`; the proxy reads the token
//! from `Proxy-Authorization: Basic …` on the `CONNECT host:port` request,
//! looks up that server's allowlist, and tunnels only if the host is allowed.
//! Plain `http://` proxy requests (absolute-form `GET http://host/path`) get
//! the same token, allowlist, and SSRF checks, then are forwarded in
//! origin-form (#1677).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::reqwest_guard::{host_allowed, host_matches_pattern};

mod http_forward;

/// A registered per-server policy: either a `Restricted` allowlist, or a
/// `BroadAudited` allow-all-except-`deny` policy.
#[derive(Clone, Default)]
struct PolicyEntry {
    allow: Vec<String>,
    deny: Vec<String>,
    /// `true` for `BroadAudited` (allow-all-except-`deny`); `false` for
    /// `Restricted` (allow-only-`allow`).
    broad: bool,
}

type Registry = Arc<Mutex<HashMap<String, PolicyEntry>>>;

#[derive(Clone)]
pub struct EgressProxyHandle {
    pub addr: SocketAddr,
    registry: Registry,
}

impl EgressProxyHandle {
    /// Test-only handle with an empty registry at a fixed address (no listener).
    #[cfg(test)]
    pub fn for_test(addr: SocketAddr) -> Self {
        Self {
            addr,
            registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a per-server allowlist (`Restricted`); returns the bearer
    /// token to embed in the child's `HTTP_PROXY` credentials.
    pub fn register(&self, allow_hosts: Vec<String>) -> String {
        self.register_policy(allow_hosts, Vec::new(), false)
    }

    /// Register a per-server policy: `broad = true` is `BroadAudited`
    /// (allow-all-except-`deny_hosts`); `broad = false` is `Restricted`
    /// (allow-only-`allow_hosts`). Returns the bearer token to embed in the
    /// child's `HTTP_PROXY` credentials.
    pub fn register_policy(
        &self,
        allow_hosts: Vec<String>,
        deny_hosts: Vec<String>,
        broad: bool,
    ) -> String {
        let token = uuid::Uuid::now_v7().simple().to_string();
        self.registry.lock().unwrap().insert(
            token.clone(),
            PolicyEntry {
                allow: allow_hosts,
                deny: deny_hosts,
                broad,
            },
        );
        token
    }
}

/// Bind `127.0.0.1:0`, spawn the accept loop, and return the handle (with the
/// chosen ephemeral port). Missing/unreadable connections are dropped.
/// `agent` is only used to word the fix in a seal-denied dial's log line.
pub async fn start_egress_proxy(agent: &str) -> std::io::Result<EgressProxyHandle> {
    let agent: Arc<str> = Arc::from(agent);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let reg = registry.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                continue;
            };
            let reg = reg.clone();
            let agent = agent.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(sock, reg, &agent).await {
                    tracing::warn!("egress proxy conn ended: {e}");
                }
            });
        }
    });
    Ok(EgressProxyHandle { addr, registry })
}

async fn handle_conn(
    mut client: TcpStream,
    registry: Registry,
    agent: &str,
) -> std::io::Result<()> {
    // Read the request head (request line + headers, up to the blank line).
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
        if client.read(&mut byte).await? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let request = http_forward::classify(&head);
    let kind = request.kind();
    let (target, forward_head) = match request {
        http_forward::Classified::Connect { target } => (target, None),
        http_forward::Classified::Forward { target, head } => (target, Some(head)),
        http_forward::Classified::Unsupported(reason) => {
            tracing::info!(reason, "egress proxy request UNSUPPORTED");
            client
                .write_all(http_forward::unsupported_response(reason).as_bytes())
                .await?;
            return Ok(());
        }
    };
    let target = target.as_str();
    let token = head
        .lines()
        .skip(1)
        .find_map(parse_proxy_auth_token)
        .and_then(decode_basic_user);

    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);

    // No credentials at all → *challenge*, don't refuse. Clients that take a
    // proxy address but carry credentials separately (Chromium: --proxy-server
    // + a config-file username/password) send a bare CONNECT first and only
    // replay Basic auth after a 407. A present-but-unknown token, or a known
    // token for a disallowed host, is a plain 403 below — no retry helps.
    let Some(token) = token else {
        tracing::info!(host, "egress proxy {kind} CHALLENGE");
        client
            .write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"mur\"\r\n\
                  Content-Length: 0\r\n\
                  Connection: close\r\n\
                  \r\n",
            )
            .await?;
        return Ok(());
    };
    let entry = registry.lock().unwrap().get(&token).cloned();
    let allowed = match &entry {
        Some(e) if e.broad => !e.deny.iter().any(|p| host_matches_pattern(host, p)),
        Some(e) => host_allowed(host, &e.allow),
        None => false,
    };

    if !allowed {
        tracing::info!(
            host,
            broad = entry.as_ref().map(|e| e.broad),
            "egress proxy {kind} DENY"
        );
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    }
    tracing::info!(
        host,
        broad = entry.as_ref().map(|e| e.broad),
        "egress proxy {kind} ALLOW"
    );
    // SSRF screen + IP-pin: resolve the CONNECT target once, drop link-local /
    // unspecified (cloud-metadata) addresses, connect to the pinned SocketAddr
    // (no re-resolution → no DNS-rebinding window). Loopback/LAN intentionally
    // kept (local-first). Backstops the hostname allow/deny list for
    // browser-rendered sub-resource CONNECTs the gateway never screened.
    // Resolve + SSRF-screen off the async worker thread: to_socket_addrs() is a
    // blocking DNS call and this one proxy serves every sandboxed child, so an
    // inline blocking resolve could starve tokio workers under concurrent slow
    // DNS. (Mirrors the gateway's spawn_blocking screen.)
    let target_owned = target.to_string();
    let safe_addrs = tokio::task::spawn_blocking(move || {
        super::reqwest_guard::screened_socket_addrs(&target_owned)
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or_default();
    // Pin the first screened address. (Trade-off: a multi-A-record/dual-stack
    // host loses std's automatic try-all-addresses fallback — accepted for the
    // no-rebinding guarantee.)
    let Some(pinned) = safe_addrs.into_iter().next() else {
        tracing::info!(host, reason = "ssrf", "egress proxy {kind} DENY");
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    };
    let mut upstream = match TcpStream::connect(pinned).await {
        Ok(u) => u,
        Err(e) => {
            match upstream_dial_hint(agent, &pinned.to_string(), &e) {
                Some(hint) => tracing::warn!(host, %pinned, "{hint}"),
                None => tracing::warn!(host, %pinned, "egress proxy upstream dial failed: {e}"),
            }
            // Answer the request rather than hanging up, so the client
            // reports a proxy failure instead of a bare reset.
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
            return Ok(());
        }
    };
    match forward_head {
        // Plain HTTP (#1677): the upstream's own status line is the reply.
        Some(head) => upstream.write_all(head.as_bytes()).await?,
        None => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?
        }
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// The actionable line for an upstream dial the seal refused, or `None` for
/// any other failure.
///
/// The proxy runs inside the sealed runtime, so its dials face the same SBPL /
/// Landlock port gate as everything else: `CONNECT ALLOW` for an allowed host
/// on a port outside that set still ends in EPERM (os error 1). The bare OS
/// error named neither the port nor the fix.
fn upstream_dial_hint(agent: &str, addr: &str, err: &std::io::Error) -> Option<String> {
    if err.raw_os_error() != Some(1) {
        return None;
    }
    let port = addr.rsplit_once(':').map(|(_, p)| p).unwrap_or(addr);
    Some(format!(
        "egress proxy: the sandbox refused the upstream dial to {addr} — port {port} \
         is not in this agent's outbound port set (the host is allowed; the port \
         is a separate grant). Fix: `mur agent perm allow-port {agent} {port}`, \
         then restart the agent"
    ))
}

/// Decode `Basic base64(user:pass)` and return `user` (our token); the password
/// half is a throwaway `x`.
fn decode_basic_user(b64: &str) -> Option<String> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    Some(s.split_once(':').map(|(u, _)| u).unwrap_or(&s).to_string())
}

/// Extract the base64 credential from a `Proxy-Authorization: Basic <b64>`
/// request-head line. HTTP header names and the auth scheme token are
/// case-insensitive (RFC 7230/7235), and hyper/reqwest emit the header name
/// **lowercase** — a case-sensitive `strip_prefix("Proxy-Authorization: Basic ")`
/// silently dropped the token, so every CONNECT resolved to `entry = None` and
/// was DENIED. Match name + scheme case-insensitively; the base64 value itself
/// stays case-sensitive.
fn parse_proxy_auth_token(line: &str) -> Option<&str> {
    let (name, value) = line.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("proxy-authorization") {
        return None;
    }
    let value = value.trim_start();
    let scheme = "basic ";
    match value.get(..scheme.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(scheme) => Some(&value[scheme.len()..]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn a_seal_denied_dial_names_the_port_and_the_grant() {
        let eperm = std::io::Error::from_raw_os_error(1);
        let msg = upstream_dial_hint("shop", "127.0.0.1:52489", &eperm)
            .expect("EPERM is the seal's port gate");
        assert!(msg.contains("port 52489"), "{msg}");
        assert!(
            msg.contains("mur agent perm allow-port shop 52489"),
            "{msg}"
        );
        // IPv6 literal: the port is still the part after the last colon.
        let msg = upstream_dial_hint("shop", "[::1]:8000", &eperm).unwrap();
        assert!(msg.contains("allow-port shop 8000"), "{msg}");
    }

    #[test]
    fn other_dial_failures_get_no_seal_hint() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert!(upstream_dial_hint("shop", "127.0.0.1:52489", &refused).is_none());
    }

    #[tokio::test]
    async fn a_failed_upstream_dial_answers_502_instead_of_hanging_up() {
        // Bind then drop: nothing listens there, so the dial is refused.
        let dead = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let proxy = start_egress_proxy("shop").await.unwrap();
        let token = proxy.register(vec!["127.0.0.1".into()]);
        let resp = connect_via(proxy.addr, &token, &dead.to_string()).await;
        assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
    }

    #[test]
    fn proxy_auth_token_is_header_case_insensitive() {
        let b64 = base64::engine::general_purpose::STANDARD.encode("mytoken:x");
        // hyper/reqwest emit the header name (and may vary the scheme) in
        // lowercase — all of these must yield the same base64 credential.
        for line in [
            format!("Proxy-Authorization: Basic {b64}"),
            format!("proxy-authorization: Basic {b64}"),
            format!("proxy-authorization: basic {b64}"),
            format!("PROXY-AUTHORIZATION: BASIC {b64}"),
        ] {
            assert_eq!(
                parse_proxy_auth_token(&line),
                Some(b64.as_str()),
                "failed to parse: {line}"
            );
            assert_eq!(
                decode_basic_user(parse_proxy_auth_token(&line).unwrap()).as_deref(),
                Some("mytoken")
            );
        }
        // Non-matching lines yield None.
        assert_eq!(parse_proxy_auth_token("Host: example.com:443"), None);
        assert_eq!(
            parse_proxy_auth_token("proxy-authorization: Bearer xyz"),
            None
        );
    }

    /// A trivial upstream that accepts one connection (so an allowed CONNECT can
    /// complete its TCP handshake to it).
    async fn upstream() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = l.accept().await;
            // Hold briefly so the proxy's connect succeeds.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        addr
    }

    async fn connect_via(proxy: SocketAddr, token: &str, target: &str) -> String {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("{token}:x"));
        let req = format!("CONNECT {target} HTTP/1.1\r\nProxy-Authorization: Basic {cred}\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    /// CONNECT with no `Proxy-Authorization` at all: the shape a client that
    /// has a proxy address but no credentials yet (Chromium) sends first.
    async fn connect_bare(proxy: SocketAddr, target: &str) -> String {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        let req = format!("CONNECT {target} HTTP/1.1\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 512];
        let n = s.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    /// F4: a bare CONNECT must be *challenged* (407 + Proxy-Authenticate), not
    /// silently refused (403) — Chromium only replays the config-file
    /// credentials after a 407. Present-but-unknown tokens stay 403.
    #[tokio::test]
    async fn bare_connect_is_challenged_with_407() {
        let up = upstream().await;
        let proxy = start_egress_proxy("test").await.unwrap();
        let _token = proxy.register(vec!["127.0.0.1".to_string()]);

        let resp = connect_bare(proxy.addr, &up.to_string()).await;
        assert!(
            resp.starts_with("HTTP/1.1 407 Proxy Authentication Required\r\n"),
            "bare CONNECT is 407: {resp}"
        );
        assert!(
            resp.contains("\r\nProxy-Authenticate: Basic realm=\"mur\"\r\n"),
            "407 carries the Basic challenge: {resp}"
        );
        assert!(
            resp.contains("\r\nContent-Length: 0\r\n")
                && resp.contains("\r\nConnection: close\r\n"),
            "407 is self-delimiting: {resp}"
        );
    }

    #[tokio::test]
    async fn allowed_host_tunnels_denied_host_403() {
        let up = upstream().await;
        let proxy = start_egress_proxy("test").await.unwrap();

        // Allowlist the upstream's loopback host → CONNECT establishes.
        let token = proxy.register(vec!["127.0.0.1".to_string()]);
        let ok = connect_via(proxy.addr, &token, &up.to_string()).await;
        assert!(
            ok.starts_with("HTTP/1.1 200"),
            "allowed CONNECT establishes: {ok}"
        );

        // Token whose allowlist excludes the target → 403.
        let token2 = proxy.register(vec!["example.com".to_string()]);
        let denied = connect_via(proxy.addr, &token2, &up.to_string()).await;
        assert!(
            denied.starts_with("HTTP/1.1 403"),
            "denied CONNECT is 403: {denied}"
        );

        // Unknown token → 403.
        let bad = connect_via(proxy.addr, "not-a-real-token", &up.to_string()).await;
        assert!(
            bad.starts_with("HTTP/1.1 403"),
            "unknown token is 403: {bad}"
        );
    }

    #[tokio::test]
    async fn broad_audited_allows_all_except_deny() {
        let up = upstream().await;
        let proxy = start_egress_proxy("test").await.unwrap();

        // BroadAudited: deny only "blocked.example"; everything else allowed,
        // including a host never mentioned in any list.
        let token = proxy.register_policy(vec![], vec!["blocked.example".to_string()], true);

        // "anything.example" isn't on any list but broad-audited allows it —
        // dial the real loopback upstream so the CONNECT can complete.
        let allowed = connect_via(proxy.addr, &token, &up.to_string()).await;
        assert!(
            allowed.starts_with("HTTP/1.1 200"),
            "broad-audited allows a host not on any list: {allowed}"
        );

        // "blocked.example" is in deny_hosts → 403 even though broad allows
        // everything else.
        let denied = connect_via(proxy.addr, &token, "blocked.example:443").await;
        assert!(
            denied.starts_with("HTTP/1.1 403"),
            "broad-audited still denies deny_hosts: {denied}"
        );
    }

    /// An upstream that records the request head it receives and answers one
    /// fixed response (#1677).
    async fn http_upstream() -> (SocketAddr, tokio::sync::oneshot::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && s.read(&mut b).await.unwrap() == 1 {
                head.push(b[0]);
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
                .await
                .unwrap();
        });
        (addr, rx)
    }

    /// Absolute-form plain-HTTP request through the proxy; returns the whole
    /// reply (the proxy closes after one response).
    async fn http_via(proxy: SocketAddr, token: &str, url: &str) -> String {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("{token}:x"));
        let req =
            format!("GET {url} HTTP/1.1\r\nHost: x\r\nProxy-Authorization: Basic {cred}\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn allowed_plain_http_is_forwarded_in_origin_form() {
        let (up, seen) = http_upstream().await;
        let proxy = start_egress_proxy("test").await.unwrap();
        let token = proxy.register(vec!["127.0.0.1".to_string()]);
        let resp = http_via(proxy.addr, &token, &format!("http://{up}/page?q=1")).await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
        assert!(resp.ends_with("hello"), "{resp}");
        let head = seen.await.unwrap();
        assert!(head.starts_with("GET /page?q=1 HTTP/1.1\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("proxy-authorization"),
            "credentials must not leak upstream: {head}"
        );
    }

    #[tokio::test]
    async fn plain_http_is_denied_and_challenged_like_connect() {
        let (up, _seen) = http_upstream().await;
        let proxy = start_egress_proxy("test").await.unwrap();
        let token = proxy.register(vec!["example.com".to_string()]);
        let denied = http_via(proxy.addr, &token, &format!("http://{up}/")).await;
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");

        let mut s = TcpStream::connect(proxy.addr).await.unwrap();
        s.write_all(format!("GET http://{up}/ HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let bare = String::from_utf8_lossy(&out);
        assert!(bare.starts_with("HTTP/1.1 407"), "{bare}");
    }

    #[tokio::test]
    async fn origin_form_request_gets_a_501_that_says_why() {
        let proxy = start_egress_proxy("test").await.unwrap();
        let mut s = TcpStream::connect(proxy.addr).await.unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let resp = String::from_utf8_lossy(&out);
        assert!(resp.starts_with("HTTP/1.1 501"), "{resp}");
        assert!(resp.contains("absolute-form"), "{resp}");
    }

    #[tokio::test]
    async fn broad_audited_link_local_target_is_ssrf_denied() {
        // A broad-audited grant (allow-all-except-deny) must STILL refuse a CONNECT
        // to a link-local / cloud-metadata IP — the SSRF screen backstops the
        // hostname allow/deny list.
        let proxy = start_egress_proxy("test").await.unwrap();
        let token = proxy.register_policy(vec![], vec![], true); // broad, empty deny
        let resp = connect_via(proxy.addr, &token, "169.254.169.254:80").await;
        assert!(
            resp.starts_with("HTTP/1.1 403"),
            "link-local CONNECT must be 403 (SSRF screen), got: {resp}"
        );
    }
}
