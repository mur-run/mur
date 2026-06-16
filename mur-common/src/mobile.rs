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
    /// Enrollment step 1 (proto ≥ 2): the phone announces intent WITHOUT sending
    /// the token. `wid` is the pairing-window id from the QR; `pubkey` is the
    /// phone's Ed25519 key to enroll. The daemon replies [`ServerFrame::PairChallenge`].
    HelloInit {
        proto: u32,
        agent: String,
        pubkey: String,
        wid: String,
    },
    /// Enrollment step 3: proof of token possession. `proof` is
    /// HMAC-SHA256(token, transcript) — the token itself is never transmitted.
    HelloProof { wid: String, proof: Vec<u8> },
    /// Reconnect by paired KEY, no enrollment token (the steady-state auth). The
    /// phone announces its `pubkey`; the daemon replies [`ServerFrame::Challenge`]
    /// with a fresh per-connection nonce, which the phone signs back in a
    /// [`ClientFrame::ResumeProof`]. Distinct from `Hello` so single-use
    /// enrollment windows never gate reconnects.
    Resume { pubkey: String, agent: String },
    /// Proof for a `Resume`: a [`SignedEnvelope`] whose `payload` is exactly the
    /// challenge nonce bytes, signed by the device key. The daemon-issued nonce
    /// makes a captured proof non-replayable on a new connection.
    ResumeProof { envelope: SignedEnvelope },
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
    /// Handshake accepted; the phone is now paired with `agent`. For a proof
    /// enrollment (proto ≥ 2), `confirm` is HMAC-SHA256(token, daemon→phone
    /// transcript) — the phone verifies it to authenticate the daemon (closes
    /// rogue-daemon/relay MITM). Empty for legacy Hello / Resume paths.
    Paired {
        agent: String,
        #[serde(default)]
        confirm: Vec<u8>,
    },
    /// Enrollment step 2 (proto ≥ 2): challenge for a [`ClientFrame::HelloInit`].
    /// `nonce` is a fresh 32-byte liveness value; `did` is the daemon agent's
    /// Ed25519 identity (multibase), which the phone cross-checks against the QR
    /// to bind the endpoint on the TLS-less LAN.
    PairChallenge {
        wid: String,
        nonce: Vec<u8>,
        did: String,
    },
    /// The handshake or a later frame was rejected.
    Rejected { reason: String },
    /// Challenge issued in reply to a [`ClientFrame::Resume`]: a fresh
    /// per-connection nonce the phone must sign (in a `ResumeProof`) to prove it
    /// holds the paired device key.
    Challenge { nonce: String },
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

// ── Enrollment proof (proto ≥ 2) ─────────────────────────────────────────────
//
// The phone proves possession of the single-use pairing token WITHOUT
// transmitting it: HMAC-SHA256(token, transcript). The token is the HMAC key,
// never part of the signed bytes and never on the wire — closing the on-path
// LAN sniff on the TLS-less WebSocket. All primitives live here (the only crate
// with the crypto deps); the daemon and SDK call these free functions.

/// Domain tag for the enrollment transcript (version-bound for anti-downgrade).
const PAIR_DOMAIN: &[u8] = b"mur-pair-v2";
/// Per-direction role tags: a daemon→phone confirm can't be replayed as a
/// phone→daemon proof (reflection defense).
pub const PAIR_ROLE_PHONE_TO_DAEMON: &[u8] = b"mur-pair-v2/phone->daemon";
pub const PAIR_ROLE_DAEMON_TO_PHONE: &[u8] = b"mur-pair-v2/daemon->phone";

/// Build the enrollment transcript bound to every field that scopes the proof,
/// using FIXED-length (u32_le) length-prefixing so no field boundary is
/// ambiguous (mirrors `bridge::envelope::signing_bytes`). The token is NOT
/// included — it is the HMAC key in [`pair_proof`], never in the signed bytes.
pub fn pair_transcript(
    role: &[u8],
    proto: u32,
    agent: &str,
    wid: &str,
    did: &str,
    phone_pubkey: &str,
    nonce: &[u8],
) -> Vec<u8> {
    fn put(out: &mut Vec<u8>, field: &[u8]) {
        out.extend_from_slice(&(field.len() as u32).to_le_bytes());
        out.extend_from_slice(field);
    }
    let mut out = Vec::new();
    put(&mut out, PAIR_DOMAIN);
    put(&mut out, role);
    out.extend_from_slice(&proto.to_le_bytes());
    put(&mut out, agent.as_bytes());
    put(&mut out, wid.as_bytes());
    put(&mut out, did.as_bytes());
    put(&mut out, phone_pubkey.as_bytes());
    put(&mut out, nonce);
    out
}

/// HMAC-SHA256(token, transcript) — full 256-bit output, never truncated.
pub fn pair_proof(token: &[u8], transcript: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(token)
        .expect("HMAC accepts a key of any length");
    mac.update(transcript);
    mac.finalize().into_bytes().into()
}

/// Constant-time byte-slice equality (length is not secret; contents compared
/// in constant time so a partial match doesn't leak via timing).
pub fn ct_verify(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && a.ct_eq(b).into()
}

/// A fresh 32-byte liveness nonce for the enrollment challenge (OS CSPRNG).
pub fn mint_nonce() -> [u8; 32] {
    use rand_core::RngCore;
    let mut n = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut n);
    n
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

    #[test]
    fn pair_proof_round_trips_and_rejects_wrong_token() {
        let token = b"the-122-bit-token";
        let t = pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            2,
            "mur",
            "wid-1",
            "did-1",
            "zPHONE",
            &[7u8; 32],
        );
        let proof = pair_proof(token, &t);
        // Same token + transcript verifies.
        assert!(ct_verify(&proof, &pair_proof(token, &t)));
        // A different token produces a different proof.
        assert!(!ct_verify(&proof, &pair_proof(b"wrong-token", &t)));
    }

    #[test]
    fn transcript_is_unambiguous_and_direction_tagged() {
        // Field boundaries are length-prefixed: moving a byte across a boundary
        // changes the transcript (no concatenation ambiguity).
        let a = pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            2,
            "mur",
            "w",
            "d",
            "p",
            &[0u8; 32],
        );
        let b = pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            2,
            "mu",
            "rw",
            "d",
            "p",
            &[0u8; 32],
        );
        assert_ne!(a, b, "length-prefixing prevents field-boundary collisions");

        // The two directions produce different transcripts under the same fields,
        // so a daemon→phone confirm can't be replayed as a phone→daemon proof.
        let phone = pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            2,
            "mur",
            "w",
            "d",
            "p",
            &[1u8; 32],
        );
        let daemon = pair_transcript(
            PAIR_ROLE_DAEMON_TO_PHONE,
            2,
            "mur",
            "w",
            "d",
            "p",
            &[1u8; 32],
        );
        assert_ne!(phone, daemon, "per-direction role tags differ");
        let tok = b"k";
        assert!(!ct_verify(
            &pair_proof(tok, &phone),
            &pair_proof(tok, &daemon)
        ));
    }

    #[test]
    fn mint_nonce_is_fresh() {
        assert_ne!(mint_nonce(), mint_nonce(), "nonces must not repeat");
    }

    #[test]
    fn enrollment_frames_never_carry_the_token() {
        // The proof is KEYED by the token, but the token itself must never appear
        // in any frame the phone sends — that is the whole point of the handshake.
        let token = "SECRET-TOKEN-do-not-leak-0000";
        let t = pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            2,
            "mur",
            "wid",
            "did",
            "zPHONE",
            &[0u8; 32],
        );
        let proof = pair_proof(token.as_bytes(), &t);
        let frames = [
            serde_json::to_string(&ClientFrame::HelloInit {
                proto: 2,
                agent: "mur".into(),
                pubkey: "zPHONE".into(),
                wid: "wid".into(),
            })
            .unwrap(),
            serde_json::to_string(&ClientFrame::HelloProof {
                wid: "wid".into(),
                proof: proof.to_vec(),
            })
            .unwrap(),
        ];
        for f in frames {
            assert!(!f.contains(token), "token leaked into a wire frame: {f}");
        }
    }
}
