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
mod protocol;
mod transport;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mur_common::a2a::{JsonRpcRequest, Message as A2aMessage, MessagePart};
use mur_common::bridge::envelope::sign_payload;
use mur_common::identity::{encode_pubkey, AgentIdentity};
use tokio::sync::mpsc::{self, UnboundedSender};

pub use error::SdkError;
use transport::Command;

/// WebSocket path the Mac endpoint (hosted in `mur-daemon`) listens on.
const MOBILE_WS_PATH: &str = "/api/v1/mobile/ws";

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
    cmd_tx: Mutex<Option<UnboundedSender<Command>>>,
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
            cmd_tx: Mutex::new(None),
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
        let hello = protocol::ClientFrame::Hello {
            pubkey: self.public_key(),
            token: pair_token,
            agent: self.default_agent.clone(),
        };
        let emit = self.make_emitter();
        self.rt.spawn(transport::run_lan(
            host,
            port,
            MOBILE_WS_PATH.to_string(),
            hello,
            rx,
            emit,
        ));
    }

    /// Send a user turn as text. The reply arrives as a [`MobileEvent::Reply`]
    /// (and a mirrored transcript) via the listener.
    pub fn send_text(&self, text: String) {
        let envelope = match self.sign_agent_send(&text) {
            Ok(env) => env,
            Err(e) => {
                self.emit(MobileEvent::Error {
                    message: format!("sign: {e}"),
                });
                return;
            }
        };
        let sent = self
            .cmd_tx
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tx| tx.send(Command::Send(envelope))));
        if !matches!(sent, Some(Ok(()))) {
            self.emit(MobileEvent::Error {
                message: "not connected".to_string(),
            });
        }
    }

    /// Tear down the current connection.
    pub fn disconnect(&self) {
        if let Ok(mut guard) = self.cmd_tx.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(Command::Disconnect);
            }
        }
    }
}

impl MobileClient {
    /// Build an `agent/send` request, canonicalize it, and sign it into an
    /// envelope using the phone's identity.
    fn sign_agent_send(
        &self,
        text: &str,
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

        let id = self.id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::from(id)),
            method: "agent/send".to_string(),
            params: Some(serde_json::Value::Object(params)),
        };

        // The envelope is signed over these exact bytes; the Mac verifies the
        // stored payload without re-serializing, so no separate canonicalizer
        // is needed for parity.
        let payload = serde_json::to_vec(&req)?;
        Ok(sign_payload(payload, &self.identity, PHONE_KEY_VERSION))
    }

    fn emit(&self, event: MobileEvent) {
        if let Ok(guard) = self.listener.lock() {
            if let Some(listener) = guard.as_ref() {
                listener.on_event(event);
            }
        }
    }

    fn make_emitter(&self) -> impl Fn(MobileEvent) + Send + 'static {
        let listener = self.listener.clone();
        move |event| {
            if let Ok(guard) = listener.lock() {
                if let Some(l) = guard.as_ref() {
                    l.on_event(event);
                }
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
