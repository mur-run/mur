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
