//! Red test for the HITL self-approval gap (open items 013579d5e612 /
//! 82b410879b5e).
//!
//! The agent's own socket (`<mur_home>/agents/<name>/agent.sock`) is inside
//! the sandbox's unix-socket allow list, and `serve_unix` discards the peer
//! credentials it reads. So any same-uid process — including a tool the agent
//! spawned — can connect, read `tool/approval_needed`, and answer
//! `tool/hitl_respond` with `allow: true` for the very call it is waiting on.
//!
//! This drives the real pieces end to end: `serve_unix` + the real
//! `HitlRespondHandler` + the real `BatchGate`, wired to the same fallback
//! notifier the supervisor uses (`sock_notif_tx`). The only thing the
//! "attacker" does is what any socket client can do.
//!
//! Expected (secure) behavior: an approval from an unauthenticated socket
//! caller must NOT release the gate, and must not use it up either — the
//! human's answer, carrying the home's approval token, still lands. A deny
//! needs no token.

use super::HitlRespondHandler;
use crate::hitl::HitlApprovals;
use crate::hitl::authority::ApprovalAuthority;
use crate::hitl::batch::{BatchGate, PendingCall};
use crate::protocol::a2a_server::Dispatcher;
use crate::transport::unix_socket::serve_unix;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Long enough that a timeout can never be mistaken for a decision.
const GATE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on how long the attacker waits for the prompt.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

const TOKEN: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

struct Rig {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: tokio::net::unix::OwnedWriteHalf,
    gate: tokio::task::JoinHandle<std::collections::HashMap<String, crate::hitl::HitlDecision>>,
    hitl_id: String,
    _tmp: tempfile::TempDir,
}

/// The real socket + handler + gate, with one dangerous call pending and its
/// prompt already read off the socket by the caller.
async fn rig() -> Rig {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("agent.sock");

    let approvals: HitlApprovals = Default::default();
    let (notif_tx, notif_rx) = tokio::sync::mpsc::channel::<Value>(16);

    let mut d = Dispatcher::new();
    d.register(
        "tool/hitl_respond",
        Box::new(HitlRespondHandler {
            pending_approvals: approvals.clone(),
            authority: ApprovalAuthority::new(Some(TOKEN.into()), true),
        }),
    );
    let d = Arc::new(d);
    let bind = sock.clone();
    tokio::spawn(async move {
        let _ = serve_unix(d, bind, notif_rx).await;
    });
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Any process that can connect to the socket.
    let stream = UnixStream::connect(&sock).await.expect("connect");
    let (read, write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    // The gate asks for approval of a dangerous call, exactly as
    // GuardedToolCall does when no per-task sink is routed.
    let gate_approvals = approvals.clone();
    let gate_notifier = notif_tx.clone();
    let gate = tokio::spawn(async move {
        let gate = BatchGate {
            task_id: "t-self-approve",
            timeout: GATE_TIMEOUT,
            approvals: &gate_approvals,
            notifier: &gate_notifier,
            store: None,
        };
        gate.resolve(vec![PendingCall {
            call_id: "c1".into(),
            step_id: "s1".into(),
            tool_name: "bash".into(),
            tool_input: json!({"command": "rm -rf ~/important"}),
            action_hash: "h1".into(),
        }])
        .await
    });

    let hitl_id = tokio::time::timeout(READ_TIMEOUT, async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            if v["method"] == "tool/approval_needed" {
                return v["params"]["calls"][0]["hitl_id"]
                    .as_str()
                    .map(String::from);
            }
        }
        None
    })
    .await
    .expect("approval prompt never reached the socket")
    .expect("prompt carried no hitl_id");

    Rig {
        lines,
        write,
        gate,
        hitl_id,
        _tmp: tmp,
    }
}

impl Rig {
    /// Send one `tool/hitl_respond` and return its JSON-RPC response.
    async fn respond(&mut self, id: u64, params: Value) -> Value {
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tool/hitl_respond", "params": params,
        });
        self.write
            .write_all(format!("{req}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(READ_TIMEOUT, async {
            while let Some(line) = self.lines.next_line().await.unwrap() {
                let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                if v["id"] == json!(id) {
                    return v;
                }
            }
            Value::Null
        })
        .await
        .expect("no response to tool/hitl_respond")
    }
}

#[tokio::test]
async fn socket_caller_cannot_approve_its_own_pending_call() {
    let mut r = rig().await;
    let id = r.hitl_id.clone();

    // No token, then a wrong one: both refused with their own code.
    for (n, params) in [
        (1, json!({ "hitl_id": id, "allow": true, "surface": "hub" })),
        (
            2,
            json!({ "hitl_id": id, "allow": true, "surface": "hub", "approval_token": "0".repeat(64) }),
        ),
    ] {
        let resp = r.respond(n, params).await;
        assert_eq!(resp["error"]["code"], -32013, "{resp}");
        let msg = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("approval refused"), "{msg}");
        assert!(!msg.contains(TOKEN), "the refusal must not echo the token");
    }
    assert!(
        !r.gate.is_finished(),
        "SELF-APPROVAL: an unauthenticated socket caller settled the HITL gate"
    );

    // The refusal did not use up the gate: the human's answer still lands.
    let resp = r
        .respond(
            3,
            json!({ "hitl_id": id, "allow": true, "surface": "hub", "approval_token": TOKEN }),
        )
        .await;
    assert!(resp["error"].is_null(), "{resp}");
    let decisions = r.gate.await.unwrap();
    let d = decisions.get("c1").expect("gate returned no decision");
    assert!(d.allow, "a token-bearing allow must release the gate");
    assert_eq!(d.surface.as_deref(), Some("hub"));
}

#[tokio::test]
async fn anyone_may_deny_without_a_token() {
    let mut r = rig().await;
    let id = r.hitl_id.clone();
    let resp = r
        .respond(
            1,
            json!({ "hitl_id": id, "allow": false, "surface": "hub" }),
        )
        .await;
    assert!(resp["error"].is_null(), "{resp}");
    let decisions = r.gate.await.unwrap();
    assert!(!decisions.get("c1").expect("no decision").allow);
}
