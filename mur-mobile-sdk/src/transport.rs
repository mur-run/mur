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
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message;

use crate::MobileEvent;
use mur_common::mobile::{ClientFrame, ServerFrame};

/// Signs a Resume challenge nonce into a proof envelope. Set only when the first
/// frame is a `Resume` (reconnect by paired key); `None` for enrollment.
pub type ResumeSigner = Arc<dyn Fn(&str) -> SignedEnvelope + Send + Sync>;

/// Context for a proto≥2 proof enrollment (set only when the first frame is a
/// `HelloInit`). The token + daemon id come from the scanned QR; the token is
/// kept here only to compute the HMAC proof and is never transmitted.
#[derive(Clone)]
pub struct EnrollCtx {
    pub token: String,
    pub expected_did: String,
    pub proto: u32,
    pub agent: String,
    pub pubkey: String,
}

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
    /// Pull channel data from the daemon.
    ChannelQuery {
        op: String,
        channel_id: Option<String>,
        since_seq: Option<u64>,
    },
}

/// Drive one LAN connection to completion.
///
/// Emits `Connecting` immediately, `Connected` once paired, and exactly one
/// terminal `Disconnected` (optionally preceded by `Error`) before returning.
#[allow(clippy::too_many_arguments)]
pub async fn run_lan<E>(
    host: String,
    port: u16,
    path: String,
    hello: ClientFrame,
    resume_signer: Option<ResumeSigner>,
    enroll_ctx: Option<EnrollCtx>,
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
    run_frame_loop(
        stream,
        hello,
        resume_signer,
        enroll_ctx,
        cmd_rx,
        emit,
        "lan",
    )
    .await;
}

/// Drive one relay connection to completion.
///
/// Identical to `run_lan` except the URL is the full WSS relay URL and a JWT
/// or API key is sent as `Authorization: Bearer` in the upgrade request.
#[allow(clippy::too_many_arguments)]
pub async fn run_relay<E>(
    relay_url: String,
    jwt: String,
    hello: ClientFrame,
    resume_signer: Option<ResumeSigner>,
    enroll_ctx: Option<EnrollCtx>,
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
    run_frame_loop(
        stream,
        hello,
        resume_signer,
        enroll_ctx,
        cmd_rx,
        emit,
        "relay",
    )
    .await;
}

/// Shared frame loop used by both `run_lan` and `run_relay` once the
/// WebSocket is open. Transport type is `"lan"` or `"relay"`.
#[allow(clippy::too_many_arguments)]
async fn run_frame_loop<S, E>(
    stream: tokio_tungstenite::WebSocketStream<S>,
    hello: ClientFrame,
    resume_signer: Option<ResumeSigner>,
    enroll_ctx: Option<EnrollCtx>,
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
    // (wid, did, nonce) stashed at PairChallenge so the Paired confirm MAC can be
    // verified (proof enrollment only).
    let mut pending_confirm: Option<(String, String, Vec<u8>)> = None;
    loop {
        tokio::select! {
            inbound = read.next() => {
                match inbound {
                    Some(Ok(Message::Text(txt))) => {
                        match serde_json::from_str::<ServerFrame>(txt.as_str()) {
                            Ok(ServerFrame::Paired { agent, confirm }) => {
                                // Proof enrollment: authenticate the daemon by
                                // verifying its confirm MAC before trusting pairing.
                                if let (Some(ctx), Some((wid, did, nonce))) = (&enroll_ctx, &pending_confirm) {
                                    let expect = mur_common::mobile::pair_proof(
                                        ctx.token.as_bytes(),
                                        &mur_common::mobile::pair_transcript(
                                            mur_common::mobile::PAIR_ROLE_DAEMON_TO_PHONE,
                                            ctx.proto, &ctx.agent, wid, did, &ctx.pubkey, nonce));
                                    if !mur_common::mobile::ct_verify(&expect, &confirm) {
                                        emit(MobileEvent::Error { message: "daemon confirmation failed — possible MITM".to_string() });
                                        emit(MobileEvent::Disconnected { reason: "bad confirm".to_string() });
                                        break;
                                    }
                                }
                                paired = true;
                                emit(MobileEvent::Connected { transport: "lan".to_string(), agent });
                            }
                            Ok(ServerFrame::Rejected { reason }) => {
                                emit(MobileEvent::Error { message: format!("rejected: {reason}") });
                                emit(MobileEvent::Disconnected { reason });
                                break;
                            }
                            Ok(ServerFrame::Challenge { nonce }) => {
                                // Reconnect challenge: sign the daemon's nonce and
                                // answer with a ResumeProof. Only valid when resuming.
                                match &resume_signer {
                                    Some(sign) => {
                                        let proof = ClientFrame::ResumeProof { envelope: sign(&nonce) };
                                        match serde_json::to_string(&proof) {
                                            Ok(t) => {
                                                if let Err(e) = write.send(Message::Text(t.into())).await {
                                                    emit(MobileEvent::Error { message: format!("send resume proof: {e}") });
                                                    emit(MobileEvent::Disconnected { reason: "send resume proof".to_string() });
                                                    break;
                                                }
                                            }
                                            Err(e) => emit(MobileEvent::Error { message: format!("encode resume proof: {e}") }),
                                        }
                                    }
                                    None => emit(MobileEvent::Error {
                                        message: "unexpected challenge (not resuming)".to_string(),
                                    }),
                                }
                            }
                            Ok(ServerFrame::PairChallenge { wid, nonce, did }) => {
                                // Enrollment challenge: verify the daemon identity
                                // against the QR's `did` (MITM defense), then prove
                                // token possession with HMAC over the transcript —
                                // the token is never sent.
                                match &enroll_ctx {
                                    Some(ctx) => {
                                        if !mur_common::mobile::ct_verify(did.as_bytes(), ctx.expected_did.as_bytes()) {
                                            emit(MobileEvent::Error { message: "daemon identity mismatch — possible MITM".to_string() });
                                            emit(MobileEvent::Disconnected { reason: "did mismatch".to_string() });
                                            break;
                                        }
                                        let proof = mur_common::mobile::pair_proof(
                                            ctx.token.as_bytes(),
                                            &mur_common::mobile::pair_transcript(
                                                mur_common::mobile::PAIR_ROLE_PHONE_TO_DAEMON,
                                                ctx.proto, &ctx.agent, &wid, &did, &ctx.pubkey, &nonce));
                                        pending_confirm = Some((wid.clone(), did.clone(), nonce.clone()));
                                        let frame = ClientFrame::HelloProof { wid, proof: proof.to_vec() };
                                        match serde_json::to_string(&frame) {
                                            Ok(t) => {
                                                if let Err(e) = write.send(Message::Text(t.into())).await {
                                                    emit(MobileEvent::Error { message: format!("send hello proof: {e}") });
                                                    emit(MobileEvent::Disconnected { reason: "send hello proof".to_string() });
                                                    break;
                                                }
                                            }
                                            Err(e) => emit(MobileEvent::Error { message: format!("encode hello proof: {e}") }),
                                        }
                                    }
                                    None => emit(MobileEvent::Error {
                                        message: "unexpected pair challenge (not enrolling)".to_string(),
                                    }),
                                }
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
                            Ok(ServerFrame::ChannelData { op, payload }) => {
                                emit_channel_data(&emit, &op, payload);
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
                            Command::ChannelQuery { op, channel_id, since_seq } => {
                                ClientFrame::ChannelQuery { op, channel_id, since_seq }
                            }
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
        "channel.updated" => {
            let channel_id = text("channel_id");
            if !channel_id.is_empty() {
                emit(MobileEvent::ChannelUpdate { channel_id });
            }
        }
        other => tracing::debug!("mur-mobile-sdk: ignoring event {other}"),
    }
}

fn emit_channel_data<E>(emit: &E, op: &str, payload: serde_json::Value)
where
    E: Fn(MobileEvent),
{
    use crate::{ChannelEventItem, ChannelListItem};
    match op {
        "list" => {
            let channels: Vec<ChannelListItem> = payload
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(|v| ChannelListItem {
                    id: v["id"].as_str().unwrap_or_default().to_string(),
                    title: v["title"].as_str().unwrap_or_default().to_string(),
                    state: v["state"].as_str().unwrap_or_default().to_string(),
                    goal: v["goal"].as_str().unwrap_or_default().to_string(),
                    updated_at: v["updated_at"].as_str().unwrap_or_default().to_string(),
                    agents: v["agents"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(|a| a.as_str().map(|s| s.to_string()))
                        .collect(),
                    turns: v["turns"].as_u64().unwrap_or(0) as u32,
                })
                .collect();
            emit(MobileEvent::ChannelList { channels });
        }
        "events" => {
            let channel_id = payload
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("channel_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let events: Vec<ChannelEventItem> = payload
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(|v| {
                    let actor = v.get("actor");
                    ChannelEventItem {
                        seq: v["seq"].as_u64().unwrap_or(0),
                        ts: v["ts"].as_str().unwrap_or_default().to_string(),
                        actor_kind: actor
                            .and_then(|a| a.get("kind").or_else(|| a.get("type")))
                            .and_then(|k| k.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        actor_name: actor
                            .and_then(|a| a.get("id").or_else(|| a.get("name")))
                            .and_then(|k| k.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        kind: v["kind"].as_str().unwrap_or_default().to_string(),
                        // `message` payloads carry `text`; HITL requests carry a
                        // `summary` — fall back so the gate card has something to show.
                        text: v["payload"]["text"]
                            .as_str()
                            .or_else(|| v["payload"]["summary"].as_str())
                            .unwrap_or_default()
                            .to_string(),
                        hitl_id: v["payload"]["hitl_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    }
                })
                .collect();
            emit(MobileEvent::ChannelEvents { channel_id, events });
        }
        other => tracing::debug!("mur-mobile-sdk: ignoring channel_data op={other}"),
    }
}
