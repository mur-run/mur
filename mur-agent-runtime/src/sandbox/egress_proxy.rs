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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::reqwest_guard::{host_allowed, host_matches_pattern};

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
pub async fn start_egress_proxy() -> std::io::Result<EgressProxyHandle> {
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
            tokio::spawn(async move {
                if let Err(e) = handle_conn(sock, reg).await {
                    tracing::debug!("egress proxy conn ended: {e}");
                }
            });
        }
    });
    Ok(EgressProxyHandle { addr, registry })
}

async fn handle_conn(mut client: TcpStream, registry: Registry) -> std::io::Result<()> {
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
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();

    // MVP supports CONNECT (https) only; plain http forwarding is a follow-up.
    let Some(target) = request_line
        .strip_prefix("CONNECT ")
        .and_then(|r| r.split(' ').next())
    else {
        client
            .write_all(b"HTTP/1.1 501 Not Implemented\r\n\r\n")
            .await?;
        return Ok(());
    };
    let token = lines
        .find_map(parse_proxy_auth_token)
        .and_then(decode_basic_user);

    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
    let entry = token
        .as_deref()
        .and_then(|t| registry.lock().unwrap().get(t).cloned());
    let allowed = match &entry {
        Some(e) if e.broad => !e.deny.iter().any(|p| host_matches_pattern(host, p)),
        Some(e) => host_allowed(host, &e.allow),
        None => false,
    };

    if !allowed {
        tracing::info!(
            host,
            broad = entry.as_ref().map(|e| e.broad),
            "egress proxy CONNECT DENY"
        );
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    }
    tracing::info!(
        host,
        broad = entry.as_ref().map(|e| e.broad),
        "egress proxy CONNECT ALLOW"
    );
    let mut upstream = TcpStream::connect(target).await?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
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
        assert_eq!(parse_proxy_auth_token("proxy-authorization: Bearer xyz"), None);
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

    #[tokio::test]
    async fn allowed_host_tunnels_denied_host_403() {
        let up = upstream().await;
        let proxy = start_egress_proxy().await.unwrap();

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
        let proxy = start_egress_proxy().await.unwrap();

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
}
