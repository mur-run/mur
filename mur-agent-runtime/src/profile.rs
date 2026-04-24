//! Profile loader with {{agent_home}} expansion + sha256 digest.

use mur_common::AgentProfile;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ProfileLoadError {
    #[error("profile not found at {0}")]
    NotFound(PathBuf),
    #[error("read error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML parse error: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
    #[error("validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub inner: AgentProfile,
    pub agent_home: PathBuf,
    pub digest: String,
    pub raw_yaml: String, // retained for re-emit / yaml_edit round-trip
}

impl Profile {
    pub fn load(agent_home: &Path) -> Result<Self, ProfileLoadError> {
        let path = agent_home.join("profile.yaml");
        if !path.exists() {
            return Err(ProfileLoadError::NotFound(path));
        }
        let raw_yaml = fs::read_to_string(&path)?;
        let expanded = expand_template(&raw_yaml, agent_home);
        let inner: AgentProfile = serde_yaml_ng::from_str(&expanded)?;
        validate_uuid_v7(&inner.id)?;
        validate_filesystem_paths(&inner, agent_home)?;
        let digest = compute_digest(&expanded);
        Ok(Self {
            inner,
            agent_home: agent_home.to_path_buf(),
            digest,
            raw_yaml,
        })
    }
}

fn expand_template(yaml: &str, agent_home: &Path) -> String {
    // Escape backslashes so Windows paths (e.g. `C:\Users\...`) survive YAML
    // double-quoted scalars, where `\U`/`\A`/... would be interpreted as escape
    // sequences. `{{agent_home}}` is expected to appear inside double-quoted
    // scalars; plain or single-quoted scalars are not supported.
    let home_str = agent_home.to_string_lossy().replace('\\', "\\\\");
    yaml.replace("{{agent_home}}", &home_str)
}

fn validate_uuid_v7(id: &str) -> Result<(), ProfileLoadError> {
    let u = uuid::Uuid::parse_str(id)
        .map_err(|e| ProfileLoadError::Validation(format!("profile.id: {e}")))?;
    if u.get_version_num() != 7 {
        return Err(ProfileLoadError::Validation(
            "profile.id must be UUIDv7".to_string(),
        ));
    }
    Ok(())
}

fn validate_filesystem_paths(p: &AgentProfile, _agent_home: &Path) -> Result<(), ProfileLoadError> {
    for entry in &p.entitlements.filesystem.read {
        if entry.starts_with('/') && !entry.contains('*') {
            let path = Path::new(entry);
            if !path.exists() {
                return Err(ProfileLoadError::Validation(format!(
                    "entitlements.filesystem.read: path '{entry}' does not exist"
                )));
            }
        }
    }
    for entry in &p.entitlements.filesystem.deny {
        if !entry.starts_with('~') && !entry.starts_with('/') {
            return Err(ProfileLoadError::Validation(format!(
                "entitlements.filesystem.deny: '{entry}' must be absolute or ~-prefixed"
            )));
        }
    }
    Ok(())
}

fn compute_digest(canonical_yaml: &str) -> String {
    let mut h = Sha256::new();
    h.update(canonical_yaml.as_bytes());
    format!("sha256:{:x}", h.finalize())
}
