//! Inbound envelope verification against an explicit trust list.
//!
//! See [`crate::bridge`] module docs for the trust model: verification is
//! transport-independent and MUST run on every inbound A2A envelope from a
//! bridge before processing.

use mur_common::bridge::envelope::{EnvelopeError, SignedEnvelope, verify_envelope_with_pubkey};
use mur_common::bridge::peer::TrustedPeer;

/// Verify a `SignedEnvelope` against an explicit trust list. Returns Ok(()) only if:
///   1. envelope's `bridge_pubkey_multibase` matches some `TrustedPeer`
///   2. that peer's pinned `key_version`, if any, matches the envelope
///   3. the Ed25519 signature verifies against `payload`
pub fn verify_inbound_envelope(
    env: &SignedEnvelope,
    peers: &[TrustedPeer],
) -> Result<(), EnvelopeError> {
    let peer = peers
        .iter()
        .find(|p| p.pubkey_multibase == env.bridge_pubkey_multibase)
        .ok_or(EnvelopeError::UntrustedPeer)?;
    if let Some(pinned) = peer.key_version
        && pinned != env.key_version
    {
        return Err(EnvelopeError::UntrustedPeer);
    }
    verify_envelope_with_pubkey(env, &peer.pubkey_multibase)
}
