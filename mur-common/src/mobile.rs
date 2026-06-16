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
//! P3 adds a voice streaming path: the phone streams raw 16 kHz mono f32 PCM
//! chunks, the Mac runs whisper.cpp for an authoritative transcript, then
//! replies with Kokoro TTS audio chunks.
//!
//! Design: `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.

use crate::bridge::envelope::SignedEnvelope;
use serde::{Deserialize, Serialize};

/// WebSocket path the daemon's mobile endpoint serves.
pub const MOBILE_WS_PATH: &str = "/api/v1/mobile/ws";

/// A2A method (carried inside a signed [`ClientFrame::Envelope`]) by which the
/// phone authoritatively responds to a HITL gate (v4c). It rides the signed
/// envelope path — NOT a plain frame — so the daemon verifies the phone's
/// Ed25519 signature before writing the gate-releasing `HitlResponse`.
pub const HITL_RESPOND_METHOD: &str = "channel/hitl_respond";

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
    /// A signed A2A request destined for the agent (text-only path).
    Envelope { envelope: SignedEnvelope },
    /// Phone begins a voice utterance. The Mac clears its audio accumulator and
    /// prepares for incoming chunks at `sample_rate` Hz, mono f32.
    AudioStreamStart { sample_rate: u32 },
    /// One chunk of raw PCM (f32 LE, `sample_rate` Hz mono, standard base64).
    /// Authenticated by the connection (paired at `Hello`); no per-chunk sig.
    AudioChunk { data: String },
    /// Phone finished speaking. Mac should run STT on the accumulated audio,
    /// forward to the agent, and stream TTS audio back.
    AudioStreamEnd,
    /// Pull channel data. `op` ∈ "list" | "events". For "events", `channel_id`
    /// is required and `since_seq` (inclusive) enables catch-up. Authenticated
    /// by the paired connection (like the audio frames).
    ChannelQuery {
        op: String,
        #[serde(default)]
        channel_id: Option<String>,
        #[serde(default)]
        since_seq: Option<u64>,
    },
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
    /// Mac whisper.cpp authoritative transcript for the user's just-spoken
    /// utterance. Overrides the on-device SFSpeech partial. `is_final: true`
    /// means this is the definitive text for this turn.
    Transcript { text: String, is_final: bool },
    /// A chunk of Kokoro TTS audio (f32 LE PCM, 24 kHz mono). The phone
    /// accumulates chunks until `done: true`, then plays them back.
    /// `base64` is the standard base64 encoding of the raw bytes.
    AudioChunk {
        base64: String,
        sample_rate: u32,
        done: bool,
    },
    /// Response to a `ChannelQuery`. `op` echoes the request; `payload` is a
    /// JSON array (channel summaries for "list", events for "events").
    ChannelData {
        op: String,
        payload: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_query_and_data_frames_round_trip() {
        let q = ClientFrame::ChannelQuery {
            op: "events".into(),
            channel_id: Some("c1".into()),
            since_seq: Some(3),
        };
        let s = serde_json::to_string(&q).unwrap();
        assert!(s.contains("\"type\":\"channel_query\""));
        let back: ClientFrame = serde_json::from_str(&s).unwrap();
        matches!(back, ClientFrame::ChannelQuery { .. });

        let d = ServerFrame::ChannelData {
            op: "list".into(),
            payload: serde_json::json!([]),
        };
        let s2 = serde_json::to_string(&d).unwrap();
        assert!(s2.contains("\"type\":\"channel_data\""));
    }
}
