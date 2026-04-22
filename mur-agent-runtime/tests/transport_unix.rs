#![cfg(unix)]

use mur_agent_runtime::protocol::a2a_server::Dispatcher;
use mur_agent_runtime::transport::unix_socket::serve_unix;
use serde_json::json;
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
        use mur_agent_runtime::protocol::a2a_server::{HandlerError, MethodHandler};
        let mut d = Dispatcher::new();
        struct Ping;
        #[async_trait]
        impl MethodHandler for Ping {
            async fn handle(
                &self,
                _: Option<serde_json::Value>,
            ) -> Result<serde_json::Value, HandlerError> {
                Ok(json!({"pong": true}))
            }
        }
        d.register("ping", Box::new(Ping));
        d
    };
    let path = sock_path.clone();
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
