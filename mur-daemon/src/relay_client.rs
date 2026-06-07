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

use std::path::PathBuf;
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
        let mut delay = Duration::from_secs(1);
        loop {
            match run_once(&mur_home, &relay_url, &api_key).await {
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

async fn run_once(mur_home: &PathBuf, relay_url: &str, api_key: &str) -> Result<()> {
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

    loop {
        tokio::select! {
            _ = hb_interval.tick() => {
                let hb = serde_json::to_string(&json!({
                    "type": "heartbeat",
                    "agent_id": "mur-mobile-relay",
                }))?;
                write.send(Message::Text(hb.into())).await?;
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
                        if let Err(e) =
                            handle_frame(mur_home, &mut write, frame, &mut audio_buf).await
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

async fn handle_frame(
    home: &PathBuf,
    write: &mut WsWrite,
    frame: ClientFrame,
    audio_buf: &mut Vec<u8>,
) -> Result<()> {
    use base64::Engine as _;
    use mur_common::bridge::envelope::verify_envelope_with_pubkey;

    match frame {
        ClientFrame::Hello {
            pubkey: _,
            token: _,
            agent,
        } => {
            let canonical = canonicalize_agent_name(home, &agent);
            relay_send(write, &ServerFrame::Paired { agent: canonical }).await?;
        }

        ClientFrame::Envelope { envelope } => {
            let pubkey = envelope.bridge_pubkey_multibase.clone();
            if verify_envelope_with_pubkey(&envelope, &pubkey).is_err() {
                relay_send(
                    write,
                    &ServerFrame::Rejected {
                        reason: "bad signature".to_string(),
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
            let user_text = extract_user_text(req.params.as_ref());
            let agent = extract_agent(req.params.as_ref(), home);
            let method = req.method.clone();
            let params = req.params.clone().unwrap_or(Value::Null);
            agent_turn(home, write, &agent, &user_text, method, params).await?;
        }

        ClientFrame::AudioStreamStart { .. } => {
            audio_buf.clear();
        }

        ClientFrame::AudioChunk { data } => {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                audio_buf.extend_from_slice(&bytes);
            }
        }

        ClientFrame::AudioStreamEnd => {
            let pcm = std::mem::take(audio_buf);
            let home_c = home.clone();
            let transcript_opt =
                tokio::task::spawn_blocking(move || crate::stt_sink::transcribe(&home_c, &pcm))
                    .await
                    .unwrap_or(None);

            let text = match transcript_opt {
                Some(t) => t,
                None => return Ok(()),
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
    home: &PathBuf,
    write: &mut WsWrite,
    agent: &str,
    user_text: &str,
    method: String,
    params: Value,
) -> Result<()> {
    let home_c = home.clone();
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

    let _ = user_text; // mirrored on Mac via mobile_server.rs; not needed here
    relay_send(
        write,
        &ServerFrame::Event {
            name: "mobile.reply".to_string(),
            payload: json!({ "text": reply_text }),
        },
    )
    .await?;

    if !reply_text.starts_with("[error]") {
        let home_c = home.clone();
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

fn extract_agent(params: Option<&Value>, home: &PathBuf) -> String {
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
