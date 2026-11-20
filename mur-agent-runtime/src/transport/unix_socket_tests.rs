//! #2488 through the real listener: a peer that passes `accepts_from` may
//! talk to the agent, but `tools/*` is served only to the agent's own
//! lineage. This test process is the "agent" (`self_pid`), so a connection
//! it opens is `SelfAgent`; pointing `self_pid` elsewhere makes the same
//! connection a stranger.

use super::*;
use crate::protocol::a2a_server::MethodHandler;
use async_trait::async_trait;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

struct Ping;

#[async_trait]
impl MethodHandler for Ping {
    async fn handle(&self, _p: Option<Value>, _c: &RequestContext) -> Result<Value, HandlerError> {
        Ok(json!("pong"))
    }
}

/// Stands in for the real `tools/*` handlers: same gate call, no runner.
struct Tools(&'static str);

#[async_trait]
impl MethodHandler for Tools {
    async fn handle(&self, _p: Option<Value>, c: &RequestContext) -> Result<Value, HandlerError> {
        c.require_tool_access(self.0)?;
        Ok(json!("ran"))
    }
}

const TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

async fn serve(self_pid: u32) -> (tempfile::TempDir, PathBuf) {
    let mut d = Dispatcher::new().with_owner_gate(
        "alice",
        crate::hitl::authority::ApprovalAuthority::new(Some(TOKEN.into()), true),
    );
    d.register("secret/set", Box::new(Ping));
    d.register("ping", Box::new(Ping));
    d.register("tools/list", Box::new(Tools("tools/list")));
    d.register("tools/call", Box::new(Tools("tools/call")));
    // Short path: sun_path is ~104 bytes on macOS.
    let dir = tempfile::Builder::new()
        .prefix("t2488")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("a.sock");
    let policy = Arc::new(AcceptPolicy {
        accepts_from: vec!["*".into()],
        agents_dir: dir.path().join("agents"),
        self_pid,
    });
    let (_tx, rx) = mpsc::channel(1);
    tokio::spawn(serve_unix_gated(
        Arc::new(d),
        sock.clone(),
        rx,
        Some(policy),
    ));
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (dir, sock)
}

async fn call(sock: &PathBuf, method: &str) -> Value {
    call_with(sock, method, json!({})).await
}

async fn call_with(sock: &PathBuf, method: &str, params: Value) -> Value {
    let s = tokio::net::UnixStream::connect(sock).await.unwrap();
    let (r, mut w) = s.into_split();
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    w.write_all(format!("{req}\n").as_bytes()).await.unwrap();
    let mut lines = BufReader::new(r).lines();
    loop {
        let line = lines.next_line().await.unwrap().expect("socket closed");
        let v: Value = serde_json::from_str(&line).unwrap();
        if v.get("id") == Some(&json!(1)) {
            return v;
        }
    }
}

#[tokio::test]
async fn a_peer_admitted_by_an_open_list_cannot_run_tools() {
    // pid 1 is never this process or its ancestor-chain match.
    let (_dir, sock) = serve(1).await;
    assert_eq!(call(&sock, "ping").await["result"], json!("pong"));
    for m in ["tools/list", "tools/call"] {
        let v = call(&sock, m).await;
        assert_eq!(v["error"]["code"], json!(-32011), "{m}: {v}");
        assert!(v.get("result").is_none(), "{m}: {v}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn the_agents_own_process_still_runs_tools() {
    let (_dir, sock) = serve(std::process::id()).await;
    for m in ["tools/list", "tools/call"] {
        assert_eq!(call(&sock, m).await["result"], json!("ran"), "{m}");
    }
}

/// #2488: an owner method over the real listener. Neither a peer nor the
/// agent's own lineage may call it bare; the owner's proof is admitted.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn owner_methods_need_the_owner_proof_from_any_socket_peer() {
    for self_pid in [1, std::process::id()] {
        let (_dir, sock) = serve(self_pid).await;
        let v = call(&sock, "secret/set").await;
        assert_eq!(
            v["error"]["code"],
            json!(-32011),
            "self_pid {self_pid}: {v}"
        );
        let p = json!({
            mur_common::owner_call::PARAM:
                mur_common::owner_call::proof(TOKEN, "alice", "secret/set")
        });
        assert_eq!(
            call_with(&sock, "secret/set", p).await["result"],
            json!("pong")
        );
    }
}
