use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The cc-proxy guarantee: a client built via `llm_client_builder()` reaches
/// its base_url DIRECTLY even when `HTTP_PROXY` points elsewhere — so the
/// per-server egress proxy / a debug cc-proxy never captures LLM traffic.
/// Without `.no_proxy()` this request would be routed to the dead proxy and
/// fail, so the test guards that the builder keeps `.no_proxy()`.
#[tokio::test]
async fn llm_client_builder_ignores_ambient_http_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = listener.accept().await {
            // Drain the request so the client's send completes, then reply
            // with an explicit close + flush + graceful shutdown. Without
            // this, dropping the socket right after write_all races the OS
            // flush and Windows aborts the connection (os error 10053).
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
            let _ = s.flush().await;
            let _ = s.shutdown().await;
        }
    });
    // reqwest reads proxy env at build time, so this needs the real
    // variable — see the guard's own docs for why one lock covers all.
    let _env = mur_common::test_env::EnvGuard::set([("HTTP_PROXY", "http://127.0.0.1:1")]);
    let client = llm_client_builder().build().unwrap();
    let resp = client.get(format!("http://{addr}/")).send().await;
    let resp = resp.expect("no_proxy client reaches base_url despite HTTP_PROXY");
    assert_eq!(resp.status(), 200);
}
