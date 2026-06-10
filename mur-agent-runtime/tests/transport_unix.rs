#![cfg(unix)]

use mur_agent_runtime::protocol::a2a_server::Dispatcher;
use mur_agent_runtime::transport::unix_socket::serve_unix;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[tokio::test]
async fn roundtrip_over_unix_socket() {
    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("a.sock");
    let (notif_tx, notif_rx) = mpsc::channel(16);
    let dispatcher = {
        use async_trait::async_trait;
        use mur_agent_runtime::protocol::a2a_server::{
            HandlerError, MethodHandler, RequestContext,
        };
        let mut d = Dispatcher::new();
        struct Ping;
        #[async_trait]
        impl MethodHandler for Ping {
            async fn handle(
                &self,
                _: Option<serde_json::Value>,
                _ctx: &RequestContext,
            ) -> Result<serde_json::Value, HandlerError> {
                Ok(json!({"pong": true}))
            }
        }
        d.register("ping", Box::new(Ping));
        d
    };
    let path = sock_path.clone();
    let dispatcher = Arc::new(dispatcher);
    tokio::spawn(async move {
        let _ = serve_unix(dispatcher, path, notif_rx).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (read, mut write) = stream.into_split();
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string() + "\n";
    write.write_all(req.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["result"]["pong"], true);
    drop(notif_tx);
}

/// Regression for the streaming-isolation fix (finding #14): a request-scoped
/// notification emitted via `RequestContext::notifier` must reach ONLY the
/// connection that issued the request, never other connected clients.
#[tokio::test]
async fn request_scoped_notification_routed_to_issuing_connection_only() {
    use std::time::Duration;
    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("iso.sock");
    let (notif_tx, notif_rx) = mpsc::channel(16);
    let dispatcher = {
        use async_trait::async_trait;
        use mur_agent_runtime::protocol::a2a_server::{
            HandlerError, MethodHandler, RequestContext,
        };
        let mut d = Dispatcher::new();
        // Emits a per-connection notification (the routing the real
        // `message/delta` path uses) before returning its response.
        struct Emit;
        #[async_trait]
        impl MethodHandler for Emit {
            async fn handle(
                &self,
                params: Option<serde_json::Value>,
                ctx: &RequestContext,
            ) -> Result<serde_json::Value, HandlerError> {
                let task_id = params
                    .as_ref()
                    .and_then(|p| p.get("task_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(n) = &ctx.notifier {
                    let _ = n
                        .send(json!({
                            "jsonrpc": "2.0",
                            "method": "test/delta",
                            "params": { "task_id": task_id, "text": "hi" },
                        }))
                        .await;
                }
                Ok(json!({"ok": true}))
            }
        }
        d.register("emit", Box::new(Emit));
        d
    };
    let path = sock_path.clone();
    let dispatcher = Arc::new(dispatcher);
    tokio::spawn(async move {
        let _ = serve_unix(dispatcher, path, notif_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Two independent connections to the same agent.
    let (read_a, mut write_a) = UnixStream::connect(&sock_path).await.unwrap().into_split();
    let (read_b, _write_b) = UnixStream::connect(&sock_path).await.unwrap().into_split();
    let mut reader_a = BufReader::new(read_a);
    let mut reader_b = BufReader::new(read_b);

    // A issues the request → handler emits a notification on A's sink.
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "emit", "params": {"task_id": "A"}})
        .to_string()
        + "\n";
    write_a.write_all(req.as_bytes()).await.unwrap();

    // A receives its own (stamped) notification AND the response, in either order.
    let mut saw_delta = false;
    let mut saw_resp = false;
    for _ in 0..4 {
        let mut l = String::new();
        match tokio::time::timeout(Duration::from_millis(500), reader_a.read_line(&mut l)).await {
            Ok(Ok(n)) if n > 0 => {
                let v: serde_json::Value = serde_json::from_str(l.trim()).unwrap();
                if v.get("method").and_then(|m| m.as_str()) == Some("test/delta") {
                    assert_eq!(v["params"]["task_id"], "A");
                    saw_delta = true;
                }
                if v.get("id") == Some(&json!(1)) {
                    saw_resp = true;
                }
            }
            _ => break,
        }
        if saw_delta && saw_resp {
            break;
        }
    }
    assert!(
        saw_delta,
        "issuing connection A must receive its notification"
    );
    assert!(saw_resp, "issuing connection A must receive its response");

    // B issued nothing and must receive NO request-scoped notification.
    let mut line_b = String::new();
    let r = tokio::time::timeout(Duration::from_millis(300), reader_b.read_line(&mut line_b)).await;
    assert!(
        r.is_err() || line_b.is_empty(),
        "connection B must not receive A's notification, got: {line_b:?}"
    );

    drop(notif_tx);
}
