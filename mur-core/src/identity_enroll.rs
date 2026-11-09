//! Human owner enrollment (at-mention phase 2, A1; D-1, D-8).
//!
//! The read side — layout, `load_pubkey`, the verified rotation chain — is in
//! `mur_common::identity::human`, which the runtime links. Minting lives here,
//! in `mur-core`, and only here: generating the human's key, saving it, and
//! writing the bootstrap attestation. The boundary that keeps an agent from
//! doing the same is the sandbox (A0: `identity/` is write-protected and
//! `keys/` is denied), not linkage.
//!
//! **Never mint a second key.** Every partial state fails closed with the
//! path named, so the owner repairs it by hand instead of a retry silently
//! replacing the key that already signed things.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::identity::human::{
    HumanIdentityError, human_identity_dir, human_key_dir, human_private_key_path,
    human_pubkey_path, human_rotations_path, load_pubkey, rotation_chain,
};
use mur_common::identity::{AgentIdentity, IdentityError, RotationAttestation, RotationReason};

/// What `enroll` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enrollment {
    /// No identity existed; both halves and the bootstrap attestation were
    /// written.
    Created { pubkey: [u8; 32] },
    /// A complete, consistent identity was already there. Nothing written.
    Existing { pubkey: [u8; 32] },
}

impl Enrollment {
    pub fn pubkey(&self) -> [u8; 32] {
        match self {
            Self::Created { pubkey } | Self::Existing { pubkey } => *pubkey,
        }
    }
}

/// Stat a path, keeping "absent" apart from "may not look" (#1010).
fn present(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| {
            format!(
                "cannot tell whether {} exists; refusing to enroll over it",
                path.display()
            )
        }),
    }
}

/// Make sure the human owner has an identity under `mur_home`, creating one
/// only when neither half exists.
///
/// | `identity/identity.pub` | `keys/.human/identity.key` | result |
/// |---|---|---|
/// | absent  | absent             | create both + bootstrap attestation v0 |
/// | present | present, matches   | `Existing` |
/// | present | absent/unreadable  | error: key missing, path named |
/// | absent  | present            | error: public half missing |
/// | present | present, mismatch  | error |
///
/// `Existing` also requires the rotation chain to verify and end at
/// `identity.pub`; a missing chain is the "crashed after the key, before the
/// attestation" state and is reported, not re-minted.
pub fn enroll(mur_home: &Path) -> Result<Enrollment> {
    let pub_path = human_pubkey_path(mur_home);
    let key_path = human_private_key_path(mur_home);
    match (present(&pub_path)?, present(&key_path)?) {
        (false, false) => create(mur_home),
        (true, true) => existing(mur_home),
        (true, false) => bail!(
            "human identity is incomplete: {} exists but the private key {} is missing. \
             Restore the key from backup; MUR will not mint a second one.",
            pub_path.display(),
            key_path.display()
        ),
        (false, true) => bail!(
            "human identity is incomplete: the private key {} exists but its public half {} \
             is missing. Restore it from backup; MUR will not mint a second key.",
            key_path.display(),
            pub_path.display()
        ),
    }
}

/// `enroll`, for call sites that need the human's key to proceed (D-2).
/// Fails loudly; never falls back to an unowned channel.
pub fn ensure_enrolled(mur_home: &Path) -> Result<[u8; 32]> {
    Ok(enroll(mur_home)?.pubkey())
}

fn existing(mur_home: &Path) -> Result<Enrollment> {
    let pubkey = load_pubkey(mur_home).context("read the human public key")?;
    let signer = HumanSigner::load(mur_home)?;
    if signer.pubkey() != pubkey {
        bail!(
            "human identity is inconsistent: {} does not match the private key {}. \
             MUR will not pick one; restore the matching pair from backup.",
            human_pubkey_path(mur_home).display(),
            human_private_key_path(mur_home).display()
        );
    }
    match rotation_chain(mur_home) {
        Ok(_) => Ok(Enrollment::Existing { pubkey }),
        Err(HumanIdentityError::NotEnrolled { path }) => bail!(
            "human identity is incomplete: both keys exist but the rotation chain {} is \
             missing (an enrollment was interrupted). MUR will not re-mint the key. If it \
             has never signed anything, remove both halves and enroll again.",
            path.display()
        ),
        Err(e) => Err(e).context("human rotation chain"),
    }
}

fn create(mur_home: &Path) -> Result<Enrollment> {
    let identity = AgentIdentity::generate();
    let pub_dir = human_identity_dir(mur_home);
    let key_dir = human_key_dir(mur_home);
    identity
        .save_split(&pub_dir, &key_dir)
        .context("save the human identity")?;
    restrict_dirs(&pub_dir, &key_dir)?;
    write_bootstrap(mur_home, &identity)?;
    Ok(Enrollment::Created {
        pubkey: identity.verifying_key_bytes(),
    })
}

/// Write the chain's seq-0 entry. Write-if-absent: the caller has already
/// established there is no chain to anchor.
pub(crate) fn write_bootstrap(mur_home: &Path, identity: &AgentIdentity) -> Result<()> {
    let path = human_rotations_path(mur_home);
    let entry = RotationAttestation::new(
        uuid::Uuid::now_v7().to_string(),
        "",
        identity.pubkey_text(),
        0,
        0,
        chrono::Utc::now().to_rfc3339(),
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let line = serde_json::to_string(&entry).context("serialize bootstrap attestation")?;
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, format!("{line}\n")).with_context(|| format!("write {}", tmp.display()))?;
    // A hard link fails with AlreadyExists instead of replacing — the
    // write-if-absent rename.
    let linked = fs::hard_link(&tmp, &path);
    let _ = fs::remove_file(&tmp);
    linked.with_context(|| format!("create {}", path.display()))
}

#[cfg(unix)]
fn restrict_dirs(pub_dir: &Path, key_dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for d in [pub_dir, key_dir] {
        fs::set_permissions(d, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", d.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_dirs(_: &Path, _: &Path) -> Result<()> {
    Ok(())
}

/// The human's signing key, loaded from `keys/.human/`.
///
/// Only a process outside the sandbox can build one: every agent is denied
/// `keys/`. Implements B3's `HumanSign` once that trait lands.
pub struct HumanSigner {
    identity: AgentIdentity,
}

impl HumanSigner {
    pub fn load(mur_home: &Path) -> Result<Self> {
        let key_dir: PathBuf = human_key_dir(mur_home);
        let pub_dir = human_identity_dir(mur_home);
        // `AgentIdentity::load` reads `private_key_dir(pub_dir)`, which is
        // `keys/.human` for `identity/` — and checks `identity.pub` matches.
        let identity = AgentIdentity::load(&pub_dir).map_err(|e| match e {
            IdentityError::NotFound => anyhow::anyhow!(
                "no human private key at {}",
                key_dir.join("identity.key").display()
            ),
            IdentityError::Denied(d) => anyhow::anyhow!(
                "the human private key is not readable here ({d}). Human writes need \
                 your own terminal, not an agent's sandbox."
            ),
            IdentityError::InvalidKey(d) => anyhow::anyhow!(
                "human identity is inconsistent: {d} ({}). MUR will not pick one; restore \
                 the matching pair from backup.",
                key_dir.display()
            ),
            other => anyhow::anyhow!("load the human private key: {other}"),
        })?;
        Ok(Self { identity })
    }

    pub fn pubkey(&self) -> [u8; 32] {
        self.identity.verifying_key_bytes()
    }

    pub(crate) fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        self.identity.signing_key()
    }

    pub fn sign_bytes(&self, msg: &[u8]) -> [u8; 64] {
        self.identity.sign_bytes(msg)
    }
}

#[cfg(test)]
#[path = "identity_enroll_tests.rs"]
mod tests;
