use super::*;
use crate::protocol::mcp_client::McpClient;
use crate::sandbox::SandboxPolicy;
use mur_common::agent::McpServerEntry;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn builds_jsonrpc_request() {
    let r = jsonrpc_request(3, "tools/list", serde_json::json!({}));
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], 3);
    assert_eq!(r["method"], "tools/list");
}

/// One request a [`Recorder`] saw: its request line and `Authorization`.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    auth: Option<String>,
}

type Reply = Arc<dyn Fn(&Seen, usize) -> String + Send + Sync>;

/// A loopback HTTP server that records every request and answers with
/// `reply(request, index)`. Zero recorded requests is the proof of zero
/// delivery.
struct Recorder {
    addr: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Recorder {
    async fn start(reply: Reply) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = Vec::new();
                let mut b = [0u8; 4096];
                // Headers, then a Content-Length body if any.
                loop {
                    let n = s.read(&mut b).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&b[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let req = Seen {
                    line: text.lines().next().unwrap_or("").to_string(),
                    auth: text.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("authorization")
                            .then(|| v.trim().to_string())
                    }),
                };
                let idx = {
                    let mut g = log.lock().unwrap();
                    g.push(req.clone());
                    g.len() - 1
                };
                let _ = s.write_all(reply(&req, idx).as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        Self { addr, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn http(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n{extra}\r\n{body}",
        body.len()
    )
}

const TOOLS_OK: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;

async fn client_for(
    url: &str,
    token_endpoint: Option<&str>,
    refresh: Option<RefreshCtx>,
) -> HttpMcpClient {
    let guard = HostGuard::exact(entry_hosts(url, token_endpoint));
    HttpMcpClient::connect(url, Some("SECRET".into()), refresh, guard)
        .await
        .unwrap()
}

fn refresh_ctx(token_endpoint: String) -> RefreshCtx {
    RefreshCtx {
        token_endpoint,
        client_id: "cid".into(),
        refresh_token: "RT".into(),
        access_token_ref: mur_common::secret::SecretRef::Env("MUR_TEST_UNUSED".into()),
    }
}

#[test]
fn entry_hosts_is_url_plus_token_endpoint_deduped() {
    assert_eq!(
        entry_hosts("https://mcp.example.com/mcp", None),
        vec!["mcp.example.com"]
    );
    assert_eq!(
        entry_hosts(
            "https://mcp.example.com/mcp",
            Some("https://auth.example.com/token")
        ),
        vec!["mcp.example.com", "auth.example.com"]
    );
    assert_eq!(
        entry_hosts(
            "https://MCP.example.com/mcp",
            Some("https://mcp.example.com/token")
        ),
        vec!["MCP.example.com".to_lowercase()]
    );
    assert!(entry_hosts("not a url", None).is_empty());
}

/// A redirect to a host outside the entry's set is not followed: the second
/// server records nothing, so the bearer never reaches it.
#[tokio::test]
async fn redirect_outside_the_entry_set_delivers_nothing() {
    let elsewhere = Recorder::start(Arc::new(|_, _| http("200 OK", "", TOOLS_OK))).await;
    let to = format!("http://localhost:{}/mcp", elsewhere.addr.port());
    let origin = Recorder::start(Arc::new(move |_, _| {
        http("307 Temporary Redirect", &format!("Location: {to}\r\n"), "")
    }))
    .await;
    let c = client_for(
        &format!("http://127.0.0.1:{}/mcp", origin.addr.port()),
        None,
        None,
    )
    .await;
    let err = c.list_tools().await.unwrap_err();
    assert!(err.to_string().contains("307"), "{err}");
    assert_eq!(origin.seen().len(), 1);
    assert!(
        elsewhere.seen().is_empty(),
        "redirect target got {:?}",
        elsewhere.seen()
    );
}

/// No loopback exemption: an entry on `localhost` redirected to `127.0.0.1`
/// is refused, because `127.0.0.1` is not a host that entry named.
#[tokio::test]
async fn redirect_to_unnamed_loopback_is_refused() {
    let lo = Recorder::start(Arc::new(|_, _| http("200 OK", "", TOOLS_OK))).await;
    let to = format!("http://127.0.0.1:{}/mcp", lo.addr.port());
    let origin = Recorder::start(Arc::new(move |_, _| {
        http("302 Found", &format!("Location: {to}\r\n"), "")
    }))
    .await;
    let c = client_for(
        &format!("http://localhost:{}/mcp", origin.addr.port()),
        None,
        None,
    )
    .await;
    assert!(c.list_tools().await.is_err());
    assert!(lo.seen().is_empty(), "loopback target got {:?}", lo.seen());
}

/// Positive control for the refusal below: a refresh to the entry's own
/// token endpoint is delivered and the retry carries the new token.
#[tokio::test]
async fn refresh_to_own_token_endpoint_is_delivered() {
    let srv = Recorder::start(Arc::new(|req: &Seen, _| {
        if req.line.starts_with("POST /token") {
            http(
                "200 OK",
                "",
                r#"{"access_token":"NEW","token_type":"Bearer"}"#,
            )
        } else if req.auth.as_deref() == Some("Bearer NEW") {
            http("200 OK", "", TOOLS_OK)
        } else {
            http("401 Unauthorized", "", "")
        }
    }))
    .await;
    let base = format!("http://127.0.0.1:{}", srv.addr.port());
    let token = format!("{base}/token");
    let c = client_for(
        &format!("{base}/mcp"),
        Some(&token),
        Some(refresh_ctx(token.clone())),
    )
    .await;
    c.list_tools().await.unwrap();
    let lines: Vec<_> = srv.seen().into_iter().map(|s| s.line).collect();
    assert!(
        lines.iter().any(|l| l.starts_with("POST /token")),
        "{lines:?}"
    );
    assert_eq!(
        srv.seen().last().unwrap().auth.as_deref(),
        Some("Bearer NEW")
    );
}

/// Entry B's refresh token cannot reach entry A's issuer: B's guard holds
/// only B's hosts, so a refresh aimed at A's token endpoint is never sent.
#[tokio::test]
async fn refresh_to_another_entrys_token_endpoint_is_refused() {
    let issuer_a = Recorder::start(Arc::new(|_, _| {
        http(
            "200 OK",
            "",
            r#"{"access_token":"STOLEN","token_type":"Bearer"}"#,
        )
    }))
    .await;
    let entry_b = Recorder::start(Arc::new(|_, _| http("401 Unauthorized", "", ""))).await;
    let b = format!("http://127.0.0.1:{}", entry_b.addr.port());
    let a_token = format!("http://localhost:{}/token", issuer_a.addr.port());
    let c = client_for(
        &format!("{b}/mcp"),
        Some(&format!("{b}/token")),
        Some(refresh_ctx(a_token)),
    )
    .await;
    assert!(matches!(c.list_tools().await, Err(McpError::Unauthorized)));
    assert!(
        issuer_a.seen().is_empty(),
        "issuer A got {:?}",
        issuer_a.seen()
    );
}

/// The authenticated session DELETE goes out through the guarded client.
#[tokio::test]
async fn shutdown_delete_goes_through_the_guard() {
    let srv = Recorder::start(Arc::new(|_, _| {
        http("200 OK", "Mcp-Session-Id: s-1\r\n", TOOLS_OK)
    }))
    .await;
    let c = client_for(
        &format!("http://127.0.0.1:{}/mcp", srv.addr.port()),
        None,
        None,
    )
    .await;
    let _ = c.list_tools().await;
    c.shutdown().await;
    let del = srv
        .seen()
        .into_iter()
        .find(|s| s.line.starts_with("DELETE /mcp"))
        .expect("DELETE sent");
    assert_eq!(del.auth.as_deref(), Some("Bearer SECRET"));
}

/// `Off` refuses a remote entry at connect, with the reason, and sends nothing.
#[tokio::test]
async fn off_refuses_remote_connect() {
    let srv = Recorder::start(Arc::new(|_, _| http("200 OK", "", TOOLS_OK))).await;
    let entry = McpServerEntry {
        name: "remote".into(),
        url: Some(format!("http://127.0.0.1:{}/mcp", srv.addr.port())),
        ..Default::default()
    };
    let off = SandboxPolicy {
        net_allow_ports: Some(vec![]),
        net_allow_hosts: Some(vec![]),
        net_loopback_allowed: false,
        ..Default::default()
    };
    assert!(off.network_off());
    let err = McpClient::connect(&entry, &off, None)
        .await
        .err()
        .expect("Off must refuse");
    assert!(err.to_string().contains("mode is `off`"), "{err}");
    assert!(srv.seen().is_empty());

    // ProxyOnly looks the same on ports and hosts but allows loopback carve-outs.
    let proxy_only = SandboxPolicy {
        net_loopback_allowed: true,
        ..off
    };
    assert!(!proxy_only.network_off());
    assert!(!SandboxPolicy::default().network_off(), "Unrestricted");
}
