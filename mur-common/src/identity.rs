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
/// under `agents/` and must be left alone. Each one's `identity.key` is covered
/// as a fixed path by the sandbox's `credential_paths()` and
/// `write_protected_paths()` (the directory itself is not):
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

/// Move one agent's private key out of the agents tree (#850 option (c), step 2).
///
/// Scoped to a SINGLE agent on purpose. Every agent runs this for its own key at
/// startup, so 27 agents starting together never contend — no two of them touch
/// the same file. A sweep that migrated everyone from whichever process got
/// there first would have that race for no gain.
///
/// Runs at startup rather than from `mur update` because `mur update` is not the
/// only upgrade path: `build.sh --install` + `mur agent restart --stale` never
/// invokes it, and neither does `brew upgrade`. A migration hooked there simply
/// would not run on those machines.
///
/// Refuses rather than overwrites when the destination already holds a
/// DIFFERENT key. Silently picking one would change the agent's identity, which
/// forges attribution on every channel it has ever written to, and there is no
/// `.prev` and no rotation attestation to undo it with.
///
/// Returns `Ok(true)` when a key was moved. Idempotent: a second call finds
/// nothing. Never an error for "nothing to migrate".
pub fn migrate_private_key(agent_dir: &Path) -> Result<bool, IdentityError> {
    let key_dir = private_key_dir(agent_dir);
    if key_dir == agent_dir {
        return Ok(false); // not an agent home — commander, publisher, host key
    }
    let legacy = agent_dir.join("identity.key");
    let target = key_dir.join("identity.key");

    let legacy_bytes = match fs::read(&legacy) {
        Ok(b) => b,
        // Nothing to move. Note this deliberately does NOT distinguish denied
        // from absent: a migration that cannot read the source has nothing to
        // do either way, and the load path (which does distinguish) is what
        // reports the problem.
        Err(_) => return Ok(false),
    };

    if let Ok(existing) = fs::read(&target) {
        if existing == legacy_bytes {
            // Already migrated, with a leftover copy behind. Remove the copy —
            // leaving a private key in the agents tree is the exposure this
            // whole change exists to remove.
            let _ = fs::remove_file(&legacy);
            return Ok(false);
        }
        return Err(IdentityError::Exists(format!(
            "{} already holds a DIFFERENT key than {}; refusing to migrate —              resolve by hand, because picking one silently changes this agent's              identity",
            target.display(),
            legacy.display()
        )));
    }

    fs::create_dir_all(&key_dir)?;
    // Copy-then-remove rather than rename: `keys/` and `agents/` are both under
    // mur_home so a rename would normally work, but a bind-mounted or symlinked
    // agents dir would make it cross-device, and a failed rename there would
    // leave the key nowhere.
    fs::write(&target, &legacy_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
    }
    // Only now is it safe to drop the original.
    fs::remove_file(&legacy)?;

    // `.prev` follows its key if present; rekey writes it beside the private
    // half, so leaving it behind would strand a usable old key in the tree.
    let legacy_prev = agent_dir.join("identity.key.prev");
    if let Ok(prev) = fs::read(&legacy_prev) {
        let target_prev = key_dir.join("identity.key.prev");
        if fs::metadata(&target_prev).is_err() {
            fs::write(&target_prev, &prev)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target_prev, fs::Permissions::from_mode(0o600))?;
            }
        }
        let _ = fs::remove_file(&legacy_prev);
    }
    Ok(true)
}

/// Environment flag telling a spawned `mur` that its parent handed it a
/// signing capability on stdin. Set only by `fleet_run`, only alongside the
/// pipe that carries it.
pub const SIGNING_HANDOFF_ENV: &str = "MUR_CHANNEL_SIGNING_STDIN";

/// The signing capability a runtime hands to a child it sealed inside its own
/// sandbox.
///
/// `<mur_home>/keys` is kernel-denied to every sandboxed process on purpose —
/// a prompt-injected agent must not be able to exfiltrate its own signing key.
/// The runtime escapes that only because it loads the key BEFORE the sandbox
/// seals; a child spawned afterwards cannot, so every channel event such a
/// child wrote was unsigned.
///
/// This travels on the child's **stdin pipe** — never argv, never the
/// environment. `ps eww` shows another same-uid process's environment, so an
/// env var here would hand the key straight back to the bash tool the sandbox
/// exists to keep it away from. A pipe is readable only by the process holding
/// the descriptor.
///
/// Design: `docs/superpowers/specs/2026-09-09-in-sandbox-channel-signing-design.md`.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SigningHandoff {
    /// The agent this key signs as. The receiver uses it only when the event
    /// it is writing names this same writer.
    pub agent: String,
    pub key_version: u32,
    /// Raw Ed25519 secret, as a JSON array of bytes — no encoding dependency
    /// stands between the two ends of the pipe.
    pub secret: [u8; SECRET_KEY_LENGTH],
}

impl AgentIdentity {
    /// Rebuild an identity from raw secret bytes.
    ///
    /// The only legitimate caller is the receiving end of a `SigningHandoff`.
    /// Anything that can reach the key file uses `load`.
    pub fn from_secret_bytes(secret: &[u8; SECRET_KEY_LENGTH]) -> Self {
        Self {
            signing: SigningKey::from_bytes(secret),
        }
    }

    /// The raw secret, for building a `SigningHandoff` and nothing else.
    ///
    /// Named for its one use so a second one has to argue for itself: putting
    /// these bytes anywhere they can be read back — a file, argv, an
    /// environment variable — undoes the reason `keys/` is denied at all.
    pub fn secret_bytes_for_handoff(&self) -> [u8; SECRET_KEY_LENGTH] {
        self.signing.to_bytes()
    }

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
        // One location only (#850 option (c) step 3). The legacy fallback is
        // gone because the migration runs at startup, in the same binary,
        // BEFORE this load — so a key still in `agents/` gets moved and then
        // found here. A machine upgrading straight past step 2 is covered for
        // the same reason: the migration is cumulative, not a one-release
        // window.
        //
        // What this does change: if the migration could not move the key, the
        // load now fails instead of quietly reading the old path. That is the
        // intent — an unmigrated key is one that is still readable to every
        // sibling, and it should be loud.
        let priv_path = private_key_dir(dir).join("identity.key");
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

mod rotation;
pub use rotation::*;

#[cfg(test)]
mod identity_readability_tests;

#[cfg(test)]
mod identity_x25519_tests;
