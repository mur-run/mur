//! Per-agent Ed25519 identity keypair.
//!
//! Loaded from `<agent_home>/identity.key` (private, 0600) and
//! `<agent_home>/identity.pub` (public, multibase-encoded text).

use ed25519_dalek::{SigningKey, VerifyingKey, SECRET_KEY_LENGTH};
use rand_core::OsRng;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity files not found")]
    NotFound,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid key material: {0}")]
    InvalidKey(String),
    #[error("multibase decode error: {0}")]
    Multibase(#[from] multibase::Error),
}

#[derive(Clone)]
pub struct AgentIdentity {
    signing: SigningKey,
}

impl std::fmt::Debug for AgentIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentIdentity")
            .field("verifying_key", &self.signing.verifying_key())
            .finish()
    }
}

impl AgentIdentity {
    /// Generate a fresh Ed25519 keypair using OS CSPRNG.
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Write both halves of the keypair to the given directory.
    /// Private key is mode 0600 on Unix.
    pub fn save(&self, dir: &Path) -> Result<(), IdentityError> {
        fs::create_dir_all(dir)?;
        let priv_path = dir.join("identity.key");
        let pub_path = dir.join("identity.pub");

        fs::write(&priv_path, self.signing.to_bytes())?;
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&priv_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&priv_path, perms)?;
        }

        let pub_text = encode_pubkey(&self.signing.verifying_key());
        fs::write(&pub_path, pub_text)?;
        Ok(())
    }

    /// Load both halves from the given directory. Prefers the private key
    /// (since we can derive pubkey from it); but also validates that a
    /// present `identity.pub` matches.
    pub fn load(dir: &Path) -> Result<Self, IdentityError> {
        let priv_path = dir.join("identity.key");
        if !priv_path.exists() {
            return Err(IdentityError::NotFound);
        }
        let bytes = fs::read(&priv_path)?;
        if bytes.len() != SECRET_KEY_LENGTH {
            return Err(IdentityError::InvalidKey(format!(
                "expected {SECRET_KEY_LENGTH} bytes, got {}",
                bytes.len()
            )));
        }
        let arr: [u8; SECRET_KEY_LENGTH] = bytes.as_slice().try_into().unwrap();
        let signing = SigningKey::from_bytes(&arr);

        let pub_path = dir.join("identity.pub");
        if pub_path.exists() {
            let text = fs::read_to_string(&pub_path)?;
            let loaded_pub = decode_pubkey(text.trim())?;
            if loaded_pub != *signing.verifying_key().as_bytes() {
                return Err(IdentityError::InvalidKey(
                    "identity.pub does not match identity.key".into(),
                ));
            }
        }

        Ok(Self { signing })
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        *self.signing.verifying_key().as_bytes()
    }

    pub fn pubkey_text(&self) -> String {
        encode_pubkey(&self.signing.verifying_key())
    }
}

/// Encode an Ed25519 public key to multibase base58btc (`z` prefix).
pub fn encode_pubkey(key: &VerifyingKey) -> String {
    multibase::encode(multibase::Base::Base58Btc, key.as_bytes())
}

/// Decode a multibase-encoded pubkey. Accepts any multibase variant.
pub fn decode_pubkey(text: &str) -> Result<[u8; 32], IdentityError> {
    let (_base, bytes) = multibase::decode(text)?;
    if bytes.len() != 32 {
        return Err(IdentityError::InvalidKey(format!(
            "pubkey must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Default location: `<agent_home>/identity.{key,pub}`.
pub fn default_dir(agent_home: &Path) -> PathBuf {
    agent_home.to_path_buf()
}
