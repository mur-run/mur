//! Out-of-tree pin of an agent's `entitlements` (#712, Linux).
//!
//! On Linux, Landlock is allow-only: the agent's own home is granted whole, so
//! the agent's bash tool can rewrite its own `profile.yaml` — and with it the
//! entitlements the next start seals. macOS denies that write in SBPL; Linux
//! cannot express the carve-out. The pin closes it for both: the supervisor
//! refuses to start when the profile's entitlements differ from the copy kept
//! at `<mur_home>/entitlement-pins/<name>.json`, a directory no agent sandbox
//! grants write to (see `LaunchChain::deny_paths`).
//!
//! Scope is `entitlements` only, not the whole profile: model, skills and
//! companion fields are edited by hand and by many writers, and none of them
//! widens what the kernel lets the agent do.
//!
//! The pin stores the canonical JSON, not a hash. Comparison re-reads the
//! stored JSON through the CURRENT `Entitlements` schema, so a runtime upgrade
//! that adds a defaulted field compares equal instead of bricking every agent.

use std::io;
use std::path::{Path, PathBuf};

use crate::agent::{AgentProfile, Entitlements};

/// Directory under `<mur_home>` holding one pin per agent.
pub const PINS_DIR: &str = "entitlement-pins";

const PIN_EXT: &str = "json";

/// Pin file format version, for a future change of layout.
const PIN_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct PinFile {
    version: u32,
    entitlements: serde_json::Value,
}

/// `<mur_home>/entitlement-pins/<name>.json`.
pub fn pin_path(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home.join(PINS_DIR).join(format!("{agent}.{PIN_EXT}"))
}

/// Outcome of comparing a profile against its pin.
#[derive(Debug, PartialEq, Eq)]
pub enum PinCheck {
    /// The profile's entitlements equal the pinned ones.
    Match,
    /// No pin yet (install predates #712, or the pin was deleted).
    Missing,
    /// Entitlements differ. Lists the top-level entitlement keys that changed.
    Mismatch { changed: Vec<String> },
}

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("io error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid pin file {path}: {msg}")]
    Invalid { path: PathBuf, msg: String },
    #[error("invalid profile {path}: {msg}")]
    Profile { path: PathBuf, msg: String },
    #[error("invalid agent name '{0}'")]
    Name(String),
}

fn canonical(ent: &Entitlements) -> serde_json::Value {
    // Entitlements holds only plain data; serialization cannot fail.
    serde_json::to_value(ent).expect("Entitlements serializes to JSON")
}

fn checked_name(agent: &str) -> Result<(), PinError> {
    crate::agent_name::validate_agent_name(agent).map_err(|_| PinError::Name(agent.to_string()))
}

/// Pin `ent` for `agent`. Temp file + rename, so a crash never leaves a torn
/// pin that would block the next start.
pub fn write_pin(mur_home: &Path, agent: &str, ent: &Entitlements) -> Result<(), PinError> {
    checked_name(agent)?;
    let path = pin_path(mur_home, agent);
    let io_err = |source| PinError::Io {
        path: path.clone(),
        source,
    };
    let dir = mur_home.join(PINS_DIR);
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    let body = PinFile {
        version: PIN_VERSION,
        entitlements: canonical(ent),
    };
    let bytes = crate::jcs::to_jcs_for(&body);
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(io_err)?;
    io::Write::write_all(&mut tmp, &bytes).map_err(io_err)?;
    tmp.persist(&path).map_err(|e| io_err(e.error))?;
    Ok(())
}

/// Parse the entitlements out of raw (unexpanded) profile YAML.
///
/// Unexpanded on purpose: writers persist `{{agent_home}}` templates, and the
/// pin must compare what is on disk, not a machine-specific expansion.
pub fn entitlements_from_yaml(yaml: &str, path: &Path) -> Result<Entitlements, PinError> {
    serde_yaml_ng::from_str::<AgentProfile>(yaml)
        .map(|p| p.entitlements)
        .map_err(|e| PinError::Profile {
            path: path.to_path_buf(),
            msg: e.to_string(),
        })
}

/// Re-pin `agent` from its on-disk `profile.yaml`. Every trusted writer that
/// changes entitlements calls this after its write lands; so does
/// `mur agent perm reseal` after the user confirms a change.
pub fn repin_from_profile(mur_home: &Path, agent: &str) -> Result<(), PinError> {
    checked_name(agent)?;
    let path = mur_home.join("agents").join(agent).join("profile.yaml");
    let yaml = std::fs::read_to_string(&path).map_err(|source| PinError::Io {
        path: path.clone(),
        source,
    })?;
    let ent = entitlements_from_yaml(&yaml, &path)?;
    write_pin(mur_home, agent, &ent)
}

/// Advance the pin after a trusted writer saved `new`, but only from a trusted
/// state: the entitlements on disk BEFORE the write (`prior`) must match the
/// pin, or there must be no pin yet. Otherwise a tampered profile would be
/// laundered into the pin by the next unrelated save (`mur agent model set`
/// reads the tampered file and writes it back); instead the pin stays stale and
/// the next start refuses until the user runs `mur agent perm reseal`.
///
/// Returns whether the pin was advanced.
pub fn advance_pin(
    mur_home: &Path,
    agent: &str,
    prior: Option<&Entitlements>,
    new: &Entitlements,
) -> Result<bool, PinError> {
    let trusted = match prior {
        // Fresh profile (create / import): nothing on disk to have tampered.
        None => true,
        Some(p) => matches!(
            check(mur_home, agent, p)?,
            PinCheck::Match | PinCheck::Missing
        ),
    };
    if trusted {
        write_pin(mur_home, agent, new)?;
    }
    Ok(trusted)
}

/// Drop `agent`'s pin (purge), so a later agent of the same name starts clean
/// instead of refusing on a stranger's pin. Missing is not an error.
pub fn remove_pin(mur_home: &Path, agent: &str) -> Result<(), PinError> {
    checked_name(agent)?;
    let path = pin_path(mur_home, agent);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PinError::Io { path, source }),
    }
}

/// Carry the pin across `mur agent rename`. No pin to move is not an error.
pub fn rename_pin(mur_home: &Path, old: &str, new: &str) -> Result<(), PinError> {
    checked_name(old)?;
    checked_name(new)?;
    let (from, to) = (pin_path(mur_home, old), pin_path(mur_home, new));
    match std::fs::rename(&from, &to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PinError::Io { path: from, source }),
    }
}

/// Compare `ent` against the pin for `agent`.
pub fn check(mur_home: &Path, agent: &str, ent: &Entitlements) -> Result<PinCheck, PinError> {
    checked_name(agent)?;
    let path = pin_path(mur_home, agent);
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(PinCheck::Missing),
        Err(source) => return Err(PinError::Io { path, source }),
    };
    let invalid = |msg: String| PinError::Invalid {
        path: path.clone(),
        msg,
    };
    let file: PinFile = serde_json::from_slice(&raw).map_err(|e| invalid(e.to_string()))?;
    if file.version != PIN_VERSION {
        return Err(invalid(format!("unsupported version {}", file.version)));
    }
    // Round-trip through the current schema: fields added since the pin was
    // written take their defaults on both sides and compare equal.
    let pinned: Entitlements =
        serde_json::from_value(file.entitlements).map_err(|e| invalid(e.to_string()))?;
    let (a, b) = (canonical(&pinned), canonical(ent));
    if a == b {
        return Ok(PinCheck::Match);
    }
    Ok(PinCheck::Mismatch {
        changed: changed_keys(&a, &b),
    })
}

fn changed_keys(a: &serde_json::Value, b: &serde_json::Value) -> Vec<String> {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
        return vec!["entitlements".to_string()];
    };
    let mut keys: Vec<String> = a
        .keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests;
