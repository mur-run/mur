//! Noise XK handshake helpers.
//!
//! Pattern: Noise_XK_25519_ChaChaPoly_BLAKE2s. Static key is the agent's
//! X25519 secret derived from its Ed25519 identity.
//!
//! Responder knows its own static key; initiator knows responder's static
//! pubkey a priori (obtained from Agent Card via hub lookup — Q2 decision).

use snow::{Builder, HandshakeState, params::NoiseParams};
use thiserror::Error;

pub const NOISE_XK_PATTERN: &str = "Noise_XK_25519_ChaChaPoly_BLAKE2s";

#[derive(Debug, Error)]
pub enum NoiseError {
    #[error("noise builder error: {0}")]
    Builder(String),
    #[error("invalid params")]
    InvalidParams,
}

/// Build a responder (server-side) handshake state. `static_secret` is 32
/// bytes of X25519 private scalar (typically derived from the agent's
/// Ed25519 identity via `AgentIdentity::to_x25519_static_secret`).
pub fn build_responder(static_secret: &[u8; 32]) -> Result<HandshakeState, NoiseError> {
    let params: NoiseParams = NOISE_XK_PATTERN
        .parse()
        .map_err(|_| NoiseError::InvalidParams)?;
    Builder::new(params)
        .local_private_key(static_secret)
        .build_responder()
        .map_err(|e| NoiseError::Builder(e.to_string()))
}

/// Build an initiator (client-side) handshake state, with prior knowledge
/// of the responder's static pubkey.
pub fn build_initiator(
    static_secret: &[u8; 32],
    remote_static_pub: &[u8; 32],
) -> Result<HandshakeState, NoiseError> {
    let params: NoiseParams = NOISE_XK_PATTERN
        .parse()
        .map_err(|_| NoiseError::InvalidParams)?;
    Builder::new(params)
        .local_private_key(static_secret)
        .remote_public_key(remote_static_pub)
        .build_initiator()
        .map_err(|e| NoiseError::Builder(e.to_string()))
}

/// Maximum frame payload size (16 MiB). Matches commander's HTTP body cap.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("incomplete frame")]
    Incomplete,
    #[error("frame too large (>{} bytes)", MAX_FRAME_BYTES)]
    TooLarge,
}

/// Decoded frame outcome.
#[derive(Debug)]
pub struct DecodedFrame<'a> {
    pub payload: &'a [u8],
    pub consumed: usize,
}

/// Encode a single JSON-RPC payload as a 4-byte BE-length-prefixed frame.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Decode the first complete frame from `buf`. Returns `Incomplete` if
/// fewer than 4+len bytes are present.
pub fn decode_frame(buf: &[u8]) -> Result<DecodedFrame<'_>, FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::Incomplete);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    if buf.len() < 4 + len {
        return Err(FrameError::Incomplete);
    }
    Ok(DecodedFrame {
        payload: &buf[4..4 + len],
        consumed: 4 + len,
    })
}
