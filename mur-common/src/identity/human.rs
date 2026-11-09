//! Where the human owner's identity lives (at-mention phase 2, D-1, D-8).
//!
//! Split in two on purpose, like an agent's:
//!
//! - `<mur_home>/identity/` holds the PUBLIC half (`identity.pub`,
//!   `rotations.jsonl`). Every agent may read it — verifying a human-signed
//!   event needs it — and no agent may write it: the rotation chain there
//!   decides which key speaks for the human.
//! - `<mur_home>/keys/.human/` holds the private key. It sits under `keys/`,
//!   which the sandbox already denies for read and write as a subtree, and
//!   `.human` can never collide with an agent because `.` is not a valid
//!   agent-name character (`agent_name::validate_agent_name`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    ChainError, ChainOptions, ChainOutcome, RotationAttestation, decode_pubkey, verify_chain,
};

/// Directory under `<mur_home>` holding the human's public key material.
pub const HUMAN_IDENTITY_DIR: &str = "identity";

/// Directory under `<mur_home>/keys` holding the human's private key.
pub const HUMAN_KEY_DIR: &str = ".human";

/// Directory under `<mur_home>` that holds every private signing key.
pub const KEYS_DIR: &str = "keys";

/// `<mur_home>/identity` — public, agent-readable, never agent-writable.
pub fn human_identity_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(HUMAN_IDENTITY_DIR)
}

/// `<mur_home>/keys/.human` — private, denied to every agent.
pub fn human_key_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(KEYS_DIR).join(HUMAN_KEY_DIR)
}

/// File name of the public key, inside [`human_identity_dir`].
pub const HUMAN_PUBKEY_FILE: &str = "identity.pub";

/// File name of the rotation chain, inside [`human_identity_dir`].
pub const HUMAN_ROTATIONS_FILE: &str = "rotations.jsonl";

/// File name of the private key, inside [`human_key_dir`].
pub const HUMAN_PRIVATE_KEY_FILE: &str = "identity.key";

/// Why the human's public identity could not be read.
///
/// `NotEnrolled` and `Unreadable` are separate on purpose (#1010): a sandbox
/// deny must never look like "no human yet", or a caller that enrolls on
/// absence would mint a second key over a perfectly good one.
#[derive(Debug, thiserror::Error)]
pub enum HumanIdentityError {
    #[error("no human identity enrolled ({path} is absent)")]
    NotEnrolled { path: PathBuf },
    #[error("human identity at {path} exists but is not readable: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("human identity at {path} is invalid: {detail}")]
    Invalid { path: PathBuf, detail: String },
}

/// The human's rotation chain, already checked by [`verify_chain`] and
/// matched against `identity.pub`. Holding one means both checks passed —
/// there is no constructor that skips them.
#[derive(Debug, Clone)]
pub struct RotationChain {
    entries: Vec<RotationAttestation>,
    head: ChainOutcome,
    head_pubkey: [u8; 32],
}

impl RotationChain {
    /// Every attestation, bootstrap first.
    pub fn entries(&self) -> &[RotationAttestation] {
        &self.entries
    }

    /// The current key. The only key that carries authority (D-9).
    pub fn head_pubkey(&self) -> [u8; 32] {
        self.head_pubkey
    }

    pub fn head_key_version(&self) -> u32 {
        self.head.head_key_version
    }
}

/// `<mur_home>/identity/identity.pub`
pub fn human_pubkey_path(mur_home: &Path) -> PathBuf {
    human_identity_dir(mur_home).join(HUMAN_PUBKEY_FILE)
}

/// `<mur_home>/identity/rotations.jsonl`
pub fn human_rotations_path(mur_home: &Path) -> PathBuf {
    human_identity_dir(mur_home).join(HUMAN_ROTATIONS_FILE)
}

/// `<mur_home>/keys/.human/identity.key`
pub fn human_private_key_path(mur_home: &Path) -> PathBuf {
    human_key_dir(mur_home).join(HUMAN_PRIVATE_KEY_FILE)
}

/// Read a file, keeping "absent" and "may not read" apart. `fs::metadata`
/// first because `Path::exists` folds every stat failure into `false`.
fn read_public(path: &Path) -> Result<String, HumanIdentityError> {
    let unreadable = |source| HumanIdentityError::Unreadable {
        path: path.to_path_buf(),
        source,
    };
    match fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(HumanIdentityError::NotEnrolled {
                path: path.to_path_buf(),
            });
        }
        Err(e) => return Err(unreadable(e)),
    }
    fs::read_to_string(path).map_err(unreadable)
}

fn invalid(path: &Path, detail: impl ToString) -> HumanIdentityError {
    HumanIdentityError::Invalid {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}

/// The human's current public key, from `identity/identity.pub`.
///
/// Reads `identity/` only — never `keys/`, which every agent is denied. This
/// is what lets a sandboxed verifier check a human signature.
pub fn load_pubkey(mur_home: &Path) -> Result<[u8; 32], HumanIdentityError> {
    let path = human_pubkey_path(mur_home);
    let text = read_public(&path)?;
    decode_pubkey(text.trim()).map_err(|e| invalid(&path, e))
}

/// The human's rotation chain, verified, with its head matched against
/// `identity.pub`.
///
/// Fails closed: a line that does not parse, a chain that does not verify, or
/// a head that disagrees with `identity.pub` is `Invalid` — never "skip the
/// bad line" (that is how a planted line would get through) and never "fall
/// back to identity.pub" (that is the single-host shortcut D-10 rules out).
pub fn rotation_chain(mur_home: &Path) -> Result<RotationChain, HumanIdentityError> {
    let path = human_rotations_path(mur_home);
    let text = read_public(&path)?;
    let entries = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str::<RotationAttestation>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(&path, e))?;
    let head = verify_chain(
        &entries,
        ChainOptions {
            allow_emergency: false,
        },
    )
    .map_err(|e: ChainError| invalid(&path, e))?;
    let head_pubkey = decode_pubkey(&head.head_pubkey).map_err(|e| invalid(&path, e))?;
    let current = load_pubkey(mur_home)?;
    if current != head_pubkey {
        return Err(invalid(
            &path,
            format!(
                "chain head (key_version {}) is not the key in {HUMAN_PUBKEY_FILE}",
                head.head_key_version
            ),
        ));
    }
    Ok(RotationChain {
        entries,
        head,
        head_pubkey,
    })
}

#[cfg(test)]
#[path = "human_tests.rs"]
mod human_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_dir_name_is_not_a_valid_agent_name() {
        assert!(crate::agent_name::validate_agent_name(HUMAN_KEY_DIR).is_err());
    }

    #[test]
    fn the_two_halves_never_share_a_directory() {
        let home = Path::new("/m");
        let public = human_identity_dir(home);
        let private = human_key_dir(home);
        assert!(!private.starts_with(&public) && !public.starts_with(&private));
    }
}
