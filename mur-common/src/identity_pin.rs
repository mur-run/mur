//! Out-of-tree pin of an agent's public identity (#2318).
//!
//! `identity.pub` and `rotations.jsonl` live in the agent's own home, which
//! the agent's sandbox grants write to whole. macOS re-denies those two files
//! in SBPL; Landlock (Linux) cannot carve a single file out of a granted
//! directory, so there the agent's `bash` can rewrite them. `verify_chain` has
//! no trust anchor of its own, so a chain the agent forged from a key it
//! generated would verify, and every verifier would accept that key as the
//! agent's identity.
//!
//! The pin is the anchor. Trusted writers (`mur agent create`, `rekey`,
//! import, clone, and the runtime's pre-seal trust-on-first-use) copy the
//! identity to `<mur_home>/identity-pins/<name>.json`, a directory no agent
//! sandbox may write (see `LaunchChain::deny_paths`). Verifiers of a pinned
//! agent read the pin and never the agent's own copies, so a forged chain or a
//! rewritten `identity.pub` changes nothing anyone trusts.
//!
//! Same mechanism and the same trust-on-first-use caveat as the entitlement
//! pin (`crate::entitlements_pin`, #712).

use std::io;
use std::path::{Path, PathBuf};

use crate::identity::{ChainOptions, RotationAttestation, verify_chain};

/// Directory under `<mur_home>` holding one identity pin per agent.
pub const IDENTITY_PINS_DIR: &str = "identity-pins";

/// The agent-home files the pin is taken from.
pub const PUBKEY_FILE: &str = "identity.pub";
pub const CHAIN_FILE: &str = "rotations.jsonl";

const PIN_EXT: &str = "json";
const PIN_VERSION: u32 = 1;

/// The pinned identity: the trusted current key and the trusted chain that
/// leads to it (empty for an identity minted without one, e.g. an import).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IdentityPin {
    pub version: u32,
    pub pubkey: String,
    pub key_version: u32,
    pub chain: Vec<RotationAttestation>,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityPinError {
    #[error("io error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid identity pin {path}: {msg}")]
    Invalid { path: PathBuf, msg: String },
    #[error("identity of '{agent}' is inconsistent: {msg}")]
    Inconsistent { agent: String, msg: String },
    #[error("invalid agent name '{0}'")]
    Name(String),
}

/// What a verifier should trust for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedIdentity {
    /// The trusted current pubkey (multibase).
    pub head_pubkey: String,
    pub head_key_version: u32,
    /// A strictly-verified chain covering `head_key_version`, when one exists.
    /// `None` means only the head key is known (no chain, or the pinned chain
    /// itself does not verify strictly, e.g. an unapproved emergency rekey).
    pub chain: Option<Vec<RotationAttestation>>,
}

/// `<mur_home>/identity-pins/<name>.json`.
pub fn pin_path(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home
        .join(IDENTITY_PINS_DIR)
        .join(format!("{agent}.{PIN_EXT}"))
}

fn checked_name(agent: &str) -> Result<(), IdentityPinError> {
    crate::agent_name::validate_agent_name(agent)
        .map_err(|_| IdentityPinError::Name(agent.to_string()))
}

/// `(mur_home, name)` when `agent_home` is `<mur_home>/agents/<name>`. Only an
/// installed agent has a pin; any other directory (an embedded runtime's cache
/// dir, a test fixture) keeps the legacy read-from-disk behavior.
pub fn split_agent_home(agent_home: &Path) -> Option<(&Path, &str)> {
    let parent = agent_home.parent()?;
    if parent.file_name()? != "agents" {
        return None;
    }
    let name = agent_home.file_name()?.to_str()?;
    crate::agent_name::validate_agent_name(name).ok()?;
    Some((parent.parent()?, name))
}

/// Parse `rotations.jsonl` text strictly: one bad line rejects the whole file.
pub fn parse_chain(text: &str) -> Option<Vec<RotationAttestation>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .ok()
}

/// Pin the identity currently on disk in `<mur_home>/agents/<agent>`.
///
/// Only trusted writers call this: the CLI right after it minted or rotated
/// the key, and the runtime before its sandbox seals. A chain whose head does
/// not match `identity.pub` is refused rather than pinned.
pub fn pin_from_disk(mur_home: &Path, agent: &str) -> Result<IdentityPin, IdentityPinError> {
    checked_name(agent)?;
    let home = mur_home.join("agents").join(agent);
    let pub_path = home.join(PUBKEY_FILE);
    let pubkey = std::fs::read_to_string(&pub_path)
        .map_err(|source| IdentityPinError::Io {
            path: pub_path.clone(),
            source,
        })?
        .trim()
        .to_string();
    let chain_path = home.join(CHAIN_FILE);
    let chain = match std::fs::read_to_string(&chain_path) {
        Ok(text) => parse_chain(&text).ok_or_else(|| IdentityPinError::Invalid {
            path: chain_path.clone(),
            msg: "unparseable rotation attestation".into(),
        })?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(IdentityPinError::Io {
                path: chain_path,
                source,
            });
        }
    };
    let key_version = match chain.last() {
        Some(head) if head.new_pubkey != pubkey => {
            return Err(IdentityPinError::Inconsistent {
                agent: agent.to_string(),
                msg: format!("{CHAIN_FILE} head is not {PUBKEY_FILE}"),
            });
        }
        Some(head) => head.new_key_version,
        None => 0,
    };
    let pin = IdentityPin {
        version: PIN_VERSION,
        pubkey,
        key_version,
        chain,
    };
    write_pin(mur_home, agent, &pin)?;
    Ok(pin)
}

/// Temp file + rename, so a crash never leaves a torn pin.
fn write_pin(mur_home: &Path, agent: &str, pin: &IdentityPin) -> Result<(), IdentityPinError> {
    let dir = mur_home.join(IDENTITY_PINS_DIR);
    let path = pin_path(mur_home, agent);
    let io_err = |source| IdentityPinError::Io {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    let bytes = crate::jcs::to_jcs_for(pin);
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(io_err)?;
    io::Write::write_all(&mut tmp, &bytes).map_err(io_err)?;
    tmp.persist(&path).map_err(|e| io_err(e.error))?;
    Ok(())
}

/// The pin for `agent`, or `None` when there is none yet.
pub fn load_pin(mur_home: &Path, agent: &str) -> Result<Option<IdentityPin>, IdentityPinError> {
    checked_name(agent)?;
    let path = pin_path(mur_home, agent);
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(IdentityPinError::Io { path, source }),
    };
    let invalid = |msg: String| IdentityPinError::Invalid {
        path: path.clone(),
        msg,
    };
    let pin: IdentityPin = serde_json::from_slice(&raw).map_err(|e| invalid(e.to_string()))?;
    if pin.version != PIN_VERSION {
        return Err(invalid(format!("unsupported version {}", pin.version)));
    }
    Ok(Some(pin))
}

/// Drop `agent`'s pin (purge, re-seed). Missing is not an error.
pub fn remove_pin(mur_home: &Path, agent: &str) -> Result<(), IdentityPinError> {
    checked_name(agent)?;
    let path = pin_path(mur_home, agent);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IdentityPinError::Io { path, source }),
    }
}

/// Carry the pin across `mur agent rename`. No pin to move is not an error.
pub fn rename_pin(mur_home: &Path, old: &str, new: &str) -> Result<(), IdentityPinError> {
    checked_name(old)?;
    checked_name(new)?;
    let (from, to) = (pin_path(mur_home, old), pin_path(mur_home, new));
    match std::fs::rename(&from, &to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IdentityPinError::Io { path: from, source }),
    }
}

fn strict() -> ChainOptions {
    ChainOptions {
        allow_emergency: false,
    }
}

/// What to trust for the agent at `agent_home`, or `None` when it has no pin
/// (not an installed agent, or not pinned yet): the caller then falls back to
/// the legacy read of the agent's own files.
///
/// With a pin, the agent's own `identity.pub` / `rotations.jsonl` are not read
/// at all. Every legitimate change to them (create, rekey, import, clone)
/// re-pins in the same command, so a disk copy that differs from the pin was
/// written by something else.
///
/// An unreadable or invalid pin is present-but-unusable: resolving nothing
/// fails closed, where falling back to disk would hand the decision back to
/// the agent.
pub fn resolve(agent_home: &Path) -> Option<Option<TrustedIdentity>> {
    let (mur_home, name) = split_agent_home(agent_home)?;
    let pin = match load_pin(mur_home, name) {
        Ok(Some(pin)) => pin,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(agent = name, error = %e, "identity pin unusable; trusting no key");
            return Some(None);
        }
    };
    let chain = (!pin.chain.is_empty() && verify_chain(&pin.chain, strict()).is_ok())
        .then(|| pin.chain.clone());
    Some(Some(TrustedIdentity {
        head_pubkey: pin.pubkey,
        head_key_version: pin.key_version,
        chain,
    }))
}

#[cfg(test)]
mod tests;
