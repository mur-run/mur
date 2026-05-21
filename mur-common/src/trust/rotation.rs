//! Key rotation manifest support (§7.1.1).
//!
//! Signed by both old and new keys. Stored at `~/.mur/trust/rotations/<fingerprint>.rotation`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationManifest {
    pub old_pubkey: String,
    pub new_pubkey: String,
    pub issued_at: String,
    pub sig_old: String,
    pub sig_new: String,
}

impl RotationManifest {
    /// Verify both signatures on the rotation manifest.
    pub fn verify(&self) -> Result<(), String> {
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        use ed25519_dalek::{Signature, VerifyingKey};

        let message = format!("{}{}{}", self.old_pubkey, self.new_pubkey, self.issued_at);
        let msg_bytes = message.as_bytes();

        let old_pk_bytes: [u8; 32] = B64
            .decode(&self.old_pubkey)
            .map_err(|e| format!("old_pubkey b64: {e}"))?
            .try_into()
            .map_err(|_| "old_pubkey not 32 bytes".to_string())?;
        let old_vk = VerifyingKey::from_bytes(&old_pk_bytes)
            .map_err(|e| format!("old_pubkey decode: {e}"))?;
        let old_sig_bytes: [u8; 64] = B64
            .decode(&self.sig_old)
            .map_err(|e| format!("sig_old b64: {e}"))?
            .try_into()
            .map_err(|_| "sig_old not 64 bytes".to_string())?;
        let old_sig = Signature::from_bytes(&old_sig_bytes);
        old_vk
            .verify_strict(msg_bytes, &old_sig)
            .map_err(|e| format!("old key signature: {e}"))?;

        let new_pk_bytes: [u8; 32] = B64
            .decode(&self.new_pubkey)
            .map_err(|e| format!("new_pubkey b64: {e}"))?
            .try_into()
            .map_err(|_| "new_pubkey not 32 bytes".to_string())?;
        let new_vk = VerifyingKey::from_bytes(&new_pk_bytes)
            .map_err(|e| format!("new_pubkey decode: {e}"))?;
        let new_sig_bytes: [u8; 64] = B64
            .decode(&self.sig_new)
            .map_err(|e| format!("sig_new b64: {e}"))?
            .try_into()
            .map_err(|_| "sig_new not 64 bytes".to_string())?;
        let new_sig = Signature::from_bytes(&new_sig_bytes);
        new_vk
            .verify_strict(msg_bytes, &new_sig)
            .map_err(|e| format!("new key signature: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    use ed25519_dalek::{Signer, SigningKey};

    fn make_signed(old: &SigningKey, new: &SigningKey, issued_at: &str) -> RotationManifest {
        let old_pk = B64.encode(old.verifying_key().as_bytes());
        let new_pk = B64.encode(new.verifying_key().as_bytes());
        let msg = format!("{}{}{}", old_pk, new_pk, issued_at);
        let sig_old = B64.encode(old.sign(msg.as_bytes()).to_bytes());
        let sig_new = B64.encode(new.sign(msg.as_bytes()).to_bytes());
        RotationManifest {
            old_pubkey: old_pk,
            new_pubkey: new_pk,
            issued_at: issued_at.to_string(),
            sig_old,
            sig_new,
        }
    }

    #[test]
    fn valid_rotation_verifies() {
        let old = SigningKey::from_bytes(&[1u8; 32]);
        let new = SigningKey::from_bytes(&[2u8; 32]);
        let manifest = make_signed(&old, &new, "2026-05-20T12:00:00Z");
        manifest.verify().unwrap();
    }

    #[test]
    fn tampered_timestamp_rejected() {
        let old = SigningKey::from_bytes(&[1u8; 32]);
        let new = SigningKey::from_bytes(&[2u8; 32]);
        let mut manifest = make_signed(&old, &new, "2026-05-20T12:00:00Z");
        manifest.issued_at = "2025-01-01T00:00:00Z".into();
        assert!(manifest.verify().is_err());
    }

    #[test]
    fn missing_new_key_signature_rejected() {
        let old = SigningKey::from_bytes(&[1u8; 32]);
        let new = SigningKey::from_bytes(&[2u8; 32]);
        let attacker = SigningKey::from_bytes(&[99u8; 32]);
        // Old signs the legitimate message but new is replaced with attacker's signature
        let mut manifest = make_signed(&old, &new, "2026-05-20T12:00:00Z");
        let attacker_sig = attacker.sign(
            format!(
                "{}{}{}",
                manifest.old_pubkey, manifest.new_pubkey, manifest.issued_at
            )
            .as_bytes(),
        );
        manifest.sig_new = B64.encode(attacker_sig.to_bytes());
        assert!(manifest.verify().is_err());
    }
}
