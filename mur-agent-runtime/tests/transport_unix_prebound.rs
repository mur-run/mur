#![cfg(unix)]
//! #2422: the supervisor binds its unix socket with std BEFORE the seal (and
//! before any tokio runtime exists), then serves it after. A long agent home
//! takes the `/tmp/mur-<uuid>.sock` fallback; the canonical path is a symlink to it.

use mur_agent_runtime::protocol::a2a_server::{
    Dispatcher, HandlerError, MethodHandler, RequestContext,
};
use mur_agent_runtime::socket_path::resolve_bind_target;
use mur_agent_runtime::transport::unix_socket::{bind_unix_std, serve_unix_listener_reporting};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

struct Ping;
#[async_trait::async_trait]
impl MethodHandler for Ping {
    async fn handle(
        &self,
        _: Option<serde_json::Value>,
        _: &RequestContext,
    ) -> Result<serde_json::Value, HandlerError> {
        Ok(json!({"pong": true}))
    }
}

#[test]
fn long_path_socket_bound_before_the_runtime_answers_through_its_symlink() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("x".repeat(120));
    let canonical = home.join("agent.sock");
    let uuid = format!("test-{}", std::process::id());
    let res = resolve_bind_target(&canonical, &uuid).unwrap();
    assert!(res.symlink_created, "a long path must take the fallback");
    assert_eq!(std::fs::read_link(&canonical).unwrap(), res.bind_path);

    // Pre-seal shape: std bind, no runtime yet.
    let listener = bind_unix_std(&res.bind_path).unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut d = Dispatcher::new();
        d.register("ping", Box::new(Ping));
        let (_tx, rx) = mpsc::channel(4);
        let bind = res.bind_path.clone();
        tokio::spawn(serve_unix_listener_reporting(
            Arc::new(d),
            listener,
            bind,
            rx,
            None,
        ));

        let stream = UnixStream::connect(&res.bind_path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string() + "\n";
        write.write_all(req.as_bytes()).await.unwrap();
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["result"]["pong"], true);
    });
    let _ = std::fs::remove_file(&res.bind_path);
}

#[test]
fn a_bind_that_cannot_succeed_fails_synchronously() {
    let tmp = TempDir::new().unwrap();
    let err = bind_unix_std(&tmp.path().join("missing").join("a.sock")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}
