//! Wire protocol for the MUR mobile app ↔ Mac daemon WebSocket endpoint.
//!
//! Shared by `mur-mobile-sdk` (the phone client) and `mur-daemon` (the Mac
//! endpoint) so both ends agree on the framing. Every message is one JSON
//! object sent as a WebSocket *text* frame. The first client frame is a
//! [`ClientFrame::Hello`] pairing handshake; once the server replies
//! [`ServerFrame::Paired`], application traffic is carried as Ed25519-signed
//! [`SignedEnvelope`]s whose `payload` is the canonical JSON of an A2A
//! `JsonRpcRequest` — the same crypto MUR uses for agent↔agent bridge traffic.
//!
//! Design: `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.

use crate::bridge::envelope::SignedEnvelope;
use serde::{Deserialize, Serialize};

/// WebSocket path the daemon's mobile endpoint serves.
pub const MOBILE_WS_PATH: &str = "/api/v1/mobile/ws";

/// Frames the phone sends to the Mac endpoint.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Pairing / auth handshake. `token` is the one-time token from the QR
    /// code; `pubkey` is the phone's multibase Ed25519 public key, which the
    /// Mac records as a paired device on success. `agent` is the canonical
    /// agent name the phone wants to talk to (e.g. `"mur"`).
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
    /// The handshake or a later frame was rejected.
    Rejected { reason: String },
    /// An asynchronous event mirrored to the phone. `name` is dot-namespaced
    /// (`mobile.transcript`, `mobile.reply`, …) to match the Hub `EventBus`
    /// names used for desktop mirroring.
    Event {
        name: String,
        payload: serde_json::Value,
    },
}
