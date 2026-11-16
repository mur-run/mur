use super::*;
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Both seals' port-gate errnos: SBPL's EPERM and Landlock's EACCES.
#[cfg(unix)]
const SEAL_ERRNOS: [i32; 2] = [libc::EPERM, libc::EACCES];

#[cfg(unix)]
#[test]
fn landlock_eacces_is_a_seal_denial_too() {
    for errno in SEAL_ERRNOS {
        let err = std::io::Error::from_raw_os_error(errno);
        let msg = upstream_dial_hint("shop", "127.0.0.1:3000".parse().unwrap(), &err)
            .unwrap_or_else(|| panic!("errno {errno} is the seal's port gate"));
        assert!(msg.contains("allow-port shop 3000"), "{msg}");
    }
}

#[cfg(unix)]
#[test]
fn a_seal_denied_dial_names_the_port_and_the_grant() {
    let eperm = std::io::Error::from_raw_os_error(libc::EPERM);
    let msg = upstream_dial_hint("shop", "127.0.0.1:52489".parse().unwrap(), &eperm)
        .expect("EPERM is the seal's port gate");
    assert!(msg.contains("port 52489"), "{msg}");
    assert!(
        msg.contains("mur agent perm allow-port shop 52489"),
        "{msg}"
    );
    // IPv6 literal: the port comes from the address, not the text.
    let msg = upstream_dial_hint("shop", "[::1]:8000".parse().unwrap(), &eperm).unwrap();
    assert!(msg.contains("allow-port shop 8000"), "{msg}");
}

/// #2418 acceptance: a live entry for `x.example.com` with no port, a
/// dial to `x.example.com:3000` the seal refuses → the tool call and the
/// client both get the port and the `allow-port` command.
/// Run per errno: Linux (Landlock EACCES) must reach the call and the
/// client exactly like macOS (SBPL EPERM).
#[cfg(unix)]
#[test]
fn a_closed_port_on_an_allowed_host_reaches_the_call_and_the_client() {
    use mur_common::egress_event::DenyReason;
    let pinned: SocketAddr = "203.0.113.7:3000".parse().unwrap();
    for errno in SEAL_ERRNOS {
        // Fresh Events per errno: the per-host throttle would mute the 2nd.
        let events = live::Events::new("gate");
        let call = events.in_flight("live", "task-1");
        let err = std::io::Error::from_raw_os_error(errno);
        let ev = report_dial_failure(&events, "gate", Some("live"), "x.example.com", pinned, &err)
            .unwrap_or_else(|| panic!("errno {errno}: a seal-refused dial raises an event"));
        assert_eq!(ev.reason, DenyReason::PortNotOpen);
        assert_eq!((ev.host.as_str(), ev.port), ("x.example.com", Some(3000)));
        assert_eq!(ev.task_id.as_deref(), Some("task-1"));
        let note = call.closed_port_note().expect("the call hears it");
        assert!(note.contains("port 3000"), "errno {errno}: {note}");
        assert!(
            note.contains("mur agent perm allow-port gate 3000"),
            "{note}"
        );
    }
    let events = live::Events::new("gate");

    // A refused connection is not the seal: no event, no note.
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    let other = events.in_flight("other", "task-2");
    assert!(
        report_dial_failure(
            &events,
            "gate",
            Some("other"),
            "y.example.com",
            pinned,
            &refused
        )
        .is_none()
    );
    assert!(other.closed_port_note().is_none());
}

#[test]
fn other_dial_failures_get_no_seal_hint() {
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    assert!(upstream_dial_hint("shop", "127.0.0.1:52489".parse().unwrap(), &refused).is_none());
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
        resp.contains("\r\nContent-Length: 0\r\n") && resp.contains("\r\nConnection: close\r\n"),
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
    let req = format!("GET {url} HTTP/1.1\r\nHost: x\r\nProxy-Authorization: Basic {cred}\r\n\r\n");
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
async fn a_revoked_token_is_denied_on_its_next_request() {
    let (up, _seen) = http_upstream().await;
    let proxy = start_egress_proxy("test").await.unwrap();
    let lease = proxy.lease_server("browser", vec!["127.0.0.1".to_string()], vec![], false);
    let token = lease.token().to_owned();
    drop(lease);
    let resp = http_via(proxy.addr, &token, &format!("http://{up}/")).await;
    assert!(resp.starts_with("HTTP/1.1 403"), "{resp}");
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

/// #1907: a denial fails now and emits one `egress/denied`; a runtime
/// grant lets the NEXT connection through with no restart.
#[tokio::test]
async fn denied_host_emits_event_and_a_grant_applies_to_later_connections() {
    let proxy = start_egress_proxy("shop").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    proxy.attach_events(tx);
    let token = proxy.register_server("browser", vec!["example.com".into()], vec![], false);

    let up = upstream().await;
    let denied = connect_via(proxy.addr, &token, &up.to_string()).await;
    assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
    let n = rx.recv().await.unwrap();
    assert_eq!(n["method"], mur_common::egress_event::DENIED_METHOD);
    assert_eq!(n["params"]["server"], "browser");
    assert_eq!(n["params"]["host"], "127.0.0.1");
    assert_eq!(n["params"]["reason"], "not_allowed");

    // Same host again inside the window: still denied, no second event.
    let again = connect_via(proxy.addr, &token, &up.to_string()).await;
    assert!(again.starts_with("HTTP/1.1 403"), "{again}");
    assert!(rx.try_recv().is_err());

    proxy.grant("browser", "127.0.0.1").unwrap();
    let up = upstream().await;
    let ok = connect_via(proxy.addr, &token, &up.to_string()).await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
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
