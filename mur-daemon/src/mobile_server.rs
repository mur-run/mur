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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Ports, bind address, token paths, and the default agent live in
// `mur_core::mobile` so the daemon and the `mur agent pair` CLI agree.

#[derive(Clone)]
struct MobileState {
    mur_home: PathBuf,
    /// Cross-transport enrollment lock: held during window-claim + add_paired so
    /// a single-use window can never enroll two devices (LAN + relay race).
    enroll_lock: Arc<tokio::sync::Mutex<()>>,
    /// Broadcast channel used to push `channel.updated` events to all
    /// connected phones while they're online.
    chan_tx: tokio::sync::broadcast::Sender<String>,
}

/// Spawn the mobile WebSocket server as a background tokio task (best-effort).
pub fn spawn(
    mur_home: PathBuf,
    enroll_lock: Arc<tokio::sync::Mutex<()>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run_server(mur_home, enroll_lock).await {
            eprintln!("murmurd mobile-server error: {e:#}");
        }
    })
}

async fn run_server(mur_home: PathBuf, enroll_lock: Arc<tokio::sync::Mutex<()>>) -> Result<()> {
    let port = mur_core::mobile::mobile_port();
    let bind = mur_core::mobile::mobile_bind();
    let addr: std::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("parse mobile bind {bind}:{port}"))?;

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
        enroll_lock,
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
    // 1. Pairing handshake. `confirm` is non-empty only for the proto≥2 proof
    //    enrollment (the daemon→phone MAC that authenticates the daemon).
    let (pubkey, agent, confirm) = match recv_text(&mut socket).await {
        Some(txt) => match serde_json::from_str::<ClientFrame>(&txt) {
            // Proto ≥ 2 enrollment: token NEVER on the wire — HMAC challenge-response.
            Ok(ClientFrame::HelloInit {
                proto,
                agent,
                pubkey,
                wid,
            }) => {
                let home = &state.mur_home;
                if proto < 2 {
                    let _ = send_frame(&mut socket, &reject("unsupported pairing protocol")).await;
                    return;
                }
                let canonical = resolve_agent(home, &agent);
                let did = match mur_core::mobile::daemon_id(home, &canonical) {
                    Some(d) => d,
                    None => {
                        let _ = send_frame(&mut socket, &reject("agent has no identity")).await;
                        return;
                    }
                };
                // Issue the challenge UNCONDITIONALLY (don't branch on window
                // existence — that would leak whether a wid is live); verification
                // happens at the proof step. Fresh 32-byte liveness nonce.
                let nonce = mur_common::mobile::mint_nonce().to_vec();
                if send_frame(
                    &mut socket,
                    &ServerFrame::PairChallenge {
                        wid: wid.clone(),
                        nonce: nonce.clone(),
                        did: did.clone(),
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
                match recv_text(&mut socket).await {
                    Some(t) => match serde_json::from_str::<ClientFrame>(&t) {
                        Ok(ClientFrame::HelloProof { wid: pwid, proof }) if pwid == wid => {
                            // Verify + record + burn atomically under the shared lock.
                            let confirm = {
                                let _guard = state.enroll_lock.lock().await;
                                match mur_core::mobile::verify_hello_proof(
                                    home, &wid, proto, &canonical, &did, &pubkey, &nonce, &proof,
                                ) {
                                    Some(c) => {
                                        if let Err(e) =
                                            mur_core::mobile::add_paired_device(home, &pubkey)
                                        {
                                            tracing::warn!(error = %e, "mobile: persist paired failed");
                                        }
                                        tracing::info!(
                                            fingerprint = %mur_core::mobile::device_fingerprint(&pubkey),
                                            "mobile: paired new device (LAN, proof)"
                                        );
                                        Some(c)
                                    }
                                    None => None,
                                }
                            };
                            match confirm {
                                Some(c) => (pubkey, canonical, c),
                                None => {
                                    let _ =
                                        send_frame(&mut socket, &reject("bad pairing proof")).await;
                                    return;
                                }
                            }
                        }
                        _ => {
                            let _ = send_frame(&mut socket, &reject("expected hello proof")).await;
                            return;
                        }
                    },
                    None => return,
                }
            }
            Ok(ClientFrame::Hello {
                pubkey,
                token,
                agent,
            }) => {
                let home = &state.mur_home;
                if mur_core::mobile::is_device_paired(home, &pubkey) {
                    // Transitional resume: an already-enrolled device reconnecting.
                    // The shipped app re-sends Hello{token} on every reconnect, so
                    // accept by paired key alone (ignore the token, claim no window).
                    // Removed once the app ships Resume; key-only, not token-gated.
                } else if mur_core::mobile::allow_legacy_pairing() {
                    // LEGACY bearer enrollment — opt-in only (default OFF), for
                    // operators mid-fleet-upgrade. Sends the token in the clear, so
                    // it reintroduces the LAN-sniff risk; the proof path is preferred.
                    let enrolled = {
                        let _guard = state.enroll_lock.lock().await;
                        if mur_core::mobile::try_consume_pair_window(home, &token) {
                            if let Err(e) = mur_core::mobile::add_paired_device(home, &pubkey) {
                                tracing::warn!(error = %e, "mobile: persist paired failed");
                            }
                            tracing::info!(
                                fingerprint = %mur_core::mobile::device_fingerprint(&pubkey),
                                "mobile: paired new device (LAN, legacy)"
                            );
                            true
                        } else {
                            false
                        }
                    };
                    if !enrolled {
                        let _ = send_frame(
                            &mut socket,
                            &reject("no active pairing window — run `mur agent pair`"),
                        )
                        .await;
                        return;
                    }
                } else {
                    let _ = send_frame(
                        &mut socket,
                        &reject("legacy pairing disabled — update the MUR app to pair"),
                    )
                    .await;
                    return;
                }
                let agent = resolve_agent(&state.mur_home, &agent);
                (pubkey, agent, Vec::new())
            }
            Ok(ClientFrame::Resume { pubkey, agent }) => {
                // Steady-state reconnect by paired key — no enrollment token.
                // Issue the challenge UNCONDITIONALLY (don't branch on
                // is_device_paired here): a paired-membership check at this point
                // would leak which device pubkeys are enrolled. resume_proof_ok
                // below requires both pairing AND a valid signature, so an unpaired
                // key fails identically to a paired key with a bad proof.
                let home = &state.mur_home;
                // Challenge-response: issue a fresh per-connection nonce; the phone
                // must sign exactly it to prove it holds the paired key (replay-safe).
                let nonce = mur_core::mobile::new_challenge_nonce();
                if send_frame(
                    &mut socket,
                    &ServerFrame::Challenge {
                        nonce: nonce.clone(),
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
                match recv_text(&mut socket).await {
                    Some(t) => match serde_json::from_str::<ClientFrame>(&t) {
                        Ok(ClientFrame::ResumeProof { envelope })
                            if mur_core::mobile::resume_proof_ok(
                                home, &pubkey, &nonce, &envelope,
                            ) =>
                        {
                            (pubkey, resolve_agent(&state.mur_home, &agent), Vec::new())
                        }
                        _ => {
                            let _ = send_frame(&mut socket, &reject("bad resume proof")).await;
                            return;
                        }
                    },
                    None => return,
                }
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
            confirm,
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
            ClientFrame::Hello { .. }
            | ClientFrame::HelloInit { .. }
            | ClientFrame::HelloProof { .. }
            | ClientFrame::Resume { .. }
            | ClientFrame::ResumeProof { .. } => {
                // Handshake frames after pairing — ignore silently.
            }

            ClientFrame::Envelope { envelope } => {
                // Auth: the envelope's key must be this connection's key, that key
                // must be a paired device (file-backed, shared with the relay), and
                // the signature must verify against it.
                if envelope.bridge_pubkey_multibase != pubkey
                    || !mur_core::mobile::is_device_paired(&state.mur_home, &pubkey)
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
mod tests;
