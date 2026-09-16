//! The MCP shim: stdio to a spawned CLI, unix socket to the agent.
//!
//! `--mcp-config` hands the CLI a command to spawn, so this runs as a child
//! of the CLI, which is a child of MUR. It therefore cannot reach
//! `GuardedToolCall` — a different process holds it — and it does not try.
//! The shim owns no tools, no policy and no vault. Every obligation stays on
//! the agent's side of the socket, which is what keeps the
//! one-execution-path test meaningful across the process split.
//!
//! See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.

use mur_mcp_proto::{Incoming, Request, read_incoming};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

/// Answers still owed to us, by request id: elicitations sent to the CLI and
/// method calls sent to the agent.
type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub async fn run(socket: std::path::PathBuf, task_id: String) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(&socket)
        .await
        .map_err(|e| anyhow::anyhow!("connect {}: {e}", socket.display()))?;
    let (sock_read, sock_write) = stream.into_split();
    let sock_write = Arc::new(Mutex::new(sock_write));

    let pending_calls: Pending = Default::default();
    let pending_elicits: Pending = Default::default();
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<Value>(16);

    // ── socket reader: method results, and prompts arriving mid-call ──
    {
        let pending_calls = pending_calls.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(sock_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("method").and_then(|m| m.as_str()) == Some("tool/approval_needed") {
                    let _ = prompt_tx.send(v).await;
                    continue;
                }
                if let Some(id) = v.get("id").and_then(id_key)
                    && let Some(tx) = pending_calls.lock().await.remove(&id)
                {
                    let _ = tx.send(v);
                }
            }
        });
    }

    // ── stdin reader: the CLI's requests, and answers to our elicitations ──
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<Request>(16);
    {
        let pending_elicits = pending_elicits.clone();
        // Blocking: stdin has no async form here, and this thread does
        // nothing but read, so it may block freely.
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            while let Some(inc) = read_incoming(&mut lock) {
                match inc {
                    Incoming::Request(r) => {
                        if req_tx.blocking_send(r).is_err() {
                            break;
                        }
                    }
                    Incoming::Response(resp) => {
                        let Some(id) = resp.get("id").and_then(id_key) else {
                            continue;
                        };
                        let pe = pending_elicits.clone();
                        let v = resp;
                        // The map is async-locked; hand the wake-up to the
                        // runtime rather than blocking this thread on it.
                        tokio::runtime::Handle::current().spawn(async move {
                            if let Some(tx) = pe.lock().await.remove(&id) {
                                let _ = tx.send(v);
                            }
                        });
                    }
                    Incoming::Unparseable(raw) => {
                        tracing::warn!(raw = %raw, "unparseable frame from the CLI");
                    }
                }
            }
        });
    }

    let mut next_id: u64 = 0;
    loop {
        tokio::select! {
            Some(req) = req_rx.recv() => {
                let resp = serve(&req, &task_id, &sock_write, &pending_calls, &mut next_id).await;
                write_stdout(&resp).await;
            }
            Some(prompt) = prompt_rx.recv() => {
                elicit(prompt, &sock_write, &pending_elicits, &mut next_id).await;
            }
            else => break,
        }
    }
    Ok(())
}

/// JSON-RPC ids are numbers or strings; both key the same map.
fn id_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

async fn write_stdout(v: &Value) {
    let mut out = tokio::io::stdout();
    let mut bytes = serde_json::to_vec(v).unwrap_or_default();
    bytes.push(b'\n');
    let _ = out.write_all(&bytes).await;
    let _ = out.flush().await;
}

async fn serve(
    req: &Request,
    task_id: &str,
    sock: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: &Pending,
    next_id: &mut u64,
) -> Value {
    let id = req.id.clone().unwrap_or(Value::Null);
    match req.method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                // The closure needs its type: `params` is `Option<Value>`
                // and inference has nothing else to go on here.
                "protocolVersion": req.params.as_ref()
                    .and_then(|p: &Value| p.get("protocolVersion").cloned())
                    .unwrap_or_else(|| json!("2025-11-25")),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mur", "version": env!("CARGO_PKG_VERSION") },
            }
        }),
        "tools/list" => match call_agent(sock, pending, next_id, "tools/list", json!({})).await {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": v["tools"]}}),
            Err(e) => rpc_error(id, &e),
        },
        "tools/call" => {
            let p = req.params.clone().unwrap_or(Value::Null);
            let args = p.get("arguments").cloned().unwrap_or(json!({}));
            let params = json!({
                "task_id": task_id,
                "name": p.get("name").cloned().unwrap_or(Value::Null),
                "arguments": args,
                // We can answer, because we can ask the CLI's user.
                "can_approve": true,
            });
            match call_agent(sock, pending, next_id, "tools/call", params).await {
                Ok(v) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [{"type": "text", "text": v["content"].as_str().unwrap_or("")}],
                        "isError": v["is_error"].as_bool().unwrap_or(false),
                    }
                }),
                // Fail closed and visibly: the model is told the call failed
                // rather than handed a fabricated empty success.
                Err(e) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("MUR agent unreachable: {e}")}],
                        "isError": true,
                    }
                }),
            }
        }
        _ => rpc_error(id, &format!("method not found: {}", req.method)),
    }
}

fn rpc_error(id: Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": message}})
}

async fn call_agent(
    sock: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: &Pending,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    *next_id += 1;
    let id = format!("shim-{next_id}");
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id.clone(), tx);
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let mut bytes = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    sock.lock()
        .await
        .write_all(&bytes)
        .await
        .map_err(|e| e.to_string())?;
    let v = rx
        .await
        .map_err(|_| "agent closed the connection".to_string())?;
    if let Some(err) = v.get("error") {
        return Err(err["message"].as_str().unwrap_or("agent error").to_string());
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// One approval prompt, asked of the CLI's user and answered back to the agent.
async fn elicit(
    prompt: Value,
    sock: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: &Pending,
    next_id: &mut u64,
) {
    let calls = prompt["params"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for c in calls {
        let Some(hitl_id) = c["hitl_id"].as_str().map(String::from) else {
            continue;
        };
        *next_id += 1;
        let id = format!("elicit-{next_id}");
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(id.clone(), tx);
        write_stdout(&json!({
            "jsonrpc": "2.0", "id": id, "method": "elicitation/create",
            "params": {
                "message": format!(
                    "MUR wants to run `{}`. Allow?",
                    c["tool_name"].as_str().unwrap_or("a tool")
                ),
                "requestedSchema": {
                    "type": "object",
                    "properties": { "allow": { "type": "boolean" } },
                    "required": ["allow"],
                },
            }
        }))
        .await;

        // Anything that is not an explicit accept-with-allow is a denial.
        // `decline` and `cancel` are denials; so is a malformed answer. The
        // gate's own timeout still runs underneath, so a CLI that never
        // replies is denied rather than hanging the turn.
        let answer = rx.await.ok();
        let allow = answer
            .as_ref()
            .map(|v| {
                v["result"]["action"] == json!("accept")
                    && v["result"]["content"]["allow"] == json!(true)
            })
            .unwrap_or(false);

        let mut bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": format!("hitl-{next_id}"),
            "method": "tool/hitl_respond",
            "params": { "hitl_id": hitl_id, "allow": allow },
        }))
        .unwrap_or_default();
        bytes.push(b'\n');
        let _ = sock.lock().await.write_all(&bytes).await;
    }
}
