use mur_agent_runtime::protocol::a2a_server::Dispatcher;
use mur_agent_runtime::transport::stdio::serve_stdio;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
use tokio::sync::mpsc;

#[tokio::test]
async fn serves_dispatch_on_stdio_pipe() {
    let (mut client, server) = duplex(65536);
    let (server_read, server_write) = tokio::io::split(server);
    let (notif_tx, notif_rx) = mpsc::channel(16);
    let dispatcher = {
        use async_trait::async_trait;
        use mur_agent_runtime::protocol::a2a_server::{
            HandlerError, MethodHandler, RequestContext,
        };
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
        let mut d = Dispatcher::new();
        d.register("ping", Box::new(Ping));
        d
    };
    tokio::spawn(serve_stdio(
        Arc::new(dispatcher),
        server_read,
        server_write,
        notif_rx,
    ));
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string() + "\n";
    client.write_all(req.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["result"]["pong"], true);
    drop(notif_tx);
}
