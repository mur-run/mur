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

/// Whether `BroadAudited` deny-list hosts are actually blocked or only
/// logged. User-facing text is generated from this, never hard-coded (D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyEnforcement {
    Advisory,
    Enforced,
}

/// Flip to `Enforced` in the same PR that makes the deny overlay airtight.
pub const DENY_OVERLAY_ENFORCEMENT: DenyEnforcement = DenyEnforcement::Advisory;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::reqwest_guard::{host_allowed, host_matches_pattern};

mod dial;
mod http_forward;
mod lease;
mod live;

pub use lease::TokenLease;
pub use live::{GrantError, InFlight};

/// A registered per-server policy: either a `Restricted` allowlist, or a
/// `BroadAudited` allow-all-except-`deny` policy.
#[derive(Clone, Default)]
struct PolicyEntry {
    /// The MCP server this token was minted for: the key runtime grants and
    /// `egress/denied` events use (a respawned child gets a fresh token).
    server: String,
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
    events: live::Events,
}

impl EgressProxyHandle {
    /// Test-only handle with an empty registry at a fixed address (no listener).
    #[cfg(test)]
    pub fn for_test(addr: SocketAddr) -> Self {
        Self {
            addr,
            registry: Arc::new(Mutex::new(HashMap::new())),
            events: live::Events::new("test"),
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
        self.register_server("", allow_hosts, deny_hosts, broad)
    }

    /// [`Self::register_policy`] for a named MCP server, so later runtime
    /// grants ([`Self::grant`]) and denial events can find it.
    pub fn register_server(
        &self,
        server: &str,
        allow_hosts: Vec<String>,
        deny_hosts: Vec<String>,
        broad: bool,
    ) -> String {
        let token = uuid::Uuid::now_v7().simple().to_string();
        self.registry.lock().unwrap().insert(
            token.clone(),
            PolicyEntry {
                server: server.to_owned(),
                allow: allow_hosts,
                deny: deny_hosts,
                broad,
            },
        );
        token
    }
}

/// The bound-but-not-yet-serving half of the proxy: a std listener that
/// [`EgressAcceptLoop::spawn`] hands to tokio. Split from the bind because the
/// supervisor binds before the sandbox seals (so the port can be carved into
/// the kernel profile) but must not start a tokio runtime until after it: on
/// Linux the seal confines only the calling thread, so worker threads created
/// earlier would stay unconfined (GHSA-j8mv-27gx-x7xj).
pub struct EgressAcceptLoop {
    listener: std::net::TcpListener,
    agent: Arc<str>,
    registry: Registry,
    events: live::Events,
}

/// Bind `127.0.0.1:0` with no runtime and return the handle (with the chosen
/// ephemeral port) plus the accept loop to start later. Connections that
/// arrive in between wait in the kernel backlog.
/// `agent` is only used to word the fix in a seal-denied dial's log line.
pub fn bind_egress_proxy(agent: &str) -> std::io::Result<(EgressProxyHandle, EgressAcceptLoop)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let events = live::Events::new(agent);
    let handle = EgressProxyHandle {
        addr,
        registry: registry.clone(),
        events: events.clone(),
    };
    let accept = EgressAcceptLoop {
        listener,
        agent: Arc::from(agent),
        registry,
        events,
    };
    Ok((handle, accept))
}

/// Bind, spawn the accept loop, and return the handle. Needs a runtime.
/// Missing/unreadable connections are dropped.
pub async fn start_egress_proxy(agent: &str) -> std::io::Result<EgressProxyHandle> {
    let (handle, accept) = bind_egress_proxy(agent)?;
    accept.spawn()?;
    Ok(handle)
}

impl EgressAcceptLoop {
    /// Start serving. Must run inside a tokio runtime.
    pub fn spawn(self) -> std::io::Result<()> {
        self.listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(self.listener)?;
        accept_loop(listener, self.registry, self.events, self.agent);
        Ok(())
    }
}

fn accept_loop(listener: TcpListener, registry: Registry, events: live::Events, agent: Arc<str>) {
    let reg = registry;
    let ev = events;
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                continue;
            };
            let reg = reg.clone();
            let agent = agent.clone();
            let ev = ev.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(sock, reg, &ev, &agent).await {
                    tracing::warn!("egress proxy conn ended: {e}");
                }
            });
        }
    });
}

async fn handle_conn(
    mut client: TcpStream,
    registry: Registry,
    events: &live::Events,
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
        // A known server refused a host: tell clients so a human can grant
        // it. The connection still fails now — no pending state (D2).
        if let Some(e) = &entry {
            let port = target.rsplit_once(':').and_then(|(_, p)| p.parse().ok());
            events.denied(&e.server, host, port, e.broad, std::time::Instant::now());
        }
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    }
    tracing::info!(
        host,
        broad = entry.as_ref().map(|e| e.broad),
        "egress proxy {kind} ALLOW"
    );
    // SSRF screen + IP-pin: resolve the CONNECT target once, drop link-local /
    // unspecified (cloud-metadata) addresses, connect only to addresses from
    // that one resolution (no re-resolution → no DNS-rebinding window).
    // Loopback/LAN intentionally kept (local-first). Backstops the hostname
    // allow/deny list for browser-rendered sub-resource CONNECTs the gateway
    // never screened.
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
    if safe_addrs.is_empty() {
        tracing::info!(host, reason = "ssrf", "egress proxy {kind} DENY");
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    }
    // Try the screened addresses in order (#2541): a dual-stack host whose
    // first address has no route still connects through a later one.
    let mut upstream = match dial::connect_first(&safe_addrs).await {
        Ok((upstream, connected)) => {
            if safe_addrs.first() != Some(&connected) {
                tracing::info!(
                    host,
                    %connected,
                    skipped = safe_addrs.iter().position(|a| *a == connected),
                    "egress proxy connected through a later resolved address"
                );
            }
            upstream
        }
        Err(failures) => {
            let server = entry.as_ref().map(|e| e.server.as_str());
            for (addr, e) in &failures {
                report_dial_failure(events, agent, server, host, *addr, e);
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

/// Log a failed upstream dial; when the seal refused it on the port (#2418),
/// also raise it to the server's in-flight tool call and to clients, so the
/// failure names the port instead of surfacing as a bare tunnel error.
fn report_dial_failure(
    events: &live::Events,
    agent: &str,
    server: Option<&str>,
    host: &str,
    pinned: SocketAddr,
    err: &std::io::Error,
) -> Option<mur_common::egress_event::EgressDenied> {
    let Some(hint) = upstream_dial_hint(agent, pinned, err) else {
        tracing::warn!(host, %pinned, "egress proxy upstream dial failed: {err}");
        return None;
    };
    tracing::warn!(host, %pinned, "{hint}");
    events.port_closed(server?, host, pinned.port(), std::time::Instant::now())
}

/// The actionable line for an upstream dial the seal refused, or `None` for
/// any other failure.
///
/// The proxy runs inside the sealed runtime, so its dials face the same SBPL /
/// Landlock port gate as everything else: `CONNECT ALLOW` for an allowed host
/// on a port outside that set is still refused. The bare OS error named
/// neither the port nor the fix.
fn upstream_dial_hint(agent: &str, addr: SocketAddr, err: &std::io::Error) -> Option<String> {
    if !is_seal_denial(err) {
        return None;
    }
    let port = addr.port();
    let fix = mur_common::egress_event::port_fix(agent, port);
    Some(format!(
        "egress proxy: the sandbox refused the upstream dial to {addr} — port {port} \
         is not in this agent's outbound port set (the host is allowed; the port \
         is a separate grant). Fix: {fix}"
    ))
}

/// The errnos the seal's port gate answers with: SBPL (macOS) gives `EPERM`,
/// Landlock's `ConnectTcp` (Linux) gives `EACCES`. Same pair as
/// `ssh_bypass::probe`. Windows has no seal, so nothing there is one.
#[cfg(unix)]
fn is_seal_denial(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(libc::EPERM | libc::EACCES))
}

#[cfg(not(unix))]
fn is_seal_denial(_err: &std::io::Error) -> bool {
    false
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
mod tests;
