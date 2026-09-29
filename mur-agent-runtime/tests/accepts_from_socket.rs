// Peer pids are only readable on these two (see `peer_info`); elsewhere every
// caller is `Unknown`, which the unit tests cover.
#![cfg(any(target_os = "linux", target_os = "macos"))]
//! Open item 013579d5e612 (gap 14): the A2A socket must enforce the profile's
//! `accepts_from`. Drives the real `serve_unix_gated` with real peer
//! credentials — the test process itself is the caller, and is made to look
//! like a running agent by writing a `running.lock` carrying its own pid.

use async_trait::async_trait;
use mur_agent_runtime::communication_policy::AcceptPolicy;
use mur_agent_runtime::protocol::a2a_server::{
    Dispatcher, HandlerError, MethodHandler, RequestContext,
};
use mur_agent_runtime::transport::unix_socket::serve_unix_gated;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// A pid no real process holds, standing in for "this agent" so the test
/// process is never mistaken for self.
const NOT_US: u32 = 0x7fff_fff0;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

struct Ping;
#[async_trait]
impl MethodHandler for Ping {
    async fn handle(&self, _: Option<Value>, _: &RequestContext) -> Result<Value, HandlerError> {
        Ok(json!({"pong": true}))
    }
}

/// Pretend the test process is a running agent called `name`.
fn register_as_agent(agents: &Path, name: &str) {
    let home = agents.join(name);
    std::fs::create_dir_all(&home).unwrap();
    let lock = json!({
        "schema": 1, "uuid": "u", "name": name, "pid": std::process::id(), "ppid": 0,
        "started_at": "t", "binary_version": "v",
        "transports": {"stdio": false, "unix_socket": null, "tcp": null, "webhook": null},
        "card_digest": "d", "capabilities": []
    });
    std::fs::write(home.join("running.lock"), lock.to_string()).unwrap();
}

async fn ping(tmp: &TempDir, agents: PathBuf, accepts_from: &[&str]) -> Value {
    let sock = tmp.path().join("a.sock");
    let mut d = Dispatcher::new();
    d.register("ping", Box::new(Ping));
    let policy = Arc::new(AcceptPolicy {
        accepts_from: accepts_from.iter().map(|s| s.to_string()).collect(),
        agents_dir: agents,
        self_pid: NOT_US,
    });
    let (_notif_tx, notif_rx) = tokio::sync::mpsc::channel(4);
    let bind = sock.clone();
    tokio::spawn(async move {
        let _ = serve_unix_gated(Arc::new(d), bind, notif_rx, Some(policy)).await;
    });
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (read, mut write) = UnixStream::connect(&sock).await.unwrap().into_split();
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(READ_TIMEOUT, BufReader::new(read).read_line(&mut line))
        .await
        .expect("no reply")
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn an_unlisted_agent_is_refused_with_communication_denied() {
    let tmp = TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    register_as_agent(&agents, "stranger");
    let resp = ping(&tmp, agents, &["notify_*"]).await;
    assert_eq!(resp["error"]["code"], -32011, "{resp}");
    assert!(resp["result"].is_null(), "{resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stranger"),
        "{resp}"
    );
}

#[tokio::test]
async fn a_listed_agent_gets_through() {
    let tmp = TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    register_as_agent(&agents, "notify_a");
    let resp = ping(&tmp, agents, &["notify_*"]).await;
    assert_eq!(resp["result"]["pong"], true, "{resp}");
}

#[tokio::test]
async fn the_user_is_not_subject_to_accepts_from() {
    let tmp = TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    let resp = ping(&tmp, agents, &[]).await;
    assert_eq!(resp["result"]["pong"], true, "{resp}");
}
