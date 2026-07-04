//! Mac-side relay client for the mobile voice endpoint (P4).
//!
//! Connects to `<relay_url>/api/v1/relay/ws` using the configured API key.
//! The relay forwards phone `ClientFrame` bytes wrapped as:
//!
//!   `{ "type": "mobile_frame", "payload": <ClientFrame JSON> }`
//!
//! This task unwraps them, processes them through the same pipeline used for
//! LAN connections, and sends `ServerFrame` bytes back wrapped the same way.
//!
//! Reconnects indefinitely with exponential backoff (1 s → 60 s).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use mur_common::mobile::{ClientFrame, ServerFrame};
use mur_core::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};
use serde_json::{Value, json};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest},
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const RELAY_WS_PATH: &str = "/api/v1/relay/ws";

/// Spawn the relay mobile client task; runs forever (reconnects on drop).
pub fn spawn(
    mur_home: PathBuf,
    relay_url: String,
    api_key: String,
    enroll_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    tokio::spawn(async move {
        // One channel watcher for the whole relay task (mirrors the LAN server):
        // filesystem changes under <home>/channels/ fan out via this broadcast so
        // each connection can push `channel.updated` and a relay-only phone gets
        // live updates instead of polling. Forgotten because the relay task lives
        // for the process lifetime — exactly one watcher, no per-reconnect leak.
        let (chan_tx, _chan_rx) = tokio::sync::broadcast::channel::<String>(256);
        {
            let tx = chan_tx.clone();
            let home = mur_home.clone();
            std::thread::spawn(move || {
                match mur_channel::watch::watch_channels(&home, move |channel_id| {
                    let _ = tx.send(channel_id);
                }) {
                    Ok(w) => std::mem::forget(w),
                    Err(e) => tracing::warn!("mobile relay channel watcher failed: {e:#}"),
                }
            });
        }

        let mut delay = Duration::from_secs(1);
        loop {
            match run_once(&mur_home, &relay_url, &api_key, &chan_tx, &enroll_lock).await {
                Ok(()) => {
                    tracing::info!("mobile relay: clean disconnect");
                    delay = Duration::from_secs(1);
                }
                Err(e) => {
                    tracing::warn!(error = %e, backoff_secs = delay.as_secs(), "mobile relay: disconnected");
                }
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    });
}

async fn run_once(
    mur_home: &Path,
    relay_url: &str,
    api_key: &str,
    chan_tx: &tokio::sync::broadcast::Sender<String>,
    enroll_lock: &tokio::sync::Mutex<()>,
) -> Result<()> {
    let ws_url = format!("{}{RELAY_WS_PATH}", relay_url.trim_end_matches('/'));

    let mut req = ws_url.into_client_request()?;
    req.headers_mut()
        .insert("Authorization", format!("Bearer {api_key}").try_into()?);

    let (ws, _) = connect_async_tls_with_config(req, None, false, None).await?;
    let (mut write, mut read) = ws.split();

    // Register with the relay hub.
    let heartbeat = serde_json::to_string(&json!({
        "type": "heartbeat",
        "agent_id": "mur-mobile-relay",
        "capabilities": ["mobile_relay"],
    }))?;
    write.send(Message::Text(heartbeat.into())).await?;
    tracing::info!("mobile relay: connected to {}", relay_url);

    let mut hb_interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    hb_interval.tick().await; // skip first immediate tick

    // Per-connection audio accumulator for voice streaming over relay.
    let mut audio_buf: Vec<u8> = Vec::new();
    // Per-connection pairing state: `None` until a valid `Hello` (correct pair
    // token) completes the handshake. Until then, no application frame is
    // processed — mirrors the LAN server, which gates the whole connection.
    let mut paired_pubkey: Option<String> = None;
    // Pending Resume challenge for this connection: (pubkey, agent, nonce).
    let mut pending_resume: Option<(String, String, String)> = None;
    // Pending enrollment-proof challenge for this connection (proto≥2).
    let mut pending_enroll: Option<PendingEnroll> = None;
    // Live channel-update feed for this connection.
    let mut chan_rx = chan_tx.subscribe();

    loop {
        tokio::select! {
            _ = hb_interval.tick() => {
                let hb = serde_json::to_string(&json!({
                    "type": "heartbeat",
                    "agent_id": "mur-mobile-relay",
                }))?;
                write.send(Message::Text(hb.into())).await?;
            }

            Ok(channel_id) = chan_rx.recv() => {
                // Push live channel updates ONLY to a paired connection — never
                // leak channel ids to an unauthenticated relay peer. A lagged
                // receiver simply drops the missed id (the phone resyncs via
                // ChannelQuery).
                if paired_pubkey.is_some() {
                    relay_send(
                        &mut write,
                        &ServerFrame::Event {
                            name: "channel.updated".to_string(),
                            payload: json!({ "channel_id": channel_id }),
                        },
                    )
                    .await?;
                }
            }

            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                };

                let txt = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => return Ok(()),
                    _ => continue,
                };

                let envelope: Value = match serde_json::from_str(&txt) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                match envelope.get("type").and_then(|t| t.as_str()) {
                    Some("heartbeat_ack") => {
                        tracing::debug!("mobile relay: heartbeat_ack");
                    }
                    Some("command") => {
                        handle_relay_command(mur_home, &mut write, &envelope).await;
                    }
                    Some("mobile_frame") => {
                        let payload = match envelope.get("payload") {
                            Some(p) => p.to_string(),
                            None => continue,
                        };
                        let frame: ClientFrame = match serde_json::from_str(&payload) {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!(error = %e, "mobile relay: bad ClientFrame");
                                continue;
                            }
                        };
                        if let Err(e) = handle_frame(
                            mur_home,
                            &mut write,
                            frame,
                            &mut audio_buf,
                            &mut paired_pubkey,
                            enroll_lock,
                            &mut pending_resume,
                            &mut pending_enroll,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "mobile relay: frame handling error");
                        }
                    }
                    Some(t) => tracing::debug!("mobile relay: ignoring type={t}"),
                    None => {}
                }
            }
        }
    }
}

type WsWrite = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

/// Whether a relay frame may be dispatched given the connection's pairing state.
/// `Hello` is the only frame allowed before pairing; every other frame (envelope,
/// channel query, voice) is an authoritative read/write that must wait for a
/// completed token handshake. Pure + unit-tested so a refactor cannot silently
/// reopen the no-Hello voice/ChannelQuery bypass.
fn frame_allowed_before_dispatch(frame: &ClientFrame, paired: &Option<String>) -> bool {
    // The handshakes that make a connection paired — Hello (transitional resume),
    // the HelloInit/HelloProof enrollment proof, and the Resume challenge-response
    // — are allowed pre-pairing; every other frame waits until paired.
    matches!(
        frame,
        ClientFrame::Hello { .. }
            | ClientFrame::HelloInit { .. }
            | ClientFrame::HelloProof { .. }
            | ClientFrame::Resume { .. }
            | ClientFrame::ResumeProof { .. }
    ) || paired.is_some()
}

/// Pending enrollment-proof challenge for one relay connection (proto≥2).
struct PendingEnroll {
    proto: u32,
    agent: String,
    did: String,
    pubkey: String,
    wid: String,
    nonce: Vec<u8>,
}

/// Whether a relay envelope is authorized for THIS connection: its key must equal
/// the device that paired the connection AND its signature must verify against a
/// paired device. Mirrors the LAN server's `bridge_pubkey == pubkey && is_paired
/// && verify`.
fn relay_envelope_authorized(
    home: &Path,
    envelope: &mur_common::bridge::envelope::SignedEnvelope,
    paired: &Option<String>,
) -> bool {
    paired.as_deref() == Some(envelope.bridge_pubkey_multibase.as_str())
        && mur_core::mobile::paired_envelope_ok(home, envelope)
}

#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    home: &Path,
    write: &mut WsWrite,
    frame: ClientFrame,
    audio_buf: &mut Vec<u8>,
    paired: &mut Option<String>,
    enroll_lock: &tokio::sync::Mutex<()>,
    // Pending Resume challenge for THIS connection: (pubkey, agent, nonce). Set
    // when a `Resume` arrives, consumed by the matching `ResumeProof`.
    pending_resume: &mut Option<(String, String, String)>,
    // Pending enrollment-proof challenge for THIS connection (proto≥2). Set on
    // `HelloInit`, consumed by the matching `HelloProof`.
    pending_enroll: &mut Option<PendingEnroll>,
) -> Result<()> {
    use base64::Engine as _;

    // Connection-level gate: the relay has no per-frame transport auth, so — like
    // the LAN server, which runs the Hello handshake before its application loop —
    // ONLY a `Hello` may be processed before this connection is paired. Every other
    // frame (envelope, channel query, voice) is an authoritative read/write and
    // must wait for a completed token handshake on THIS connection.
    if !frame_allowed_before_dispatch(&frame, paired) {
        relay_send(
            write,
            &ServerFrame::Rejected {
                reason: "not paired".to_string(),
            },
        )
        .await?;
        return Ok(());
    }

    match frame {
        ClientFrame::Hello {
            pubkey,
            token: _,
            agent,
        } => {
            // Over relay, Hello is ONLY a transitional resume for an
            // already-paired device (the shipped app re-sends Hello on reconnect).
            // Relay NEVER does bearer-token enrollment — a new device must use the
            // HelloInit/HelloProof proof handshake. (LAN has an opt-in legacy path;
            // the relay does not, since the token would cross the relay hub.)
            if mur_core::mobile::is_device_paired(home, &pubkey) {
                *paired = Some(pubkey);
                let canonical = canonicalize_agent_name(home, &agent);
                relay_send(
                    write,
                    &ServerFrame::Paired {
                        agent: canonical,
                        confirm: Vec::new(),
                    },
                )
                .await?;
            } else {
                relay_send(
                    write,
                    &ServerFrame::Rejected {
                        reason: "pair this device with the QR (proof handshake required)"
                            .to_string(),
                    },
                )
                .await?;
            }
        }

        ClientFrame::HelloInit {
            proto,
            agent,
            pubkey,
            wid,
        } => {
            // Enrollment step 1 (proto≥2): issue the challenge UNCONDITIONALLY
            // (don't leak whether the wid is live); verify at the proof step.
            // In-flight guard (like Resume): ignore a duplicate HelloInit while a
            // challenge is pending, so a peer can't amplify work by spamming it.
            if proto < 2 || pending_enroll.is_some() {
                if proto < 2 {
                    relay_send(
                        write,
                        &ServerFrame::Rejected {
                            reason: "unsupported pairing protocol".to_string(),
                        },
                    )
                    .await?;
                }
                return Ok(());
            }
            let canonical = canonicalize_agent_name(home, &agent);
            match mur_core::mobile::daemon_id(home, &canonical) {
                Some(did) => {
                    let nonce = mur_common::mobile::mint_nonce().to_vec();
                    *pending_enroll = Some(PendingEnroll {
                        proto,
                        agent: canonical,
                        did: did.clone(),
                        pubkey,
                        wid: wid.clone(),
                        nonce: nonce.clone(),
                    });
                    relay_send(write, &ServerFrame::PairChallenge { wid, nonce, did }).await?;
                }
                None => {
                    relay_send(
                        write,
                        &ServerFrame::Rejected {
                            reason: "agent has no identity".to_string(),
                        },
                    )
                    .await?;
                }
            }
        }

        ClientFrame::HelloProof { wid, proof } => {
            // Enrollment step 3: verify against the pending challenge for THIS wid.
            let done = match pending_enroll.take() {
                Some(p) if p.wid == wid => {
                    let _guard = enroll_lock.lock().await;
                    mur_core::mobile::verify_hello_proof(
                        home, &wid, p.proto, &p.agent, &p.did, &p.pubkey, &p.nonce, &proof,
                    )
                    .map(|confirm| {
                        if let Err(e) = mur_core::mobile::add_paired_device(home, &p.pubkey) {
                            tracing::warn!(error = %e, "mobile relay: persist paired failed");
                        }
                        tracing::info!(
                            fingerprint = %mur_core::mobile::device_fingerprint(&p.pubkey),
                            "mobile: paired new device (relay, proof)"
                        );
                        (p.pubkey, p.agent, confirm)
                    })
                }
                _ => None,
            };
            match done {
                Some((pubkey, agent, confirm)) => {
                    *paired = Some(pubkey);
                    relay_send(write, &ServerFrame::Paired { agent, confirm }).await?;
                }
                None => {
                    relay_send(
                        write,
                        &ServerFrame::Rejected {
                            reason: "bad pairing proof".to_string(),
                        },
                    )
                    .await?;
                }
            }
        }

        ClientFrame::Resume { pubkey, agent } => {
            // Steady-state reconnect by paired key. In-flight guard: ignore a
            // duplicate Resume while one is already pending, so an unauthenticated
            // peer can't amplify work (nonce mint + Challenge send) by spamming
            // Resume. Issue the challenge UNCONDITIONALLY (no is_device_paired
            // branch here) so we don't leak which pubkeys are enrolled —
            // resume_proof_ok gates pairing + signature at the proof step.
            if pending_resume.is_none() {
                let nonce = mur_core::mobile::new_challenge_nonce();
                *pending_resume = Some((pubkey, agent, nonce.clone()));
                relay_send(write, &ServerFrame::Challenge { nonce }).await?;
            }
        }

        ClientFrame::ResumeProof { envelope } => match pending_resume.take() {
            Some((pubkey, agent, nonce))
                if mur_core::mobile::resume_proof_ok(home, &pubkey, &nonce, &envelope) =>
            {
                *paired = Some(pubkey);
                let canonical = canonicalize_agent_name(home, &agent);
                relay_send(
                    write,
                    &ServerFrame::Paired {
                        agent: canonical,
                        confirm: Vec::new(),
                    },
                )
                .await?;
            }
            _ => {
                relay_send(
                    write,
                    &ServerFrame::Rejected {
                        reason: "bad resume proof".to_string(),
                    },
                )
                .await?;
            }
        },

        ClientFrame::Envelope { envelope } => {
            // Pinned to the device that paired THIS connection: the envelope's key
            // must equal it AND its signature must verify against a paired device.
            // (Mirrors the LAN server's bridge_pubkey == pubkey && is_paired && verify.)
            if !relay_envelope_authorized(home, &envelope, paired) {
                relay_send(
                    write,
                    &ServerFrame::Rejected {
                        reason: "unauthorized".to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
            let req: mur_common::a2a::JsonRpcRequest =
                match serde_json::from_slice(&envelope.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "mobile relay: bad payload");
                        return Ok(());
                    }
                };
            let method = req.method.clone();
            let params = req.params.clone().unwrap_or(Value::Null);
            // v4c: an authoritative HITL approval rides THIS signed envelope
            // (verified just above), so the gate-releasing write only fires for a
            // signature-checked frame — never an unsigned one.
            if method == mur_common::mobile::HITL_RESPOND_METHOD {
                if let Some((channel_id, hitl_id)) =
                    mur_core::mobile::respond_hitl_from_params(home, &params)
                {
                    relay_send(
                        write,
                        &ServerFrame::Event {
                            name: "hitl.ack".to_string(),
                            payload: serde_json::json!({ "hitl_id": hitl_id, "channel_id": channel_id }),
                        },
                    )
                    .await?;
                }
            } else {
                let user_text = extract_user_text(req.params.as_ref());
                let agent = extract_agent(req.params.as_ref(), home);
                agent_turn(home, write, &agent, &user_text, method, params).await?;
            }
        }

        ClientFrame::AudioStreamStart { .. } => {
            audio_buf.clear();
        }

        ClientFrame::AudioChunk { data } => {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                audio_buf.extend_from_slice(&bytes);
            }
        }

        ClientFrame::ChannelQuery {
            op,
            channel_id,
            since_seq,
        } => {
            let payload = mur_core::mobile::channel_query(home, &op, channel_id, since_seq)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "mobile relay: channel_query failed");
                    serde_json::Value::Array(vec![])
                });
            relay_send(
                write,
                &ServerFrame::ChannelData {
                    op: op.clone(),
                    payload,
                },
            )
            .await?;
        }

        ClientFrame::AudioStreamEnd => {
            let pcm = std::mem::take(audio_buf);
            let home_c = home.to_path_buf();
            let outcome =
                tokio::task::spawn_blocking(move || crate::stt_sink::transcribe(&home_c, &pcm))
                    .await
                    .unwrap_or(crate::stt_sink::SttOutcome::Empty);

            let text = match outcome {
                crate::stt_sink::SttOutcome::Text(t) => t,
                crate::stt_sink::SttOutcome::Empty => return Ok(()),
                crate::stt_sink::SttOutcome::ModelsMissing => {
                    let agent = canonicalize_agent_name(home, "mur");
                    let hint = format!(
                        "語音模型尚未安裝。請在 Mac 上執行 `mur agent voice {agent} download`（約 1.4 GB），完成後重啟 daemon 再用語音對話。"
                    );
                    relay_send(
                        write,
                        &ServerFrame::Event {
                            name: "mobile.reply".to_string(),
                            payload: serde_json::json!({ "text": hint }),
                        },
                    )
                    .await?;
                    return Ok(());
                }
            };

            relay_send(
                write,
                &ServerFrame::Transcript {
                    text: text.clone(),
                    is_final: true,
                },
            )
            .await?;

            let msg = mur_common::a2a::Message {
                role: "user".to_string(),
                parts: vec![mur_common::a2a::MessagePart::Text { text: text.clone() }],
            };
            let agent = canonicalize_agent_name(home, "mur");
            let params = {
                let mut m = serde_json::Map::new();
                m.insert("agent".to_string(), Value::String(agent.clone()));
                m.insert("message".to_string(), serde_json::to_value(&msg)?);
                Value::Object(m)
            };
            agent_turn(
                home,
                write,
                &agent,
                &text,
                "message/send".to_string(),
                params,
            )
            .await?;
        }
    }
    Ok(())
}

async fn agent_turn(
    home: &Path,
    write: &mut WsWrite,
    agent: &str,
    user_text: &str,
    method: String,
    params: Value,
) -> Result<()> {
    // v4c: capture the explicit target channel before `params` is moved.
    let channel_id = params
        .get("channel_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let home_c = home.to_path_buf();
    let agent_c = agent.to_string();
    let dialed = tokio::task::spawn_blocking(move || {
        dial_method(&home_c, &agent_c, &method, params, DialMode::Auto)
    })
    .await;

    let reply_text = match dialed {
        Ok(Ok(v)) => extract_reply_text(&v),
        Ok(Err(e)) => format!("[error] {e}"),
        Err(e) => format!("[error] dial task: {e}"),
    };

    if !reply_text.starts_with("[error]") {
        mur_core::mobile::persist_mobile_exchange_into(
            home,
            agent,
            channel_id.as_deref(),
            user_text,
            &reply_text,
        );
    }
    relay_send(
        write,
        &ServerFrame::Event {
            name: "mobile.reply".to_string(),
            payload: json!({ "text": reply_text }),
        },
    )
    .await?;

    if !reply_text.starts_with("[error]") {
        let home_c = home.to_path_buf();
        let text = reply_text.clone();
        if let Some((b64, sample_rate)) =
            tokio::task::spawn_blocking(move || crate::tts_sink::synthesize(&home_c, &text))
                .await
                .unwrap_or(None)
        {
            relay_send(
                write,
                &ServerFrame::AudioChunk {
                    base64: b64,
                    sample_rate,
                    done: true,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn relay_send(write: &mut WsWrite, frame: &ServerFrame) -> Result<()> {
    let payload_str = serde_json::to_string(frame)?;
    let payload_val: Value = serde_json::from_str(&payload_str)?;
    let wrapped =
        serde_json::to_string(&json!({ "type": "mobile_frame", "payload": payload_val }))?;
    write.send(Message::Text(wrapped.into())).await?;
    Ok(())
}

/// Handles a `{"type":"command", "id", "action", "params"}` frame from the
/// mur-server relay (`internal/relay/hub.go` `Command`/`sendToAgent`). Only
/// `action == "install_request"` is implemented today (Dashboard "Install to
/// Hub" button). Always acks with a `result` frame carrying the same `id` —
/// `Hub.SendCommand` blocks on that ack to consider the command delivered —
/// so this never lets a bad/unknown payload hang the caller or crash the
/// relay read loop.
async fn handle_relay_command(mur_home: &Path, write: &mut WsWrite, envelope: &Value) {
    let Some(cmd_id) = envelope.get("id").and_then(|v| v.as_str()) else {
        tracing::warn!("mobile relay: command frame missing id, dropping");
        return;
    };
    let action = envelope.get("action").and_then(|v| v.as_str());

    let (success, error) = match action {
        Some("install_request") => {
            match serde_json::from_value::<mur_core::install_request::InstallRequest>(
                envelope
                    .get("params")
                    .cloned()
                    .unwrap_or(Value::Null)
                    .as_object()
                    .map(|obj| {
                        let mut obj = obj.clone();
                        obj.insert("request_id".to_string(), json!(cmd_id));
                        Value::Object(obj)
                    })
                    .unwrap_or(Value::Null),
            ) {
                Ok(req) => {
                    match mur_core::install_request::record_install_request(mur_home, &req) {
                        Ok(_) => (true, None),
                        Err(e) => {
                            tracing::warn!(error = %e, "mobile relay: record_install_request failed");
                            (false, Some(e.to_string()))
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "mobile relay: bad install_request params");
                    (false, Some(format!("bad install_request params: {e}")))
                }
            }
        }
        other => {
            tracing::warn!(action = ?other, "mobile relay: unknown command action");
            (false, Some(format!("unknown action: {other:?}")))
        }
    };

    let result = json!({
        "type": "result",
        "id": cmd_id,
        "success": success,
        "data": Value::Null,
        "error": error,
    });
    if let Ok(text) = serde_json::to_string(&result)
        && let Err(e) = write.send(Message::Text(text.into())).await
    {
        tracing::warn!(error = %e, "mobile relay: failed to send command result");
    }
}

// --- helpers ---

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

fn extract_agent(params: Option<&Value>, home: &Path) -> String {
    let name = params
        .and_then(|p| p.get("agent"))
        .and_then(|a| a.as_str())
        .unwrap_or("mur");
    canonicalize_agent_name(home, name)
}

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
    "[no reply]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;

    fn signed_envelope(id: &AgentIdentity) -> mur_common::bridge::envelope::SignedEnvelope {
        mur_common::bridge::envelope::sign_payload(b"{}".to_vec(), id, 1)
    }

    #[test]
    fn only_hello_is_allowed_before_pairing() {
        let unpaired: Option<String> = None;
        let hello = ClientFrame::Hello {
            pubkey: "pk".to_string(),
            token: "t".to_string(),
            agent: "mur".to_string(),
        };
        // Hello may proceed pre-pairing; every other frame is gated.
        assert!(frame_allowed_before_dispatch(&hello, &unpaired));
        for frame in [
            ClientFrame::AudioStreamEnd,
            ClientFrame::AudioStreamStart { sample_rate: 16000 },
            ClientFrame::ChannelQuery {
                op: "list".to_string(),
                channel_id: None,
                since_seq: None,
            },
        ] {
            assert!(
                !frame_allowed_before_dispatch(&frame, &unpaired),
                "non-Hello frame must be rejected before pairing (closes the \
                 no-Hello voice/ChannelQuery bypass)"
            );
        }

        // Once paired, the same frames are allowed.
        let paired = Some("pk".to_string());
        assert!(frame_allowed_before_dispatch(
            &ClientFrame::AudioStreamEnd,
            &paired
        ));
        assert!(frame_allowed_before_dispatch(
            &ClientFrame::ChannelQuery {
                op: "events".to_string(),
                channel_id: Some("c".to_string()),
                since_seq: None,
            },
            &paired
        ));
    }

    #[test]
    fn relay_envelope_authorized_pins_to_the_connection_device() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let id = AgentIdentity::generate();
        let env = signed_envelope(&id);
        let pk = env.bridge_pubkey_multibase.clone();
        mur_core::mobile::add_paired_device(home, &pk).unwrap();

        // Paired device, key matches the connection, signature intact → authorized.
        assert!(relay_envelope_authorized(home, &env, &Some(pk.clone())));

        // Connection not yet paired (None) → rejected even though the device is in
        // the store and the signature is valid.
        assert!(!relay_envelope_authorized(home, &env, &None));

        // Connection paired as a DIFFERENT device → rejected (no cross-device use).
        assert!(!relay_envelope_authorized(
            home,
            &env,
            &Some("other".to_string())
        ));

        // Tampered payload → signature no longer verifies → rejected.
        let mut tampered = env.clone();
        tampered.payload = br#"{"x":1}"#.to_vec();
        assert!(!relay_envelope_authorized(home, &tampered, &Some(pk)));
    }
}
