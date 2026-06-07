//! LAN WebSocket transport.
//!
//! Runs as a single async task on the client's runtime. It owns one WebSocket
//! connection to the Mac endpoint, performs the pairing handshake, then
//! multiplexes outbound [`Command`]s (signed envelopes) against inbound
//! [`ServerFrame`]s, translating the latter into [`MobileEvent`]s delivered
//! through the `emit` callback.
//!
//! Relay (off-LAN, `wss://` via mur-server) will reuse this same frame loop in
//! P4 with a different URL + TLS; the protocol is transport-agnostic.

use futures_util::{SinkExt, StreamExt};
use mur_common::bridge::envelope::SignedEnvelope;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message;

use crate::MobileEvent;
use mur_common::mobile::{ClientFrame, ServerFrame};

/// Commands the client pushes to the live transport task.
pub enum Command {
    /// Send a signed A2A request to the agent (text path).
    Send(SignedEnvelope),
    /// Begin a voice utterance stream at `sample_rate` Hz.
    AudioStreamStart { sample_rate: u32 },
    /// One chunk of raw PCM (f32 LE) to forward to the Mac.
    AudioFrame { data: Vec<u8> },
    /// End the voice utterance; trigger Mac-side STT + agent turn.
    AudioStreamEnd,
    /// Close the connection and end the task.
    Disconnect,
}

/// Drive one LAN connection to completion.
///
/// Emits `Connecting` immediately, `Connected` once paired, and exactly one
/// terminal `Disconnected` (optionally preceded by `Error`) before returning.
pub async fn run_lan<E>(
    host: String,
    port: u16,
    path: String,
    hello: ClientFrame,
    cmd_rx: UnboundedReceiver<Command>,
    emit: E,
) where
    E: Fn(MobileEvent) + Send + 'static,
{
    emit(MobileEvent::Connecting);

    let url = format!("ws://{host}:{port}{path}");
    let stream = match tokio_tungstenite::connect_async(url.as_str()).await {
        Ok((stream, _resp)) => stream,
        Err(e) => {
            emit(MobileEvent::Error {
                message: format!("connect failed: {e}"),
            });
            emit(MobileEvent::Disconnected {
                reason: "connect failed".to_string(),
            });
            return;
        }
    };
    run_frame_loop(stream, hello, cmd_rx, emit, "lan").await;
}

/// Drive one relay connection to completion.
///
/// Identical to `run_lan` except the URL is the full WSS relay URL and a JWT
/// or API key is sent as `Authorization: Bearer` in the upgrade request.
pub async fn run_relay<E>(
    relay_url: String,
    jwt: String,
    hello: ClientFrame,
    cmd_rx: UnboundedReceiver<Command>,
    emit: E,
) where
    E: Fn(MobileEvent) + Send + 'static,
{
    emit(MobileEvent::Connecting);

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = match relay_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => {
            emit(MobileEvent::Error {
                message: format!("bad relay URL: {e}"),
            });
            emit(MobileEvent::Disconnected {
                reason: "bad relay URL".to_string(),
            });
            return;
        }
    };
    if let Ok(hv) = format!("Bearer {jwt}").try_into() {
        req.headers_mut().insert("Authorization", hv);
    }

    let stream =
        match tokio_tungstenite::connect_async_tls_with_config(req, None, false, None).await {
            Ok((stream, _resp)) => stream,
            Err(e) => {
                emit(MobileEvent::Error {
                    message: format!("relay connect failed: {e}"),
                });
                emit(MobileEvent::Disconnected {
                    reason: "relay connect failed".to_string(),
                });
                return;
            }
        };
    run_frame_loop(stream, hello, cmd_rx, emit, "relay").await;
}

/// Shared frame loop used by both `run_lan` and `run_relay` once the
/// WebSocket is open. Transport type is `"lan"` or `"relay"`.
async fn run_frame_loop<S, E>(
    stream: tokio_tungstenite::WebSocketStream<S>,
    hello: ClientFrame,
    mut cmd_rx: UnboundedReceiver<Command>,
    emit: E,
    _transport: &'static str,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    E: Fn(MobileEvent) + Send + 'static,
{
    let (mut write, mut read) = stream.split();

    // Pairing handshake.
    let hello_txt = match serde_json::to_string(&hello) {
        Ok(t) => t,
        Err(e) => {
            emit(MobileEvent::Error {
                message: format!("encode hello: {e}"),
            });
            emit(MobileEvent::Disconnected {
                reason: "encode hello".to_string(),
            });
            return;
        }
    };
    if let Err(e) = write.send(Message::Text(hello_txt.into())).await {
        emit(MobileEvent::Error {
            message: format!("send hello: {e}"),
        });
        emit(MobileEvent::Disconnected {
            reason: "send hello".to_string(),
        });
        return;
    }

    let mut paired = false;
    loop {
        tokio::select! {
            inbound = read.next() => {
                match inbound {
                    Some(Ok(Message::Text(txt))) => {
                        match serde_json::from_str::<ServerFrame>(txt.as_str()) {
                            Ok(ServerFrame::Paired { agent }) => {
                                paired = true;
                                emit(MobileEvent::Connected { transport: "lan".to_string(), agent });
                            }
                            Ok(ServerFrame::Rejected { reason }) => {
                                emit(MobileEvent::Error { message: format!("rejected: {reason}") });
                                emit(MobileEvent::Disconnected { reason });
                                break;
                            }
                            Ok(ServerFrame::Event { name, payload }) => {
                                emit_event(&emit, &name, payload);
                            }
                            Ok(ServerFrame::Transcript { text, is_final }) => {
                                emit(MobileEvent::Transcript {
                                    role: "user".to_string(),
                                    text,
                                    is_final,
                                });
                            }
                            Ok(ServerFrame::AudioChunk { base64, sample_rate, done }) => {
                                use base64::Engine as _;
                                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&base64) {
                                    emit(MobileEvent::AudioChunk { data: bytes, sample_rate, done });
                                }
                            }
                            Err(e) => tracing::warn!("mur-mobile-sdk: bad server frame: {e}"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        emit(MobileEvent::Disconnected { reason: "closed by server".to_string() });
                        break;
                    }
                    // Ping/Pong/Binary frames are not part of the app protocol.
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        emit(MobileEvent::Error { message: format!("ws error: {e}") });
                        emit(MobileEvent::Disconnected { reason: "ws error".to_string() });
                        break;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        if !paired {
                            emit(MobileEvent::Error { message: "not paired yet".to_string() });
                            continue;
                        }
                        let frame = match cmd {
                            Command::Send(envelope) => ClientFrame::Envelope { envelope },
                            Command::AudioStreamStart { sample_rate } => {
                                ClientFrame::AudioStreamStart { sample_rate }
                            }
                            Command::AudioFrame { data } => {
                                use base64::Engine as _;
                                let encoded =
                                    base64::engine::general_purpose::STANDARD.encode(&data);
                                ClientFrame::AudioChunk { data: encoded }
                            }
                            Command::AudioStreamEnd => ClientFrame::AudioStreamEnd,
                            Command::Disconnect => {
                                let _ = write.close().await;
                                emit(MobileEvent::Disconnected {
                                    reason: "client disconnect".to_string(),
                                });
                                break;
                            }
                        };
                        match serde_json::to_string(&frame) {
                            Ok(txt) => {
                                if let Err(e) = write.send(Message::Text(txt.into())).await {
                                    emit(MobileEvent::Error {
                                        message: format!("send failed: {e}"),
                                    });
                                    emit(MobileEvent::Disconnected {
                                        reason: "send failed".to_string(),
                                    });
                                    break;
                                }
                            }
                            Err(e) => {
                                emit(MobileEvent::Error {
                                    message: format!("encode frame: {e}"),
                                })
                            }
                        }
                    }
                    None => {
                        let _ = write.close().await;
                        emit(MobileEvent::Disconnected { reason: "client disconnect".to_string() });
                        break;
                    }
                }
            }
        }
    }
}

/// Translate a mirrored server event into a typed [`MobileEvent`].
fn emit_event<E>(emit: &E, name: &str, payload: serde_json::Value)
where
    E: Fn(MobileEvent),
{
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    match name {
        "mobile.reply" => emit(MobileEvent::Reply { text: text("text") }),
        "mobile.transcript" => emit(MobileEvent::Transcript {
            role: payload
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("agent")
                .to_string(),
            text: text("text"),
            is_final: payload
                .get("final")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        }),
        other => tracing::debug!("mur-mobile-sdk: ignoring event {other}"),
    }
}
