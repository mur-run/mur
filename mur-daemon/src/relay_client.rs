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
pub fn spawn(mur_home: PathBuf, relay_url: String, api_key: String) {
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
            match run_once(&mur_home, &relay_url, &api_key, &chan_tx).await {
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
    matches!(frame, ClientFrame::Hello { .. }) || paired.is_some()
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

async fn handle_frame(
    home: &Path,
    write: &mut WsWrite,
    frame: ClientFrame,
    audio_buf: &mut Vec<u8>,
    paired: &mut Option<String>,
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
            token,
            agent,
        } => {
            // Token-gate the relay handshake exactly like the LAN server, pin the
            // device key (shared paired-device store), and mark THIS connection
            // paired so later frames are authorized against this device.
            match mur_core::mobile::read_pair_token(home) {
                Some(t) if t == token => {
                    if let Err(e) = mur_core::mobile::add_paired_device(home, &pubkey) {
                        tracing::warn!(error = %e, "mobile relay: persist paired failed");
                    }
                    *paired = Some(pubkey);
                    let canonical = canonicalize_agent_name(home, &agent);
                    relay_send(write, &ServerFrame::Paired { agent: canonical }).await?;
                }
                _ => {
                    relay_send(
                        write,
                        &ServerFrame::Rejected {
                            reason: "invalid pairing token".to_string(),
                        },
                    )
                    .await?;
                }
            }
        }

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
