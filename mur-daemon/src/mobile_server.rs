//! LAN WebSocket endpoint for the MUR mobile app (P1).
//!
//! Binds a LAN-reachable address (default `0.0.0.0:9430`, override with
//! `MUR_MOBILE_PORT` / `MUR_MOBILE_BIND`) and serves
//! [`mur_common::mobile::MOBILE_WS_PATH`]. Flow:
//!
//! 1. Phone connects and sends [`ClientFrame::Hello`] with the one-time
//!    pairing token (from the QR) + its Ed25519 public key. On a matching
//!    token the key is recorded as a paired device and we reply
//!    [`ServerFrame::Paired`].
//! 2. Subsequent [`ClientFrame::Envelope`] frames are Ed25519-verified against
//!    the paired key, the inner A2A `JsonRpcRequest` is dialed to the agent via
//!    `mur_core::a2a_dial`, and the reply is returned as a `mobile.reply`
//!    event.
//! 3. Each turn is also mirrored to `~/.mur/agents/<agent>/mobile-events.jsonl`
//!    — the sink the Hub tails to show the same conversation (P1 #3b wires the
//!    Hub renderer).
//!
//! Security for P1: pairing token + per-message Ed25519 signature. TLS and the
//! off-LAN relay path land in P4; full `trusted_peers` profile integration is a
//! follow-up (the paired-device store here is the daemon-side equivalent).

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use base64::Engine as _;
use chrono::Utc;
use mur_common::a2a::{JsonRpcRequest, Message as A2aMessage, MessagePart};
use mur_common::bridge::envelope::verify_envelope_with_pubkey;
use mur_common::mobile::{ClientFrame, MOBILE_WS_PATH, ServerFrame};
use mur_core::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Ports, bind address, token paths, and the default agent live in
// `mur_core::mobile` so the daemon and the `mur agent pair` CLI agree.

#[derive(Clone)]
struct MobileState {
    mur_home: PathBuf,
    pair_token: String,
    paired: Arc<Mutex<HashSet<String>>>,
    /// Broadcast channel used to push `channel.updated` events to all
    /// connected phones while they're online.
    chan_tx: tokio::sync::broadcast::Sender<String>,
}

impl MobileState {
    fn is_paired(&self, pubkey: &str) -> bool {
        self.paired
            .lock()
            .map(|s| s.contains(pubkey))
            .unwrap_or(false)
    }

    fn add_paired(&self, pubkey: &str) {
        let mut changed = false;
        if let Ok(mut set) = self.paired.lock() {
            changed = set.insert(pubkey.to_string());
            if changed {
                let all: Vec<String> = set.iter().cloned().collect();
                // Shared store with the relay transport (mur-core), so a device
                // paired here is recognized over relay and vice versa.
                if let Err(e) = mur_core::mobile::persist_paired_devices(&self.mur_home, &all) {
                    tracing::warn!(error = %e, "mobile: persist paired devices failed");
                }
            }
        }
        if changed {
            tracing::info!(pubkey = %pubkey, "mobile: paired new device");
        }
    }
}

/// Spawn the mobile WebSocket server as a background tokio task (best-effort).
pub fn spawn(mur_home: PathBuf, pair_token: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run_server(mur_home, pair_token).await {
            eprintln!("murmurd mobile-server error: {e:#}");
        }
    })
}

async fn run_server(mur_home: PathBuf, pair_token: String) -> Result<()> {
    let port = mur_core::mobile::mobile_port();
    let bind = mur_core::mobile::mobile_bind();
    let addr: std::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("parse mobile bind {bind}:{port}"))?;

    let paired = mur_core::mobile::load_paired_devices(&mur_home);

    let (chan_tx, _chan_rx) = tokio::sync::broadcast::channel::<String>(256);
    {
        let tx = chan_tx.clone();
        let home = mur_home.clone();
        std::thread::spawn(move || {
            match mur_channel::watch::watch_channels(&home, move |channel_id| {
                let _ = tx.send(channel_id);
            }) {
                Ok(w) => std::mem::forget(w),
                Err(e) => tracing::warn!("mobile channel watcher failed: {e:#}"),
            }
        });
    }

    let state = MobileState {
        mur_home,
        pair_token,
        paired: Arc::new(Mutex::new(paired)),
        chan_tx,
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind mobile server to {addr}"))?;
    eprintln!("murmurd mobile-server listening on {addr}{MOBILE_WS_PATH}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the axum router for the mobile endpoint (shared by the server and tests).
fn router(state: MobileState) -> Router {
    Router::new()
        .route(MOBILE_WS_PATH, get(ws_handler))
        .with_state(state)
}

async fn ws_handler(State(state): State<MobileState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: MobileState) {
    // 1. Pairing handshake.
    let (pubkey, agent) = match recv_text(&mut socket).await {
        Some(txt) => match serde_json::from_str::<ClientFrame>(&txt) {
            Ok(ClientFrame::Hello {
                pubkey,
                token,
                agent,
            }) => {
                if token != state.pair_token {
                    let _ = send_frame(&mut socket, &reject("invalid pairing token")).await;
                    return;
                }
                state.add_paired(&pubkey);
                let agent = resolve_agent(&state.mur_home, &agent);
                (pubkey, agent)
            }
            _ => {
                let _ = send_frame(&mut socket, &reject("expected hello")).await;
                return;
            }
        },
        None => return,
    };

    if send_frame(
        &mut socket,
        &ServerFrame::Paired {
            agent: agent.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }

    // 2. Application loop. Audio stream state is per-connection (one utterance
    //    at a time; a new AudioStreamStart resets the accumulator).
    let mut audio_buf: Vec<u8> = Vec::new();
    let mut chan_rx = state.chan_tx.subscribe();

    loop {
        let txt = tokio::select! {
            txt = recv_text(&mut socket) => {
                match txt {
                    Some(t) => t,
                    None => break,
                }
            }
            Ok(channel_id) = chan_rx.recv() => {
                let _ = send_frame(
                    &mut socket,
                    &ServerFrame::Event {
                        name: "channel.updated".into(),
                        payload: serde_json::json!({ "channel_id": channel_id }),
                    },
                ).await;
                continue;
            }
        };
        let frame = match serde_json::from_str::<ClientFrame>(&txt) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "mobile: bad client frame");
                continue;
            }
        };

        match frame {
            ClientFrame::Hello { .. } => {
                // Duplicate hello after pairing — ignore silently.
            }

            ClientFrame::Envelope { envelope } => {
                // Auth: the envelope's key must be the paired key and the
                // signature must verify against it.
                if envelope.bridge_pubkey_multibase != pubkey
                    || !state.is_paired(&pubkey)
                    || verify_envelope_with_pubkey(&envelope, &pubkey).is_err()
                {
                    let _ = send_frame(&mut socket, &reject("unauthorized")).await;
                    continue;
                }

                let req: JsonRpcRequest = match serde_json::from_slice(&envelope.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "mobile: bad payload");
                        continue;
                    }
                };

                let method = req.method.clone();
                let params = req.params.clone().unwrap_or(Value::Null);
                // v4c: an authoritative HITL approval rides THIS signed envelope
                // (verified just above), so the gate-releasing write only fires for
                // a frame we proved came from the paired device — never unsigned.
                if method == mur_common::mobile::HITL_RESPOND_METHOD {
                    if let Some((channel_id, hitl_id)) = mur_core::mobile::respond_hitl_from_params(
                        state.mur_home.as_path(),
                        &params,
                    ) {
                        let _ = send_frame(
                            &mut socket,
                            &ServerFrame::Event {
                                name: "hitl.ack".to_string(),
                                payload: json!({ "hitl_id": hitl_id, "channel_id": channel_id }),
                            },
                        )
                        .await;
                    }
                } else {
                    let user_text = extract_user_text(req.params.as_ref());
                    if !handle_agent_turn(&mut socket, &state, &agent, &user_text, method, params)
                        .await
                    {
                        break;
                    }
                }
            }

            ClientFrame::AudioStreamStart { sample_rate } => {
                audio_buf.clear();
                tracing::debug!(sample_rate, "mobile: audio stream start");
            }

            ClientFrame::AudioChunk { data } => {
                match base64::engine::general_purpose::STANDARD.decode(&data) {
                    Ok(bytes) => audio_buf.extend_from_slice(&bytes),
                    Err(e) => tracing::warn!(error = %e, "mobile: bad audio chunk base64"),
                }
            }

            ClientFrame::ChannelQuery {
                op,
                channel_id,
                since_seq,
            } => {
                let home = state.mur_home.clone();
                let payload = mur_core::mobile::channel_query(&home, &op, channel_id, since_seq)
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "mobile: channel_query failed");
                        serde_json::Value::Array(vec![])
                    });
                let _ = send_frame(
                    &mut socket,
                    &ServerFrame::ChannelData {
                        op: op.clone(),
                        payload,
                    },
                )
                .await;
            }

            ClientFrame::AudioStreamEnd => {
                tracing::debug!(bytes = audio_buf.len(), "mobile: audio stream end → STT");
                let pcm = std::mem::take(&mut audio_buf);
                let home = state.mur_home.clone();

                // STT: whisper.cpp (blocking) → authoritative transcript.
                let outcome =
                    tokio::task::spawn_blocking(move || crate::stt_sink::transcribe(&home, &pcm))
                        .await
                        .unwrap_or(crate::stt_sink::SttOutcome::Empty);

                let transcript_text = match outcome {
                    crate::stt_sink::SttOutcome::Text(t) => t,
                    crate::stt_sink::SttOutcome::Empty => {
                        tracing::debug!("mobile: STT no speech; skipping turn");
                        continue;
                    }
                    crate::stt_sink::SttOutcome::ModelsMissing => {
                        // Honest feedback instead of a silent drop: tell the user
                        // how to install the voice model. The phone renders
                        // `mobile.reply` events as agent chat bubbles.
                        let hint = format!(
                            "語音模型尚未安裝。請在 Mac 上執行 `mur agent voice {agent} download`（約 1.4 GB），完成後重啟 daemon 再用語音對話。"
                        );
                        tracing::info!("mobile: STT models missing — sent install hint to phone");
                        mirror(
                            state.mur_home.as_path(),
                            &agent,
                            "mobile.reply",
                            &json!({ "text": hint }),
                        );
                        let _ = send_frame(
                            &mut socket,
                            &ServerFrame::Event {
                                name: "mobile.reply".to_string(),
                                payload: json!({ "text": hint }),
                            },
                        )
                        .await;
                        continue;
                    }
                };

                // Send whisper result to phone so it can override the on-device
                // SFSpeech partial transcript with the authoritative text.
                let _ = send_frame(
                    &mut socket,
                    &ServerFrame::Transcript {
                        text: transcript_text.clone(),
                        is_final: true,
                    },
                )
                .await;

                // Dial agent using the authoritative transcript text.
                let msg = A2aMessage {
                    role: "user".to_string(),
                    parts: vec![MessagePart::Text {
                        text: transcript_text.clone(),
                    }],
                };
                let params = {
                    let mut m = serde_json::Map::new();
                    m.insert("agent".to_string(), Value::String(agent.clone()));
                    m.insert(
                        "message".to_string(),
                        serde_json::to_value(&msg).unwrap_or(Value::Null),
                    );
                    Value::Object(m)
                };
                if !handle_agent_turn(
                    &mut socket,
                    &state,
                    &agent,
                    &transcript_text,
                    "message/send".to_string(),
                    params,
                )
                .await
                {
                    break;
                }
            }
        }
    }
}

/// Dial the agent, mirror both sides, send reply + TTS audio to the phone.
/// Returns `false` if the WebSocket connection died (caller should break).
async fn handle_agent_turn(
    socket: &mut WebSocket,
    state: &MobileState,
    agent: &str,
    user_text: &str,
    method: String,
    params: Value,
) -> bool {
    mirror(
        state.mur_home.as_path(),
        agent,
        "mobile.transcript",
        &json!({
            "role": "user",
            "text": user_text,
            "final": true,
        }),
    );

    // v4c: capture the explicit target channel before `params` is moved into the
    // dial; `None` lets the persist resolve the agent's latest/new channel.
    let channel_id = params
        .get("channel_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let home = state.mur_home.clone();
    let agent_c = agent.to_string();
    let dialed = tokio::task::spawn_blocking(move || {
        dial_method(&home, &agent_c, &method, params, DialMode::Auto)
    })
    .await;

    let reply_text = match dialed {
        Ok(Ok(value)) => extract_reply_text(&value),
        Ok(Err(e)) => format!("[error] {e}"),
        Err(e) => format!("[error] dial task: {e}"),
    };

    mirror(
        state.mur_home.as_path(),
        agent,
        "mobile.reply",
        &json!({ "text": reply_text }),
    );
    if !reply_text.starts_with("[error]") {
        mur_core::mobile::persist_mobile_exchange_into(
            state.mur_home.as_path(),
            agent,
            channel_id.as_deref(),
            user_text,
            &reply_text,
        );
    }
    if send_frame(
        socket,
        &ServerFrame::Event {
            name: "mobile.reply".to_string(),
            payload: json!({ "text": reply_text }),
        },
    )
    .await
    .is_err()
    {
        return false;
    }

    // TTS: synthesize reply and stream audio back (skipped if models absent).
    if !reply_text.starts_with("[error]") {
        let home = state.mur_home.clone();
        let text = reply_text.clone();
        if let Some((b64, sample_rate)) =
            tokio::task::spawn_blocking(move || crate::tts_sink::synthesize(&home, &text))
                .await
                .unwrap_or(None)
        {
            let _ = send_frame(
                socket,
                &ServerFrame::AudioChunk {
                    base64: b64,
                    sample_rate,
                    done: true,
                },
            )
            .await;
        }
    }
    true
}

// ── helpers ──────────────────────────────────────────────────────────────

async fn recv_text(socket: &mut WebSocket) -> Option<String> {
    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(Message::Text(t)) => return Some(t.to_string()),
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue, // ping/pong/binary
            Err(_) => return None,
        }
    }
    None
}

async fn send_frame(socket: &mut WebSocket, frame: &ServerFrame) -> Result<(), axum::Error> {
    let txt = serde_json::to_string(frame).unwrap_or_default();
    socket.send(Message::Text(txt.into())).await
}

fn reject(reason: &str) -> ServerFrame {
    ServerFrame::Rejected {
        reason: reason.to_string(),
    }
}

fn resolve_agent(home: &Path, requested: &str) -> String {
    let name = if requested.trim().is_empty() {
        mur_core::mobile::DEFAULT_MOBILE_AGENT
    } else {
        requested
    };
    canonicalize_agent_name(home, name)
}

/// Extract the user's text from an `agent/send` request's params.
fn extract_user_text(params: Option<&Value>) -> String {
    params
        .and_then(|p| p.get("message"))
        .and_then(|m| m.get("parts"))
        .and_then(|parts| parts.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Pull the agent's reply text out of a dialed A2A result (a `Task`).
fn extract_reply_text(value: &Value) -> String {
    if let Some(messages) = value.get("messages").and_then(|m| m.as_array()) {
        for message in messages.iter().rev() {
            let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if (role == "agent" || role == "assistant")
                && let Some(parts) = message.get("parts").and_then(|p| p.as_array())
            {
                let text: String = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
        return text.to_string();
    }
    value.to_string()
}

/// Append a turn to the per-agent mirror log the Hub tails.
fn mirror(home: &Path, agent: &str, name: &str, payload: &Value) {
    let dir = home.join("agents").join(agent);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, "mobile: mirror dir");
        return;
    }
    let line = json!({
        "ts": Utc::now().to_rfc3339(),
        "name": name,
        "payload": payload,
    })
    .to_string();
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("mobile-events.jsonl"))
    {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => tracing::warn!(error = %e, "mobile: mirror write"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use mur_common::a2a::{JsonRpcRequest, Message as A2aMessage, MessagePart};
    use mur_common::bridge::envelope::{SignedEnvelope, sign_payload};
    use mur_common::identity::{AgentIdentity, encode_pubkey};
    use std::net::SocketAddr;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    const TEST_TOKEN: &str = "test-token";
    type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    async fn start_server(token: &str) -> (SocketAddr, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let (chan_tx, _) = tokio::sync::broadcast::channel(8);
        let state = MobileState {
            mur_home: home.clone(),
            pair_token: token.to_string(),
            paired: Arc::new(Mutex::new(HashSet::new())),
            chan_tx,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, tmp)
    }

    async fn connect(addr: SocketAddr) -> Ws {
        let url = format!("ws://{addr}{MOBILE_WS_PATH}");
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .unwrap();
        ws
    }

    async fn send_frame(ws: &mut Ws, frame: &ClientFrame) {
        let txt = serde_json::to_string(frame).unwrap();
        ws.send(WsMessage::Text(txt.into())).await.unwrap();
    }

    async fn recv_server(ws: &mut Ws) -> ServerFrame {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("recv timeout")
                .expect("stream ended")
                .expect("ws error");
            if let WsMessage::Text(t) = msg {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    fn make_envelope(id: &AgentIdentity, text: &str) -> SignedEnvelope {
        let msg = A2aMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                text: text.to_string(),
            }],
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "agent".to_string(),
            serde_json::Value::String("mur".to_string()),
        );
        params.insert("message".to_string(), serde_json::to_value(&msg).unwrap());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "message/send".to_string(),
            params: Some(serde_json::Value::Object(params)),
        };
        let payload = serde_json::to_vec(&req).unwrap();
        sign_payload(payload, id, 1)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_bad_token() {
        let (addr, _tmp) = start_server(TEST_TOKEN).await;
        let mut ws = connect(addr).await;
        send_frame(
            &mut ws,
            &ClientFrame::Hello {
                pubkey: "zBogus".to_string(),
                token: "wrong".to_string(),
                agent: "mur".to_string(),
            },
        )
        .await;
        assert!(matches!(
            recv_server(&mut ws).await,
            ServerFrame::Rejected { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepts_pairing() {
        let (addr, _tmp) = start_server(TEST_TOKEN).await;
        let id = AgentIdentity::generate();
        let mut ws = connect(addr).await;
        send_frame(
            &mut ws,
            &ClientFrame::Hello {
                pubkey: encode_pubkey(&id.verifying_key()),
                token: TEST_TOKEN.to_string(),
                agent: "mur".to_string(),
            },
        )
        .await;
        match recv_server(&mut ws).await {
            ServerFrame::Paired { agent } => assert_eq!(agent, "mur"),
            other => panic!("expected Paired, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_unpaired_envelope() {
        let (addr, _tmp) = start_server(TEST_TOKEN).await;
        let id_a = AgentIdentity::generate();
        let mut ws = connect(addr).await;
        send_frame(
            &mut ws,
            &ClientFrame::Hello {
                pubkey: encode_pubkey(&id_a.verifying_key()),
                token: TEST_TOKEN.to_string(),
                agent: "mur".to_string(),
            },
        )
        .await;
        assert!(matches!(
            recv_server(&mut ws).await,
            ServerFrame::Paired { .. }
        ));

        // An envelope signed by a DIFFERENT identity than the paired one.
        let id_b = AgentIdentity::generate();
        send_frame(
            &mut ws,
            &ClientFrame::Envelope {
                envelope: make_envelope(&id_b, "intrude"),
            },
        )
        .await;
        assert!(matches!(
            recv_server(&mut ws).await,
            ServerFrame::Rejected { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn valid_envelope_mirrors_user_transcript() {
        let (addr, tmp) = start_server(TEST_TOKEN).await;
        let id = AgentIdentity::generate();
        let mut ws = connect(addr).await;
        send_frame(
            &mut ws,
            &ClientFrame::Hello {
                pubkey: encode_pubkey(&id.verifying_key()),
                token: TEST_TOKEN.to_string(),
                agent: "mur".to_string(),
            },
        )
        .await;
        assert!(matches!(
            recv_server(&mut ws).await,
            ServerFrame::Paired { .. }
        ));

        send_frame(
            &mut ws,
            &ClientFrame::Envelope {
                envelope: make_envelope(&id, "hello mur"),
            },
        )
        .await;

        // The user's turn is mirrored before the agent dial, so it appears
        // regardless of whether an agent is actually running.
        let path = tmp.path().join("agents/mur/mobile-events.jsonl");
        let mut found = false;
        for _ in 0..50 {
            if let Ok(s) = std::fs::read_to_string(&path)
                && s.contains("mobile.transcript")
                && s.contains("hello mur")
            {
                found = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(found, "expected mirrored transcript at {}", path.display());
    }
}
