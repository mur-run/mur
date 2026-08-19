//! Per-agent Ed25519 identity keypair.
//!
//! Loaded from `<agent_home>/identity.key` (private, 0600) and
//! `<agent_home>/identity.pub` (public, multibase-encoded text).

use ed25519_dalek::{SECRET_KEY_LENGTH, SigningKey, VerifyingKey};
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
    /// Refused to overwrite a key that is already there. `save` is
    /// write-if-absent by contract: a private key has no `.prev` and no
    /// rotation attestation behind it, so clobbering one is unrecoverable.
    /// Callers that legitimately replace a key (`mur agent rekey`) write to a
    /// scratch directory and rename.
    #[error("refusing to overwrite an existing identity key at {0}")]
    Exists(String),
    /// The key is there but this process may not read it — a sandbox deny, or
    /// wrong ownership. Distinct from `NotFound` on purpose: callers that treat
    /// an absent key as "not signed yet" must NOT treat an unreadable one the
    /// same way, or a deny silently downgrades signing to unsigned.
    #[error("identity key exists but is not readable: {0}")]
    Denied(String),
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

/// Where an agent's PRIVATE key lives: `<mur_home>/keys/<name>/identity.key`
/// for a directory of the form `<mur_home>/agents/<name>`, and `dir` itself for
/// anything else.
///
/// #850 option (c). Both sandbox rules that protect or expose key material work
/// by ENUMERATING `agents/*` — the sibling-key deny (#975) and the peer-public
/// grant (#1006) — so both miss an agent created after the policy sealed.
/// Neither backend can express "a file with this name under any agent home", so
/// the rule has to become a subtree, and that means the private half cannot
/// share a directory with the public half.
///
/// The narrow mapping is deliberate. Three callers pass a directory that is NOT
/// under `agents/` and must be left alone, each already covered as a fixed path
/// by `credential_paths()`:
///
/// - `<mur_home>/commander` (commander signing identity)
/// - `<mur_home>/publisher` (skill publisher identity, #1013)
/// - `<mur_home>` itself (the host key)
///
/// Keying off "parent is literally `agents`" is what keeps those out.
pub fn private_key_dir(dir: &Path) -> PathBuf {
    let is_agent_home = dir
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "agents");
    if !is_agent_home {
        return dir.to_path_buf();
    }
    let (Some(name), Some(mur_home)) = (dir.file_name(), dir.parent().and_then(|p| p.parent()))
    else {
        return dir.to_path_buf();
    };
    mur_home.join("keys").join(name)
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
    ///
    /// **Write-if-absent.** Refuses when `identity.key` already exists, because
    /// `fs::write` truncates and a private key has nothing behind it to restore
    /// from — no `.prev`, and no rotation attestation to bridge the swap, so
    /// every event the old key signed silently stops attributing. A caller that
    /// means to replace a key writes to a scratch directory and renames, which
    /// is what `mur agent rekey` does.
    ///
    /// This is not hypothetical caution: `mur skill publish` guarded on
    /// `publisher-identity.key` and wrote `identity.key`, so it overwrote the
    /// host key on every machine that had one (#1011).
    pub fn save(&self, dir: &Path) -> Result<(), IdentityError> {
        fs::create_dir_all(dir)?;
        // Private half goes to `keys/<name>/` for an agent home, `dir` itself
        // otherwise (#850 option (c), step 1). Public half never moves — it is
        // what peers read to verify, and `agents/` staying public-only is the
        // whole point.
        let key_dir = private_key_dir(dir);
        fs::create_dir_all(&key_dir)?;
        let priv_path = key_dir.join("identity.key");
        let pub_path = dir.join("identity.pub");

        // `exists()` is the right call here despite #1010: a false from a
        // denied stat means we are about to fail the write anyway, and the
        // conservative reading (treat unknown as "might exist") would block
        // legitimate first-time saves under a sandbox.
        if priv_path.exists() {
            return Err(IdentityError::Exists(priv_path.display().to_string()));
        }

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
        // Read the new location first, then fall back to the legacy one
        // in-place under `agents/`. The fallback is not optional: `mur update`
        // restarts agents one at a time, so during a rollout some agents are
        // still on code that wrote the key to the old path. Without the
        // fallback, the first agent to restart after a move would fail to
        // start. It is removed only once the migration (step 2) has run
        // everywhere.
        let key_dir = private_key_dir(dir);
        let new_path = key_dir.join("identity.key");
        let legacy_path = dir.join("identity.key");
        let priv_path = if new_path != legacy_path && fs::metadata(&new_path).is_ok() {
            new_path
        } else {
            legacy_path
        };
        // `Path::exists()` cannot be used here: it answers false for ANY stat
        // failure, so a sandbox deny is indistinguishable from a missing file
        // and the caller's "no key yet" branch runs when the truth is "you may
        // not read this key". Ask for the metadata and keep the error kind.
        if let Err(e) = fs::metadata(&priv_path) {
            return Err(match e.kind() {
                io::ErrorKind::NotFound => IdentityError::NotFound,
                _ => IdentityError::Denied(format!("{}: {e}", priv_path.display())),
            });
        }
        let bytes = fs::read(&priv_path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => IdentityError::NotFound,
            io::ErrorKind::PermissionDenied => {
                IdentityError::Denied(format!("{}: {e}", priv_path.display()))
            }
            _ => IdentityError::Io(e),
        })?;
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

    /// Load ONLY the public key from `<dir>/identity.pub` — for verifiers
    /// (inbox ingest, proposal review) that must not require the private key.
    pub fn load_pubkey(dir: &Path) -> Result<[u8; 32], IdentityError> {
        let path = dir.join("identity.pub");
        if !path.exists() {
            return Err(IdentityError::NotFound);
        }
        decode_pubkey(fs::read_to_string(&path)?.trim())
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    /// Sign `msg` with the Ed25519 private key and return the raw 64-byte
    /// signature. Callers that only have a `&AgentIdentity` (and therefore
    /// cannot import `ed25519_dalek::Signer` themselves) should use this
    /// instead of calling `signing_key().sign()` directly.
    pub fn sign_bytes(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing.sign(msg).to_bytes()
    }

    /// Sign `msg` and encode the signature as multibase Base58Btc — the exact
    /// encoding `verify_bytes` decodes (mirrors mur-channel/src/sign.rs).
    pub fn sign_multibase(&self, msg: &[u8]) -> String {
        multibase::encode(multibase::Base::Base58Btc, self.sign_bytes(msg))
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

    /// Alias for `pubkey_text()` — returns the verifying key as multibase
    /// base58btc (`z`-prefixed string), matching the `bridge_pubkey_multibase`
    /// field used in signed envelopes.
    pub fn public_key_multibase(&self) -> String {
        encode_pubkey(&self.signing.verifying_key())
    }

    /// Derive the X25519 static secret usable by Noise XK.
    ///
    /// Ed25519 and X25519 both use Curve25519 underneath; the Ed25519
    /// SigningKey scalar maps directly to an X25519 StaticSecret.
    /// ed25519-dalek 2.x exposes `to_scalar_bytes()` for exactly this.
    pub fn to_x25519_static_secret(&self) -> x25519_dalek::StaticSecret {
        let scalar_bytes = self.signing.to_scalar_bytes();
        x25519_dalek::StaticSecret::from(scalar_bytes)
    }
}

/// Verify a multibase-encoded Ed25519 signature over `msg` against `pubkey`.
/// Fail-closed: any decode/length/verify error returns false.
pub fn verify_bytes(pubkey: &[u8; 32], msg: &[u8], sig_multibase: &str) -> bool {
    let Ok((_, sig_bytes)) = multibase::decode(sig_multibase) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    vk.verify_strict(msg, &ed25519_dalek::Signature::from_bytes(&sig_arr))
        .is_ok()
}

/// True iff `bytes` is a valid Ed25519 verifying key (on-curve), not just 32 bytes.
pub fn valid_ed25519_pubkey(bytes: &[u8; 32]) -> bool {
    VerifyingKey::from_bytes(bytes).is_ok()
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

/// Convert an Ed25519 public key to its X25519 (Montgomery `u`) public key.
///
/// Ed25519 and X25519 share Curve25519; an Ed25519 verifying key is an Edwards
/// point whose Montgomery form is the corresponding X25519 public key. This is
/// the public-key analogue of [`AgentIdentity::to_x25519_static_secret`], and
/// lets us match a Noise-XK peer's authenticated static key against a peer's
/// Ed25519 identity. Returns `None` if `ed_pub` is not a valid Edwards point.
pub fn ed25519_pub_to_x25519(ed_pub: &[u8; 32]) -> Option<[u8; 32]> {
    let compressed = curve25519_dalek::edwards::CompressedEdwardsY(*ed_pub);
    let point = compressed.decompress()?;
    Some(point.to_montgomery().to_bytes())
}

/// Decode a multibase Ed25519 pubkey and convert it to its X25519 public key.
pub fn x25519_pub_from_multibase(text: &str) -> Result<[u8; 32], IdentityError> {
    let ed = decode_pubkey(text)?;
    ed25519_pub_to_x25519(&ed)
        .ok_or_else(|| IdentityError::InvalidKey("pubkey is not a valid Edwards point".into()))
}

/// Default location: `<agent_home>/identity.{key,pub}`.
pub fn default_dir(agent_home: &Path) -> PathBuf {
    agent_home.to_path_buf()
}

// ---------------------------------------------------------------------------
// RotationAttestation — proof that a key rotation was authorized by the
// holder of the prior identity key.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

/// Why a rotation happened. Free-form audit hint; does not affect verification
/// rules other than `Emergency`, which permits an empty signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationReason {
    Scheduled,
    SuspectCompromise,
    OwnerChange,
    Emergency,
}

/// Cryptographic proof of an identity-key rotation.
///
/// `signature` is multibase base58btc Ed25519 over `canonical_bytes()`
/// (which serializes every field except `signature` itself).
///
/// For `reason = Emergency`, signature MAY be empty — those rotations
/// require out-of-band admin approval to take effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationAttestation {
    /// Schema version. Always 1 for now.
    pub schema: u32,
    /// Agent UUIDv7 — stable across rotations.
    pub uuid: String,
    /// Signing algorithm. "ed25519" for now.
    pub algorithm: String,
    /// Outgoing pubkey (multibase). Empty string for the bootstrap entry only.
    pub old_pubkey: String,
    /// Incoming pubkey (multibase). Always present.
    pub new_pubkey: String,
    pub old_key_version: u32,
    /// = old_key_version + 1 for non-emergency rotations.
    pub new_key_version: u32,
    /// RFC3339 timestamp.
    pub rotated_at: String,
    pub reason: RotationReason,
    /// Multibase Ed25519 signature over canonical_bytes(). Empty for
    /// Emergency reason or for the bootstrap entry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// True only for the create-time entry (no prior key existed).
    #[serde(default, skip_serializing_if = "is_false")]
    pub bootstrap: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl RotationAttestation {
    /// Build a new (unsigned) attestation.
    pub fn new(
        uuid: impl Into<String>,
        old_pubkey: impl Into<String>,
        new_pubkey: impl Into<String>,
        old_key_version: u32,
        new_key_version: u32,
        rotated_at: impl Into<String>,
        reason: RotationReason,
    ) -> Self {
        Self {
            schema: 1,
            uuid: uuid.into(),
            algorithm: "ed25519".into(),
            old_pubkey: old_pubkey.into(),
            new_pubkey: new_pubkey.into(),
            old_key_version,
            new_key_version,
            rotated_at: rotated_at.into(),
            reason,
            signature: String::new(),
            bootstrap: false,
        }
    }

    /// Mark this attestation as the bootstrap entry written at agent
    /// create time. Bootstrap entries have empty `old_pubkey` and empty
    /// `signature`; they exist only to anchor the rotation chain.
    pub fn into_bootstrap(mut self) -> Self {
        self.bootstrap = true;
        self.old_pubkey = String::new();
        self.signature = String::new();
        self
    }

    /// Canonical bytes used for signing. Serializes every field of `self`
    /// EXCEPT `signature` (which is being computed) using JSON with sorted
    /// keys and no whitespace.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        canonical_json(&clone)
    }

    /// Compute the Ed25519 signature using the given signing key and store
    /// it in `self.signature`. Idempotent.
    pub fn sign(&mut self, signing: &ed25519_dalek::SigningKey) {
        use ed25519_dalek::Signer;
        let sig = signing.sign(&self.canonical_bytes());
        self.signature = multibase::encode(multibase::Base::Base58Btc, sig.to_bytes());
    }

    /// Verify `self.signature` against the supplied multibase-encoded
    /// `old_pubkey`. Returns `Ok(())` on a valid signature.
    ///
    /// Bootstrap entries (`bootstrap = true`) are accepted unconditionally —
    /// they have nothing to verify against.
    /// Emergency entries (`reason = Emergency`) with empty signature are
    /// REJECTED here; callers must use `verify_or_emergency` if they want
    /// the emergency-allowed semantics.
    pub fn verify(&self, old_pubkey: &str) -> Result<(), IdentityError> {
        if self.bootstrap {
            return Ok(());
        }
        if self.signature.is_empty() {
            return Err(IdentityError::InvalidKey(
                "attestation signature is empty".into(),
            ));
        }
        let pub_bytes = decode_pubkey(old_pubkey)?;
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes)
            .map_err(|e| IdentityError::InvalidKey(format!("verifying key: {e}")))?;
        let (_base, sig_bytes) = multibase::decode(&self.signature)?;
        let sig_arr: [u8; 64] = sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::InvalidKey("signature length != 64".into()))?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
        verifying
            .verify_strict(&self.canonical_bytes(), &sig)
            .map_err(|e| IdentityError::InvalidKey(format!("signature: {e}")))?;
        Ok(())
    }

    /// Like `verify`, but accepts emergency rotations with empty signature.
    /// Caller is responsible for the out-of-band approval check.
    pub fn verify_or_emergency(&self, old_pubkey: &str) -> Result<(), IdentityError> {
        if self.reason == RotationReason::Emergency && self.signature.is_empty() {
            return Ok(());
        }
        self.verify(old_pubkey)
    }
}

// ---------------------------------------------------------------------------
// Chain verification — M5.1
// ---------------------------------------------------------------------------

/// Per-call options for `verify_chain`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChainOptions {
    /// If true, accept emergency entries with empty signature (i.e. use
    /// `verify_or_emergency` instead of strict `verify`). Commander code
    /// that has out-of-band approval already should set this true; peer
    /// code that is mirroring without approval should leave it false.
    pub allow_emergency: bool,
}

/// Outcome of a successful chain verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainOutcome {
    /// Highest key_version observed.
    pub head_key_version: u32,
    /// Pubkey at head_key_version.
    pub head_pubkey: String,
    /// Total entries (including bootstrap).
    pub length: usize,
}

/// Errors from `verify_chain`.
#[derive(Debug)]
pub enum ChainError {
    /// Chain is empty or first entry is not a bootstrap.
    MissingBootstrap,
    /// Chain skipped a key_version (e.g. went 1 -> 3).
    VersionSkip { expected: u32, got: u32 },
    /// `a[i].old_pubkey` does not match `a[i-1].new_pubkey`.
    PubkeyDiscontinuity { at_version: u32 },
    /// Same `new_key_version` appears twice in the chain.
    DuplicateVersion(u32),
    /// Bad Ed25519 signature on a non-bootstrap, non-emergency entry.
    BadSignature { at_version: u32, detail: String },
    /// Emergency entry encountered with `allow_emergency = false`.
    EmergencyDisallowed { at_version: u32 },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBootstrap => {
                write!(
                    f,
                    "chain must start with a bootstrap entry (bootstrap=true, key_version=0)"
                )
            }
            Self::VersionSkip { expected, got } => {
                write!(f, "version skip: expected {expected}, got {got}")
            }
            Self::PubkeyDiscontinuity { at_version } => {
                write!(
                    f,
                    "pubkey discontinuity at key_version {at_version}: old_pubkey does not match prior new_pubkey"
                )
            }
            Self::DuplicateVersion(v) => write!(f, "duplicate key_version {v}"),
            Self::BadSignature { at_version, detail } => {
                write!(f, "bad signature at key_version {at_version}: {detail}")
            }
            Self::EmergencyDisallowed { at_version } => {
                write!(
                    f,
                    "emergency attestation at key_version {at_version} requires allow_emergency=true"
                )
            }
        }
    }
}

impl std::error::Error for ChainError {}

/// Walk the chain top-to-bottom and verify it forms a valid history.
/// Returns the head pubkey + version on success.
pub fn verify_chain(
    chain: &[RotationAttestation],
    opts: ChainOptions,
) -> std::result::Result<ChainOutcome, ChainError> {
    if chain.is_empty() {
        return Err(ChainError::MissingBootstrap);
    }
    let first = &chain[0];
    if !first.bootstrap || first.new_key_version != 0 {
        return Err(ChainError::MissingBootstrap);
    }

    let mut prev_pubkey = first.new_pubkey.clone();
    let mut prev_version = 0u32;
    let mut seen_versions = std::collections::HashSet::new();
    seen_versions.insert(0u32);

    for (i, a) in chain.iter().enumerate().skip(1) {
        // No duplicate versions
        if !seen_versions.insert(a.new_key_version) {
            return Err(ChainError::DuplicateVersion(a.new_key_version));
        }
        // Strict +1 succession
        let expected = prev_version + 1;
        if a.old_key_version != prev_version || a.new_key_version != expected {
            return Err(ChainError::VersionSkip {
                expected,
                got: a.new_key_version,
            });
        }
        // Pubkey continuity
        if a.old_pubkey != prev_pubkey {
            return Err(ChainError::PubkeyDiscontinuity {
                at_version: a.new_key_version,
            });
        }
        // Signature (or emergency allowance)
        if a.reason == RotationReason::Emergency {
            if !opts.allow_emergency {
                return Err(ChainError::EmergencyDisallowed {
                    at_version: a.new_key_version,
                });
            }
            // Lenient verify: empty signature is fine for emergency
            if let Err(e) = a.verify_or_emergency(&a.old_pubkey) {
                return Err(ChainError::BadSignature {
                    at_version: a.new_key_version,
                    detail: e.to_string(),
                });
            }
        } else if let Err(e) = a.verify(&a.old_pubkey) {
            return Err(ChainError::BadSignature {
                at_version: a.new_key_version,
                detail: e.to_string(),
            });
        }

        prev_pubkey = a.new_pubkey.clone();
        prev_version = a.new_key_version;
        let _ = i; // silence unused
    }

    Ok(ChainOutcome {
        head_key_version: prev_version,
        head_pubkey: prev_pubkey,
        length: chain.len(),
    })
}

/// Canonical JSON: sorted keys, no whitespace. Used so that signers and
/// verifiers compute identical byte sequences regardless of language /
/// serializer choices.
fn canonical_json<T: serde::Serialize>(value: &T) -> Vec<u8> {
    // serde_json with a BTreeMap-like ordering. The simplest approach: round-trip
    // through a `serde_json::Value`, then walk it depth-first emitting bytes.
    let v: serde_json::Value =
        serde_json::to_value(value).expect("serialize should not fail for our types");
    let mut out = Vec::new();
    write_canonical(&mut out, &v);
    out
}

fn write_canonical(out: &mut Vec<u8>, v: &serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => {
            // serde_json::to_string handles escaping for us
            let escaped = serde_json::to_string(s).unwrap();
            out.extend_from_slice(escaped.as_bytes());
        }
        Value::Array(arr) => {
            out.push(b'[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Sort keys for deterministic output
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                let kesc = serde_json::to_string(k).unwrap();
                out.extend_from_slice(kesc.as_bytes());
                out.push(b':');
                write_canonical(out, &map[*k]);
            }
            out.push(b'}');
        }
    }
}

#[cfg(test)]
mod identity_readability_tests {
    use super::*;

    /// A key that exists but cannot be read must NOT report as absent.
    ///
    /// `Path::exists()` answers false for any stat failure, so before this the
    /// two were indistinguishable and every caller's "no key yet" branch ran
    /// when the truth was "you may not read this key" — which is exactly what
    /// a sandbox deny produces.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_key_is_denied_not_notfound() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        AgentIdentity::generate().save(dir.path()).unwrap();
        let key = dir.path().join("identity.key");

        // Precondition: readable right now, so the assert below is about the
        // permission change and nothing else.
        assert!(AgentIdentity::load(dir.path()).is_ok());

        // Case 1 — the file is unreadable but still STATtable (chmod 000).
        // `exists()` says true here, so this exercises the read mapping.
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = AgentIdentity::load(dir.path()).unwrap_err();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            matches!(unreadable, IdentityError::Denied(_)),
            "an unreadable key must be Denied, got {unreadable:?}"
        );

        // Case 2 — the file cannot even be STATted, because the directory
        // holding it is not searchable. THIS is what a sandbox deny looks
        // like, and it is the case `Path::exists()` gets wrong: it answers
        // false, so the old code reported NotFound and every caller's
        // "no key yet" branch ran.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let unstattable = AgentIdentity::load(dir.path()).unwrap_err();
        let exists_lies = !key.exists();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            exists_lies,
            "precondition: Path::exists() must be answering false here, or this \
             case is not reproducing a sandbox deny"
        );
        assert!(
            matches!(unstattable, IdentityError::Denied(_)),
            "a key that cannot be STATted must be Denied, not NotFound, got {unstattable:?}"
        );
    }

    /// An agent home's private key moves; its public half does not.
    #[test]
    fn an_agent_key_moves_and_the_public_half_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let mur = tmp.path();
        let agent = mur.join("agents").join("pm");
        AgentIdentity::generate().save(&agent).unwrap();

        assert!(
            mur.join("keys").join("pm").join("identity.key").exists(),
            "private key must live under keys/"
        );
        assert!(
            !agent.join("identity.key").exists(),
            "the agents tree must hold no private key"
        );
        assert!(
            agent.join("identity.pub").exists(),
            "the public half must stay where peers read it"
        );
    }

    /// The three callers that pass a directory outside `agents/` must be left
    /// exactly as they are — the spec named this as the main correctness risk.
    #[test]
    fn non_agent_identities_are_not_remapped() {
        let tmp = tempfile::tempdir().unwrap();
        let mur = tmp.path();
        for dir in [
            mur.to_path_buf(),     // host key
            mur.join("commander"), // commander signing identity
            mur.join("publisher"), // skill publisher identity (#1013)
        ] {
            assert_eq!(
                private_key_dir(&dir),
                dir,
                "{} must not be remapped",
                dir.display()
            );
            AgentIdentity::generate().save(&dir).unwrap();
            assert!(
                dir.join("identity.key").exists(),
                "{} lost its key to the remap",
                dir.display()
            );
        }
    }

    /// A key still in the legacy location must load, because `mur update`
    /// restarts agents ONE AT A TIME. Without this, the first agent to restart
    /// after the move would fail to start mid-rollout.
    #[test]
    fn a_legacy_key_still_loads_from_the_agents_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agents").join("legacy");
        std::fs::create_dir_all(&agent).unwrap();
        let id = AgentIdentity::generate();
        // Write it the OLD way, bypassing `save`.
        std::fs::write(agent.join("identity.key"), id.signing.to_bytes()).unwrap();

        let loaded = AgentIdentity::load(&agent).expect("legacy key must still load");
        assert_eq!(loaded.pubkey_text(), id.pubkey_text());
    }

    /// ...and when both exist, the new location wins, so a completed migration
    /// is authoritative even if a stale file is left behind.
    #[test]
    fn the_new_location_wins_over_a_leftover_legacy_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mur = tmp.path();
        let agent = mur.join("agents").join("dual");
        let current = AgentIdentity::generate();
        current.save(&agent).unwrap();
        let stale = AgentIdentity::generate();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("identity.key"), stale.signing.to_bytes()).unwrap();

        let loaded = AgentIdentity::load(&agent).unwrap();
        assert_eq!(
            loaded.pubkey_text(),
            current.pubkey_text(),
            "the migrated key must win over the leftover"
        );
    }

    /// `save` must never clobber an existing private key.
    ///
    /// This is the mechanism that would have turned #1011 into a loud failure:
    /// `mur skill publish` called `save` on a directory that already held the
    /// HOST key, and `fs::write` truncated it. There is no `.prev` and no
    /// rotation attestation for such a swap, so the old key's signatures stop
    /// attributing with nothing to restore from.
    #[test]
    fn save_refuses_to_overwrite_an_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let first = AgentIdentity::generate();
        first.save(dir.path()).unwrap();
        let original = std::fs::read(dir.path().join("identity.key")).unwrap();

        let err = AgentIdentity::generate().save(dir.path()).unwrap_err();

        assert!(
            matches!(err, IdentityError::Exists(_)),
            "expected Exists, got {err:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("identity.key")).unwrap(),
            original,
            "the existing key was modified despite the refusal"
        );
    }

    /// ...but a first save into a fresh directory still works, which is every
    /// legitimate caller (agent create, export minting a missing key, rekey
    /// writing to its scratch dir).
    #[test]
    fn save_into_an_empty_directory_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let id = AgentIdentity::generate();
        id.save(dir.path()).unwrap();
        assert_eq!(
            AgentIdentity::load(dir.path()).unwrap().pubkey_text(),
            id.pubkey_text()
        );
    }

    /// ...and a genuinely absent key still reports NotFound, because callers
    /// legitimately treat that as "nothing signed yet".
    #[test]
    fn a_missing_key_is_still_notfound() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            AgentIdentity::load(dir.path()).unwrap_err(),
            IdentityError::NotFound
        ));
    }
}

#[cfg(test)]
mod identity_x25519_tests {
    use super::*;

    #[test]
    fn x25519_pub_matches_secret_derivation() {
        // The public-side Ed25519→X25519 conversion must equal the X25519
        // public derived from the agent's own static secret — otherwise the
        // Noise peer-auth allowlist would never match `get_remote_static()`.
        let id = AgentIdentity::generate();
        let from_secret = x25519_dalek::PublicKey::from(&id.to_x25519_static_secret());
        let from_pub = x25519_pub_from_multibase(&id.public_key_multibase()).unwrap();
        assert_eq!(from_secret.as_bytes(), &from_pub);
    }
}
