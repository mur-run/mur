//! Signing and verifying a reference.

use crate::payload::{RefPayload, WireRef};
use mur_common::identity::{AgentIdentity, canonical_json, verify_bytes};
use std::path::Path;

/// Domain-separation prefix of the signed bytes. A channel event is signed
/// over plain canonical JSON, so neither signature can stand in for the other.
pub const REF_SIGN_DOMAIN: &[u8] = b"mur.fleet_shared_ref.v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("the reference names an invalid member")]
    InvalidMember,
    #[error("no public key for the member at that key version")]
    NoKey,
    #[error("the signature does not verify")]
    BadSignature,
}

/// Sign `payload` as `identity`, which must be `payload.member`'s identity.
pub fn sign_ref(identity: &AgentIdentity, payload: &RefPayload) -> WireRef {
    let sig = identity.sign_multibase(&signed_bytes(payload));
    WireRef {
        payload: payload.clone(),
        sig,
    }
}

/// Verify `wire` against the key `<mur_home>/agents/<member>` has at
/// `payload.key_version`.
pub fn verify_ref(mur_home: &Path, wire: &WireRef) -> Result<(), VerifyError> {
    let payload = &wire.payload;
    // The member becomes a path component below: check the name first so a
    // crafted reference cannot point resolution outside `agents/`.
    if !mur_common::fleet::valid_member_name(&payload.member) {
        return Err(VerifyError::InvalidMember);
    }
    let agent_home = mur_home.join("agents").join(&payload.member);
    let pubkey = mur_channel::sign::resolve_writer_pubkey(&agent_home, Some(payload.key_version))
        .ok_or(VerifyError::NoKey)?;
    if verify_bytes(&pubkey, &signed_bytes(payload), &wire.sig) {
        Ok(())
    } else {
        Err(VerifyError::BadSignature)
    }
}

/// The bytes a reference signature covers: the domain prefix, then the
/// canonical JSON of the payload (which carries `v` and `kind`).
fn signed_bytes(payload: &RefPayload) -> Vec<u8> {
    let mut bytes = REF_SIGN_DOMAIN.to_vec();
    bytes.extend(canonical_json(payload));
    bytes
}
