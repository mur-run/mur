//! A2A dial from inside a runtime, for the `ask` tool: one JSON-RPC request
//! over the peer's unix socket, one result back.
//!
//! The runtime cannot use `mur-core`'s dialer (that crate pulls LanceDB into
//! every agent process), so this is the minimum the consult needs. The macOS
//! sandbox already allows connecting to a peer's `agent.sock` under
//! `<mur_home>/agents` (`sandbox/macos.rs`, `unix_socket_allow_paths`).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// Idle silence tolerated between frames from a peer that beats
/// (`turn/heartbeat` every 30 s), matching the CLI dialer's window.
pub const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Upper bound on one response line, as on the server side.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// The peer's socket and advertised proto, from its `running.lock`. Errors
/// name the agent: not running, no socket, or a proto too old for `method`.
pub fn peer_socket(mur_home: &Path, agent: &str, method: &str) -> Result<String> {
    mur_common::validate_agent_name(agent)
        .map_err(|e| anyhow!("invalid agent name {agent:?}: {e}"))?;
    let lock_path = mur_home.join("agents").join(agent).join("running.lock");
    let bytes = match std::fs::read(&lock_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("agent '{agent}' is not running (start it with `mur agent start {agent}`)")
        }
        Err(e) => return Err(e).with_context(|| format!("read {}", lock_path.display())),
    };
    let lock: mur_common::LockFile =
        serde_json::from_slice(&bytes).context("parse the peer's running.lock")?;
    let needed = mur_common::build::method_min_proto(method);
    if lock.proto_version < needed {
        bail!(
            "agent '{agent}' runs an older runtime (proto {}) that does not serve {method} (needs {needed}); \
             run `mur agent restart {agent}`",
            lock.proto_version
        );
    }
    lock.transports
        .unix_socket
        .ok_or_else(|| anyhow!("agent '{agent}' has no unix-socket transport"))
}

/// Send `method(params)` to the socket at `sock` and return the `result`.
/// Notifications (heartbeats, deltas) are skipped; each one re-arms the idle
/// timeout. A JSON-RPC error comes back as an `Err` carrying its message.
#[cfg(unix)]
pub async fn call(sock: &str, method: &str, params: Value, idle: Duration) -> Result<Value> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;
    use tokio_util::codec::{FramedRead, LinesCodec};

    let stream = tokio::time::timeout(idle, tokio::net::UnixStream::connect(sock))
        .await
        .map_err(|_| anyhow!("connect {sock}: timed out"))?
        .with_context(|| format!("connect {sock}"))?;
    let (read, mut write) = stream.into_split();
    let id = uuid::Uuid::now_v7().to_string();
    let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    write.write_all(format!("{req}\n").as_bytes()).await?;
    write.flush().await?;

    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
    loop {
        let line = tokio::time::timeout(idle, lines.next())
            .await
            .map_err(|_| anyhow!("{method}: no word from the peer for {}s", idle.as_secs()))?
            .ok_or_else(|| anyhow!("{method}: the peer closed the connection without answering"))?
            .context("read the peer's reply")?;
        let frame: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if frame.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            continue; // a notification
        }
        if let Some(err) = frame.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("{msg}");
        }
        return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
    }
}

#[cfg(not(unix))]
pub async fn call(_sock: &str, method: &str, _params: Value, _idle: Duration) -> Result<Value> {
    bail!("{method}: peer dial needs a unix socket, which this platform does not have")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A one-shot server: reads one request, sends a notification, then the
    /// reply produced by `answer`.
    async fn serve_once(
        dir: &Path,
        answer: impl FnOnce(&Value) -> Value + Send + 'static,
    ) -> String {
        let sock = dir.join("p.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (r, mut w) = s.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let beat = json!({ "jsonrpc": "2.0", "method": "turn/heartbeat", "params": {} });
            w.write_all(format!("{beat}\n").as_bytes()).await.unwrap();
            let reply = answer(&req);
            w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        });
        sock.display().to_string()
    }

    #[tokio::test]
    async fn the_result_is_returned_past_a_notification() {
        let td = tempfile::tempdir().unwrap();
        let sock = serve_once(td.path(), |req| {
            json!({ "jsonrpc": "2.0", "id": req["id"], "result": { "echo": req["params"]["x"] } })
        })
        .await;
        let out = call(&sock, "m", json!({ "x": 7 }), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out["echo"], 7);
    }

    #[tokio::test]
    async fn a_jsonrpc_error_carries_its_message() {
        let td = tempfile::tempdir().unwrap();
        let sock = serve_once(td.path(), |req| {
            json!({ "jsonrpc": "2.0", "id": req["id"], "error": { "code": -32011, "message": "consult refused: nope" } })
        })
        .await;
        let err = call(&sock, "m", json!({}), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("consult refused: nope"), "{err}");
    }

    #[test]
    fn a_peer_that_is_not_running_is_named() {
        let td = tempfile::tempdir().unwrap();
        let err = peer_socket(td.path(), "tech-writer", "channel/consult").unwrap_err();
        assert!(
            err.to_string().contains("'tech-writer' is not running"),
            "{err}"
        );
    }

    #[test]
    fn a_traversal_name_is_refused_before_any_read() {
        let td = tempfile::tempdir().unwrap();
        let err = peer_socket(td.path(), "../x", "channel/consult").unwrap_err();
        assert!(err.to_string().contains("invalid agent name"), "{err}");
    }
}
