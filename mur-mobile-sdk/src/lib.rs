//! MUR mobile core SDK.
//!
//! A thin, mobile-safe Rust core shared by the iOS app (now) and a future
//! Android app, exposed through **UniFFI**. It owns the *network* A2A client
//! (the desktop dial path in `mur-core` speaks Unix-domain sockets, which a
//! phone cannot reach), Ed25519 envelope signing reused from `mur-common`, and
//! the event stream surfaced to Swift/Kotlin via a callback interface.
//!
//! Deliberately excluded: whisper/Kokoro voice engines (Hybrid keeps those on
//! the Mac) and any GUI/desktop deps. See
//! `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.
//!
//! ## Lifecycle
//! 1. [`MobileClient::new`] — load/create the phone's identity keypair.
//! 2. [`MobileClient::set_listener`] — register the foreign event sink.
//! 3. [`MobileClient::connect_lan`] — dial the Mac endpoint and pair.
//! 4. [`MobileClient::send_text`] — send a turn; replies arrive as events.

uniffi::setup_scaffolding!();

mod error;
mod transport;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mur_common::a2a::{JsonRpcRequest, Message as A2aMessage, MessagePart};
use mur_common::bridge::envelope::sign_payload;
use mur_common::identity::{AgentIdentity, encode_pubkey};
use tokio::sync::mpsc::{self, UnboundedSender};

pub use error::SdkError;
use transport::Command;

/// Key-rotation version stamped into envelopes for the phone's identity.
/// First-generation keys are version 1; rotation (P4+) increments it.
const PHONE_KEY_VERSION: u32 = 1;

/// Subdirectory under the app home where the phone's identity is stored.
const IDENTITY_SUBDIR: &str = "mobile";

/// Construction-time configuration for a [`MobileClient`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileConfig {
    /// App-private directory for SDK state (identity keypair, etc.). On iOS
    /// this is the app container; the keypair itself is mirrored into the
    /// Keychain by the app layer.
    pub mur_home: String,
    /// Canonical on-disk name of the agent to talk to (P1: `"mur"`).
    pub default_agent: String,
    /// Base URL of the relay (mur-server) for the off-LAN fallback. Unused in
    /// P1 (LAN only); wired in P4.
    pub relay_base_url: Option<String>,
}

/// A channel summary row returned by `list_channels`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelListItem {
    pub id: String,
    pub title: String,
    pub state: String,
    pub goal: String,
    pub updated_at: String,
    pub agents: Vec<String>,
    pub turns: u32,
}

/// A single channel event returned by `fetch_channel_events`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelEventItem {
    pub seq: u64,
    pub ts: String,
    pub actor_kind: String,
    pub actor_name: String,
    pub kind: String,
    pub text: String,
    /// Set on `HitlRequest` events so the phone can respond to the gate (v4c);
    /// empty for every other event kind.
    pub hitl_id: String,
}

/// Events pushed to the foreign listener. The connection lifecycle and the
/// conversation both flow through this single stream.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileEvent {
    /// Dialing the endpoint.
    Connecting,
    /// Paired and ready. `transport` is `"lan"` or `"relay"`.
    Connected { transport: String, agent: String },
    /// The connection ended (terminal for this attempt).
    Disconnected { reason: String },
    /// A transcript line (user or agent). `is_final` distinguishes an
    /// authoritative transcript from an interim partial.
    Transcript {
        role: String,
        text: String,
        is_final: bool,
    },
    /// An agent reply (text; spoken audio is streamed separately in P3).
    Reply { text: String },
    /// A non-fatal error worth surfacing in the UI.
    Error { message: String },
    /// A chunk of Kokoro TTS audio (f32 LE PCM). Accumulate until `done`.
    AudioChunk {
        data: Vec<u8>,
        sample_rate: u32,
        done: bool,
    },
    /// Response to `list_channels()`. Arrives async; update the channel list UI.
    ChannelList { channels: Vec<ChannelListItem> },
    /// Response to `fetch_channel_events()`. Arrives async.
    ChannelEvents {
        channel_id: String,
        events: Vec<ChannelEventItem>,
    },
    /// The daemon notified us that a channel was updated (live-push).
    ChannelUpdate { channel_id: String },
}

/// Foreign-implemented sink for [`MobileEvent`]s. Swift/Kotlin provide this.
#[uniffi::export(callback_interface)]
pub trait MobileEventListener: Send + Sync {
    fn on_event(&self, event: MobileEvent);
}

/// The handle the app holds for the lifetime of a session.
#[derive(uniffi::Object)]
pub struct MobileClient {
    rt: tokio::runtime::Runtime,
    identity: AgentIdentity,
    default_agent: String,
    id_counter: AtomicU64,
    listener: Arc<Mutex<Option<Box<dyn MobileEventListener>>>>,
    cmd_tx: Arc<Mutex<Option<UnboundedSender<Command>>>>,
}

#[uniffi::export]
impl MobileClient {
    /// Create a client, loading the phone's identity keypair from
    /// `<mur_home>/mobile/` (generating + persisting one on first run).
    #[uniffi::constructor]
    pub fn new(config: MobileConfig) -> Result<Arc<Self>, SdkError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("mur-mobile-sdk")
            .build()
            .map_err(|e| SdkError::Runtime { msg: e.to_string() })?;

        let id_dir = Path::new(&config.mur_home).join(IDENTITY_SUBDIR);
        let identity = load_or_create_identity(&id_dir)?;

        Ok(Arc::new(Self {
            rt,
            identity,
            default_agent: config.default_agent,
            id_counter: AtomicU64::new(0),
            listener: Arc::new(Mutex::new(None)),
            cmd_tx: Arc::new(Mutex::new(None)),
        }))
    }

    /// Register (or replace) the event listener.
    pub fn set_listener(&self, listener: Box<dyn MobileEventListener>) {
        if let Ok(mut guard) = self.listener.lock() {
            *guard = Some(listener);
        }
    }

    /// The phone's multibase Ed25519 public key — encode this into the QR /
    /// share so the Mac can add it to the agent's `trusted_peers`.
    pub fn public_key(&self) -> String {
        encode_pubkey(&self.identity.verifying_key())
    }

    /// Connect to the Mac endpoint over LAN and pair. `pair_token` is the
    /// one-time token from the QR code. Progress arrives via the listener.
    pub fn connect_lan(&self, host: String, port: u16, pair_token: String) {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut guard) = self.cmd_tx.lock() {
            *guard = Some(tx);
        }
        let hello = mur_common::mobile::ClientFrame::Hello {
            pubkey: self.public_key(),
            token: pair_token,
            agent: self.default_agent.clone(),
        };
        let emit = self.make_emitter();
        self.rt.spawn(transport::run_lan(
            host,
            port,
            mur_common::mobile::MOBILE_WS_PATH.to_string(),
            hello,
            rx,
            emit,
        ));
    }

    /// Connect via the mur-server relay using a JWT or API key.
    /// `relay_ws_url` should be the full WSS path, e.g.
    /// `"wss://relay.mur.run/api/v1/relay/mobile/ws"`.
    /// `pair_token` is still required — it is sent in the initial `Hello` so
    /// the Mac daemon can verify the phone's identity on first relay connection.
    ///
    /// Reconnects automatically with exponential backoff on drops.
    pub fn connect_relay(&self, relay_ws_url: String, jwt: String, pair_token: String) {
        let cmd_tx_slot = self.cmd_tx.clone(); // Arc clone — OK
        let listener = self.listener.clone(); // Arc clone — OK
        let pubkey = self.public_key();
        let default_agent = self.default_agent.clone();

        // Build an emitter each loop iteration from the shared listener Arc.
        let make_emit = move || {
            let listener = listener.clone();
            move |event: MobileEvent| {
                if let Ok(guard) = listener.lock()
                    && let Some(l) = guard.as_ref()
                {
                    l.on_event(event);
                }
            }
        };

        self.rt.spawn(async move {
            let mut delay = std::time::Duration::from_secs(2);
            loop {
                let (tx, rx) = mpsc::unbounded_channel();
                if let Ok(mut guard) = cmd_tx_slot.lock() {
                    *guard = Some(tx);
                }
                let hello = mur_common::mobile::ClientFrame::Hello {
                    pubkey: pubkey.clone(),
                    token: pair_token.clone(),
                    agent: default_agent.clone(),
                };
                let emit = make_emit();
                transport::run_relay(relay_ws_url.clone(), jwt.clone(), hello, rx, emit).await;

                // Brief pause then reconnect.
                let emit2 = make_emit();
                emit2(MobileEvent::Disconnected {
                    reason: format!("relay disconnected; retry in {}s", delay.as_secs()),
                });
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(60));
            }
        });
    }

    /// Send a user turn as text. The reply arrives as a [`MobileEvent::Reply`]
    /// (and a mirrored transcript) via the listener. `channel_id` (v4c) drops the
    /// turn into a specific channel; `None` uses the agent's latest/new channel.
    pub fn send_text(&self, text: String, channel_id: Option<String>) {
        let envelope = match self.sign_agent_send(&text, channel_id.as_deref()) {
            Ok(env) => env,
            Err(e) => {
                self.emit(MobileEvent::Error {
                    message: format!("sign: {e}"),
                });
                return;
            }
        };
        self.send_cmd(Command::Send(envelope));
    }

    /// Signal the Mac that an audio utterance is starting. Call before the
    /// first `send_audio_frame`. `sample_rate` should be 16 000 (Hz).
    pub fn begin_audio_stream(&self, sample_rate: u32) {
        self.send_cmd(Command::AudioStreamStart { sample_rate });
    }

    /// Send one chunk of raw f32 LE PCM (16 kHz mono) to the Mac.
    pub fn send_audio_frame(&self, data: Vec<u8>) {
        self.send_cmd(Command::AudioFrame { data });
    }

    /// Signal end of utterance. The Mac runs whisper.cpp, dials the agent,
    /// and streams the TTS reply back as [`MobileEvent::AudioChunk`]s.
    pub fn end_audio_stream(&self) {
        self.send_cmd(Command::AudioStreamEnd);
    }

    /// Tear down the current connection.
    pub fn disconnect(&self) {
        self.send_cmd(Command::Disconnect);
    }

    /// Request the full channel list. The response arrives as
    /// [`MobileEvent::ChannelList`] via the listener.
    pub fn list_channels(&self) {
        self.send_cmd(Command::ChannelQuery {
            op: "list".into(),
            channel_id: None,
            since_seq: None,
        });
    }

    /// Request events for a specific channel, optionally from `since_seq`
    /// (inclusive). Response arrives as [`MobileEvent::ChannelEvents`].
    pub fn fetch_channel_events(&self, channel_id: String, since_seq: Option<u64>) {
        self.send_cmd(Command::ChannelQuery {
            op: "events".into(),
            channel_id: Some(channel_id),
            since_seq,
        });
    }

    /// Respond to a HITL gate (v4c). The approval rides a SIGNED envelope (method
    /// `channel/hitl_respond`) so the daemon verifies the phone's Ed25519 signature
    /// before writing the v3d-signed `HitlResponse` that releases the gate — the
    /// approval is never trusted from an unsigned frame.
    pub fn hitl_respond(&self, channel_id: String, hitl_id: String, allow: bool, reason: String) {
        let envelope = match self.sign_hitl_respond(&channel_id, &hitl_id, allow, &reason) {
            Ok(env) => env,
            Err(e) => {
                self.emit(MobileEvent::Error {
                    message: format!("sign: {e}"),
                });
                return;
            }
        };
        self.send_cmd(Command::Send(envelope));
    }
}

impl MobileClient {
    fn send_cmd(&self, cmd: Command) {
        let sent = self
            .cmd_tx
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tx| tx.send(cmd)));
        if !matches!(sent, Some(Ok(()))) {
            self.emit(MobileEvent::Error {
                message: "not connected".to_string(),
            });
        }
    }

    /// Build a `message/send` request and sign it into an envelope using the
    /// phone's identity.
    fn sign_agent_send(
        &self,
        text: &str,
        channel_id: Option<&str>,
    ) -> Result<mur_common::bridge::envelope::SignedEnvelope, serde_json::Error> {
        let msg = A2aMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                text: text.to_string(),
            }],
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "agent".to_string(),
            serde_json::Value::String(self.default_agent.clone()),
        );
        params.insert("message".to_string(), serde_json::to_value(&msg)?);
        // v4c: drop this turn into an explicit channel (else the daemon resolves
        // the agent's latest/new channel).
        if let Some(cid) = channel_id {
            params.insert(
                "channel_id".to_string(),
                serde_json::Value::String(cid.to_string()),
            );
        }

        let id = self.id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::from(id)),
            method: "message/send".to_string(),
            params: Some(serde_json::Value::Object(params)),
        };

        // The envelope is signed over these exact bytes; the Mac verifies the
        // stored payload without re-serializing, so no separate canonicalizer
        // is needed for parity.
        let payload = serde_json::to_vec(&req)?;
        Ok(sign_payload(payload, &self.identity, PHONE_KEY_VERSION))
    }

    /// Build a `channel/hitl_respond` request and sign it into an envelope (v4c).
    /// The daemon verifies this signature before releasing the gate, so the
    /// approval is only ever trusted when it provably came from this phone's key.
    fn sign_hitl_respond(
        &self,
        channel_id: &str,
        hitl_id: &str,
        allow: bool,
        reason: &str,
    ) -> Result<mur_common::bridge::envelope::SignedEnvelope, serde_json::Error> {
        let mut params = serde_json::Map::new();
        params.insert(
            "agent".to_string(),
            serde_json::Value::String(self.default_agent.clone()),
        );
        params.insert(
            "channel_id".to_string(),
            serde_json::Value::String(channel_id.to_string()),
        );
        params.insert(
            "hitl_id".to_string(),
            serde_json::Value::String(hitl_id.to_string()),
        );
        params.insert("allow".to_string(), serde_json::Value::Bool(allow));
        params.insert(
            "reason".to_string(),
            serde_json::Value::String(reason.to_string()),
        );

        let id = self.id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::from(id)),
            method: mur_common::mobile::HITL_RESPOND_METHOD.to_string(),
            params: Some(serde_json::Value::Object(params)),
        };
        let payload = serde_json::to_vec(&req)?;
        Ok(sign_payload(payload, &self.identity, PHONE_KEY_VERSION))
    }

    fn emit(&self, event: MobileEvent) {
        if let Ok(guard) = self.listener.lock()
            && let Some(listener) = guard.as_ref()
        {
            listener.on_event(event);
        }
    }

    fn make_emitter(&self) -> impl Fn(MobileEvent) + Send + 'static {
        let listener = self.listener.clone();
        move |event| {
            if let Ok(guard) = listener.lock()
                && let Some(l) = guard.as_ref()
            {
                l.on_event(event);
            }
        }
    }
}

/// Load the phone identity from `dir`, or generate and persist a new one.
fn load_or_create_identity(dir: &Path) -> Result<AgentIdentity, SdkError> {
    if let Ok(identity) = AgentIdentity::load(dir) {
        return Ok(identity);
    }
    std::fs::create_dir_all(dir).map_err(|e| SdkError::Identity {
        msg: format!("create {}: {e}", dir.display()),
    })?;
    let identity = AgentIdentity::generate();
    identity.save(dir).map_err(|e| SdkError::Identity {
        msg: format!("save: {e}"),
    })?;
    Ok(identity)
}
