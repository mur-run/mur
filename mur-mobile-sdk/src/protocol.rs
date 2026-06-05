//! Wire protocol between the phone SDK and the Mac-side WebSocket endpoint.
//!
//! Every message is a single JSON object sent as a WebSocket *text* frame.
//! The first client frame is a [`ClientFrame::Hello`] pairing handshake; once
//! the server replies [`ServerFrame::Paired`], subsequent application traffic
//! is carried as Ed25519-signed [`SignedEnvelope`]s
//! ([`ClientFrame::Envelope`]). The envelope's `payload` is the canonical JSON
//! of an A2A `JsonRpcRequest` — the same crypto MUR uses for agent↔agent
//! bridge traffic, reused verbatim so the Mac side can verify against the
//! agent's `trusted_peers` allowlist.

use mur_common::bridge::envelope::SignedEnvelope;
use serde::{Deserialize, Serialize};

/// Frames the phone sends to the Mac endpoint.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Pairing / auth handshake. `token` is the one-time token from the QR
    /// code; `pubkey` is the phone's multibase Ed25519 public key, which the
    /// Mac adds to the agent's `trusted_peers` on success.
    Hello {
        pubkey: String,
        token: String,
        agent: String,
    },
    /// A signed A2A request destined for the agent.
    Envelope { envelope: SignedEnvelope },
}

/// Frames the Mac endpoint sends back to the phone.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Handshake accepted; the phone is now paired with `agent`.
    Paired { agent: String },
    /// Handshake or a later request was rejected.
    Rejected { reason: String },
    /// An asynchronous event from the agent/runtime, mirrored to the phone.
    /// `name` is dot-namespaced (`mobile.transcript`, `mobile.reply`, …) to
    /// match the Hub `EventBus` names used for desktop mirroring.
    Event {
        name: String,
        payload: serde_json::Value,
    },
}
